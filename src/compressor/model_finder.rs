use std::{cell::RefCell, rc::Rc};

use super::{
    compress_config::ModelConfig,
    model::{HashTable, Model, NOrderByteData, MODEL4K_NORDER_MASKS},
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
    let mut mixed_models = MODEL4K_NORDER_MASKS
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
