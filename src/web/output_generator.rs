use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::compressor::records::BLOCK;
use crate::web::wasm_decoder::{self, WasmOptions};
use crate::compressor::compress_config::{ModelConfig, StaticModelParams, StreamLayout};
use crate::compressor::mixer::{ApmContext, Selector};
use anyhow::{anyhow, bail, Context, Result};
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
    pub static_model_params: StaticModelParams,
    /// The WebAssembly modules and record ranges of the stream, for their models
    pub layout: StreamLayout,
    /// Loader shown while the web output decompresses
    pub loader: Option<Loader>,
    /// How the web output decodes
    pub decoder: Decoder,
    /// Script run first in the web output, before the loader and decoding
    pub boot_js: Option<String>,
    /// Base64 runs taken out of the main JavaScript, which the decoder puts back
    pub base64: Option<Base64Runs>,
    /// Decode with a WebAssembly decoder instead of the JS one
    pub wasm: Option<WasmOptions>,
}

/// Base64 runs taken out of the main JavaScript. The stream has the JavaScript with a marker
/// byte for each run, and then the bytes that the runs decode to.
#[derive(Debug, Clone)]
pub struct Base64Runs {
    pub marker: u8,
    /// Number of decoded bytes of each run
    pub lengths: Vec<usize>,
}

impl Base64Runs {
    /// JavaScript that puts the runs back into `text`, the main JavaScript of the stream `d` as a
    /// string, with the decoded bytes from offset `js_main_len` on
    fn restore(&self, text: &str, js_main_len: usize) -> String {
        format!(
            "((k, n) => {text}.replace(/\\x{:02x}/g, () => btoa(d.slice(k, k += n.shift()).reduce((s, c) => s + String.fromCharCode(c), \"\"))))({js_main_len}, {:?})",
            self.marker, self.lengths
        )
    }

    fn total_len(&self) -> usize {
        self.lengths.iter().sum()
    }
}

/// JavaScript that evaluates to the main JavaScript as a string, from the decoded stream `d`
fn main_js_source(js_main_len: usize, base64: Option<&Base64Runs>) -> String {
    let text = format!("new TextDecoder().decode(d.slice(0, {js_main_len}))");
    match base64 {
        Some(runs) => runs.restore(&text, js_main_len),
        None => text,
    }
}

/// How the web output decodes
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoder {
    /// In one go on the main thread; the page does not repaint until it is done
    Serial,
    /// On the main thread in slices of about 12 ms, so the page repaints and the loader shows progress
    Sliced,
    /// In a web worker, so the page stays responsive and the loader shows progress
    Worker,
}

/// Loader shown while the web output decompresses
#[derive(Debug, Clone)]
pub struct Loader {
    /// Markup inserted into the page before decoding
    pub html: Option<String>,
    /// Script run before decoding. The sliced and worker decoders report progress to `rsqz.progress` when set
    pub js: Option<String>,
    /// Keep the loader when the main JavaScript runs, instead of clearing the page
    pub keep: bool,
}

bitflags! {
    /// Represents a set of flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ModelRef: u32 {
        const None = 0b00000000;
        const NOrderByte = 0b00000001;
        const Mixer = 0b00000010;
        const Word = 0b00001000;
        const CharClass = 0b00100000;
        const TokenPosition = 0b01000000;
        const MatchPredictor = 0b10000000;
        const IndirectByte = 0b100000000;
        const Match = 0b1000000000;
        const JsContext = 0b10000000000;
        const WasmContext = 0b100000000000;
        const Record = 0b1000000000000;
        const ContextMixer = 0b10000000000000;
    }
}

