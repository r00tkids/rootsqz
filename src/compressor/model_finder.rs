use crate::compressor::compress_config::CompressConfig;

pub fn create_default_compress_config() -> CompressConfig {
    serde_json::from_str(include_str!("default_config.json"))
        .expect("Failed to parse embedded default compression config")
}
