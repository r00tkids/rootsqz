use std::{cell::RefCell, rc::Rc};

use super::{
    compress_config::ModelConfig,
    model::{HashTable, Model, NOrderByteData},
};

#[allow(dead_code)]
pub struct ModelFinder {
    pub default_model: Box<dyn Model>,
}

impl ModelFinder {
    #[allow(dead_code)]
    pub fn new() -> Self {
        let model = Box::new(create_default_model_config());

        Self {
            default_model: model
                .create_model(Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(26))))
                .unwrap(),
        }
    }
}

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