pub fn generate_js_decompression_code(
    model_config: &ModelConfig,
    static_model_params: &StaticModelParams,
    layout: &StreamLayout,
    features_used: &mut ModelRef,
) -> String {
    let mut out_src = "let model = ".to_owned();
    let mut wasm_ranges = Some(layout.wasm.as_slice()).filter(|ranges| !ranges.is_empty());
    out_src += &generate_js_ctors(
        model_config,
        static_model_params,
        &mut wasm_ranges,
        features_used,
    );

    out_src += ";\n";
    if features_used.contains(ModelRef::Record) {
        // As the gap before each range and its length, in blocks, and its stride
        let mut end = 0;
        let table: Vec<u32> = layout
            .records
            .iter()
            .flat_map(|&[start, range_end, stride]| {
                assert!(start % BLOCK == 0 && range_end % BLOCK == 0);
                let gap = start - std::mem::replace(&mut end, range_end);
                [gap / BLOCK, (range_end - start) / BLOCK, stride]
            })
            .collect();
        out_src = format!("let Records = {table:?};\n") + &out_src;
    }

    let mut static_src: String = include_str!("js_source/utils.js").to_owned();
    static_src += include_str!("js_source/coder.js");

    if features_used.intersects(
        ModelRef::NOrderByte
            | ModelRef::Word
            | ModelRef::CharClass
            | ModelRef::TokenPosition
            | ModelRef::IndirectByte
            | ModelRef::JsContext
            | ModelRef::WasmContext
            | ModelRef::Record,
    ) {
        let bits = static_model_params.hash_table_pow2_size;
        static_src += &format!(
            "let counters = new Uint32Array(2 ** {}), counterBits = {bits};\n",
            bits + 1
        );
        let mut counters = include_str!("js_source/counters.js").replace(
            "count + 0.2",
            &format!("count + {}", static_model_params.count_offset),
        );
        let scale = static_model_params.context_input_scale;
        if scale != 1. {
            // The scales of the inputs, as ContextStats::pred computes them
            for (from, factor) in [
                ("probStretch(p) * 0.5,", 0.5),
                ("(p - 0.5) * 3,", 3.),
                ("/ U24Max) * 0.4,", 0.4),
                ("-info) * 0.25 :", 0.25),
                ("counters[i] >>> 24 ? 0 : 1", 1.),
            ] {
                assert!(counters.contains(from));
                counters = counters.replace(from, &scaled_input(from, factor * scale));
            }
        }
        if features_used.contains(ModelRef::ContextMixer) {
            // The ContextMixer also takes the count of the counter of the bit as the state of a
            // model, as ContextStats::count
            let learn = "    l: (bit, s) => {";
            assert!(counters.contains(learn));
            counters = counters.replace(
                learn,
                &format!("    s: () => ctx == null ? 0 : counters[i] >>> 24,\n{learn}"),
            );
        }
        static_src += &counters;
    }
    for (feature, source) in [
        (ModelRef::NOrderByte, include_str!("js_source/norder_byte.js")),
        (ModelRef::Word, include_str!("js_source/word.js")),
        (ModelRef::CharClass, include_str!("js_source/char_class.js")),
        (ModelRef::IndirectByte, include_str!("js_source/indirect_byte.js")),
        (ModelRef::TokenPosition, include_str!("js_source/token_position.js")),
        (ModelRef::JsContext, include_str!("js_source/js_context.js")),
        (ModelRef::Mixer, include_str!("js_source/mixer.js")),
        (ModelRef::ContextMixer, include_str!("js_source/context_mixer.js")),
        (ModelRef::WasmContext, include_str!("js_source/wasm.js")),
        (ModelRef::Record, include_str!("js_source/record_model.js")),
        (ModelRef::MatchPredictor, include_str!("js_source/match_predictor.js")),
        (ModelRef::Match, include_str!("js_source/match_model.js")),
    ] {
        if features_used.contains(feature) {
            static_src += source;
        }
    }

    static_src + "\n" + out_src.as_str()
}

/// `input` of counters.js with its constant factor replaced by `factor`
fn scaled_input(input: &str, factor: f64) -> String {
    match input {
        "counters[i] >>> 24 ? 0 : 1" => format!("counters[i] >>> 24 ? 0 : {factor}"),
        _ => {
            let (head, rest) = input.rsplit_once("* ").unwrap();
            let end = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
            format!("{head}* {factor}{end}")
        }
    }
}

