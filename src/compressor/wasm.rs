//! Context models for WebAssembly modules in the stream.
//!
//! `WasmParser` follows the bytes of each module as they are coded, so that the models know what
//! the next byte is: which field, which byte of a LEB128 number, the opcodes before it, and the
//! previous value of the same field. It parses the sections the models gain most from, the code
//! and the exports, and takes the other sections as plain bytes. The JS decoder
//! (`js_source/wasm.js`) parses in exactly the same way, so keep the two in step.

use std::{cell::RefCell, rc::Rc};

use super::context_map::{ContextStats, SharedTable};
use super::model::Model;

// Field kinds: what the next byte of a module is. The kinds below FIRST_LEB are single bytes
// (or groups of them), the others LEB128 numbers. The numbers are shared with wasm.js.
pub const OUT: u8 = 0;
const HDR: u8 = 1;
const SEC_ID: u8 = 2;
const NAME: u8 = 3;
const RAW: u8 = 4;
const VALTYPE: u8 = 6;
const EXPORT_KIND: u8 = 11;
const OPCODE: u8 = 12;
const F32: u8 = 14;
const F64: u8 = 15;
const V128: u8 = 16;
const LANE: u8 = 17;
const FIRST_LEB: u8 = 19;
const SEC_SIZE: u8 = 19;
const COUNT: u8 = 20;
const NAME_LEN: u8 = 21;
const TYPEIDX: u8 = 24;
const INDEX: u8 = 26;
const BODY_SIZE: u8 = 27;
const LOCAL_DECLS: u8 = 28;
const LOCAL_N: u8 = 29;
const SUB_OP: u8 = 30;
const LABEL: u8 = 32;
const BR_COUNT: u8 = 33;
const TABLEIDX: u8 = 35;
const ALIGN: u8 = 38;
const OFFSET: u8 = 39;
const MEMIDX: u8 = 40;
const NUM_KINDS: usize = 43;

/// The kind of the immediate of the opcodes up to 0x44, as `48 + kind`; `o` is call_indirect,
/// a type index and a table index. The other kinds: 31 block type, 34 function index,
/// 36 local index, 37 global index, 41 i32, 42 i64.
const IMMEDIATES: &[u8] = b"00OOO0000000PPQ0RoRoHH0000000000TTTUUSS0VVVVVVVVVVVVVVVVVVVVVVVXXYZ>?";

/// Counts read from the module are capped, so that a damaged module can't blow up the parser.
const MAX_COUNT: u32 = 1 << 24;

fn mix(hash: u32, value: u32) -> u32 {
    let hash = (hash ^ value).wrapping_mul(0x9E3779B1);
    hash ^ (hash >> 15)
}

/// Follows the modules in the stream byte by byte.
pub struct WasmParser {
    /// Stream ranges `[start, end)` of the modules
    ranges: Vec<(u32, u32)>,
    /// Stream position of the next byte
    pos: u32,
    /// Fields still to come, the next one last, with how many times it repeats
    stack: Vec<(u8, u32)>,
    sec: u32,
    sec_end: u32,
    body_end: u32,
    prefix: u32,
    /// Value of the LEB128 number read so far, and how many of its bytes
    leb: u32,
    leb_bytes: u32,
    /// Bytes read of the current field
    field_pos: u32,
    /// The last eight bytes, the last one first
    bytes: [u32; 8],
    /// The last three opcodes, the last one first
    ops: [u32; 3],
    /// The last value of each field kind, and the one before
    last: [u32; NUM_KINDS],
    last2: [u32; NUM_KINDS],
    /// The byte at each position of the last field of each kind
    column: [u8; NUM_KINDS * 32],
    /// Whether a WasmContext runs the parser
    has_runner: bool,
}

impl WasmParser {
    pub fn new(ranges: Vec<(u32, u32)>) -> Self {
        let starts_at_0 = ranges.iter().any(|range| range.0 == 0);
        let mut parser = Self {
            ranges,
            pos: 0,
            stack: Vec::new(),
            sec: 0,
            sec_end: 0,
            body_end: 0,
            prefix: 0,
            leb: 0,
            leb_bytes: 0,
            field_pos: 0,
            bytes: [0; 8],
            ops: [0; 3],
            last: [0; NUM_KINDS],
            last2: [0; NUM_KINDS],
            column: [0; NUM_KINDS * 32],
            has_runner: false,
        };
        if starts_at_0 {
            parser.push(HDR, 8);
        }
        parser
    }

    /// Kind of the next byte, or `OUT` outside the modules.
    fn kind(&self) -> u8 {
        self.stack.last().map_or(OUT, |entry| entry.0)
    }

