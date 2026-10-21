use super::utils::{prob_stretch, U24_MAX};
use std::{cell::RefCell, rc::Rc};

/// The counters of all models that hash their contexts. A slot is two words: the probability
/// (24 bits) and count (8 bits) of a context, and the check of its key (12 bits), run count
/// (4 bits), bit history state (8 bits) and run byte (8 bits). A key
/// whose check differs from that of its slot takes the slot over with fresh statistics, which
/// predicts better than sharing them with another context. `counters.js` in the JS decoder
/// computes the same.
pub struct ContextTable {
    pub slots: Vec<u32>,
    bits: u32,
    /// Added to the count of a counter when it learns: the step to the bit is 1 / (count + offset)
    pub count_offset: f64,
    /// Multiplies the inputs that `ContextStats` gives the mixer
    pub input_scale: f64,
}

pub type SharedTable = Rc<RefCell<ContextTable>>;

impl ContextTable {
    pub fn new(pow2_size: u32) -> Self {
        tracing::debug!("Context table size: {} MiB", 8usize << pow2_size >> 20);
        Self {
            slots: vec![0; 2 << pow2_size],
            bits: pow2_size,
            count_offset: 0.2,
            input_scale: 1.,
        }
    }

    /// The index of the first word of the slot of `key`
    pub fn find(&mut self, key: u32) -> usize {
        // Never 0, so an unused slot has no context
        let check = key.wrapping_mul(0x2545F491) >> 20 | 1;
        let slot = ((key ^ key >> 15).wrapping_mul(0x2C1B3C6D) >> (32 - self.bits)) as usize * 2;
        if self.slots[slot + 1] >> 20 != check {
            self.slots[slot] = U24_MAX >> 1;
            self.slots[slot + 1] = check << 20;
        }
        slot
    }

    /// Moves the counter at word `i` towards `bit`, by less the more often it has learnt, up to
    /// `max_count` times
    pub fn learn(&mut self, i: usize, bit: u8, max_count: u32) {
        self.slots[i] = update_counter(self.slots[i], bit, max_count, self.count_offset);
    }
}

/// A counter, probability (24 bits) and count (8 bits), after learning `bit`
pub fn update_counter(counter: u32, bit: u8, max_count: u32, count_offset: f64) -> u32 {
    let mut count = counter >> 24;
    if count < max_count {
        count += 1;
    }
    let prob = (counter & U24_MAX) as i32;
    let prob = prob
        + (U24_MAX as f64 * ((bit as f64 - prob as f64 / U24_MAX as f64) / (count as f64 + count_offset)))
            as i32;
    prob as u32 & U24_MAX | count << 24
}

/// The bit history state after `bit`: the last 7 bits of the context after a leading 1
fn next_state(state: u32, bit: u8) -> u32 {
    let state = state.max(1) << 1 | bit as u32;
    if state > 255 {
        state & 127 | 128
    } else {
        state
    }
}

/// Inputs that a model with `ContextStats` gives the mixer for each bit
pub const CONTEXT_INPUTS: usize = 5;

/// What a model knows of its contexts in the `ContextTable`, and the inputs it gives the mixer.
/// The mixer learns better with smaller inputs, so most are scaled down:
/// - the stretched probability of the context, at half scale, and the probability minus 1/2;
/// - the stretched probability that the bit history of the context, kept in its slot, has had
///   in the contexts of the model, by bit history and byte so far (a StateMap);
/// - the run of the context of the byte: while the byte so far agrees with the last byte seen in
///   that context, its next bit, weighted by how many times in a row that byte came;
/// - 1 for a context not seen before, and 0 otherwise.
pub struct ContextStats {
    table: SharedTable,
    max_count: u32,
    slot: usize,
    /// Counters by bit history state and byte so far
    states: Vec<u32>,
    state_idx: usize,
    /// Slot of the first bit of the byte, which keeps the run, and its check
    run_slot: usize,
    run_check: u32,
    run_byte: u32,
    run_count: u32,
    /// The count of the counter of the bit, 0 for a bit the model does not predict
    count: u32,
}

impl ContextStats {
    pub fn new(table: SharedTable, max_count: u32) -> Self {
        assert!(max_count <= 255);
        Self {
            table,
            max_count,
            slot: 0,
            states: vec![U24_MAX >> 1; 1 << 16],
            state_idx: 0,
            run_slot: 0,
            run_check: 0,
            run_byte: 0,
            run_count: 0,
            count: 0,
        }
    }

    /// Pushes zero inputs, for a bit the model does not predict
    pub fn pred_none(&mut self, out: &mut Vec<f64>) {
        self.count = 0;
        out.extend([0.; CONTEXT_INPUTS]);
    }

    /// The count of the counter of the bit of the last `pred`, the `Model::state` of the model
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Pushes the inputs for the bit with context `key`, where `bit_ctx` is the byte so far
    /// after a leading 1
    pub fn pred(&mut self, key: u32, bit_ctx: u32, out: &mut Vec<f64>) {
        let mut table = self.table.borrow_mut();
        self.slot = table.find(key);
        let word = table.slots[self.slot];
        let info = table.slots[self.slot + 1];
        self.count = word >> 24;
        if bit_ctx == 1 {
            self.run_slot = self.slot;
            self.run_check = info >> 20;
            self.run_byte = info & 255;
            self.run_count = info >> 16 & 15;
        }
        // The scales as constants, in which the JS decoder gets them
        let scale = table.input_scale;
        let p = (word & U24_MAX) as f64 / U24_MAX as f64;
        out.push(prob_stretch(p) * (0.5 * scale));
        out.push((p - 0.5) * (3. * scale));
        self.state_idx = ((info >> 8 & 255) << 8 | bit_ctx) as usize;
        out.push(
            prob_stretch((self.states[self.state_idx] & U24_MAX) as f64 / U24_MAX as f64)
                * (0.4 * scale),
        );
        let bits = 31 - bit_ctx.leading_zeros();
        out.push(
            if self.run_count != 0 && (self.run_byte | 256) >> (8 - bits) == bit_ctx {
                if self.run_byte >> (7 - bits) & 1 != 0 {
                    self.run_count as f64 * (0.25 * scale)
                } else {
                    -(self.run_count as f64) * (0.25 * scale)
                }
            } else {
                0.
            },
        );
        out.push(if word >> 24 == 0 { scale } else { 0. });
    }

    pub fn update(&mut self, bit: u8) {
        let mut table = self.table.borrow_mut();
        table.learn(self.slot, bit, self.max_count);
        let info = table.slots[self.slot + 1];
        table.slots[self.slot + 1] = info & !0xff00 | next_state(info >> 8 & 255, bit) << 8;
        let state = &mut self.states[self.state_idx];
        *state = update_counter(*state, bit, 127, table.count_offset);
    }

    /// Updates the run of the context of the byte that ended, if no other context took its slot
    pub fn byte_end(&mut self, byte: u32) {
        let mut table = self.table.borrow_mut();
        let info = table.slots[self.run_slot + 1];
        if info >> 20 == self.run_check {
            let count = if self.run_byte == byte {
                (self.run_count + 1).min(15)
            } else {
                1
            };
            table.slots[self.run_slot + 1] = info & 0xfff0ff00 | count << 16 | byte;
        }
    }
}
