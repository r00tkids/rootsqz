//! Search for the model config that packs given inputs smallest.
//!
//! The cost of a config is the size of the compressed stream plus the bytes of the web output
//! that are not the stream: the decoder is generated from the config, so a model or a parameter
//! the search adds is paid for in decoder bytes, and a model type the search drops takes its
//! code out of the decoder. The search is a first-improvement hill climb over the neighbours of
//! the config (one changed parameter, one added or removed model, all models of a type removed),
//! evaluated in parallel batches, the moves that came closest to an improvement before first. It
//! stops when no neighbour is better. It is deterministic: the result does not depend on the
//! number of threads.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    compress_config::{CompressConfig, ModelConfig, RecordConfig, WasmContextConfig},
    mixer::{ApmConfig, ApmContext, Selector},
    model_finder::SIZE_64K_WASM_CONFIG_JSON,
    Encoder,
};
use crate::web::output_generator::{web_overhead, WebLayout};

/// One input of the search: the stream that is packed, and how the web output lays it out.
pub struct Demo {
    pub name: String,
    pub stream: Vec<u8>,
    pub layout: WebLayout,
}

/// Options of `search`.
pub struct SearchOptions {
    /// Number of configs evaluated at the same time
    pub threads: usize,
    /// Number of neighbours evaluated before the best of them is taken, if it is better
    pub batch: usize,
    /// Stop after this many evaluations
    pub max_evals: usize,
    /// Configs whose decoder would take more memory than this for a demo, by `decoder_memory`,
    /// are skipped
    pub max_memory: usize,
    /// File where the search keeps its `SearchState` after every batch, and resumes from
    pub state: Option<PathBuf>,
}

/// What a search has learnt besides its best config, so that it can resume where it stopped
#[derive(Default, Serialize, Deserialize)]
struct SearchState {
    seed: u64,
    evals: usize,
    /// How much each move changed the cost the last time it was tried, by its label
    tried: HashMap<String, i64>,
    /// The results of `pack`, by the hash of `pack_cache`
    packed: HashMap<String, PackResult>,
}

impl SearchState {
    fn load(path: &PathBuf) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&json).with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Writes the state through a temporary file, so that a stopped search leaves a whole one
    fn save(&self, path: &PathBuf) -> Result<()> {
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, serde_json::to_string(self)?)?;
        std::fs::rename(&temp, path)?;
        Ok(())
    }
}

/// The default of `SearchOptions::max_memory`: 1 GB, which browsers give a page
pub const MAX_DECODER_MEMORY: usize = 1_000_000_000;

type PackResult = (usize, (usize, usize));

/// The results of `pack`, by a hash of the model and the demo
fn pack_cache() -> &'static Mutex<HashMap<String, PackResult>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PackResult>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Size of the compressed stream of `demo` with `config`, and the mixer weight sets and
/// weights the model allocated (see `Model::weights`). Configs that differ only in models the
/// demo does not get, such as WebAssembly models for a demo without modules, are packed once.
fn pack(config: &CompressConfig, demo: &Demo) -> Result<PackResult> {
    let mut params = config.static_model_params.clone();
    // The stream models are in the model below if the demo gets them
    params.wasm.models.clear();
    params.records.models.clear();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        format!("{:?}", config.with_stream_models(&demo.layout.stream)),
        format!("{params:?}"),
        &demo.name,
    )
        .hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());
    if let Some(result) = pack_cache().lock().unwrap().get(&key) {
        return Ok(*result);
    }
    let result = pack_uncached(config, demo)?;
    pack_cache().lock().unwrap().insert(key, result);
    Ok(result)
}

fn pack_uncached(config: &CompressConfig, demo: &Demo) -> Result<PackResult> {
    let mut encoded: Vec<u8> = Vec::new();
    let weights;
    {
        let model = config.create_model_for(&demo.layout.stream)?;
        let mut encoder = Encoder::new(model, &mut encoded)?;
        encoder.encode_section(demo.stream.as_slice())?;
        weights = encoder.model().weights();
        encoder.finish()?;
    }
    Ok((encoded.len(), weights))
}

/// Size of the compressed stream of `demo` with `config`.
pub fn stream_size(config: &CompressConfig, demo: &Demo) -> Result<usize> {
    Ok(pack(config, demo)?.0)
}

/// Memory of the JS decoder that is not in its tables: node, the code, the input and output, and
/// the arrays the models allocate for every bit. With the weights at 16 bytes each, which leaves
/// room for the slack of the garbage collector, this makes the estimate of the 64k preset 0-40 MB
/// above the peak RSS of node 24 on the 2026 64k demos (850-960 MB).
const DECODER_BASE_MEMORY: usize = 245_000_000;