    /// Byte index in the next field: of the LEB128 number, or of the group of bytes.
    fn index(&self) -> u32 {
        if self.kind() >= FIRST_LEB {
            self.leb_bytes.min(31)
        } else {
            self.field_pos.min(31)
        }
    }

    fn push(&mut self, kind: u8, count: u32) {
        if count > 0 {
            self.stack.push((kind, count.min(MAX_COUNT)));
            self.field_pos = 0;
        }
    }

    /// Learns the next byte.
    fn update(&mut self, byte: u8) {
        let kind = self.kind();
        let pos = self.pos + 1;
        if kind != OUT {
            let index = self.index() as usize;
            let k = kind as usize;
            self.column[k * 32 + index] = byte;
            let mut done = true;
            let mut value = byte as u32;
            if kind >= FIRST_LEB {
                if self.leb_bytes < 5 {
                    self.leb |= ((byte & 127) as u32) << (7 * self.leb_bytes);
                }
                self.leb_bytes += 1;
                value = self.leb;
                done = byte < 128;
            }
            self.field_pos += 1;
            if done {
                self.leb = 0;
                self.leb_bytes = 0;
                self.last2[k] = self.last[k];
                self.last[k] = value;
                let top = self.stack.len() - 1;
                if self.stack[top].1 > 1 {
                    self.stack[top].1 -= 1;
                } else {
                    self.stack.pop();
                    self.field_pos = 0;
                }
                self.follow(kind, value, pos);
            }
        }
        self.bytes.rotate_right(1);
        self.bytes[0] = byte as u32;
        self.pos = pos;

        if self.ranges.iter().any(|range| range.0 == pos) {
            self.stack.clear();
            self.sec_end = 0;
            self.push(HDR, 8);
        } else if self.ranges.iter().any(|range| range.1 == pos) {
            self.stack.clear();
        } else if kind != OUT && self.kind() != HDR {
            if pos == self.sec_end || (self.sec == 10 && pos == self.body_end) {
                self.stack.clear();
            }
            // Push the next record of the section, instruction of the function, or section
            if self.stack.is_empty() {
                if pos >= self.sec_end {
                    self.push(SEC_ID, 1);
                } else if self.sec == 7 {
                    self.push(INDEX, 1);
                    self.push(EXPORT_KIND, 1);
                    self.push(NAME_LEN, 1);
                } else if self.sec == 10 {
                    self.push(if pos < self.body_end { OPCODE } else { BODY_SIZE }, 1);
                } else {
                    self.push(RAW, 1);
                }
            }
        }
    }

    /// Pushes the fields that follow a field with this value. `pos` is where they start.
    fn follow(&mut self, kind: u8, value: u32, pos: u32) {
        let count = value.min(MAX_COUNT);
        match kind {
            SEC_ID => {
                self.sec = value;
                self.push(SEC_SIZE, 1);
            }
            SEC_SIZE => {
                self.sec_end = pos + count;
                if self.sec == 7 || self.sec == 10 {
                    self.push(COUNT, 1);
                }
            }
            NAME_LEN => self.push(NAME, count),
            BODY_SIZE => {
                self.body_end = pos + count;
                self.push(LOCAL_DECLS, 1);
            }
            LOCAL_DECLS => self.push(LOCAL_N, count),
            LOCAL_N => self.push(VALTYPE, 1),
            OPCODE => {
                self.ops = [value, self.ops[0], self.ops[1]];
                let mut immediate = IMMEDIATES
                    .get(value as usize)
                    .map_or(0, |&c| c as u32 - 48);
                if immediate == 63 {
                    self.push(TABLEIDX, 1);
                    immediate = TYPEIDX as u32;
                }
                if (0xfc..=0xfe).contains(&value) {
                    self.prefix = value;
                    immediate = SUB_OP as u32;
                }
                if immediate != 0 {
                    let count = match value {
                        0x43 => 4,
                        0x44 => 8,
                        _ => 1,
                    };
                    self.push(immediate as u8, count);
                }
            }
            SUB_OP => {
                self.ops[0] = (self.prefix << 9) | (value & 511);
                if self.prefix == 0xfc {
                    // memory.copy and the like: approximated as two indexes for the even ones
                    if (8..18).contains(&value) {
                        self.push(MEMIDX, 2 - (value & 1));
                    }
                } else if value < 12 || value == 92 || value == 93 {
                    self.push(ALIGN, 1);
                } else if value == 12 || value == 13 {
                    // v128.const and i8x16.shuffle
                    self.push(4 + value as u8, 16);
                } else if (21..35).contains(&value) {
                    self.push(LANE, 1);
                } else if (84..92).contains(&value) {
                    self.push(LANE, 1);
                    self.push(ALIGN, 1);
                }
            }
            BR_COUNT => self.push(LABEL, count + 1),
            ALIGN => self.push(OFFSET, 1),
            _ => {}
        }
    }
}

