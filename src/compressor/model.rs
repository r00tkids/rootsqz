use super::utils::{prob_squash, prob_stretch, U24_MAX};
use std::{
    cell::RefCell,
    ops::{Index, IndexMut},
    rc::Rc,
    vec,
};

pub trait Model {
    fn pred(&mut self) -> f64;
    fn learn(&mut self, bit: u8);
}

#[derive(Clone, Copy)]
pub struct NOrderByteData(u32);

impl Default for NOrderByteData {
    fn default() -> Self {
        // Start with half probability
        Self(U24_MAX >> 1)
    }
}

impl NOrderByteData {
    fn count(&self) -> u32 {
        (self.0 & 0xFF000000) >> 24
    }

    fn prob(&self) -> i32 {
        (self.0 & U24_MAX) as i32
    }

    fn set_count(&mut self, new_count: u32) {
        self.0 = ((new_count << 24) & 0xFF000000) | self.0 & U24_MAX;
    }

    fn set_prob(&mut self, new_prob: i32) {
        self.0 = (new_prob as u32 & U24_MAX) | (self.0 & 0xFF000000);
    }
}

pub struct HashTable<Record> {
    table: Vec<Record>,
    hash_mask: usize,
}

fn hash(mut value: u32, shift: u32) -> u32 {
    const K_MUL: u32 = 0x9E35A7BD;
    value ^= value >> shift;
    K_MUL.wrapping_mul(value) >> shift
}

impl<Record> HashTable<Record>
where
    Record: Default + Clone,
{
    pub fn new(pow2_size: u32) -> Self {
        let context_size = (1 << pow2_size) as usize;
        println!(
            "Hash table Size: {} MiB",
            (size_of::<Record>() * context_size) / (1024 * 1024)
        );

        Self {
            table: vec![Record::default(); context_size],
            hash_mask: context_size - 1,
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn get<'a>(&'a self, key: u32) -> &'a Record {
        &self.table[key as usize & self.hash_mask]
    }

    pub fn get_mut<'a>(&'a mut self, key: u32) -> &'a mut Record {
        &mut self.table[key as usize & self.hash_mask]
    }
}

/// NOrderByte model for byte predictions
/// Can describe [0, 8] order models and partial models
/// It also supports being a word model
/// (using characters as window filters)
pub struct NOrderByte {
    ctx: u32,
    hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
    max_count: u32,

    magic_num: u32,
    prev_bytes: u64,
    mask: u64,
    is_word_model: bool,
    class_order: u8,
    transitions: Option<Vec<u16>>,
    transition_mask: usize,

    bit_ctx: u32,
}

impl NOrderByte {
    pub fn new_norder_model(
        byte_mask: u8,
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
        max_count: u32,
    ) -> Self {
        assert!(max_count <= 255);

        let mut bit_mask: u64 = 0;
        for i in 0..8 {
            bit_mask |= ((byte_mask >> i) & 1) as u64 * (0xff << (i * 8));
        }

        Self {
            ctx: 0,
            bit_ctx: 1,
            magic_num: hash(byte_mask as u32, 2),
            max_count: max_count,
            hash_table: hash_table,
            prev_bytes: 0,
            mask: bit_mask,
            is_word_model: false,
            class_order: 0,
            transitions: None,
            transition_mask: 0,
        }
    }

    pub fn new_word_model(
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
        max_count: u32,
    ) -> Self {
        Self {
            ctx: 0,
            bit_ctx: 1,
            magic_num: hash(1337 as u32, 2),
            max_count: max_count,
            hash_table: hash_table,
            prev_bytes: 2166136261,
            mask: u64::MAX,
            is_word_model: true,
            class_order: 0,
            transitions: None,
            transition_mask: 0,
        }
    }

    pub fn new_char_class_model(
        order: u8,
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
        max_count: u32,
    ) -> Self {
        assert!((1..=8).contains(&order));
        assert!(max_count <= 255);
        Self {
            ctx: 0,
            bit_ctx: 1,
            magic_num: hash(0x434c4153 ^ order as u32, 2),
            max_count,
            hash_table,
            prev_bytes: 0,
            mask: (1u64 << (order * 3)) - 1,
            is_word_model: false,
            class_order: order,
            transitions: None,
            transition_mask: 0,
        }
    }

