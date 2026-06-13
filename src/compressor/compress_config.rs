use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::model::{
    AdaptiveProbabilityMap, HashTable, LnMixerPred, Model, NOrderByte, NOrderByteData,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressConfig {
    pub model: ModelConfig,
    #[serde(default)]
    pub static_model_params: StaticModelParams,
}

impl CompressConfig {
    pub fn create_model(&self) -> Result<Box<dyn Model>> {
        let hash_table = Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(
            self.static_model_params.hash_table_pow2_size,
        )));
        self.model
            .create_model(hash_table, &self.static_model_params)
    }
}

#[allow(unused)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticModelParams {
    #[serde(default = "default_hash_table_pow2_size")]
    pub hash_table_pow2_size: u32,
    pub mixer: MixerModelParams,
}

impl Default for StaticModelParams {
    fn default() -> Self {
        Self {
            hash_table_pow2_size: default_hash_table_pow2_size(),
            mixer: Default::default(),
        }
    }
}

#[allow(unused)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerModelParams {
    #[serde(default = "default_learning_rate")]
    pub learning_rate: f64,
    #[serde(default = "default_context_learning_rate")]
    pub context_learning_rate: f64,
    #[serde(default = "default_context_fixed_weight")]
    pub context_fixed_weight: f64,
}

impl Default for MixerModelParams {
    fn default() -> Self {
        Self {
            learning_rate: default_learning_rate(),
            context_learning_rate: default_context_learning_rate(),
            context_fixed_weight: default_context_fixed_weight(),
        }
    }
}

fn default_hash_table_pow2_size() -> u32 {
    26
}

fn default_learning_rate() -> f64 {
    0.0004
}

fn default_context_learning_rate() -> f64 {
    0.022
}

fn default_context_fixed_weight() -> f64 {
    0.3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ModelConfig {
    NOrderByte { byte_mask: String },
    Mixer { models: Vec<ModelConfig> },
    AdaptiveProbabilityMap(Box<ModelConfig>),
    Word,
}

impl ModelConfig {
    pub fn create_model(
        &self,
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
        static_model_params: &StaticModelParams,
    ) -> Result<Box<dyn Model>> {
        Ok(match self {
            ModelConfig::NOrderByte { byte_mask } => {
                let byte_mask = u8::from_str_radix(byte_mask.trim_start_matches("0b"), 2)?;
                Box::new(NOrderByte::new_norder_model(byte_mask, hash_table, 15))
            }
            ModelConfig::Mixer { models } => Box::new(LnMixerPred::new(
                models
                    .iter()
                    .map(|config| config.create_model(hash_table.clone(), static_model_params))
                    .collect::<Result<Vec<_>>>()?,
            )),
            ModelConfig::AdaptiveProbabilityMap(model_config) => {
                Box::new(AdaptiveProbabilityMap::new(
                    19,
                    model_config.create_model(hash_table.clone(), static_model_params)?,
                ))
            }
            ModelConfig::Word => Box::new(NOrderByte::new_word_model(hash_table, 15)),
        })
    }
}
