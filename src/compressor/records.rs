//! Models for the byte ranges of binary files that hold records of a fixed size.
//!
//! The encoder finds the ranges (`detect_records`), and they travel in the output as
//! `[start, end, stride]`. The `Record` models predict only in them. The JS decoder
//! (`js_source/record_model.js`) makes the same predictions, so keep the two in step.

use std::rc::Rc;

use super::context_map::{ContextStats, SharedTable};
use super::model::Model;
use super::utils::prob_squash;

/// Bytes per block that gets a stride of its own. The ranges start and end at multiples of it.
pub const BLOCK: u32 = 512;
/// Smallest record size looked for.
const MIN_STRIDE: usize = 2;
/// Largest record size looked for.
const MAX_STRIDE: usize = 64;
/// A stride whose score is at most this much below the best stride's is preferred if it divides it.
const DIVISOR_TOLERANCE: f64 = 0.05;

/// The `[start, end, stride]` ranges of `stream` from `from` on that may hold records: each block
/// of `BLOCK` bytes of the stream with its record size, so the first one can start before `from`,
/// and the last one can end after the end of the stream. Adjacent blocks of one size are merged.
/// None if the stream has no bytes from `from` on.
pub fn detect_records(stream: &[u8], from: usize) -> Vec<[u32; 3]> {
    let mut ranges: Vec<[u32; 3]> = Vec::new();
    if from >= stream.len() {
        return ranges;
    }
    let block = BLOCK as usize;
    for start in (from / block * block..stream.len()).step_by(block) {
        let end = (start + block).min(stream.len());
        let stride = block_stride(stream, start, end);
        let (start, end) = (start as u32, start as u32 + BLOCK);
        match ranges.last_mut() {
            Some(last) if last[1] == start && last[2] == stride => last[1] = end,
            _ => ranges.push([start, end, stride]),
        }
    }
    ranges
}

/// Bits that `model` spends on each byte of `stream`.
pub fn byte_costs(mut model: Box<dyn Model>, stream: &[u8]) -> Vec<f64> {
    stream
        .iter()
        .map(|&byte| {
            (0..8)
                .rev()
                .map(|i| {
                    let prob = prob_squash(model.pred());
                    let bit = (byte >> i) & 1;
                    model.learn(bit);
                    -(if bit == 1 { prob } else { 1. - prob }).max(1e-9).log2()
                })
                .sum::<f64>()
        })
        .collect()
}

/// About how many bits a range costs in the range table of the decoder.
const RANGE_COST_BITS: f64 = 24.;

/// The parts of the `candidates` ranges where the record models save bits: `without` and `with`
/// are the bits spent on each byte of the stream without them and with them in the candidates.
/// A part is a block of the range, and adjacent blocks that save bits make a range of their own if
/// they save more than the range costs in the decoder.
pub fn select_records(candidates: &[[u32; 3]], without: &[f64], with: &[f64]) -> Vec<[u32; 3]> {
    let gain = |start: u32, end: u32| -> f64 {
        (start..end.min(with.len() as u32))
            .map(|i| without[i as usize] - with[i as usize])
            .sum()
    };
    let mut selected: Vec<([u32; 3], f64)> = Vec::new();
    for &[start, end, stride] in candidates {
        for block in (start..end).step_by(BLOCK as usize) {
            let block_end = block + BLOCK;
            let block_gain = gain(block, block_end);
            if block_gain <= 0. {
                continue;
            }
            match selected.last_mut() {
                Some((last, last_gain)) if last[1] == block && last[2] == stride => {
                    last[1] = block_end;
                    *last_gain += block_gain;
                }
                _ => selected.push(([block, block_end, stride], block_gain)),
            }
        }
    }
    selected
        .into_iter()
        .filter(|(_, gain)| *gain > RANGE_COST_BITS)
        .map(|(range, _)| range)
        .collect()
}

/// The record size of `data[start..end]`: the distance with the most equal bytes, or the
/// smallest divisor of it with almost as many.
fn block_stride(data: &[u8], start: usize, end: usize) -> u32 {
    let score = |d: usize| {
        let from = start.max(d);
        let same = (from..end).filter(|&i| data[i] == data[i - d]).count();
        same as f64 / (end - from).max(1) as f64
    };
    let scores: Vec<f64> = (1..=MAX_STRIDE).map(score).collect();
    let best = (MIN_STRIDE..=MAX_STRIDE)
        .max_by(|&a, &b| scores[a - 1].total_cmp(&scores[b - 1]).then(b.cmp(&a)))
        .unwrap();
    let stride = (MIN_STRIDE..=best)
        .find(|&d| best % d == 0 && scores[d - 1] >= scores[best - 1] - DIVISOR_TOLERANCE)
        .unwrap();
    stride as u32
}

/// Mixes `value` into the hash `h` of a record context, the same way as the JS decoder.
fn record_mix(h: u32, value: u32) -> u32 {
    (h ^ value).wrapping_mul(0x9E3779B1).rotate_left(13)
}

/// Finds the record range of each position of the stream, going forward.
pub struct RecordCursor {
    /// `[start, end, stride]` of each range, sorted by start
    ranges: Rc<Vec<[u32; 3]>>,
    idx: usize,
}

impl RecordCursor {
    pub fn new(ranges: Rc<Vec<[u32; 3]>>) -> Self {
        Self { ranges, idx: 0 }
    }