/// `wasm_ranges` are the stream ranges of the WebAssembly modules, given to the first WasmContext.
fn generate_js_ctors(
    model_config: &ModelConfig,
    static_model_params: &StaticModelParams,
    wasm_ranges: &mut Option<&[(u32, u32)]>,
    features_used: &mut ModelRef,
) -> String {
    match model_config {
        ModelConfig::NOrderByte {
            byte_mask,
            max_count,
        } => {
            *features_used |= ModelRef::NOrderByte;
            let mask = u8::from_str_radix(byte_mask.trim_start_matches("0b"), 2).unwrap_or(0);
            match max_count {
                Some(max_count) => format!("NOrderByte({mask}, {max_count})"),
                None => format!("NOrderByte({mask})"),
            }
        }
        ModelConfig::CharClass { order, max_count } => {
            *features_used |= ModelRef::CharClass;
            format!("CharClass({order}, {})", max_count.unwrap_or(15))
        }
        ModelConfig::Mixer { models } => {
            *features_used |= ModelRef::Mixer;
            let models_js: Vec<String> = models
                .iter()
                .map(|c| generate_js_ctors(c, static_model_params, wasm_ranges, features_used))
                .collect();
            format!(
                "Mixer([{}], {}, {}, {})",
                models_js.join(", "),
                static_model_params.mixer.learning_rate,
                static_model_params.mixer.context_learning_rate,
                static_model_params.mixer.context_weight_scale
            )
        }
        ModelConfig::ContextMixer {
            models,
            selectors,
            learning_rate,
            final_learning_rate,
            init_weight,
            lr_boost,
            apms,
        } => {
            *features_used |= ModelRef::ContextMixer;
            let models_js: Vec<String> = models
                .iter()
                .map(|c| generate_js_ctors(c, static_model_params, wasm_ranges, features_used))
                .collect();
            let selectors: Vec<String> = selectors
                .iter()
                .map(|s| match s {
                    Selector::None => 0,
                    Selector::Order1 => 1,
                    Selector::Known => 2,
                    Selector::Context(model) => 100 + model,
                    Selector::State(model) => 200 + model,
                }.to_string())
                .collect();
            let apms: Vec<String> = apms
                .iter()
                .map(|apm| {
                    let (ctx, model, bits) = match apm.ctx {
                        ApmContext::Order0 => (0, 0, None),
                        ApmContext::Order1 => (1, 0, None),
                        ApmContext::Order2 => (2, 0, Some(apm.bits)),
                        ApmContext::State(model) => (3, model, None),
                        ApmContext::Context(model) => (4, model, Some(apm.bits)),
                    };
                    let bits = bits.map(|bits| format!(",{bits}")).unwrap_or_default();
                    format!("[{ctx},{model},{},{}{bits}]", apm.limit, apm.weight)
                })
                .collect();
            format!(
                "ContextMixer([{}], [{}], {learning_rate}, {final_learning_rate}, {init_weight}, {lr_boost}, [{}])",
                models_js.join(", "),
                selectors.join(","),
                apms.join(",")
            )
        }
        ModelConfig::AdaptiveProbabilityMap(_) => {
            unreachable!("create_model rejects AdaptiveProbabilityMap")
        }
        ModelConfig::Word => {
            *features_used |= ModelRef::Word;
            format!("Word({})", static_model_params.word_max_count)
        }
        ModelConfig::TokenPosition => {
            *features_used |= ModelRef::TokenPosition;
            format!(
                "TokenPosition({}, {}, {})",
                static_model_params.token_max_count,
                static_model_params.token_context_bytes,
                static_model_params.token_max_position
            )
        }
        ModelConfig::MatchPredictor {
            context_bytes,
            confidence,
            table_bits,
        } => {
            *features_used |= ModelRef::MatchPredictor;
            format!(
                "MatchPredictor({}, {}, {})",
                context_bytes, confidence, table_bits
            )
        }
        ModelConfig::IndirectByte {
            context_bytes,
            max_count,
        } => {
            *features_used |= ModelRef::IndirectByte;
            format!("IndirectByte({context_bytes}, {max_count})")
        }
        ModelConfig::Match {
            min_len,
            candidates,
            expected_byte_context,
            second,
        } => {
            *features_used |= ModelRef::Match;
            format!(
                "MatchModel({min_len}, {candidates}, {})",
                *expected_byte_context as u8 | (*second as u8) << 1
            )
        }
        ModelConfig::JsContext { variant, max_count } => {
            *features_used |= ModelRef::JsContext;
            format!("JsContext({}, {})", variant, max_count.unwrap_or(15))
        }
        ModelConfig::WasmContext { fields, max_count } => {
            *features_used |= ModelRef::WasmContext;
            let max_count = max_count.unwrap_or(15);
            match wasm_ranges.take() {
                Some(ranges) => {
                    let ranges: Vec<String> = ranges
                        .iter()
                        .map(|(start, end)| format!("[{start},{end}]"))
                        .collect();
                    format!("WasmContext({fields}, {max_count}, [{}])", ranges.join(","))
                }
                None => format!("WasmContext({fields}, {max_count})"),
            }
        }
        ModelConfig::Record(config) => {
            *features_used |= ModelRef::Record;
            format!(
                "RecordModel({:?}, {}, {}, {})",
                config.dists,
                config.column as u8,
                config.delta,
                config.max_count.unwrap_or(15)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::compress_config::MixerModelParams;

    #[test]
    fn mixer_params_are_emitted_to_js_constructor() {
        let mut static_model_params = StaticModelParams::default();
        static_model_params.mixer = MixerModelParams {
            learning_rate: 0.001,
            context_learning_rate: 0.02,
            context_weight_scale: 0.4,
        };

        let model_config = ModelConfig::Mixer {
            models: vec![ModelConfig::NOrderByte {
                byte_mask: "0b00000000".to_string(),
                max_count: None,
            }],
        };
        let mut features_used = ModelRef::None;

        let js =
            generate_js_decompression_code(&model_config, &static_model_params, &StreamLayout::default(), &mut features_used);

        assert!(js.contains("Mixer([NOrderByte(0)], 0.001, 0.02, 0.4)"));
    }

    #[test]
    fn word_count_is_emitted_to_js_constructor() {
        let mut params = StaticModelParams::default();
        params.word_max_count = 2;
        let mut features_used = ModelRef::None;

        let js = generate_js_decompression_code(&ModelConfig::Word, &params, &StreamLayout::default(), &mut features_used);

        assert!(js.contains("Word(2)"));
    }

    #[test]
    fn byte_model_count_cap_is_emitted_to_js_constructor() {
        let model_config = ModelConfig::NOrderByte {
            byte_mask: "0b00000001".to_string(),
            max_count: Some(3),
        };
        let mut features_used = ModelRef::None;
        let js = generate_js_decompression_code(
            &model_config,
            &StaticModelParams::default(),
            &StreamLayout::default(), &mut features_used,
        );
        assert!(js.contains("let model = NOrderByte(1, 3);"));
    }

    #[test]
    fn token_position_params_are_emitted_to_js_constructor() {
        let mut params = StaticModelParams::default();
        params.token_max_count = 4;
        params.token_context_bytes = 0;
        params.token_max_position = 2;
        let mut features_used = ModelRef::None;
        let js = generate_js_decompression_code(
            &ModelConfig::TokenPosition,
            &params,
            &StreamLayout::default(), &mut features_used,
        );
        assert!(js.contains("let model = TokenPosition(4, 0, 2);"));
        assert!(features_used.contains(ModelRef::TokenPosition));
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

/// The files written by `render_output` and what they consist of, for the report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputFile {
    pub name: String,
    pub size: usize,
    pub parts: Vec<OutputPart>,
}

/// A byte range of an output file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputPart {
    pub name: String,
    pub offset: usize,
    pub length: usize,
    pub description: String,
}

impl OutputPart {
    fn new(
        name: impl Into<String>,
        offset: usize,
        length: usize,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            offset,
            length,
            description: description.into(),
        }
    }
}

pub struct BundledFile {
    pub path: PathBuf,
    pub start_offset: u32,
    pub length: u32,
}

/// Pack a small web demo as a Brotli stream appended to its HTML loader.
pub fn render_brotli_output(
    output_dir: &Path,
    main_js: &[u8],
    files: &[FileWithContent],
    boot_js: Option<&str>,
) -> Result<()> {
    // The script runs before the fetch, minified, as the Brotli header is not compressed
    let boot_js = match boot_js {
        Some(js) => uglify_src(&format!("(() => {{\n{js}\n}})();"))
            .context("Failed to minify the boot script")?
            .trim_end()
            .trim_end_matches(';')
            .to_owned()
            + ";",
        None => String::new(),
    };
    fs::create_dir_all(output_dir).context("Failed to create output directory")?;
    let mut source = main_js.to_vec();
    let mut files_map = Vec::new();
    for file in files {
        let name = file
            .path
            .file_name()
            .context("File name")?
            .to_str()
            .context("File name to str")?;
        let start = source.len();
        source.extend_from_slice(&file.content);
        files_map.push(format!(
            "{}:a.slice({},{})",
            serde_json::to_string(name)?,
            start,
            source.len()
        ));
    }

    let mut compressed = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 11, 22);
        writer
            .write_all(&source)
            .context("Failed to compress demo with Brotli")?;
    }
    let template = include_str!("templates/web/brotli.html");
    let render = |offset: usize| -> Result<String> {
        Ok(Handlebars::new().render_template(
            template,
            &json!({
                "offset": offset,
                "end": offset + compressed.len(),
                "boot_js": boot_js,
                "js_len": main_js.len(),
                "files_map": format!("{{{}}}", files_map.join(",")),
            }),
        )?)
    };
    let mut header = render(0)?;
    loop {
        let next = render(header.len())?;
        if next.len() == header.len() {
            header = next;
            break;
        }
        header = next;
    }
    let path = output_dir.join("index.html");
    let mut writer =
        BufWriter::new(fs::File::create(&path).context("Failed to create index.html")?);
    writer.write_all(header.as_bytes())?;
    writer.write_all(&compressed)?;
    writer.flush()?;
    println!(
        "Generated '{}' ({} bytes)",
        path.display(),
        header.len() + compressed.len()
    );
    Ok(())
}

