//! A paq8-style mixer: the inputs of the submodels go to several weight sets, each chosen by a
//! context, and a final weight set mixes their outputs. Adaptive probability maps refine the result.
//! The JS decoder (`js_source/context_mixer.js`) computes the same in the same order, so keep the
//! two in step: dot products are plain loops from 0, with no fused multiply-add.

use serde::{Deserialize, Serialize};

use super::model::Model;

use super::utils::{prob_squash, prob_stretch};

/// Context that selects the weight set of a first layer mixer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Selector {
    /// One weight set
    None,
    /// The previous byte and the bits of the current byte
    Order1,
    /// The number of submodels that know their context, and the bit position
    Known,
    /// The `Model::context` of the submodel at this index, and the bits of the current byte
    Context(usize),
    /// The `Model::state` of the submodel at this index up to 255, and the bits of the current byte
    State(usize),
}

impl Selector {
    /// The index of the weight set
    fn context(
        self,
        known: usize,
        bit_ctx: u32,
        history: u32,
        states: &[u32],
        contexts: &[u32],
    ) -> usize {
        (match self {
            Selector::None => 0,
            Selector::Order1 => (history & 255) << 8 | bit_ctx,
            Selector::Known => known as u32 * 8 + bit_ctx.ilog2(),
            Selector::Context(i) => contexts[i] << 8 | bit_ctx,
            Selector::State(i) => states[i].min(255) << 8 | bit_ctx,
        }) as usize
    }
}

/// Context of an adaptive probability map, with the bits of the current byte.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApmContext {
    Order0,
    /// The previous byte
    Order1,
    /// A hash of the two previous bytes, of `bits` bits
    Order2,
    /// The state of the submodel at this index, up to 63
    State(usize),
    /// The `Model::context` of the submodel at this index, hashed to `bits` bits
    Context(usize),
}

/// An adaptive probability map after the mixer: refines the probability of the mixer for a
/// context, interpolating between buckets of the stretched probability. The refined
/// probabilities of all maps are averaged with that of the mixer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApmConfig {
    pub ctx: ApmContext,
    /// Bits of the hash of an `Order2` or `Context` context
    #[serde(default = "default_apm_bits")]
    pub bits: u32,
    /// Limit of the counts of the buckets, which sets the slowest learning rate
    pub limit: u32,
    /// The weight of the refined probability in the average, 0 to 1
    pub weight: f64,
}

fn default_apm_bits() -> u32 {
    12
}

/// The buckets are at the stretched probabilities -APM_RANGE to APM_RANGE, 1 apart
const APM_RANGE: f64 = 8.;
const APM_BUCKETS: usize = 17;

struct Apm {
    config: ApmConfig,
    probs: Vec<f64>,
    counts: Vec<u32>,
    index: usize,
    frac: f64,
}

impl Apm {
    fn new(config: ApmConfig) -> Self {
        let contexts = match config.ctx {
            ApmContext::Order0 => 256,
            ApmContext::Order1 => 1 << 16,
            ApmContext::Order2 => 256 << config.bits,
            ApmContext::State(_) => 64 * 256,
            ApmContext::Context(_) => 256 << config.bits,
        };
        let mut probs = vec![0.; contexts * APM_BUCKETS];
        for (i, p) in probs.iter_mut().enumerate() {
            *p = prob_squash((i % APM_BUCKETS) as f64 - APM_RANGE);
        }
        Self {
            counts: vec![0; probs.len()],
            probs,
            config,
            index: 0,
            frac: 0.,
        }
    }

    /// The refined probability
    fn refine(
        &mut self,
        st: f64,
        history: u32,
        bit_ctx: u32,
        states: &[u32],
        contexts: &[u32],
    ) -> f64 {
        let ctx = match self.config.ctx {
            ApmContext::Order0 => bit_ctx,
            ApmContext::Order1 => (history & 255) << 8 | bit_ctx,
            ApmContext::Order2 => {
                ((history & 0xffff).wrapping_mul(0x9E35A7BD) >> (32 - self.config.bits)) << 8
                    | bit_ctx
            }
            ApmContext::State(i) => states[i].min(63) << 8 | bit_ctx,
            ApmContext::Context(i) => {
                (contexts[i].wrapping_mul(0x9E35A7BD) >> (32 - self.config.bits)) << 8 | bit_ctx
            }
        };
        let x = st.clamp(-APM_RANGE, 7.999) + APM_RANGE;
        let bucket = x.floor();
        self.frac = x - bucket;
        self.index = ctx as usize * APM_BUCKETS + bucket as usize;
        self.probs[self.index] * (1. - self.frac) + self.probs[self.index + 1] * self.frac
    }

    fn learn(&mut self, bit: u8) {
        for (i, w) in [(self.index, 1. - self.frac), (self.index + 1, self.frac)] {
            let n = &mut self.counts[i];
            *n = (*n + 1).min(self.config.limit);
            self.probs[i] += (bit as f64 - self.probs[i]) * w / (*n as f64 + 0.5);
        }
    }
}

/// How many times a weight set has to be trained before its learning rate is halfway down from
/// the boosted one
const LR_HALFLIFE: f64 = 16.;

/// A weight set and how many times it was trained
#[derive(Default)]
struct WeightSet {
    weights: Vec<f64>,
    count: u32,
}