    /// `(start, stride)` of the range at `pos`, which must not be less than in the last call.
    pub fn at(&mut self, pos: u32) -> Option<(u32, u32)> {
        while self.idx < self.ranges.len() && pos >= self.ranges[self.idx][1] {
            self.idx += 1;
        }
        match self.ranges.get(self.idx) {
            Some(&[start, _, stride]) if pos >= start => Some((start, stride)),
            _ => None,
        }
    }
}

/// Predicts bits of the byte ranges that hold records of a fixed size, from the bytes at
/// given distances, which can be in records, and from the column in the record.
/// Outside the ranges it predicts nothing and learns nothing.
pub struct RecordModel {
    stats: ContextStats,
    dists: Vec<i32>,
    column: bool,
    delta: i32,
    magic: u32,
    records: RecordCursor,
    history: Vec<u8>,
    active: bool,
    ctx: u32,
    bit_ctx: u32,
    /// 1 followed by a bit per bit of the byte so far, which is 1 where it was the expected one
    match_ctx: u32,
    /// The expected byte: predicted with `delta`, 255 without
    expected: u32,
}

impl RecordModel {
    /// A positive distance is in bytes, a negative one in records.
    pub fn new(
        hash_table: SharedTable,
        ranges: Rc<Vec<[u32; 3]>>,
        dists: Vec<i32>,
        column: bool,
        delta: i32,
        max_count: u32,
    ) -> Self {
        assert!(max_count <= 255);
        let mut magic = record_mix(delta as u32, column as u32);
        for &d in &dists {
            magic = record_mix(magic, d as u32);
        }
        let mut model = Self {
            stats: ContextStats::new(hash_table, max_count),
            dists,
            column,
            delta,
            magic,
            records: RecordCursor::new(ranges),
            history: Vec::new(),
            active: false,
            ctx: 0,
            bit_ctx: 1,
            match_ctx: 1,
            expected: 0,
        };
        model.update_ctx();
        model
    }

    fn update_ctx(&mut self) {
        let pos = self.history.len() as u32;
        let Some((start, stride)) = self.records.at(pos) else {
            self.active = false;
            return;
        };
        self.active = true;
        let mut h = self.magic;
        if self.column {
            h = record_mix(h, (pos - start) % stride);
        }
        for &d in &self.dists {
            let back = if d > 0 { d as u32 } else { (-d) as u32 * stride };
            h = record_mix(
                h,
                if back <= pos {
                    self.history[(pos - back) as usize] as u32
                } else {
                    256
                },
            );
        }
        self.expected = 255;
        if self.delta != 0 {
            let Some((expected, k)) = self.predicted_byte(pos, start, stride) else {
                self.active = false;
                return;
            };
            self.expected = expected;
            h = record_mix(h, k);
        }
        self.ctx = record_mix(h, h >> 15);
    }
}

impl RecordModel {
    /// The byte at `pos` that continues the little endian words at the same place in the last
    /// records (`delta` bytes per word): linearly from two records if `delta` is positive, the same
    /// as in the last record if negative. Corrected by the bytes of the word that are known already.
    /// Returns the byte and its index in the word, or `None` without enough records.
    fn predicted_byte(&self, pos: u32, start: u32, stride: u32) -> Option<(u32, u32)> {
        let w = self.delta.unsigned_abs();
        let rec = start + (pos - start) / stride * stride;
        let ws = rec + (pos - rec) / w * w;
        let k = pos - ws;
        if ws + w > rec + stride || ws < start + stride * (1 + (self.delta > 0) as u32) {
            return None;
        }
        let word = |at: u32| {
            (0..w).fold(0u64, |v, i| v | (self.history[(at + i) as usize] as u64) << (8 * i))
        };
        let m = 1u64 << (8 * w);
        let a = word(ws - stride);
        let p = if self.delta > 0 {
            (2 * a + m - word(ws - 2 * stride)) % m
        } else {
            a
        };
        let km = 1u64 << (8 * k);
        let low = (0..k).fold(0u64, |v, i| v | (self.history[(ws + i) as usize] as u64) << (8 * i));
        let mut d = (low + km - p % km) % km;
        if k > 0 && d >= km / 2 {
            d = d + m - km;
        }
        Some(((((p + d) % m) >> (8 * k)) as u32 & 255, k))
    }
}

impl RecordModel {
    /// The bit of the expected byte at the next bit.
    fn expected_bit(&self) -> u8 {
        (self.expected >> (7 - self.bit_ctx.ilog2())) as u8 & 1
    }
}

/// The counters predict whether a bit is the one of the expected byte, which is 255 without
/// `delta`, at the key of the context and of which bits so far were the expected ones. The JS
/// decoder feeds a `CounterModel` with these bits.
impl Model for RecordModel {
    fn state(&self) -> u32 {
        self.stats.count()
    }

    fn pred_inputs(&mut self, out: &mut Vec<f64>) {
        if !self.active {
            return self.stats.pred_none(out);
        }
        let first = out.len();
        self.stats.pred(self.ctx ^ self.match_ctx, self.match_ctx, out);
        if self.expected_bit() == 0 {
            for p in &mut out[first..] {
                *p = -*p;
            }
        }
    }

    fn learn(&mut self, bit: u8) {
        let matched = (bit == self.expected_bit()) as u8;
        if self.active {
            self.stats.update(matched);
        }
        self.match_ctx = (self.match_ctx << 1) | matched as u32;
        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            if self.active {
                self.stats.byte_end(self.match_ctx & 255);
            }
            self.history.push(self.bit_ctx as u8);
            self.bit_ctx = 1;
            self.match_ctx = 1;
            self.update_ctx();
        }
    }
}