pub fn render_output(
    output_options: OutputGenerationOptions,
    size_before_compression: usize,
    encoded_data: Vec<u8>,
    js_main_len: usize,
    bundled_files: Vec<BundledFile>,
    extra_files: Vec<FileWithContent>,
) -> Result<Vec<OutputFile>> {
    debug!("Rendering output with options: {:?}", output_options);

    let OutputGenerationOptions {
        output_dir,
        target,
        model_config,
        static_model_params,
        layout,
        loader,
        decoder,
        boot_js,
        base64,
        wasm,
    } = output_options;

    fs::create_dir_all(&output_dir).context("Failed to create output directory")?;

    // The node output decodes in one go
    let decoder = if target == Target::Node { Decoder::Serial } else { decoder };
    let (decompression_code, wasm_bytes) = match wasm {
        Some(options) => {
            let module = wasm_decoder::generate(
                &model_config,
                &static_model_params,
                &layout,
                encoded_data.len(),
                size_before_compression,
                options,
            )?;
            info!("WebAssembly decoder: {} bytes", module.bytes.len());
            (wasm_glue(&module, encoded_data.len(), size_before_compression, decoder), Some(module.bytes))
        }
        None => {
            let mut features_used = ModelRef::None;
            let mut decompression_code = generate_js_decompression_code(
                &model_config,
                &static_model_params,
                &layout,
                &mut features_used,
            );
            // Only the decoder that the boot code calls is included
            decompression_code += match decoder {
                Decoder::Serial => include_str!("js_source/decompress.js"),
                Decoder::Sliced => include_str!("js_source/coder_sliced.js"),
                Decoder::Worker => include_str!("js_source/coder_worker.js"),
            };
            (decompression_code, None)
        }
    };

    Ok(match target {
        Target::Web => {
            let mut pre_compressed_parts = Vec::new();
            let html_path = output_dir.join("index.html");
            let output_file =
                fs::File::create(&html_path).expect("Failed to create index.html file");

            let mut compressed_data = Vec::<u8>::new();
            encode_compressed_data(&mut compressed_data, &encoded_data)
                .context("Failed to encode compressed data")?;

            let mut files_map = "{".to_owned();
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

                pre_compressed_parts.push((
                    format!("pre-compressed file {}", file.path.display()),
                    start_offset,
                    file.content.len(),
                ));
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

            let (decompressor_code_ugly, deflated_code) = boot_code(
                &decompression_code,
                encoded_data.len(),
                size_before_compression,
                &files_map,
                &main_js_source(js_main_len, base64.as_ref()),
                loader.as_ref(),
                decoder,
                boot_js.as_deref(),
                wasm_bytes.as_deref(),
            )?;

            let html_header_str = html_header(deflated_code.len())?;
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

            let payload_offset = html_header_bytes.len() + deflated_code.len();
            let mut parts = vec![
                OutputPart::new(
                    "html header",
                    0,
                    html_header_bytes.len(),
                    "HTML that inflates and runs the decompressor",
                ),
                OutputPart::new(
                    "decompressor",
                    html_header_bytes.len(),
                    deflated_code.len(),
                    match &wasm_bytes {
                        Some(bytes) => format!(
                            "decompressor JavaScript, {} bytes minified, with the WebAssembly decoder of {} bytes, deflated",
                            decompressor_code_ugly.len() - js_string_literal(bytes).len(),
                            bytes.len()
                        ),
                        None => format!(
                            "decompressor JavaScript, {} bytes minified, deflated",
                            decompressor_code_ugly.len()
                        ),
                    },
                ),
                OutputPart::new(
                    "compressed stream",
                    payload_offset,
                    encoded_data.len(),
                    "arithmetic coded stream of the main JavaScript and the --file files",
                ),
            ];
            for (name, offset, length) in pre_compressed_parts {
                parts.push(OutputPart::new(
                    name,
                    payload_offset + offset,
                    length,
                    "stored as it is",
                ));
            }
            vec![OutputFile {
                name: "index.html".to_owned(),
                size: final_size,
                parts,
            }]
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
            let index_src = reg
                .render_template(
                    include_str!("templates/node/index.mjs"),
                    &json!({
                        "decompressor_source": match &wasm_bytes {
                            Some(bytes) => insert_wasm(&decompression_code, bytes)?,
                            None => decompression_code,
                        },
                        "wasm": wasm_bytes.is_some(),
                        "encoded_len": encoded_data.len(),
                        "decoded_len": size_before_compression,
                        "input_file": "input.pack",
                        "output_file": "output.bin",
                        // The node output is the original stream, so it puts the runs back as bytes
                        "restore": base64.as_ref().map(|runs| format!(
                            "d => Buffer.concat([Buffer.from({}, 'latin1'), d.slice({})])",
                            runs.restore(&format!("Buffer.from(d.slice(0, {js_main_len})).toString('latin1')"), js_main_len),
                            js_main_len + runs.total_len()
                        )),
                    }),
                )
                .context("Failed to render node decompressor template")?;
            BufWriter::new(writer)
                .write_all(index_src.as_bytes())
                .context("Failed to write node decompressor")?;

            vec![
                OutputFile {
                    name: "index.mjs".to_owned(),
                    size: index_src.len(),
                    parts: vec![OutputPart::new(
                        "decompressor",
                        0,
                        index_src.len(),
                        "decompressor JavaScript",
                    )],
                },
                OutputFile {
                    name: "input.pack".to_owned(),
                    size: encoded_data.len(),
                    parts: vec![OutputPart::new(
                        "compressed stream",
                        0,
                        encoded_data.len(),
                        "arithmetic coded stream of the main JavaScript and the --file files",
                    )],
                },
            ]
        }
    })
}