    /// Maps a recent byte suffix to its likely successor, then uses that successor
    /// as the bit model context. This differs from masking the recent bytes.
    pub fn new_indirect_model(
        context_bytes: u8,
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
        max_count: u32,
    ) -> Self {
        assert!((1..=3).contains(&context_bytes));
        let mut model = Self::new_norder_model(0, hash_table, max_count);
        let size = 1usize << (8 * context_bytes);
        model.magic_num = hash(9191, 2);
        model.transitions = Some(vec![0; size]);
        model.transition_mask = size - 1;
        model
    }
}

fn char_class(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => 1,
        b'0'..=b'9' => 2,
        b' ' | b'\t' | b'\r' | b'\n' => 3,
        b'\'' | b'"' | b'`' => 4,
        b'(' | b'[' | b'{' => 5,
        b')' | b']' | b'}' => 6,
        b'.' | b',' | b':' | b';' | b'+' | b'-' | b'*' | b'=' | b'<' | b'>' | b'!' | b'&'
        | b'|' | b'%' | b'^' | b'~' | b'?' | b'/' | b'\\' => 7,
        _ => 0,
    }
}

impl Model for NOrderByte {
    fn pred(&mut self) -> f64 {
        let entry = self
            .hash_table
            .borrow()
            .get(self.ctx ^ self.bit_ctx)
            .clone();

        prob_stretch(entry.prob() as f64 / U24_MAX as f64)
    }

    fn learn(&mut self, bit: u8) {
        {
            let mut hash_table = self.hash_table.borrow_mut();
            let inst = hash_table.get_mut(self.ctx ^ self.bit_ctx);

            let (mut count, mut prob) = (inst.count(), inst.prob());
            if count < self.max_count {
                count += 1;
            }

            let count_sqrt = count as f64 + 0.2;
            // Learning function
            prob += (U24_MAX as f64 * ((bit as f64 - (prob as f64 / U24_MAX as f64)) / count_sqrt))
                as i32;

            inst.set_count(count);
            inst.set_prob(prob);
        }

        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            let current_byte = self.bit_ctx & 0xff;

            if let Some(transitions) = &mut self.transitions {
                // A saturating vote keeps a stable candidate while allowing replacement.
                let old_context = self.prev_bytes as usize & self.transition_mask;
                let entry = &mut transitions[old_context];
                let candidate = (*entry & 255) as u8;
                let count = (*entry >> 8) as u8;
                *entry = if count == 0 || (count == 1 && candidate != current_byte as u8) {
                    256 | current_byte as u16
                } else if candidate == current_byte as u8 {
                    ((count.saturating_add(1) as u16) << 8) | current_byte as u16
                } else {
                    (((count - 1) as u16) << 8) | candidate as u16
                };
                self.prev_bytes = (self.prev_bytes << 8) | current_byte as u64;
            } else if self.is_word_model {
                let next_char = current_byte as u8 as char;
                if next_char.is_ascii_alphanumeric() || matches!(next_char, '_' | '.' | '[' | ']') {
                    self.prev_bytes = self.prev_bytes ^ next_char.to_ascii_lowercase() as u64;
                    self.prev_bytes = self.prev_bytes.wrapping_mul(16777619) >> 16;
                } else {
                    self.prev_bytes = 2166136261;
                }
            } else if self.class_order != 0 {
                self.prev_bytes = (self.prev_bytes << 3) | char_class(current_byte as u8) as u64;
            } else {
                self.prev_bytes = (self.prev_bytes << 8) | current_byte as u64;
            }

            let masked_prev_bytes = if let Some(transitions) = &self.transitions {
                let entry = transitions[self.prev_bytes as usize & self.transition_mask];
                (entry & 255) as u64
            } else {
                self.prev_bytes & self.mask
            };
            self.ctx = (hash((masked_prev_bytes >> 32) as u32, 3)
                .wrapping_mul(9)
                .wrapping_add(hash(masked_prev_bytes as u32, 3)))
            .wrapping_add(1) // To ensure ctx doesn't overlap between models
            .wrapping_mul(self.magic_num);

            // Reset bit_ctx
            self.bit_ctx = 1;
        }
    }
}

/// Predicts bits from the position in a lexical token and the last delimiter.
/// This state is independent of the Word model's rolling character hash.
pub struct TokenPosition {
    hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
    max_count: u32,
    context_bytes: u8,
    max_position: u8,
    kind: u8,
    position: u8,
    delimiter: u8,
    prev_bytes: u16,
    escaped: bool,
    bit_ctx: u32,
    ctx: u32,
}

