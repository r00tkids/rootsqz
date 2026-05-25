use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::compressor::compress_config::ModelConfig;
use anyhow::{anyhow, Context, Result};
use bitflags::bitflags;
use clap::ValueEnum;
use handlebars::Handlebars;
use serde_json::json;
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub struct OutputGenerationOptions {
    pub output_dir: PathBuf,
    pub target: Target,
    pub model_config: ModelConfig,
}

bitflags! {
    /// Represents a set of flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ModelRef: u32 {
        const None = 0b00000000;
        const NOrderByte = 0b00000001;
        const Mixer = 0b00000010;
        const AdaptiveProbabilityMap = 0b00000100;
        const Word = 0b00001000;
        const HashTable = 0b00010000;
    }
}

pub fn generate_js_decompression_code(
    model_config: &ModelConfig,
    features_used: &mut ModelRef,
) -> String {
    let mut static_src: String = "".to_owned();
    let mut out_src = "let model = ".to_owned();
    out_src += generate_js_ctors(model_config, features_used).as_str();

    out_src += ";\n";

    static_src += include_str!("js_source/hash_map.js");
    static_src += include_str!("js_source/coder.js");
    static_src += include_str!("js_source/utils.js");

    if *features_used & (ModelRef::NOrderByte | ModelRef::Word) != ModelRef::None {
        static_src += include_str!("js_source/norder_byte.js");
    }

    if features_used.contains(ModelRef::Mixer) {
        static_src += include_str!("js_source/mixer.js");
    }

    if features_used.contains(ModelRef::AdaptiveProbabilityMap) {
        static_src += include_str!("js_source/adaptive_probability_map.js");
    }

    static_src + "\n" + out_src.as_str()
}