/// An estimate of the peak memory of the JS decoder of `demo` with `config`, given the weight
/// sets and weights its mixers allocate: the typed arrays that the models and APMs allocate,
/// and the arrays of weights.
pub fn decoder_memory(config: &CompressConfig, demo: &Demo, (sets, weights): (usize, usize)) -> usize {
    fn tables(model: &ModelConfig, len: usize) -> usize {
        match model {
            ModelConfig::Mixer { models } => models.iter().map(|m| tables(m, len)).sum(),
            ModelConfig::ContextMixer { models, apms, .. } => {
                let apms: usize = apms
                    .iter()
                    .map(|apm| match apm.ctx {
                        ApmContext::Order0 => 17 << 8,
                        ApmContext::Order1 => 17 << 16,
                        ApmContext::State(_) => 17 << 14,
                        ApmContext::Order2 | ApmContext::Context(_) => 17 << (8 + apm.bits),
                    })
                    .sum();
                // A probability and a count per bucket
                apms * 12 + models.iter().map(|m| tables(m, len)).sum::<usize>()
            }
            ModelConfig::AdaptiveProbabilityMap(model) => tables(model, len),
            // The heads, the probabilities and counts, and the history and the chain as arrays
            ModelConfig::Match { .. } => (4 << 22) + (5 * 72 << 16) + 16 * len,
            ModelConfig::MatchPredictor { table_bits, .. } => 65536 + (8 << table_bits),
            // The bit history states of a counter model
            ModelConfig::IndirectByte { context_bytes, .. } => {
                (4 << 16) + (2 << (8 * *context_bytes as usize))
            }
            _ => 4 << 16,
        }
    }
    let model = config.with_stream_models(&demo.layout.stream);
    DECODER_BASE_MEMORY
        + (8 << config.static_model_params.hash_table_pow2_size)
        + tables(&model, demo.stream.len())
        + sets * 48
        + weights * 16
}

fn overhead_cache() -> &'static Mutex<HashMap<String, usize>> {
    static CACHE: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Bytes of the web output of `demo` besides the stream: the HTML header and the boot code.
pub fn overhead(config: &CompressConfig, demo: &Demo) -> Result<usize> {
    let model = config.with_stream_models(&demo.layout.stream);
    let key = format!("{model:?} {:?} {}", config.static_model_params, demo.name);
    if let Some(size) = overhead_cache().lock().unwrap().get(&key) {
        return Ok(*size);
    }
    let size = web_overhead(&model, &config.static_model_params, &demo.layout)?;
    overhead_cache().lock().unwrap().insert(key, size);
    Ok(size)
}

/// Size of the web output of `demo` packed with `config`, and the estimated memory of its
/// decoder.
pub fn demo_eval(config: &CompressConfig, demo: &Demo) -> Result<(usize, usize)> {
    let (stream, weights) = pack(config, demo)?;
    Ok((
        stream + overhead(config, demo)?,
        decoder_memory(config, demo, weights),
    ))
}

/// Size of the web output of `demo` packed with `config`.
#[cfg(test)]
pub fn demo_cost(config: &CompressConfig, demo: &Demo) -> Result<usize> {
    Ok(demo_eval(config, demo)?.0)
}

/// Total size of the web outputs of all `demos`, each packed on its own with `config`.
pub fn cost(config: &CompressConfig, demos: &[Demo]) -> Result<usize> {
    cost_within(config, demos, usize::MAX)
}

/// `cost`, or an error if the decoder of a demo would take more than `max_memory`.
fn cost_within(config: &CompressConfig, demos: &[Demo], max_memory: usize) -> Result<usize> {
    let mut total = 0;
    for demo in demos {
        let (size, memory) = demo_eval(config, demo)?;
        anyhow::ensure!(
            memory <= max_memory,
            "The decoder of {} would take {memory} bytes",
            demo.name
        );
        total += size;
    }
    Ok(total)
}

/// Costs of `configs`, evaluated on `threads` threads.
fn costs(
    configs: &[CompressConfig],
    demos: &[Demo],
    threads: usize,
    max_memory: usize,
) -> Vec<Option<usize>> {
    let next = Mutex::new(0usize);
    let results = Mutex::new(vec![None; configs.len()]);
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| loop {
                let index = {
                    let mut next = next.lock().unwrap();
                    *next += 1;
                    *next - 1
                };
                if index >= configs.len() {
                    break;
                }
                // A config the model rejects, e.g. one with an out of range parameter, is skipped
                let cost = cost_within(&configs[index], demos, max_memory).ok();
                results.lock().unwrap()[index] = cost;
            });
        }
    });
    results.into_inner().unwrap()
}

const COUNTS: [u32; 16] = [1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 30, 40, 60, 100, 255];
const MATCH_LENGTHS: [u32; 13] = [1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 24, 32];
const MATCH_CANDIDATES: [u32; 6] = [1, 2, 4, 8, 16, 32];

/// The neighbours of `value` in `ladder`, or the nearest entry if it is not in it.
fn ladder_steps(ladder: &[u32], value: u32) -> Vec<u32> {
    match ladder.iter().position(|&v| v == value) {
        Some(i) => [i.checked_sub(1), Some(i + 1)]
            .into_iter()
            .flatten()
            .filter_map(|i| ladder.get(i).copied())
            .collect(),
        None => vec![*ladder.iter().min_by_key(|&&v| v.abs_diff(value)).unwrap()],
    }
}

/// Rounds to two significant digits, which keep the number short in the decoder.
fn round2(value: f64) -> f64 {
    if value == 0. {
        return 0.;
    }
    let scale = 10f64.powi(1 - value.abs().log10().floor() as i32);
    let rounded = (value * scale).round() / scale;
    // Through the decimal text, so that the value is the one the JS decoder parses
    format!("{rounded:.12}").parse().unwrap()
}

fn float_steps(value: f64) -> Vec<f64> {
    [0.6, 0.8, 1.25, 1.6]
        .iter()
        .map(|factor| round2(value * factor))
        .filter(|&v| v != value)
        .collect()
}

fn mask_value(mask: &str) -> u8 {
    u8::from_str_radix(mask.trim_start_matches("0b"), 2).unwrap_or(0)
}