/// The boot code of the web output, minified and deflated.
fn boot_code(
    decompression_code: &str,
    encoded_len: usize,
    decoded_len: usize,
    files_map: &str,
    main_js: &str,
    loader: Option<&Loader>,
    decoder: Decoder,
    boot_js: Option<&str>,
    wasm: Option<&[u8]>,
) -> Result<(String, Vec<u8>)> {
    let decompressor_code = Handlebars::new()
        .render_template(
            include_str!("templates/web/boot.js"),
            &json!({
                "decompressor_source": decompression_code,
                "encoded_len": encoded_len,
                "decoded_len": decoded_len,
                "files_map": files_map,
                "main_js": main_js,
                "loader": loader.is_some(),
                // Trimmed, as the trailing newline of a file would be packed as well
                "loader_html": serde_json::to_string(
                    loader.and_then(|loader| loader.html.as_deref()).unwrap_or("").trim()
                )?,
                "loader_js": loader.and_then(|loader| loader.js.as_deref()),
                "keep": loader.is_some_and(|loader| loader.keep),
                "sliced": decoder == Decoder::Sliced,
                "worker": decoder == Decoder::Worker,
                "boot_js": boot_js,
                "wasm": wasm.is_some(),
            }),
        )
        .context("Failed to render decompression code template")?;

    // The loader and boot scripts are part of the boot code, so a syntax error in them fails here
    let mut decompressor_code_ugly = uglify_src(&decompressor_code)
        .context("Failed to minify the boot code, including the loader and boot scripts")?;
    if let Some(bytes) = wasm {
        // After minifying, which would escape the bytes
        decompressor_code_ugly = insert_wasm(&decompressor_code_ugly, bytes)?;
    }

    info!(
        "Decompression code size before deflate: {}",
        decompressor_code_ugly.len()
    );

    let deflated_code =
        deflate_text(&decompressor_code_ugly).context("Failed to deflate decompression code")?;

    info!(
        "Decompression code size after deflate: {}",
        deflated_code.len()
    );
    Ok((decompressor_code_ugly, deflated_code))
}

