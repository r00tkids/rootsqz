use std::{cell::RefCell, rc::Rc};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::mixer::{ApmConfig, ApmContext, ContextMixer, Selector};
use super::model::{
    JsContext, LnMixerPred, MatchModel, MatchPredictor, Model,
    NOrderByte, TokenPosition,
};
use super::context_map::{ContextTable, SharedTable};
use super::records::RecordModel;
use super::wasm::{SharedWasmParser, WasmContext, WasmParser};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressConfig {
    pub model: ModelConfig,
    #[serde(default)]
    pub static_model_params: StaticModelParams,
}

/// What the encoder found in the stream, which adds models to the configured one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamLayout {
    /// Stream ranges `[start, end)` of the WebAssembly modules, for the WasmContext models
    pub wasm: Vec<(u32, u32)>,
    /// `[start, end, stride]` of the ranges that hold records of `stride` bytes, sorted by start,
    /// for the Record models
    pub records: Vec<[u32; 3]>,
}

/// State that the models of one stream share.
pub struct SharedState {
    pub wasm: Option<SharedWasmParser>,
    pub records: Rc<Vec<[u32; 3]>>,
}

impl CompressConfig {
    #[cfg(test)]
    pub fn create_model(&self) -> Result<Box<dyn Model>> {
        self.create_model_for(&StreamLayout::default())
    }

    /// Creates the model for a stream with this layout: `with_stream_models` of the configured one.
    pub fn create_model_for(&self, layout: &StreamLayout) -> Result<Box<dyn Model>> {
        let mut hash_table = ContextTable::new(self.static_model_params.hash_table_pow2_size);
        hash_table.count_offset = self.static_model_params.count_offset;
        hash_table.input_scale = self.static_model_params.context_input_scale;
        let hash_table = Rc::new(RefCell::new(hash_table));
        let shared = SharedState {
            wasm: (!layout.wasm.is_empty())
                .then(|| Rc::new(RefCell::new(WasmParser::new(layout.wasm.clone())))),
            records: Rc::new(layout.records.clone()),
        };
        self.with_stream_models(layout)
            .create_model(hash_table, &self.static_model_params, &shared)
    }

    /// The model with models added to its outermost mixer: the WebAssembly context models of
    /// `static_model_params.wasm` if the stream has modules, and the record models of
    /// `static_model_params.records` if it has record ranges.
    pub fn with_stream_models(&self, layout: &StreamLayout) -> ModelConfig {
        let mut model = self.model.clone();
        let mut extra: Vec<ModelConfig> = Vec::new();
        if !layout.wasm.is_empty() {
            extra.extend(self.static_model_params.wasm.models.iter().map(|config| {
                ModelConfig::WasmContext {
                    fields: config.fields,
                    max_count: config.max_count,
                }
            }));
        }
        if !layout.records.is_empty() {
            extra.extend(
                self.static_model_params
                    .records
                    .models
                    .iter()
                    .map(|config| ModelConfig::Record(config.clone())),
            );
        }
        if extra.is_empty() {
            return model;
        }
        let mut target = &mut model;
        loop {
            match target {
                ModelConfig::AdaptiveProbabilityMap(inner) => target = inner,
                ModelConfig::Mixer { models } | ModelConfig::ContextMixer { models, .. } => {
                    models.extend(extra);
                    return model;
                }
                _ => {
                    let models = std::iter::once(model).chain(extra).collect();
                    return ModelConfig::Mixer { models };
                }
            }
        }
    }
}

#[allow(unused)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticModelParams {
    #[serde(default = "default_hash_table_pow2_size")]
    pub hash_table_pow2_size: u32,
    #[serde(default = "default_word_max_count")]
    pub word_max_count: u32,
    #[serde(default = "default_token_max_count")]
    pub token_max_count: u32,
    #[serde(default = "default_token_context_bytes")]
    pub token_context_bytes: u8,
    #[serde(default = "default_token_max_position")]
    pub token_max_position: u8,
    /// The counters of the context models move by 1 / (count + count_offset) towards each bit
    #[serde(default = "default_count_offset")]
    pub count_offset: f64,
    /// Multiplies the mixer inputs of the context models
    #[serde(default = "default_context_input_scale")]
    pub context_input_scale: f64,
    #[serde(default)]
    pub mixer: MixerModelParams,
    #[serde(default)]
    pub wasm: WasmModelParams,
    #[serde(default)]
    pub records: RecordModelParams,
}