const _: () = assert!(V128 == 4 + 12 && LANE == 4 + 13 && F32 == 14 && F64 == 15);

pub type SharedWasmParser = Rc<RefCell<WasmParser>>;

/// Predicts the bytes of the modules from what the parser knows about the next one. The bits
/// of `fields` choose what is in the context, besides the field kind, the byte index in the
/// field and the section: bits 0 to 7 the last eight bytes, as in the byte mask of NOrderByte,
/// 0x100, 0x200, 0x400 the last three opcodes, 0x800 the last value of the same kind of field,
/// 0x1000 the byte at the same position of the last field of the same kind, and 0x2000 the value
/// of that kind before the last. With 0x4000, the context of each bit is, while the byte so far
/// is the expected one, the number of bits and the next expected bit: the LEB128 byte of the
/// larger of the last two values plus one, or the byte of the last field at the same position.
///
/// The first one created runs the parser.
pub struct WasmContext {
    parser: SharedWasmParser,
    runs_parser: bool,
    stats: ContextStats,
    fields: u32,
    active: bool,
    ctx: u32,
    bit_ctx: u32,
    /// The byte expected next, with 256 set, or 0
    expected: u32,
}

impl WasmContext {
    pub fn new(
        parser: SharedWasmParser,
        hash_table: SharedTable,
        fields: u32,
        max_count: u32,
    ) -> Self {
        assert!(max_count <= 255);
        let runs_parser = !std::mem::replace(&mut parser.borrow_mut().has_runner, true);
        Self {
            parser,
            runs_parser,
            stats: ContextStats::new(hash_table, max_count),
            fields,
            active: false,
            ctx: 0,
            bit_ctx: 1,
            expected: 0,
        }
    }

    fn update_ctx(&mut self) {
        let parser = self.parser.borrow();
        let kind = parser.kind();
        let k = kind as usize;
        let index = parser.index();
        self.active = kind != OUT;
        let mut hash = mix(
            mix(self.fields + 0x5741534d, kind as u32 | index << 8),
            parser.sec,
        );
        let ops = &parser.ops;
        let values = parser.bytes.iter().chain(ops).copied().chain([
            parser.last[k],
            parser.column[k * 32 + index as usize] as u32,
            parser.last2[k],
        ]);
        for (i, value) in values.enumerate() {
            if self.fields >> i & 1 != 0 {
                hash = mix(hash, value);
            }
        }
        self.ctx = hash.wrapping_mul(0x2545F491);
        self.expected = 0;
        if self.fields & 0x4000 != 0 {
            self.expected = 256
                | if kind < FIRST_LEB {
                    parser.column[k * 32 + index as usize] as u32
                } else if index < 5 {
                    // The LEB128 byte of the larger of the last two values plus one
                    let value = parser.last[k].max(parser.last2[k]) as u64 + 1;
                    (value >> (7 * index) & 127) as u32
                        | if value >> (7 * index + 7) != 0 { 128 } else { 0 }
                } else {
                    0
                };
        }
    }

    /// The context of the next bit: while the bits of the byte are those of the expected byte,
    /// how many there are and the next expected bit, and otherwise the bits.
    fn key(&self) -> u32 {
        let bits = 31 - self.bit_ctx.leading_zeros();
        if self.expected >> (8 - bits) == self.bit_ctx {
            256 | bits << 1 | (self.expected >> (7 - bits) & 1)
        } else {
            self.bit_ctx
        }
    }
}

impl Model for WasmContext {
    fn state(&self) -> u32 {
        self.stats.count()
    }

    fn pred_inputs(&mut self, out: &mut Vec<f64>) {
        if !self.active {
            return self.stats.pred_none(out);
        }
        self.stats.pred(self.ctx ^ self.key(), self.bit_ctx, out);
    }

    fn learn(&mut self, bit: u8) {
        if self.active {
            self.stats.update(bit);
        }
        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            if self.active {
                self.stats.byte_end(self.bit_ctx & 255);
            }
            if self.runs_parser {
                self.parser.borrow_mut().update(self.bit_ctx as u8);
            }
            self.bit_ctx = 1;
            self.update_ctx();
        }
    }
}