/// What the web output of a stream is made of besides the compressed stream and the decoder,
/// so that `web_overhead` can weigh the decoder of a model config.
#[derive(Debug, Clone)]
pub struct WebLayout {
    /// The WebAssembly modules and record ranges of the stream, whose models `model_config` has
    pub stream: StreamLayout,
    pub base64: Option<Base64Runs>,
    pub js_main_len: usize,
    pub decoded_len: usize,
    /// About the length of the compressed stream, which the boot code has as a number
    pub encoded_len: usize,
    /// The files of the stream: name, start and length
    pub files: Vec<(String, u32, u32)>,
}

/// Bytes of the web output that are not the compressed stream, with the serial decoder and no
/// loader: the HTML header and the deflated boot code, whose decoder `model_config` makes.
/// Used to weigh a config in the search.
pub fn web_overhead(
    model_config: &ModelConfig,
    static_model_params: &StaticModelParams,
    layout: &WebLayout,
) -> Result<usize> {
    let mut features_used = ModelRef::None;
    let mut decompression_code = generate_js_decompression_code(
        model_config,
        static_model_params,
        &layout.stream,
        &mut features_used,
    );
    decompression_code += include_str!("js_source/decompress.js");
    let files_map: Vec<String> = layout
        .files
        .iter()
        .map(|(name, start, len)| format!("{name:?}: d.slice({start},{})", start + len))
        .collect();
    let (_, deflated_code) = boot_code(
        &decompression_code,
        layout.encoded_len,
        layout.decoded_len,
        &format!("{{{}}}", files_map.join(", ")),
        &main_js_source(layout.js_main_len, layout.base64.as_ref()),
        None,
        Decoder::Serial,
        None,
        None,
    )?;
    Ok(html_header(deflated_code.len())?.len() + deflated_code.len())
}

