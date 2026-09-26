use crate::compressor::compress_config::CompressConfig;

pub fn create_default_compress_config() -> CompressConfig {
    serde_json::from_str(include_str!("default_config.json"))
        .expect("Failed to parse embedded default compression config")
}

pub fn create_64k_compress_config() -> CompressConfig {
    serde_json::from_str(include_str!("size_64k_config.json"))
        .expect("Failed to parse embedded 64k compression config")
}