fn mask_text(mask: u8) -> String {
    format!("0b{mask:08b}")
}

/// The name of the decoder code that a model needs, shared by all models of its type
fn model_type(model: &ModelConfig) -> &'static str {
    match model {
        ModelConfig::NOrderByte { .. } => "NOrderByte",
        ModelConfig::CharClass { .. } => "CharClass",
        ModelConfig::Mixer { .. } => "Mixer",
        ModelConfig::ContextMixer { .. } => "ContextMixer",
        ModelConfig::AdaptiveProbabilityMap(_) => "AdaptiveProbabilityMap",
        ModelConfig::Word => "Word",
        ModelConfig::TokenPosition => "TokenPosition",
        ModelConfig::MatchPredictor { .. } => "MatchPredictor",
        ModelConfig::IndirectByte { .. } => "IndirectByte",
        ModelConfig::Match { .. } => "Match",
        ModelConfig::JsContext { .. } => "JsContext",
        ModelConfig::WasmContext { .. } => "WasmContext",
        ModelConfig::Record(_) => "Record",
    }
}

/// A changed copy of a config, with what changed.
struct Move {
    label: String,
    config: CompressConfig,
}

/// All configs that differ from `config` by one step. `wasm` and `records` tell whether the inputs
/// have WebAssembly modules and record ranges, for which the models of `static_model_params.wasm`
/// and `static_model_params.records` are added.
fn neighbours(config: &CompressConfig, wasm: bool, records: bool) -> Vec<Move> {
    let mut moves = Vec::new();
    let Some(models) = config.model.mixed_models() else {
        return moves;
    };
    let mut push = |label: String, change: &dyn Fn(&mut CompressConfig)| {
        let mut config = config.clone();
        change(&mut config);
        moves.push(Move { label, config });
    };

    let params = &config.static_model_params;
    if let ModelConfig::ContextMixer {
        learning_rate,
        final_learning_rate,
        init_weight,
        lr_boost,
        ..
    } = &config.model
    {
        let rates = [
            ("learning_rate", *learning_rate),
            ("final_learning_rate", *final_learning_rate),
            ("init_weight", *init_weight),
            ("lr_boost", *lr_boost),
        ];
        for (index, (name, value)) in rates.into_iter().enumerate() {
            for v in float_steps(value) {
                push(format!("{name} {v}"), &|c| {
                    if let ModelConfig::ContextMixer {
                        learning_rate,
                        final_learning_rate,
                        init_weight,
                        lr_boost,
                        ..
                    } = &mut c.model
                    {
                        *[learning_rate, final_learning_rate, init_weight, lr_boost][index] = v;
                    }
                });
            }
        }
        mixer_neighbours(config, models, &mut push);
    } else {
        for v in float_steps(params.mixer.learning_rate) {
            push(format!("learning_rate {v}"), &|c| {
                c.static_model_params.mixer.learning_rate = v
            });
        }
        for v in float_steps(params.mixer.context_learning_rate) {
            push(format!("context_learning_rate {v}"), &|c| {
                c.static_model_params.mixer.context_learning_rate = v
            });
        }
        for v in float_steps(params.mixer.context_weight_scale) {
            push(format!("context_weight_scale {v}"), &|c| {
                c.static_model_params.mixer.context_weight_scale = v
            });
        }
    }
    for v in float_steps(params.count_offset) {
        push(format!("count_offset {v}"), &|c| {
            c.static_model_params.count_offset = v
        });
    }
    for v in float_steps(params.context_input_scale) {
        push(format!("context_input_scale {v}"), &|c| {
            c.static_model_params.context_input_scale = v
        });
    }
    // Larger tables are skipped by `SearchOptions::max_memory` when the decoder would not fit
    for v in [params.hash_table_pow2_size - 1, params.hash_table_pow2_size + 1] {
        if (16..=28).contains(&v) {
            push(format!("hash_table_pow2_size {v}"), &|c| {
                c.static_model_params.hash_table_pow2_size = v
            });
        }
    }
    let has = |pred: &dyn Fn(&ModelConfig) -> bool| models.iter().any(|m| pred(m));
    if has(&|m| matches!(m, ModelConfig::Word)) {
        for v in ladder_steps(&COUNTS, params.word_max_count) {
            push(format!("word_max_count {v}"), &|c| {
                c.static_model_params.word_max_count = v
            });
        }
    }
    if has(&|m| matches!(m, ModelConfig::TokenPosition)) {
        for v in ladder_steps(&COUNTS, params.token_max_count) {
            push(format!("token_max_count {v}"), &|c| {
                c.static_model_params.token_max_count = v
            });
        }
        for v in 0..=2u8 {
            if v != params.token_context_bytes {
                push(format!("token_context_bytes {v}"), &|c| {
                    c.static_model_params.token_context_bytes = v
                });
            }
        }
        for v in ladder_steps(
            &[1, 2, 3, 4, 6, 8, 12, 15, 30, 60, 255],
            params.token_max_position as u32,
        ) {
            push(format!("token_max_position {v}"), &|c| {
                c.static_model_params.token_max_position = v as u8
            });
        }
    }

    let masks: Vec<u8> = models
        .iter()
        .filter_map(|m| match m {
            ModelConfig::NOrderByte { byte_mask, .. } => Some(mask_value(byte_mask)),
            _ => None,
        })
        .collect();
    let set_model = |c: &mut CompressConfig, index: usize, model: ModelConfig| {
        if let Some(models) = c.model.mixed_models_mut() {
            models[index] = model;
        }
    };
    let add_model = |c: &mut CompressConfig, model: ModelConfig| {
        if let Some(models) = c.model.mixed_models_mut() {
            models.push(model);
        }
    };

    // The code of a model type is in the decoder while one model of the type is, so models that
    // pay for the code only together are dropped together
    let mut types: Vec<&str> = models.iter().map(model_type).collect();
    types.sort();
    types.dedup();
    for model_type_name in types {
        let count = models
            .iter()
            .filter(|m| model_type(m) == model_type_name)
            .count();
        if count > 1 && count < models.len() {
            push(format!("remove all {model_type_name}"), &|c| {
                c.model
                    .retain_mixed_models(|_, m| model_type(m) != model_type_name)
            });
        }
    }

    for (index, model) in models.iter().enumerate() {
        if models.len() > 2 {
            push(format!("remove {}", model.label()), &|c| {
                c.model.retain_mixed_models(|i, _| i != index)
            });
        }
        match model {
            ModelConfig::NOrderByte {
                byte_mask,
                max_count,
            } => {
                let mask = mask_value(byte_mask);
                for bit in 0..8 {
                    let flipped = mask ^ (1 << bit);
                    if masks.contains(&flipped) {
                        continue;
                    }
                    let changed = ModelConfig::NOrderByte {
                        byte_mask: mask_text(flipped),
                        max_count: *max_count,
                    };
                    push(format!("{} -> {}", model.label(), changed.label()), &|c| {
                        set_model(c, index, changed.clone())
                    });
                    let added = ModelConfig::NOrderByte {
                        byte_mask: mask_text(flipped),
                        max_count: None,
                    };
                    push(format!("add {}", added.label()), &|c| {
                        add_model(c, added.clone())
                    });
                }
                for v in ladder_steps(&COUNTS, max_count.unwrap_or(15)) {
                    let changed = ModelConfig::NOrderByte {
                        byte_mask: byte_mask.clone(),
                        max_count: (v != 15).then_some(v),
                    };
                    push(format!("{} max_count {v}", model.label()), &|c| {
                        set_model(c, index, changed.clone())
                    });
                }
            }
            ModelConfig::CharClass { order, max_count } => {
                for o in [order.saturating_sub(1), order + 1] {
                    if (1..=8).contains(&o) {
                        let changed = ModelConfig::CharClass {
                            order: o,
                            max_count: *max_count,
                        };
                        push(format!("{} -> order {o}", model.label()), &|c| {
                            set_model(c, index, changed.clone())
                        });
                    }
                }
                for v in ladder_steps(&COUNTS, max_count.unwrap_or(15)) {
                    let changed = ModelConfig::CharClass {
                        order: *order,
                        max_count: (v != 15).then_some(v),
                    };
                    push(format!("{} max_count {v}", model.label()), &|c| {
                        set_model(c, index, changed.clone())
                    });
                }
            }
            ModelConfig::IndirectByte {
                context_bytes,
                max_count,
            } => {
                for b in [context_bytes.saturating_sub(1), context_bytes + 1] {
                    if (1..=3).contains(&b) {
                        let changed = ModelConfig::IndirectByte {
                            context_bytes: b,
                            max_count: *max_count,
                        };
                        push(format!("{} -> {b}", model.label()), &|c| {
                            set_model(c, index, changed.clone())
                        });
                    }
                }
                for v in ladder_steps(&COUNTS, *max_count) {
                    let changed = ModelConfig::IndirectByte {
                        context_bytes: *context_bytes,
                        max_count: v,
                    };
                    push(format!("{} max_count {v}", model.label()), &|c| {
                        set_model(c, index, changed.clone())
                    });
                }
            }
            ModelConfig::MatchPredictor {
                context_bytes,
                confidence,
                table_bits,
            } => {
                let mut variants = Vec::new();
                for b in [context_bytes.saturating_sub(1), context_bytes + 1] {
                    if (1..=4).contains(&b) {
                        variants.push((b, *confidence, *table_bits));
                    }
                }
                let confidences = [0.9, 0.94, 0.96, 0.97, 0.98, 0.99, 0.995, 0.998];
                let i = confidences
                    .iter()
                    .position(|c| c >= confidence)
                    .unwrap_or(confidences.len() - 1);
                for j in [i.checked_sub(1), Some(i + 1)].into_iter().flatten() {
                    if let Some(&c) = confidences.get(j) {
                        variants.push((*context_bytes, c, *table_bits));
                    }
                }
                for t in [table_bits.saturating_sub(2), table_bits + 2] {
                    if (8..=20).contains(&t) {
                        variants.push((*context_bytes, *confidence, t));
                    }
                }
                for (b, conf, t) in variants {
                    let changed = ModelConfig::MatchPredictor {
                        context_bytes: b,
                        confidence: conf,
                        table_bits: t,
                    };
                    push(format!("MatchPredictor {b} {conf} {t}"), &|c| {
                        set_model(c, index, changed.clone())
                    });
                }
            }
            ModelConfig::Match {
                min_len,
                candidates,
                expected_byte_context,
                second,
            } => {
                let mut variants = Vec::new();
                for l in ladder_steps(&MATCH_LENGTHS, *min_len) {
                    variants.push((l, *candidates, *expected_byte_context, *second));
                }
                for n in ladder_steps(&MATCH_CANDIDATES, *candidates) {
                    variants.push((*min_len, n, *expected_byte_context, *second));
                }
                variants.push((*min_len, *candidates, !expected_byte_context, *second));
                variants.push((*min_len, *candidates, *expected_byte_context, !second));
                for (min_len, candidates, expected_byte_context, second) in variants {
                    let changed = ModelConfig::Match {
                        min_len,
                        candidates,
                        expected_byte_context,
                        second,
                    };
                    push(
                        format!(
                            "{} -> {min_len} {candidates} {expected_byte_context} {second}",
                            model.label()
                        ),
                        &|c| set_model(c, index, changed.clone()),
                    );
                }
            }
            ModelConfig::JsContext { variant, max_count } => {
                for v in ladder_steps(&COUNTS, max_count.unwrap_or(15)) {
                    let changed = ModelConfig::JsContext {
                        variant: *variant,
                        max_count: (v != 15).then_some(v),
                    };
                    push(format!("{} max_count {v}", model.label()), &|c| {
                        set_model(c, index, changed.clone())
                    });
                }
            }
            _ => {}
        }
    }

    let mut additions = Vec::new();
    if !has(&|m| matches!(m, ModelConfig::Word)) {
        additions.push(ModelConfig::Word);
    }
    if !has(&|m| matches!(m, ModelConfig::TokenPosition)) {
        additions.push(ModelConfig::TokenPosition);
    }
    if !has(&|m| matches!(m, ModelConfig::MatchPredictor { .. })) {
        additions.push(ModelConfig::MatchPredictor {
            context_bytes: 2,
            confidence: 0.97,
            table_bits: 16,
        });
    }
    for order in 1..=8 {
        if !has(&|m| matches!(m, ModelConfig::CharClass { order: o, .. } if *o == order)) {
            additions.push(ModelConfig::CharClass {
                order,
                max_count: None,
            });
        }
    }
    for context_bytes in 1..=3 {
        if !has(
            &|m| matches!(m, ModelConfig::IndirectByte { context_bytes: b, .. } if *b == context_bytes),
        ) {
            additions.push(ModelConfig::IndirectByte {
                context_bytes,
                max_count: 15,
            });
        }
    }
    for variant in 0..=3 {
        if !has(&|m| matches!(m, ModelConfig::JsContext { variant: v, .. } if *v == variant)) {
            additions.push(ModelConfig::JsContext {
                variant,
                max_count: None,
            });
        }
    }
    for min_len in [2, 4, 8, 16, 32] {
        if !has(&|m| matches!(m, ModelConfig::Match { min_len: l, .. } if *l == min_len)) {
            additions.push(ModelConfig::Match {
                min_len,
                candidates: 1,
                expected_byte_context: false,
                second: false,
            });
        }
    }
    for added in additions {
        push(format!("add {}", added.label()), &|c| {
            add_model(c, added.clone())
        });
    }

    if wasm {
        let wasm_models = &params.wasm.models;
        let fields: Vec<u32> = wasm_models.iter().map(|m| m.fields).collect();
        if !wasm_models.is_empty() {
            push("remove all WasmContext".to_owned(), &|c| {
                c.static_model_params.wasm.models.clear()
            });
        } else {
            push("add the WasmContext models of 64k-wasm".to_owned(), &|c| {
                let preset: CompressConfig = serde_json::from_str(SIZE_64K_WASM_CONFIG_JSON).unwrap();
                c.static_model_params.wasm = preset.static_model_params.wasm;
            });
        }
        for (index, model) in wasm_models.iter().enumerate() {
            let label = format!("WasmContext {:#x}", model.fields);
            push(format!("remove {label}"), &|c| {
                c.static_model_params.wasm.models.remove(index);
            });
            // Bits 0 to 14 select what is in the context, see the CTX_ constants of wasm.rs
            for bit in 0..15 {
                let flipped = model.fields ^ (1 << bit);
                if fields.contains(&flipped) {
                    continue;
                }
                push(format!("{label} -> {flipped:#x}"), &|c| {
                    c.static_model_params.wasm.models[index].fields = flipped
                });
                push(format!("add WasmContext {flipped:#x}"), &|c| {
                    c.static_model_params.wasm.models.push(WasmContextConfig {
                        fields: flipped,
                        max_count: None,
                    })
                });
            }
            for v in ladder_steps(&COUNTS, model.max_count.unwrap_or(15)) {
                push(format!("{label} max_count {v}"), &|c| {
                    c.static_model_params.wasm.models[index].max_count = (v != 15).then_some(v)
                });
            }
        }
    }
    if records {
        record_neighbours(config, &mut push);
    }
    moves
}