impl Default for StaticModelParams {
    fn default() -> Self {
        Self {
            hash_table_pow2_size: default_hash_table_pow2_size(),
            word_max_count: default_word_max_count(),
            token_max_count: default_token_max_count(),
            token_context_bytes: default_token_context_bytes(),
            token_max_position: default_token_max_position(),
            count_offset: default_count_offset(),
            context_input_scale: default_context_input_scale(),
            mixer: Default::default(),
            wasm: Default::default(),
            records: Default::default(),
        }
    }
}

/// The models added when the stream has WebAssembly modules, none by default. The `64k-wasm`
/// preset has them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WasmModelParams {
    #[serde(default)]
    pub models: Vec<WasmContextConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmContextConfig {
    /// What the context is made of, see the `CTX_` constants in `wasm.rs`
    pub fields: u32,
    #[serde(default)]
    pub max_count: Option<u32>,
}

/// The models added when the stream has record ranges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordModelParams {
    #[serde(default = "default_record_models")]
    pub models: Vec<RecordConfig>,
}

impl Default for RecordModelParams {
    fn default() -> Self {
        Self {
            models: default_record_models(),
        }
    }
}

/// A model of the bytes in the record ranges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordConfig {
    /// The bytes in the context: bytes back if positive, whole records back if negative
    #[serde(default)]
    pub dists: Vec<i32>,
    /// Whether the position in the record is in the context
    #[serde(default)]
    pub column: bool,
    /// Bytes of the little endian words that the model predicts from the last records: linearly
    /// from the last two if positive, the same as in the last one if negative. 0 for none
    #[serde(default)]
    pub delta: i32,
    #[serde(default)]
    pub max_count: Option<u32>,
}

fn default_record_models() -> Vec<RecordConfig> {
    let plain = [
        (&[-1][..], true),
        (&[-1, -2], true),
        (&[-1, -2, -3], false),
        (&[1, 2], true),
        // Sparse contexts
        (&[8], false),
        (&[4], false),
        (&[7, 8], false),
        (&[4, 8], false),
    ]
    .into_iter()
    .map(|(dists, column)| RecordConfig {
        dists: dists.to_vec(),
        column,
        delta: 0,
        max_count: Some(120),
    });
    let delta = [2, 4, -2, -4].into_iter().map(|delta| RecordConfig {
        dists: Vec::new(),
        column: false,
        delta,
        max_count: Some(10),
    });
    plain.chain(delta).collect()
}

#[allow(unused)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerModelParams {
    #[serde(default = "default_learning_rate")]
    pub learning_rate: f64,
    #[serde(default = "default_context_learning_rate")]
    pub context_learning_rate: f64,
    #[serde(default = "default_context_weight_scale")]
    pub context_weight_scale: f64,
}

impl Default for MixerModelParams {
    fn default() -> Self {
        Self {
            learning_rate: default_learning_rate(),
            context_learning_rate: default_context_learning_rate(),
            context_weight_scale: default_context_weight_scale(),
        }
    }
}

fn default_hash_table_pow2_size() -> u32 {
    26
}

fn default_word_max_count() -> u32 {
    15
}

fn default_token_max_count() -> u32 {
    4
}
fn default_token_context_bytes() -> u8 {
    1
}
fn default_token_max_position() -> u8 {
    15
}

fn default_context_input_scale() -> f64 {
    1.
}

fn default_count_offset() -> f64 {
    0.2
}

fn default_learning_rate() -> f64 {
    0.0004
}

fn default_context_learning_rate() -> f64 {
    0.022
}