impl TokenPosition {
    pub fn new(
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
        max_count: u32,
        context_bytes: u8,
        max_position: u8,
    ) -> Self {
        assert!(max_count <= 255);
        assert!(context_bytes <= 2);
        assert!(max_position > 0);
        Self {
            hash_table,
            max_count,
            context_bytes,
            max_position,
            kind: 0,
            position: 0,
            delimiter: 0,
            prev_bytes: 0,
            escaped: false,
            bit_ctx: 1,
            ctx: 0,
        }
    }

    fn advance(&mut self, byte: u8) {
        if self.kind >= 3 {
            let quote = match self.kind {
                3 => b'\'',
                4 => b'"',
                _ => b'`',
            };
            if self.escaped {
                self.escaped = false;
                self.position = self.position.saturating_add(1).min(self.max_position);
            } else if byte == b'\\' {
                self.escaped = true;
                self.position = self.position.saturating_add(1).min(self.max_position);
            } else if byte == quote {
                self.kind = 0;
                self.position = 0;
                self.delimiter = byte;
            } else {
                self.position = self.position.saturating_add(1).min(self.max_position);
            }
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            self.kind = match byte {
                b'\'' => 3,
                b'"' => 4,
                _ => 5,
            };
            self.position = 0;
        } else if byte.is_ascii_alphabetic()
            || byte == b'_'
            || byte == b'$'
            || (self.kind == 1 && byte.is_ascii_digit())
        {
            self.position = if self.kind == 1 {
                self.position.saturating_add(1).min(self.max_position)
            } else {
                1
            };
            self.kind = 1;
        } else if byte.is_ascii_digit() {
            self.position = if self.kind == 2 {
                self.position.saturating_add(1).min(self.max_position)
            } else {
                1
            };
            self.kind = 2;
        } else {
            self.kind = 0;
            self.position = 0;
            if !byte.is_ascii_whitespace() {
                self.delimiter = byte;
            }
        }
        self.prev_bytes = (self.prev_bytes << 8) | byte as u16;
        let recent = match self.context_bytes {
            0 => 0,
            1 => self.prev_bytes & 0xff,
            _ => self.prev_bytes,
        };
        let state =
            (self.kind as u32) | ((self.position as u32) << 3) | ((self.delimiter as u32) << 11);
        self.ctx = hash(state ^ ((recent as u32) << 19) ^ 0x6b8b4567, 3).wrapping_mul(0x85ebca6b);
    }
}

impl Model for TokenPosition {
    fn pred(&mut self) -> f64 {
        let entry = *self.hash_table.borrow().get(self.ctx ^ self.bit_ctx);
        prob_stretch(entry.prob() as f64 / U24_MAX as f64)
    }

    fn learn(&mut self, bit: u8) {
        {
            let mut table = self.hash_table.borrow_mut();
            let inst = table.get_mut(self.ctx ^ self.bit_ctx);
            let count = (inst.count() + 1).min(self.max_count);
            let mut prob = inst.prob();
            prob += (U24_MAX as f64
                * ((bit as f64 - prob as f64 / U24_MAX as f64) / (count as f64 + 0.2)))
                as i32;
            inst.set_count(count);
            inst.set_prob(prob);
        }
        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            let byte = (self.bit_ctx & 255) as u8;
            self.advance(byte);
            self.bit_ctx = 1;
        }
    }
}

/// Predicts bits by copying the byte following an earlier occurrence of the
/// current byte context. Both the history and lookup table have fixed bounds.
pub struct MatchPredictor {
    history: Vec<u8>,
    keys: Vec<u32>,
    positions: Vec<usize>,
    position: usize,
    context: u32,
    context_mask: u32,
    table_mask: usize,
    match_position: Option<usize>,
    partial_byte: u16,
    confidence: f64,
}

impl MatchPredictor {
    pub fn new(context_bytes: u32, confidence: f64, table_bits: u32) -> Self {
        assert!((1..=4).contains(&context_bytes));
        assert!((0.5..1.0).contains(&confidence));
        assert!((8..=20).contains(&table_bits));
        let table_size = 1usize << table_bits;
        Self {
            history: vec![0; 1 << 16],
            keys: vec![0; table_size],
            positions: vec![0; table_size],
            position: 0,
            context: 0,
            context_mask: if context_bytes == 4 {
                u32::MAX
            } else {
                (1 << (8 * context_bytes)) - 1
            },
            table_mask: table_size - 1,
            match_position: None,
            partial_byte: 1,
            confidence,
        }
    }
}

impl Model for MatchPredictor {
    fn pred(&mut self) -> f64 {
        match self.match_position {
            Some(pos) if pos < self.position && self.position - pos <= self.history.len() => {
                let shift = 7 - self.partial_byte.ilog2();
                let bit = (self.history[pos & (self.history.len() - 1)] >> shift) & 1;
                prob_stretch(if bit == 1 {
                    self.confidence
                } else {
                    1.0 - self.confidence
                })
            }
            _ => 0.0,
        }
    }