/// Whether the model keeps counters in the `ContextTable`, whose count is its `Model::state`
fn is_counter_model(model: &ModelConfig) -> bool {
    matches!(
        model,
        ModelConfig::NOrderByte { .. }
            | ModelConfig::CharClass { .. }
            | ModelConfig::Word
            | ModelConfig::TokenPosition
            | ModelConfig::IndirectByte { .. }
            | ModelConfig::JsContext { .. }
    )
}

const APM_LIMITS: [u32; 7] = [15, 31, 63, 127, 255, 511, 1023];

/// The moves of the selectors and APMs of a `ContextMixer` config with these `models`
fn mixer_neighbours(
    config: &CompressConfig,
    models: &[ModelConfig],
    push: &mut dyn FnMut(String, &dyn Fn(&mut CompressConfig)),
) {
    let ModelConfig::ContextMixer {
        selectors, apms, ..
    } = &config.model
    else {
        return;
    };
    fn mixer(c: &mut CompressConfig) -> (&mut Vec<Selector>, &mut Vec<ApmConfig>) {
        match &mut c.model {
            ModelConfig::ContextMixer {
                selectors, apms, ..
            } => (selectors, apms),
            _ => unreachable!(),
        }
    }
    // Contexts that models offer: the lexer context of the JS context models (the same for all of
    // them) and the states of the match models and of the counter models (their counts)
    let js = models
        .iter()
        .position(|m| matches!(m, ModelConfig::JsContext { .. }));
    let states: Vec<usize> = (0..models.len())
        .filter(|&i| matches!(models[i], ModelConfig::Match { .. }) || is_counter_model(&models[i]))
        .collect();

    let mut candidates = vec![Selector::None, Selector::Order1, Selector::Known];
    candidates.extend(js.map(Selector::Context));
    candidates.extend(states.iter().map(|&i| Selector::State(i)));
    let label = |selector: &Selector| match selector {
        Selector::Context(i) | Selector::State(i) => {
            format!("{selector:?} {}", models[*i].label())
        }
        _ => format!("{selector:?}"),
    };
    for (index, selector) in selectors.iter().enumerate() {
        if selectors.len() > 1 {
            push(format!("remove selector {}", label(selector)), &|c| {
                mixer(c).0.remove(index);
            });
        }
    }
    for selector in candidates.iter().filter(|s| !selectors.contains(s)) {
        push(format!("add selector {}", label(selector)), &|c| {
            mixer(c).0.push(*selector)
        });
    }

    let apm_label = |ctx: &ApmContext| match ctx {
        ApmContext::Context(i) | ApmContext::State(i) => format!("APM {ctx:?} {}", models[*i].label()),
        _ => format!("APM {ctx:?}"),
    };
    for (index, apm) in apms.iter().enumerate() {
        let name = apm_label(&apm.ctx);
        push(format!("remove {name}"), &|c| {
            mixer(c).1.remove(index);
        });
        for v in ladder_steps(&APM_LIMITS, apm.limit) {
            push(format!("{name} limit {v}"), &|c| mixer(c).1[index].limit = v);
        }
        for v in float_steps(apm.weight) {
            push(format!("{name} weight {v}"), &|c| mixer(c).1[index].weight = v);
        }
        if matches!(apm.ctx, ApmContext::Order2 | ApmContext::Context(_)) {
            for v in [apm.bits - 1, apm.bits + 1] {
                if (6..=16).contains(&v) {
                    push(format!("{name} bits {v}"), &|c| mixer(c).1[index].bits = v);
                }
            }
        }
    }
    let mut contexts = vec![ApmContext::Order0, ApmContext::Order1, ApmContext::Order2];
    contexts.extend(js.map(ApmContext::Context));
    contexts.extend(states.iter().map(|&i| ApmContext::State(i)));
    for ctx in contexts {
        if apms.iter().any(|apm| apm.ctx == ctx) {
            continue;
        }
        let weight = apms.first().map_or(0.1, |apm| apm.weight);
        push(format!("add {}", apm_label(&ctx)), &|c| {
            mixer(c).1.push(ApmConfig {
                ctx,
                bits: 10,
                limit: 127,
                weight,
            })
        });
    }
}