fn generate_js_ctors(model_config: &ModelConfig, features_used: &mut ModelRef) -> String {
    match model_config {
        ModelConfig::NOrderByte { byte_mask } => {
            *features_used |= ModelRef::NOrderByte;
            *features_used |= ModelRef::HashTable;
            format!("NOrderByte({}, 0)", byte_mask)
        }
        ModelConfig::Mixer { models } => {
            *features_used |= ModelRef::Mixer;
            let models_js: Vec<String> = models
                .into_iter()
                .map(|c| generate_js_ctors(c, features_used))
                .collect();
            format!("LnMixerPred([{}])", models_js.join(", "))
        }
        ModelConfig::AdaptiveProbabilityMap(model_config) => {
            *features_used |= ModelRef::AdaptiveProbabilityMap;
            let inner_js = generate_js_ctors(model_config, features_used);
            format!("AdaptiveProbabilityMap(19, {})", inner_js)
        }
        ModelConfig::Word => {
            *features_used |= ModelRef::Word;
            "NOrderByte(0, 1)".to_string()
        }
        ModelConfig::Model4k => {
            format!("throw new Error('Model4k is not supported in JS decompressor')")
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug, Clone)]
pub enum Target {
    Web,
    Node,
}

pub struct FileWithContent {
    pub path: PathBuf,
    pub content: Vec<u8>,
}

pub struct BundledFile {
    pub path: PathBuf,
    pub start_offset: u32,
    pub length: u32,
}

pub fn render_output(
    output_options: OutputGenerationOptions,
    size_before_compression: usize,
    encoded_data: Vec<u8>,
    js_main_len: usize,
    bundled_files: Vec<BundledFile>,
    extra_files: Vec<FileWithContent>,
) -> Result<()> {
    debug!("Rendering output with options: {:?}", output_options);

    let OutputGenerationOptions {
        output_dir,
        target,
        model_config,
    } = output_options;

    fs::create_dir_all(&output_dir).context("Failed to create output directory")?;

    let mut features_used = ModelRef::None;
    let decompression_code = generate_js_decompression_code(&model_config, &mut features_used);

    Ok(match target {
        Target::Web => {
            let html_path = output_dir.join("index.html");
            let output_file =
                fs::File::create(&html_path).expect("Failed to create index.html file");

            let mut compressed_data = Vec::<u8>::new();
            encode_compressed_data(&mut compressed_data, &encoded_data)
                .context("Failed to encode compressed data")?;

            let mut files_map = "files:{".to_owned();
            let mut offset = 0;
            let mut is_first = true;
            for file in &extra_files {
                if is_first {
                    is_first = false;
                } else {
                    files_map += ", ";
                }

                let start_offset = encoded_data.len() + offset;
                files_map += &format!(
                    "\"{}\": a.slice({},{})",
                    file.path
                        .file_name()
                        .context("File name")?
                        .to_str()
                        .context("File name to str")?,
                    start_offset,
                    start_offset + file.content.len()
                );

                offset += file.content.len();
                compressed_data.extend_from_slice(&file.content);
            }

            for file in &bundled_files {
                if is_first {
                    is_first = false;
                } else {
                    files_map += ", ";
                }

                files_map += &format!(
                    "\"{}\": d.slice({},{})",
                    file.path
                        .file_name()
                        .context("File name")?
                        .to_str()
                        .context("File name to str")?,
                    file.start_offset,
                    file.start_offset + file.length
                );
            }

            files_map += "}";

            let decompressor_code = Handlebars::new()
                .render_template(
                    include_str!("templates/web/boot.js"),
                    &json!({
                        "decompressor_source": decompression_code,
                        "encoded_len": encoded_data.len(),
                        "decoded_len": size_before_compression,
                        "files_map": files_map,
                        "js_main_len": js_main_len,
                    }),
                )
                .context("Failed to render decompression code template")?;

            let decompressor_code_ugly =
                uglify_src(&decompressor_code).expect("Failed to uglify decompression code");

            info!(
                "Decompression code size before deflate: {}",
                decompressor_code_ugly.len()
            );

            let deflated_code = deflate_text(&decompressor_code_ugly)
                .context("Failed to deflate decompression code")?;

            info!(
                "Decompression code size after deflate: {}",
                deflated_code.len()
            );

            let html_header_str = Handlebars::new()
                .render_template(
                    include_str!("templates/web/index.html"),
                    &json!({
                        "decompressor_end": 171 + deflated_code.len(),
                    }),
                )
                .context("Failed to render html header template")?;
            let html_header_bytes = html_header_str.as_bytes();

            info!(
                "Final overhead: {}",
                html_header_str.len() + deflated_code.len()
            );

            let mut writer = BufWriter::new(output_file);
            writer
                .write_all(html_header_bytes)
                .context("Failed to write HTML header")?;

            writer
                .write_all(deflated_code.as_slice())
                .context("Failed to write decompression code")?;

            writer
                .write_all(&compressed_data)
                .context("Failed to write compressed data")?;

            let final_size = html_header_bytes.len() + deflated_code.len() + compressed_data.len();

            if final_size > size_before_compression {
                println!(
                    "WARNING: Final size ({}) is larger than original size ({})",
                    final_size, size_before_compression
                );
            } else {
                println!(
                    "Generated 'index.html' ({} bytes) with a space saving of {:.2}%",
                    final_size,
                    100. * (1. - final_size as f64 / size_before_compression as f64)
                );
            }
        }
        Target::Node => {
            let encoded_data_path = output_dir.join("input.pack");
            let mut encoded_data_file = BufWriter::new(
                fs::File::create(&encoded_data_path).context("Failed to create input.bin file")?,
            );
            encode_compressed_data(&mut encoded_data_file, &encoded_data)
                .context("Failed to encode compressed data")?;

            let index_src_path = output_dir.join("index.mjs");
            let writer =
                fs::File::create(&index_src_path).expect("Failed to create index.html file");

            let reg = Handlebars::new();
            reg.render_template_to_write(
                include_str!("templates/node/index.mjs"),
                &json!({
                    "decompressor_source": decompression_code,
                    "encoded_len": encoded_data.len(),
                    "decoded_len": size_before_compression,
                    "input_file": "input.pack",
                    "output_file": "output.bin",
                }),
                writer,
            )
            .context("Failed to render node decompressor template")?
        }
    })
}

fn deflate_text(text: &str) -> Result<Vec<u8>> {
    let mut encoded_data = Vec::new();
    let mut writer =
        flate2::write::DeflateEncoder::new(&mut encoded_data, flate2::Compression::best());
    writer.write_all(text.as_bytes())?;
    writer.finish()?;
    Ok(encoded_data)
}

fn encode_compressed_data<T: Write>(writer: &mut T, encoded_data: &[u8]) -> Result<()> {
    writer
        .write(encoded_data)
        .context("Failed to write encoded data to output")?;

    Ok(())
}

fn uglify_src(text: &str) -> Result<String> {
    let child = Command::new("uglifyjs")
        .arg("--compress")
        .arg("--mangle")
        .arg("--toplevel")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context(
            "Failed to run uglifyjs. Run 'npm install -g uglify-js' to install it globally.",
        )?;

    child
        .stdin
        .as_ref()
        .context("Failed to get stdin for uglifyjs")?
        .write_all(text.as_bytes())
        .context("Failed to write to uglifyjs stdin")?;

    let output = child
        .wait_with_output()
        .context("Failed to read output from uglifyjs")?;

    if !output.status.success() {
        return Err(anyhow!("UglifyJS failed with status: {}", output.status));
    }

    Ok(String::from_utf8(output.stdout).context("Failed to parse uglified output")?)
}