fn default_context_weight_scale() -> f64 {
    0.3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ModelConfig {
    NOrderByte {
        byte_mask: String,
        #[serde(default)]
        max_count: Option<u32>,
    },
    CharClass {
        order: u8,
        #[serde(default)]
        max_count: Option<u32>,
    },
    Mixer {
        models: Vec<ModelConfig>,
    },
    /// See `ContextMixer`
    ContextMixer {
        models: Vec<ModelConfig>,
        selectors: Vec<Selector>,
        learning_rate: f64,
        final_learning_rate: f64,
        init_weight: f64,
        #[serde(default)]
        lr_boost: f64,
        #[serde(default)]
        apms: Vec<ApmConfig>,
    },
    #[serde(serialize_with = "serialize_boxed")]
    AdaptiveProbabilityMap(Box<ModelConfig>),
    Word,
    TokenPosition,
    MatchPredictor {
        context_bytes: u32,
        confidence: f64,
        #[serde(default = "default_match_table_bits")]
        table_bits: u32,
    },
    IndirectByte {
        context_bytes: u8,
        max_count: u32,
    },
    Match {
        min_len: u32,
        #[serde(default = "default_match_candidates")]
        candidates: u32,
        #[serde(default)]
        expected_byte_context: bool,
        #[serde(default)]
        second: bool,
    },
    JsContext {
        variant: u8,
        #[serde(default)]
        max_count: Option<u32>,
    },
    /// Added by `CompressConfig::with_stream_models`; predicts nothing outside the modules.
    WasmContext {
        fields: u32,
        #[serde(default)]
        max_count: Option<u32>,
    },
    /// Added by `CompressConfig::with_stream_models`; predicts nothing outside the record ranges.
    Record(RecordConfig),
}

/// Serializes through a `serde_json::Value`, which bounds the serializer types that the
/// recursive config instantiates.
fn serialize_boxed<S: serde::Serializer>(
    model: &ModelConfig,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serde_json::to_value(model)
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

fn default_match_candidates() -> u32 {
    1
}

fn default_match_table_bits() -> u32 {
    16
}

impl ModelConfig {
    pub fn create_model(
        &self,
        hash_table: SharedTable,
        static_model_params: &StaticModelParams,
        shared: &SharedState,
    ) -> Result<Box<dyn Model>> {
        Ok(match self {
            ModelConfig::NOrderByte {
                byte_mask,
                max_count,
            } => {
                let byte_mask = u8::from_str_radix(byte_mask.trim_start_matches("0b"), 2)?;
                Box::new(NOrderByte::new_norder_model(
                    byte_mask,
                    hash_table,
                    max_count.unwrap_or(15),
                ))
            }
            ModelConfig::CharClass { order, max_count } => {
                anyhow::ensure!((1..=8).contains(order), "CharClass order must be 1..=8");
                anyhow::ensure!(
                    max_count.unwrap_or(15) <= 255,
                    "CharClass max_count must be <= 255"
                );
                Box::new(NOrderByte::new_char_class_model(
                    *order,
                    hash_table,
                    max_count.unwrap_or(15),
                ))
            }
            ModelConfig::Mixer { models } => Box::new(LnMixerPred::new(
                models
                    .iter()
                    .map(|config| config.create_model(hash_table.clone(), static_model_params, shared))
                    .collect::<Result<Vec<_>>>()?,
                static_model_params.mixer.learning_rate,
                static_model_params.mixer.context_learning_rate,
                static_model_params.mixer.context_weight_scale,
            )),
            ModelConfig::ContextMixer {
                models,
                selectors,
                learning_rate,
                final_learning_rate,
                init_weight,
                lr_boost,
                apms,
            } => {
                // The decoder takes the model of a selector as 100 + i or 200 + i
                let selector_models = selectors.iter().filter_map(|selector| match selector {
                    Selector::State(model) | Selector::Context(model) => Some(*model),
                    _ => None,
                });
                let apm_models = apms.iter().filter_map(|apm| match apm.ctx {
                    ApmContext::State(model) | ApmContext::Context(model) => Some(model),
                    _ => None,
                });
                for model in selector_models.chain(apm_models) {
                    anyhow::ensure!(
                        model < models.len().min(100),
                        "ContextMixer selectors and APMs must name one of its first 100 models, not {model}"
                    );
                }
                Box::new(ContextMixer::new(
                    models
                        .iter()
                        .map(|config| config.create_model(hash_table.clone(), static_model_params, shared))
                        .collect::<Result<Vec<_>>>()?,
                    selectors.clone(),
                    *learning_rate,
                    *final_learning_rate,
                    *init_weight,
                    *lr_boost,
                    apms.clone(),
                ))
            }
            ModelConfig::AdaptiveProbabilityMap(_) => anyhow::bail!(
                "The decoder has no AdaptiveProbabilityMap; use the apms of a ContextMixer"
            ),
            ModelConfig::Word => Box::new(NOrderByte::new_word_model(
                hash_table,
                static_model_params.word_max_count,
            )),
            ModelConfig::TokenPosition => Box::new(TokenPosition::new(
                hash_table,
                static_model_params.token_max_count,
                static_model_params.token_context_bytes,
                static_model_params.token_max_position,
            )),
            ModelConfig::MatchPredictor {
                context_bytes,
                confidence,
                table_bits,
            } => Box::new(MatchPredictor::new(
                *context_bytes,
                *confidence,
                *table_bits,
            )),
            ModelConfig::IndirectByte {
                context_bytes,
                max_count,
            } => Box::new(NOrderByte::new_indirect_model(
                *context_bytes,
                hash_table,
                *max_count,
            )),
            ModelConfig::Match {
                min_len,
                candidates,
                expected_byte_context,
                second,
            } => Box::new(MatchModel::new(
                *min_len,
                *candidates,
                *expected_byte_context,
                *second,
            )),
            ModelConfig::JsContext { variant, max_count } => Box::new(JsContext::new(
                *variant,
                hash_table,
                max_count.unwrap_or(15),
            )),
            ModelConfig::WasmContext { fields, max_count } => Box::new(WasmContext::new(
                shared
                    .wasm
                    .clone()
                    .context("WasmContext is only used with WebAssembly modules")?,
                hash_table,
                *fields,
                max_count.unwrap_or(15),
            )),
            ModelConfig::Record(config) => {
                anyhow::ensure!(
                    [0, 2, 4, -2, -4].contains(&config.delta),
                    "Record delta must be 0, 2, 4, -2 or -4"
                );
                anyhow::ensure!(
                    config.max_count.unwrap_or(15) <= 255,
                    "Record max_count must be <= 255"
                );
                Box::new(RecordModel::new(
                    hash_table,
                    shared.records.clone(),
                    config.dists.clone(),
                    config.column,
                    config.delta,
                    config.max_count.unwrap_or(15),
                ))
            }
        })
    }
}

impl ModelConfig {
    /// The models of a mixer
    pub fn mixed_models(&self) -> Option<&Vec<ModelConfig>> {
        match self {
            ModelConfig::Mixer { models } | ModelConfig::ContextMixer { models, .. } => Some(models),
            _ => None,
        }
    }

    pub fn mixed_models_mut(&mut self) -> Option<&mut Vec<ModelConfig>> {
        match self {
            ModelConfig::Mixer { models } | ModelConfig::ContextMixer { models, .. } => Some(models),
            _ => None,
        }
    }

    /// Keeps the models of a mixer for which `keep(index, model)` is true. The selectors and
    /// APMs of a `ContextMixer` that take their context from a removed model go too.
    pub fn retain_mixed_models(&mut self, keep: impl Fn(usize, &ModelConfig) -> bool) {
        let Some(models) = self.mixed_models_mut() else {
            return;
        };
        // The new index of each model, if it stays
        let mut new_index = Vec::new();
        let mut kept = 0;
        for (index, model) in models.iter().enumerate() {
            new_index.push(keep(index, model).then(|| {
                kept += 1;
                kept - 1
            }));
        }
        let mut index = 0;
        models.retain(|_| {
            index += 1;
            new_index[index - 1].is_some()
        });
        if let ModelConfig::ContextMixer {
            selectors, apms, ..
        } = self
        {
            selectors.retain_mut(|selector| match selector {
                Selector::State(model) | Selector::Context(model) => {
                    new_index[*model].map(|i| *model = i).is_some()
                }
                _ => true,
            });
            apms.retain_mut(|apm| match &mut apm.ctx {
                ApmContext::State(model) | ApmContext::Context(model) => {
                    new_index[*model].map(|i| *model = i).is_some()
                }
                _ => true,
            });
        }
    }

    /// Short description of the model, e.g. `NOrderByte 0b00000111`.
    pub fn label(&self) -> String {
        match self {
            ModelConfig::NOrderByte { byte_mask, .. } => format!("NOrderByte {byte_mask}"),
            ModelConfig::CharClass { order, .. } => format!("CharClass order {order}"),
            ModelConfig::Mixer { models } | ModelConfig::ContextMixer { models, .. } => {
                format!("Mixer of {}", models.len())
            }
            ModelConfig::AdaptiveProbabilityMap(_) => "AdaptiveProbabilityMap".to_owned(),
            ModelConfig::Word => "Word".to_owned(),
            ModelConfig::TokenPosition => "TokenPosition".to_owned(),
            ModelConfig::MatchPredictor { context_bytes, .. } => {
                format!("MatchPredictor context {context_bytes}")
            }
            ModelConfig::IndirectByte { context_bytes, .. } => {
                format!("IndirectByte context {context_bytes}")
            }
            ModelConfig::Match { min_len, .. } => format!("Match min_len {min_len}"),
            ModelConfig::JsContext { variant, .. } => format!("JsContext {variant}"),
            ModelConfig::WasmContext { fields, .. } => format!("WasmContext {fields}"),
            ModelConfig::Record(config) => format!(
                "Record {:?}{}{}",
                config.dists,
                if config.column { " column" } else { "" },
                if config.delta != 0 {
                    format!(" delta {}", config.delta)
                } else {
                    String::new()
                }
            ),
        }
    }

    /// Names of the submodels whose predictions `Model::submodel_preds` appends, in the same order.
    /// A name is the path in the config followed by the label, e.g. `model.models[2] NOrderByte 0b00000011`.
    pub fn submodel_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        self.append_submodel_names("model", &mut names);
        names
    }

    fn append_submodel_names(&self, path: &str, out: &mut Vec<String>) {
        match self {
            ModelConfig::Mixer { models } | ModelConfig::ContextMixer { models, .. } => {
                for (index, model) in models.iter().enumerate() {
                    let child_path = format!("{path}.models[{index}]");
                    out.push(format!("{child_path} {}", model.label()));
                    model.append_submodel_names(&child_path, out);
                }
            }
            ModelConfig::AdaptiveProbabilityMap(model) => {
                let child_path = format!("{path}.input");
                out.push(format!("{child_path} {}", model.label()));
                model.append_submodel_names(&child_path, out);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CompressConfig, ModelConfig};
    use crate::compressor::model_finder::{
        SIZE_64K_CONFIG_JSON, SIZE_64K_FAST_CONFIG_JSON, SIZE_64K_SEMI_FAST_CONFIG_JSON,
        SIZE_64K_WASM_CONFIG_JSON,
    };

    #[test]
    fn wasm_preset_is_the_64k_preset_with_wasm_models() {
        let mut size_64k: CompressConfig = serde_json::from_str(SIZE_64K_CONFIG_JSON).unwrap();
        let wasm: CompressConfig = serde_json::from_str(SIZE_64K_WASM_CONFIG_JSON).unwrap();
        assert!(size_64k.static_model_params.wasm.models.is_empty());
        assert!(!wasm.static_model_params.wasm.models.is_empty());
        size_64k.static_model_params.wasm = wasm.static_model_params.wasm.clone();
        assert_eq!(
            serde_json::to_value(&size_64k).unwrap(),
            serde_json::to_value(&wasm).unwrap()
        );
    }

    #[test]
    fn fast_presets_mix_fewer_models_than_the_64k_preset() {
        let model_count = |json| {
            let config: CompressConfig = serde_json::from_str(json).unwrap();
            config.create_model().unwrap();
            let ModelConfig::ContextMixer { models, .. } = &config.model else { panic!() };
            models.len()
        };
        let size_64k = model_count(SIZE_64K_CONFIG_JSON);
        let semi_fast = model_count(SIZE_64K_SEMI_FAST_CONFIG_JSON);
        assert!(model_count(SIZE_64K_FAST_CONFIG_JSON) < semi_fast);
        assert!(semi_fast < size_64k);
    }

    fn create(model: &str) -> anyhow::Result<()> {
        let json = format!(
            r#"{{"model":{model},"static_model_params":{{"hash_table_pow2_size":10}}}}"#
        );
        let config: CompressConfig = serde_json::from_str(&json)?;
        config.create_model().map(|_| ())
    }

    #[test]
    fn rejects_configs_the_decoder_cannot_mirror() {
        let mixer = |selector: &str| {
            format!(
                r#"{{"type":"ContextMixer","models":[{{"type":"NOrderByte","byte_mask":"0b1"}}],
                "selectors":[{selector}],"learning_rate":0.01,"final_learning_rate":0.004,
                "init_weight":0.1,"lr_boost":2,"apms":[]}}"#
            )
        };
        create(&mixer(r#"{"State":0}"#)).unwrap();
        // A model that the mixer does not have
        assert!(create(&mixer(r#"{"State":1}"#)).is_err());
        assert!(create(&mixer(r#"{"Context":1}"#)).is_err());
        // The JS decoder has no AdaptiveProbabilityMap
        let mut config: CompressConfig = serde_json::from_str(
            r#"{"model":{"type":"Word"},"static_model_params":{"hash_table_pow2_size":10}}"#,
        )
        .unwrap();
        config.model = ModelConfig::AdaptiveProbabilityMap(Box::new(config.model));
        let error = config.create_model().err().unwrap().to_string();
        assert!(error.contains("AdaptiveProbabilityMap"), "{error}");
    }
}