    fn learn(&mut self, bit: u8) {
        if let Some(pos) = self.match_position {
            let shift = 7 - self.partial_byte.ilog2();
            if pos >= self.position
                || self.position - pos > self.history.len()
                || (self.history[pos & (self.history.len() - 1)] >> shift) & 1 != bit
            {
                self.match_position = None;
            }
        }
        self.partial_byte = (self.partial_byte << 1) | bit as u16;
        if self.partial_byte >= 256 {
            let byte = self.partial_byte as u8;
            let history_slot = self.position & (self.history.len() - 1);
            self.history[history_slot] = byte;
            self.position += 1;
            self.context = ((self.context << 8) | byte as u32) & self.context_mask;
            let slot = self.context.wrapping_mul(0x9E35A7BD) as usize & self.table_mask;
            if let Some(pos) = self.match_position {
                self.match_position = Some(pos + 1);
            } else {
                let prior = self.positions[slot];
                if prior != 0
                    && self.keys[slot] == self.context
                    && self.position - (prior - 1) <= self.history.len()
                {
                    self.match_position = Some(prior - 1);
                }
            }
            self.keys[slot] = self.context;
            self.positions[slot] = self.position + 1;
            self.partial_byte = 1;
        }
    }
}

pub struct ModelWithWeight {
    pub model: Box<dyn Model>,
    pub weight: f64,
}

pub struct LnMixerPred {
    pub models_with_weight: Vec<ModelWithWeight>,
    last_p: Vec<f64>,
    weights: Vec<Vec<Vec<f64>>>,
    prev_byte: u32,
    bit_ctx: u32,
    last_total_p: f64,
    learning_rate: f64,
    context_learning_rate: f64,
    context_weight_scale: f64,
}

impl LnMixerPred {
    pub fn new(
        models: Vec<Box<dyn Model>>,
        learning_rate: f64,
        context_learning_rate: f64,
        context_weight_scale: f64,
    ) -> Self {
        let num_models = models.len();
        let mut models_with_weight = Vec::new();
        for model in models {
            models_with_weight.push(ModelWithWeight {
                model: model,
                weight: 1. / num_models as f64, // Default weight, adjusted by learning later
            });
        }

        Self {
            last_p: vec![0.; models_with_weight.len()],
            last_total_p: 0.,
            models_with_weight: models_with_weight,
            weights: vec![vec![vec![]; 255]; 256],
            bit_ctx: 1,
            prev_byte: 0,
            learning_rate,
            context_learning_rate,
            context_weight_scale,
        }
    }
}

impl Model for LnMixerPred {
    fn pred(&mut self) -> f64 {
        let mut sum = 0.;

        let weights = &mut self.weights[self.prev_byte as usize][self.bit_ctx as usize - 1];
        let mut i = 0;
        for model in &mut self.models_with_weight {
            let model_weight = if weights.is_empty() {
                // We don't have 1-order weight so just use the context independent weight
                model.weight
            } else {
                // Mix in 1-order weight with context independent weight
                f64::mul_add(weights[i], self.context_weight_scale, model.weight)
            };

            let p = model.model.pred();
            self.last_p[i] = p;
            sum += p * model_weight;

            i += 1;
        }

        self.last_total_p = prob_squash(sum);
        sum
    }

    fn learn(&mut self, bit: u8) {
        let weights = &mut self.weights[self.prev_byte as usize][self.bit_ctx as usize - 1];

        if weights.is_empty() {
            weights.reserve(self.models_with_weight.len());
            for i in 0..self.models_with_weight.len() {
                weights.push(self.models_with_weight[i].weight);
            }
        }

        let pred_err = bit as f64 - self.last_total_p;

        let mut i = 0;
        for model in &mut self.models_with_weight {
            model.model.learn(bit);
            let p = self.last_p[i];

            model.weight += self.learning_rate * pred_err * p;
            weights[i] += self.context_learning_rate * pred_err * p;

            i += 1;
        }

        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;

        if self.bit_ctx >= 256 {
            self.bit_ctx &= 0xff;
            self.prev_byte = self.bit_ctx;
            self.bit_ctx = 1;
        }
    }
}

#[derive(Clone, Default)]
pub struct SSEPredData([NOrderByteData; 32]);