/// The JavaScript that runs the WebAssembly decoder: `wasm_decoder.js` and the function that the
/// boot code calls, with the lengths and addresses as numbers
pub(crate) fn wasm_glue(module: &wasm_decoder::WasmDecoder, encoded_len: usize, decoded_len: usize, decoder: Decoder) -> String {
    let code = include_str!("js_source/wasm_decoder.js").to_owned()
        + match decoder {
            Decoder::Serial => include_str!("js_source/wasm_decompress.js"),
            Decoder::Sliced => include_str!("js_source/wasm_sliced.js"),
            Decoder::Worker => include_str!("js_source/wasm_worker.js"),
        };
    code.replace("WASM_ENCODED_LEN", &encoded_len.to_string())
        .replace("WASM_DECODED_LEN", &decoded_len.to_string())
        .replace("WASM_INPUT", &module.input.to_string())
        .replace("WASM_OUTPUT", &module.output.to_string())
}

/// `code` with the string "@@WASM@@" of the glue replaced by the bytes of the module
pub(crate) fn insert_wasm(code: &str, bytes: &[u8]) -> Result<String> {
    for quoted in ["\"@@WASM@@\"", "'@@WASM@@'", "`@@WASM@@`"] {
        if code.matches(quoted).count() == 1 {
            return Ok(code.replacen(quoted, &js_string_literal(bytes), 1));
        }
    }
    bail!("The boot code has no place for the WebAssembly decoder")
}