/// The moves of the record models of `static_model_params.records`
fn record_neighbours(
    config: &CompressConfig,
    push: &mut dyn FnMut(String, &dyn Fn(&mut CompressConfig)),
) {
    let records = &config.static_model_params.records.models;
    let label = |record: &RecordConfig| ModelConfig::Record(record.clone()).label();
    if !records.is_empty() {
        push("remove all Record".to_owned(), &|c| {
            c.static_model_params.records.models.clear()
        });
    }
    for (index, record) in records.iter().enumerate() {
        let name = label(record);
        push(format!("remove {name}"), &|c| {
            c.static_model_params.records.models.remove(index);
        });
        for v in ladder_steps(&COUNTS, record.max_count.unwrap_or(15)) {
            push(format!("{name} max_count {v}"), &|c| {
                c.static_model_params.records.models[index].max_count = (v != 15).then_some(v)
            });
        }
        let mut variants = Vec::new();
        if record.delta == 0 {
            variants.push(RecordConfig {
                column: !record.column,
                ..record.clone()
            });
            // One more or one less byte or record back in the context
            if let Some(&last) = record.dists.last() {
                let next = if last < 0 { last - 1 } else { last + 1 };
                if !record.dists.contains(&next) {
                    let mut dists = record.dists.clone();
                    dists.push(next);
                    variants.push(RecordConfig {
                        dists,
                        ..record.clone()
                    });
                }
                if record.dists.len() > 1 {
                    variants.push(RecordConfig {
                        dists: record.dists[..record.dists.len() - 1].to_vec(),
                        ..record.clone()
                    });
                }
            }
        }
        for changed in variants {
            if records.contains(&changed) {
                continue;
            }
            push(format!("{name} -> {}", label(&changed)), &|c| {
                c.static_model_params.records.models[index] = changed.clone()
            });
        }
    }
    let additions = [
        (vec![-1], false, 0),
        (vec![1], true, 0),
        (vec![1], false, 0),
        (vec![-2], true, 0),
        (vec![1, 2, 3], true, 0),
        (vec![-1, 1], true, 0),
        (vec![2], false, 0),
        (vec![], true, 0),
        (vec![], false, 2),
        (vec![], false, 4),
        (vec![], false, -2),
        (vec![], false, -4),
    ];
    for (dists, column, delta) in additions {
        let added = RecordConfig {
            dists,
            column,
            delta,
            max_count: Some(if delta == 0 { 120 } else { 10 }),
        };
        if records
            .iter()
            .any(|r| (&r.dists, r.column, r.delta) == (&added.dists, added.column, added.delta))
        {
            continue;
        }
        push(format!("add {}", label(&added)), &|c| {
            c.static_model_params.records.models.push(added.clone())
        });
    }
}

