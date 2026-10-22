#[cfg(test)]
use crate::compressor::compress_config::CompressConfig;

pub const DEFAULT_CONFIG_JSON: &str = include_str!("default_config.json");
pub const SIZE_64K_CONFIG_JSON: &str = include_str!("size_64k_config.json");
pub const SIZE_64K_WASM_CONFIG_JSON: &str = include_str!("size_64k_wasm_config.json");
pub const SIZE_64K_FAST_CONFIG_JSON: &str = include_str!("size_64k_fast_config.json");
pub const SIZE_64K_SEMI_FAST_CONFIG_JSON: &str = include_str!("size_64k_semi_fast_config.json");

#[cfg(test)]
pub fn create_default_compress_config() -> CompressConfig {
    serde_json::from_str(DEFAULT_CONFIG_JSON)
        .expect("Failed to parse embedded default compression config")
}
