use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::compressor::model::Model4k;

use super::model::{
    AdaptiveProbabilityMap, HashTable, LnMixerPred, Model, NOrderByte, NOrderByteData,
};

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressConfig {
    pub model: ModelConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ModelConfig {
    NOrderByte { byte_mask: String },
    Mixer { models: Vec<ModelConfig> },
    AdaptiveProbabilityMap(Box<ModelConfig>),
    Word,
    // A predefined model that uses NOrderByte and mixer, optimized for being procedurally generated
    Model4k,
}

impl ModelConfig {
    pub fn create_model(
        &self,
        hash_table: Rc<RefCell<HashTable<NOrderByteData>>>,
    ) -> Result<Box<dyn Model>> {
        Ok(match self {
            ModelConfig::NOrderByte { byte_mask } => {
                let byte_mask = u8::from_str_radix(byte_mask.trim_start_matches("0b"), 2)?;
                Box::new(NOrderByte::new_norder_model(byte_mask, hash_table, 15))
            }
            ModelConfig::Mixer { models } => Box::new(LnMixerPred::new(
                models
                    .iter()
                    .map(|config| config.create_model(hash_table.clone()))
                    .collect::<Result<Vec<_>>>()?,
            )),
            ModelConfig::AdaptiveProbabilityMap(model_config) => Box::new(
                AdaptiveProbabilityMap::new(19, model_config.create_model(hash_table.clone())?),
            ),
            ModelConfig::Word => Box::new(NOrderByte::new_word_model(hash_table, 15)),
            ModelConfig::Model4k => Box::new(Model4k::default()),
        })
    }
}