/// A deterministic shuffle, so that a batch holds moves of different kinds.
fn shuffle<T>(items: &mut [T], seed: &mut u64) {
    for i in (1..items.len()).rev() {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        items.swap(i, (*seed % (i as u64 + 1)) as usize);
    }
}

/// Searches for the config with the lowest `cost` on `demos`, starting from `start`.
/// `log` is called with each improvement and the config that made it.
pub fn search(
    start: CompressConfig,
    demos: &[Demo],
    options: &SearchOptions,
    mut log: impl FnMut(&str, &CompressConfig),
) -> Result<(CompressConfig, usize)> {
    let wasm = demos.iter().any(|demo| !demo.layout.stream.wasm.is_empty());
    let records = demos.iter().any(|demo| !demo.layout.stream.records.is_empty());
    let resumed = match &options.state {
        Some(path) if path.exists() => Some(SearchState::load(path)?),
        _ => None,
    };
    if let Some(state) = &resumed {
        pack_cache().lock().unwrap().extend(state.packed.clone());
    }
    let mut best = start;
    let mut best_cost = cost(&best, demos)?;
    log(&format!("start: {best_cost}"), &best);
    let (mut evals, mut seed, mut tried) = match resumed {
        Some(state) => (state.evals, state.seed, state.tried),
        None => (1, 0x9E3779B97F4A7C15u64, HashMap::new()),
    };
    // `tried` holds how much each move changed the cost the last time it was tried. Most moves
    // change about as much after an improvement as before, so the moves not tried yet come
    // first, then those that came closest to an improvement.
    let save = |seed: u64, evals: usize, tried: &HashMap<String, i64>| -> Result<()> {
        let Some(path) = &options.state else {
            return Ok(());
        };
        SearchState {
            seed,
            evals,
            tried: tried.clone(),
            packed: pack_cache().lock().unwrap().clone(),
        }
        .save(path)
    };
    'outer: loop {
        let mut moves = neighbours(&best, wasm, records);
        shuffle(&mut moves, &mut seed);
        moves.sort_by_key(|m| tried.get(&m.label).copied().unwrap_or(i64::MIN));
        for batch in moves.chunks(options.batch.max(1)) {
            if evals >= options.max_evals {
                break 'outer;
            }
            let configs: Vec<CompressConfig> = batch.iter().map(|m| m.config.clone()).collect();
            let results = costs(&configs, demos, options.threads, options.max_memory);
            evals += batch.len();
            for (m, result) in batch.iter().zip(&results) {
                let change = result.map_or(i64::MAX, |cost| cost as i64 - best_cost as i64);
                tried.insert(m.label.clone(), change);
            }
            let improved = results.iter().flatten().any(|&cost| cost < best_cost);
            if !improved {
                save(seed, evals, &tried)?;
            }
            if let Some((index, cost)) = results
                .iter()
                .enumerate()
                .filter_map(|(i, c)| c.map(|c| (i, c)))
                .min_by_key(|&(i, c)| (c, i))
            {
                if cost < best_cost {
                    log(
                        &format!(
                            "{cost} ({:+}) after {evals} evaluations: {}",
                            cost as i64 - best_cost as i64,
                            batch[index].label
                        ),
                        &configs[index],
                    );
                    best = configs[index].clone();
                    best_cost = cost;
                    // After `log`, which writes the best config that the state goes with
                    save(seed, evals, &tried)?;
                    // The other moves were made for the old config
                    continue 'outer;
                }
            }
        }
        // No neighbour is better
        break;
    }
    log(
        &format!("done: {best_cost} after {evals} evaluations"),
        &best,
    );
    Ok((best, best_cost))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::model_finder::SIZE_64K_CONFIG_JSON;

    #[test]
    fn neighbours_drop_whole_model_types() {
        let config: CompressConfig = serde_json::from_str(SIZE_64K_CONFIG_JSON).unwrap();
        let labels = |wasm| -> Vec<String> {
            neighbours(&config, wasm, false)
                .into_iter()
                .map(|m| m.label)
                .collect()
        };
        let without_wasm = labels(false);
        for label in ["remove all JsContext", "remove all Match"] {
            let found = without_wasm.iter().find(|l| *l == label).unwrap();
            let moved = neighbours(&config, false, false)
                .into_iter()
                .find(|m| &m.label == found)
                .unwrap();
            let models = moved.config.model.mixed_models().unwrap();
            let name = label.trim_start_matches("remove all ");
            assert!(models.iter().all(|m| model_type(m) != name));
        }
        assert!(!without_wasm.iter().any(|l| l.contains("WasmContext")));
        // Removing a model keeps the APMs of the other models on them
        let removed = neighbours(&config, false, false)
            .into_iter()
            .find(|m| m.label == "remove NOrderByte 0b00000000")
            .unwrap();
        let (ModelConfig::ContextMixer { apms: before, .. }, ModelConfig::ContextMixer { apms, .. }) =
            (&config.model, &removed.config.model)
        else {
            panic!()
        };
        let state_models = |apms: &Vec<ApmConfig>| -> Vec<usize> {
            apms.iter()
                .filter_map(|apm| match apm.ctx {
                    ApmContext::State(model) => Some(model),
                    _ => None,
                })
                .collect()
        };
        let shifted: Vec<usize> = state_models(before).iter().map(|m| m - 1).collect();
        assert_eq!(state_models(apms), shifted);
        // The wasm models are only in the 64k-wasm preset, which the search can add
        let with_wasm = labels(true);
        assert!(with_wasm.contains(&"add the WasmContext models of 64k-wasm".to_owned()));
        let wasm: CompressConfig = serde_json::from_str(SIZE_64K_WASM_CONFIG_JSON).unwrap();
        assert!(neighbours(&wasm, true, false)
            .iter()
            .any(|m| m.label == "remove all WasmContext"));
    }

    #[test]
    fn neighbours_step_the_mixer_tables_and_records() {
        let config: CompressConfig = serde_json::from_str(SIZE_64K_CONFIG_JSON).unwrap();
        let moves = neighbours(&config, false, true);
        let find = |label: &str| {
            moves
                .iter()
                .find(|m| m.label == label)
                .unwrap_or_else(|| panic!("no move {label}"))
        };
        let scale = config.static_model_params.context_input_scale;
        let moved = find(&format!("context_input_scale {}", round2(scale * 1.25)));
        assert_eq!(moved.config.static_model_params.context_input_scale, round2(scale * 1.25));
        let size = config.static_model_params.hash_table_pow2_size;
        find(&format!("hash_table_pow2_size {}", size + 1));
        find(&format!("hash_table_pow2_size {}", size - 1));

        let mixer = |m: &Move| match &m.config.model {
            ModelConfig::ContextMixer {
                selectors, apms, ..
            } => (selectors.clone(), apms.clone()),
            _ => panic!(),
        };
        let ModelConfig::ContextMixer { apms: before_apms, .. } = &config.model else {
            panic!()
        };
        let (selectors, apms) = mixer(find("remove selector Order1"));
        assert!(!selectors.contains(&Selector::Order1));
        assert_eq!(apms.len(), before_apms.len());
        let (selectors, _) = mixer(find("add selector State(2) NOrderByte 0b00000011"));
        assert_eq!(selectors.last(), Some(&Selector::State(2)));
        let index = |ctx| before_apms.iter().position(|apm| apm.ctx == ctx).unwrap();
        let (order1, order2) = (index(ApmContext::Order1), index(ApmContext::Order2));
        let (_, apms) = mixer(find("APM Order1 limit 255"));
        assert_eq!(apms[order1].limit, 255);
        let (_, apms) = mixer(find("APM Order2 bits 11"));
        assert_eq!(apms[order2].bits, 11);
        let (_, apms) = mixer(find("APM Order1 weight 0.13"));
        assert_eq!(apms[order1].weight, 0.13);
        let (_, apms) = mixer(find("remove APM Order1"));
        assert_eq!(apms.len(), before_apms.len() - 1);
        let (_, apms) = mixer(find("add APM State(18) JsContext 0"));
        assert_eq!(apms.last().unwrap().ctx, ApmContext::State(18));
        find("add CharClass order 5");

        let records = |m: &Move| m.config.static_model_params.records.models.clone();
        let before = config.static_model_params.records.models.clone();
        assert!(records(find("remove all Record")).is_empty());
        assert_eq!(records(find("remove Record [-1] column")).len(), before.len() - 1);
        assert_eq!(records(find("Record [-1] column max_count 100"))[0].max_count, Some(100));
        assert_eq!(records(find("Record [-1] column -> Record [-1]"))[0].column, false);
        let moved = records(find("Record [-1, -2, -3, -4] -> Record [-1, -2, -3]"));
        assert_eq!(moved[1].dists, [-1, -2, -3]);
        assert_eq!(records(find("add Record [-2] column")).len(), before.len() + 1);
        assert!(!neighbours(&config, false, false)
            .iter()
            .any(|m| m.label.contains("Record")));
    }

    #[test]
    fn search_state_survives_a_restart() {
        let path = std::env::temp_dir().join(format!("rootsqz-state-{}.json", std::process::id()));
        let state = SearchState {
            seed: 7,
            evals: 25,
            tried: [("count_offset 0.08".to_owned(), -3)].into(),
            packed: [("00ff".to_owned(), (100, (2, 30)))].into(),
        };
        state.save(&path).unwrap();
        let loaded = SearchState::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((loaded.seed, loaded.evals), (7, 25));
        assert_eq!(loaded.tried, state.tried);
        assert_eq!(loaded.packed, state.packed);
    }

    #[test]
    fn round2_keeps_two_digits() {
        assert_eq!(round2(0.0004 * 1.25), 0.0005);
        assert_eq!(round2(0.044 * 0.8), 0.035);
        assert_eq!(float_steps(0.2), vec![0.12, 0.16, 0.25, 0.32]);
        // A value of 0 has no steps
        assert_eq!(round2(0.), 0.);
        assert!(float_steps(0.).is_empty());
    }
}