/// A JavaScript string literal of the characters with the codes of `bytes`. The boot code is UTF-8,
/// so the bytes from 128 on take two bytes before deflate.
fn js_string_literal(bytes: &[u8]) -> String {
    let mut literal = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => literal += "\\\"",
            b'\\' => literal += "\\\\",
            b'\n' => literal += "\\n",
            b'\r' => literal += "\\r",
            _ => literal.push(b as char),
        }
    }
    literal.push('"');
    literal
}

/// The HTML that inflates and runs the boot code of `deflated_len` bytes
fn html_header(deflated_len: usize) -> Result<String> {
    Handlebars::new()
        .render_template(
            include_str!("templates/web/index.html"),
            &json!({
                "decompressor_end": 171 + deflated_len,
            }),
        )
        .context("Failed to render html header template")
}

/// Raw deflate with zopfli, which finds a smaller encoding than zlib at its best level
fn deflate_text(text: &str) -> Result<Vec<u8>> {
    let mut encoded_data = Vec::new();
    let options = zopfli::Options {
        // The boot code is a few KiB, so many iterations take well under a second
        iteration_count: std::num::NonZeroU64::new(1000).unwrap(),
        ..Default::default()
    };
    zopfli::compress(
        options,
        zopfli::Format::Deflate,
        text.as_bytes(),
        &mut encoded_data,
    )?;
    Ok(encoded_data)
}

fn encode_compressed_data<T: Write>(writer: &mut T, encoded_data: &[u8]) -> Result<()> {
    writer
        .write(encoded_data)
        .context("Failed to write encoded data to output")?;

    Ok(())
}

/// Builds the command that runs UglifyJS.
///
/// `ROOTSQZ_UGLIFYJS` names the program to run instead of `uglifyjs` from PATH.
/// If `ROOTSQZ_NODE` is set as well, `ROOTSQZ_UGLIFYJS` is a script run by that node executable.
/// The npm package sets both to use its own uglify-js dependency.
fn uglify_command(default_program: &str) -> Command {
    match std::env::var_os("ROOTSQZ_UGLIFYJS") {
        Some(uglifyjs) => match std::env::var_os("ROOTSQZ_NODE") {
            Some(node) => {
                let mut command = Command::new(node);
                command.arg(uglifyjs);
                command
            }
            None => Command::new(uglifyjs),
        },
        None => Command::new(default_program),
    }
}

fn spawn_uglify(default_program: &str) -> std::io::Result<std::process::Child> {
    uglify_command(default_program)
        // More passes make the boot code a little smaller, e.g. 27 bytes after deflate for the ray tracer test
        .arg("--compress")
        .arg("passes=3")
        .arg("--mangle")
        .arg("--toplevel")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
}

fn uglify_src(text: &str) -> Result<String> {
    let mut spawned = spawn_uglify("uglifyjs");
    // npm installs uglifyjs as a .cmd shim on Windows, which is not found without the extension
    if cfg!(windows) && matches!(&spawned, Err(err) if err.kind() == std::io::ErrorKind::NotFound) {
        spawned = spawn_uglify("uglifyjs.cmd");
    }
    let child = spawned.context(
        "Failed to run uglifyjs. Run 'npm install -g uglify-js' to install it globally, or set ROOTSQZ_UGLIFYJS.",
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