/// Mixes the inputs of its submodels with one weight set per selector, chosen by the
/// selector's context, and mixes those outputs again with a final weight set.
pub struct ContextMixer {
    models: Vec<Box<dyn Model>>,
    selectors: Vec<Selector>,
    learning_rate: f64,
    final_learning_rate: f64,
    init_weight: f64,
    lr_boost: f64,
    inputs: Vec<f64>,
    /// The weight sets of each selector by context, empty until used
    sets: Vec<Vec<WeightSet>>,
    ctxs: Vec<usize>,
    /// The index of the first input of each model
    first_inputs: Vec<usize>,
    dots: Vec<f64>,
    final_weights: Vec<f64>,
    final_dot: f64,
    bit_ctx: u32,
    history: u32,
    apms: Vec<Apm>,
    /// The `Model::state` and `Model::context` of each submodel
    states: Vec<u32>,
    contexts: Vec<u32>,
}

impl ContextMixer {
    pub fn new(
        models: Vec<Box<dyn Model>>,
        selectors: Vec<Selector>,
        learning_rate: f64,
        final_learning_rate: f64,
        init_weight: f64,
        lr_boost: f64,
        apms: Vec<ApmConfig>,
    ) -> Self {
        let k = selectors.len();
        let num_models = models.len();
        Self {
            first_inputs: vec![0; num_models],
            states: vec![0; num_models],
            contexts: vec![0; num_models],
            models,
            sets: (0..k).map(|_| Vec::new()).collect(),
            ctxs: vec![0; k],
            dots: vec![0.; k],
            final_weights: vec![1. / k as f64; k],
            final_dot: 0.,
            selectors,
            learning_rate,
            final_learning_rate,
            init_weight,
            lr_boost,
            inputs: vec![],
            bit_ctx: 1,
            history: 0,
            apms: apms.into_iter().map(Apm::new).collect(),
        }
    }
}

impl Model for ContextMixer {
    fn pred(&mut self) -> f64 {
        self.inputs.clear();
        let mut known = 0;
        for (i, model) in self.models.iter_mut().enumerate() {
            self.first_inputs[i] = self.inputs.len();
            model.inputs(&mut self.inputs);
            self.states[i] = model.state();
            self.contexts[i] = model.context();
            known += (self.states[i] > 0) as usize;
        }
        self.inputs.push(0.5);
        let n = self.inputs.len();
        let mut final_dot = 0.;
        for (s, selector) in self.selectors.iter().enumerate() {
            let ctx =
                selector.context(known, self.bit_ctx, self.history, &self.states, &self.contexts);
            self.ctxs[s] = ctx;
            let sets = &mut self.sets[s];
            if sets.len() <= ctx {
                sets.resize_with(ctx + 1, Default::default);
            }
            let set = &mut sets[ctx];
            if set.weights.is_empty() {
                set.weights = vec![self.init_weight; n];
            }
            let mut dot = 0.;
            for (w, x) in set.weights.iter().zip(&self.inputs) {
                dot += w * x;
            }
            let dot = dot.clamp(-40., 40.);
            self.dots[s] = dot;
            final_dot += dot * self.final_weights[s];
        }
        self.final_dot = final_dot;
        if self.apms.is_empty() {
            return final_dot;
        }
        let p_mixer = prob_squash(final_dot);
        let mut p = p_mixer;
        for apm in &mut self.apms {
            let refined = apm.refine(
                final_dot,
                self.history,
                self.bit_ctx,
                &self.states,
                &self.contexts,
            );
            p = p + (refined - p_mixer) * apm.config.weight;
        }
        prob_stretch(p)
    }

    fn learn(&mut self, bit: u8) {
        for (s, dot) in self.dots.iter().enumerate() {
            let set = &mut self.sets[s][self.ctxs[s]];
            let lr = self.learning_rate
                * (1. + self.lr_boost * LR_HALFLIFE / (LR_HALFLIFE + set.count as f64));
            set.count += 1;
            let err = (bit as f64 - prob_squash(*dot)) * lr;
            for (w, x) in set.weights.iter_mut().zip(&self.inputs) {
                *w += err * x;
            }
        }
        let err = (bit as f64 - prob_squash(self.final_dot)) * self.final_learning_rate;
        for (s, dot) in self.dots.iter().enumerate() {
            self.final_weights[s] += err * dot;
        }
        for model in &mut self.models {
            model.learn(bit);
        }
        for apm in &mut self.apms {
            apm.learn(bit);
        }
        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            self.history = (self.history << 8) | (self.bit_ctx & 255);
            self.bit_ctx = 1;
        }
    }

    fn weights(&self) -> (usize, usize) {
        let mut total = (0, 0);
        for set in self.sets.iter().flatten().filter(|set| !set.weights.is_empty()) {
            total.0 += 1;
            total.1 += set.weights.len();
        }
        for model in &self.models {
            let (sets, weights) = model.weights();
            total.0 += sets;
            total.1 += weights;
        }
        total
    }

    fn submodel_preds(&self, out: &mut Vec<f32>) {
        for (model, &i) in self.models.iter().zip(&self.first_inputs) {
            out.push(self.inputs[i] as f32);
            model.submodel_preds(out);
        }
    }
}
