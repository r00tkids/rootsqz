use crate::compressor::compress_config::{CompressConfig, MixerModelParams, StaticModelParams};

use super::compress_config::ModelConfig;

pub fn create_default_model_config() -> ModelConfig {
    let mut byte_masks = Vec::new();

    let mut byte_mask = 0;
    byte_masks.push(byte_mask);
    for i in 0..8 {
        byte_mask |= 1 << i;
        byte_masks.push(byte_mask);
    }

    let mut byte_mask = 0;
    for i in 0..4 {
        byte_mask |= 1 << i;
        byte_masks.push(byte_mask << 1);
    }

    let mut byte_mask = 0;
    for i in 0..4 {
        byte_mask |= 1 << i;
        byte_masks.push(byte_mask << 2);
    }

    let mut mixed_models = byte_masks
        .into_iter()
        .map(|mask| ModelConfig::NOrderByte {
            byte_mask: format!("0b{:08b}", mask),
        })
        .collect::<Vec<_>>();

    mixed_models.push(ModelConfig::Word);

    ModelConfig::Mixer {
        models: mixed_models.clone(),
    }
}

pub fn create_default_compress_config() -> CompressConfig {
    CompressConfig {
        model: create_default_model_config(),
        static_model_params: StaticModelParams::default()
    }
}