impl Index<usize> for SSEPredData {
    type Output = NOrderByteData;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl IndexMut<usize> for SSEPredData {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

pub struct AdaptiveProbabilityMap {
    ctx: u32,
    hash_table: HashTable<SSEPredData>,
    max_count: u32,

    current_prob_idx: usize,
    next_prob_idx: usize,
    current_prob_weight: f64,

    prev_bytes: u64,
    mask: u64,

    bit_ctx: u32,

    input_model: Box<dyn Model>,
}

impl AdaptiveProbabilityMap {
    pub fn new(pow2_size: u32, input_model: Box<dyn Model>) -> AdaptiveProbabilityMap {
        AdaptiveProbabilityMap {
            ctx: 0,
            hash_table: HashTable::<SSEPredData>::new(pow2_size),
            max_count: 255,
            current_prob_idx: 0,
            next_prob_idx: 0,
            current_prob_weight: 1.,

            prev_bytes: 0,
            mask: 0xffffff,

            bit_ctx: 1,
            input_model: input_model,
        }
    }

    fn update_counter(inst: &mut NOrderByteData, bit: u8, max_count: u32, weight: f64) {
        if weight <= 0. {
            return;
        }

        let (mut count, mut prob) = (inst.count(), inst.prob());
        if count < max_count {
            count += 1;
        }

        // Learning function
        prob += (U24_MAX as f64
            * weight
            * ((bit as f64 - (prob as f64 / U24_MAX as f64)) / ((count + 30) as f64 + 1.5)))
            as i32;
        inst.set_count(count);
        inst.set_prob(prob);
    }
}

impl Model for AdaptiveProbabilityMap {
    fn pred(&mut self) -> f64 {
        let p = self.input_model.pred();
        let p_ptr = p.max(-8.).min(7.5) * 2.;
        let p_idx_f = p_ptr.floor();
        let p_idx_c = p_ptr.ceil();

        let delta_f = p_ptr - p_idx_f;
        let delta_c = p_idx_c - p_ptr;
        if delta_f <= delta_c {
            // Use floor
            self.current_prob_idx = (p_idx_f as i32 + 16) as usize;
            self.next_prob_idx = self.current_prob_idx + 1;
            if self.next_prob_idx >= 32 {
                self.next_prob_idx = 31;
            }
            self.current_prob_weight = 1. - delta_f;
        } else {
            // Use ceil
            self.current_prob_idx = (p_idx_c as i32 + 16) as usize;
            self.next_prob_idx = self.current_prob_idx - 1;
            self.current_prob_weight = 1. - delta_c;
        }

        let counter1 = {
            let counter1: &mut NOrderByteData =
                &mut self.hash_table.get_mut(self.ctx ^ self.bit_ctx)[self.current_prob_idx];
            if counter1.count() == 0 {
                let prob = (prob_squash(p) * U24_MAX as f64) as i32 & U24_MAX as i32;
                counter1.set_prob(prob);
            }
            counter1.clone()
        };

        let counter2 = {
            let counter2: &mut NOrderByteData =
                &mut self.hash_table.get_mut(self.ctx ^ self.bit_ctx)[self.next_prob_idx];
            if counter2.count() == 0 {
                let prob = (prob_squash(p) * U24_MAX as f64) as i32 & U24_MAX as i32;
                counter2.set_prob(prob);
            }
            counter2.clone()
        };

        let new_p = self.current_prob_weight * (counter1.prob() as f64 / U24_MAX as f64)
            + (1. - self.current_prob_weight) * (counter2.prob() as f64 / U24_MAX as f64);
        prob_stretch(new_p)
    }

    fn learn(&mut self, bit: u8) {
        {
            let counters = self.hash_table.get_mut(self.ctx ^ self.bit_ctx);
            Self::update_counter(
                &mut counters[self.current_prob_idx],
                bit,
                self.max_count,
                self.current_prob_weight,
            );
            if self.next_prob_idx != self.current_prob_idx {
                Self::update_counter(
                    &mut counters[self.next_prob_idx],
                    bit,
                    self.max_count,
                    1. - self.current_prob_weight,
                );
            }
        }

        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            self.bit_ctx &= 0xff;

            self.prev_bytes = ((self.prev_bytes << 8) | self.bit_ctx as u64) & self.mask;
            // Remove the extra leading bit before using it in the ctx
            self.ctx = hash((self.prev_bytes >> 32) as u32, 3)
                .wrapping_mul(9)
                .wrapping_add(hash(self.prev_bytes as u32, 3));

            // Reset bit_ctx
            self.bit_ctx = 1;
        }

        self.input_model.learn(bit);
    }
}
