use crate::utils::{prob_squash, prob_stretch, U24_MAX};
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
            max_count: 15,
            hash_table: hash_table,
            prev_bytes: 0,
            mask: bit_mask,
            is_word_model: false,
        }
    }

    pub fn new_word_model(
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
        _max_count: u32,
    ) -> Self {
        Self {
            ctx: 0,
            bit_ctx: 1,
            magic_num: hash(1337 as u32, 2),
            max_count: 15,
            hash_table: hash_table,
            prev_bytes: 2166136261,
            mask: u64::MAX,
            is_word_model: true,
        }
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

            if self.is_word_model {
                let next_char = current_byte as u8 as char;
                if next_char.is_ascii_alphanumeric() {
                    self.prev_bytes = self.prev_bytes ^ next_char.to_ascii_lowercase() as u64;
                    self.prev_bytes = self.prev_bytes.wrapping_mul(16777619) >> 16;
                } else {
                    self.prev_bytes = 2166136261;
                }
            } else {
                self.prev_bytes = (self.prev_bytes << 8) | current_byte as u64;
            }

            let masked_prev_bytes = self.prev_bytes & self.mask;
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
}

impl LnMixerPred {
    pub fn new(models: Vec<Box<dyn Model>>) -> Self {
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
                model.weight
            } else {
                f64::mul_add(weights[i], 0.3, model.weight)
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

        const LEARNING_RATE: f64 = 0.0004;
        const LEARNING_RATE_CTX: f64 = 0.022;
        let mut i = 0;
        for model in &mut self.models_with_weight {
            model.model.learn(bit);
            let p = self.last_p[i];

            model.weight += LEARNING_RATE * pred_err * p;
            weights[i] += LEARNING_RATE_CTX * pred_err * p;

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

            prev_bytes: 0,
            mask: 0xffffff,

            bit_ctx: 1,
            input_model: input_model,
        }
    }
}

impl Model for AdaptiveProbabilityMap {
    fn pred(&mut self) -> f64 {
        let p = self.input_model.pred();
        // TODO: Interpolate probabilities
        let p_ptr = p.max(-8.).min(7.5) * 2.;
        let p_idx_f = p_ptr.floor();
        let p_idx_c = p_ptr.ceil();

        let delta_f = p_ptr - p_idx_f;
        let delta_c = p_idx_c - p_ptr;
        let t;
        let mut next_idx;
        if delta_f <= delta_c {
            // Use floor
            self.current_prob_idx = (p_idx_f as i32 + 16) as usize;
            next_idx = self.current_prob_idx + 1;
            if next_idx >= 32 {
                next_idx = 31;
            }
            t = 1. - delta_f;
        } else {
            // Use ceil
            self.current_prob_idx = (p_idx_c as i32 + 16) as usize;
            next_idx = self.current_prob_idx - 1;
            t = 1. - delta_c;
        }
        if next_idx >= 32 {
            println!(
                "Warning: next_idx is out of bounds: {}, {}, {}, {}, {}",
                next_idx, self.current_prob_idx, t, p, p_ptr
            );
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
                &mut self.hash_table.get_mut(self.ctx ^ self.bit_ctx)[next_idx];
            if counter2.count() == 0 {
                let prob = (prob_squash(p) * U24_MAX as f64) as i32 & U24_MAX as i32;
                counter2.set_prob(prob);
            }
            counter2.clone()
        };

        let new_p = t * (counter1.prob() as f64 / U24_MAX as f64)
            + (1. - t) * (counter2.prob() as f64 / U24_MAX as f64);
        prob_stretch(new_p)
    }

    fn learn(&mut self, bit: u8) {
        {
            let inst = &mut self.hash_table.get_mut(self.ctx ^ self.bit_ctx)
                [self.current_prob_idx as usize];

            let (mut count, mut prob) = (inst.count(), inst.prob());
            if count < self.max_count {
                count += 1;
            }

            // Learning function
            prob += (U24_MAX as f64
                * ((bit as f64 - (prob as f64 / U24_MAX as f64)) / ((count + 30) as f64 + 1.5)))
                as i32;
            inst.set_count(count);
            inst.set_prob(prob);
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
