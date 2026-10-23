use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

pub mod base64;
pub mod output_generator;
pub mod wasm_decoder;

use crate::{
    compressor::{
        compress_config::{CompressConfig, StreamLayout},
        model_finder::{
            DEFAULT_CONFIG_JSON, SIZE_64K_CONFIG_JSON, SIZE_64K_FAST_CONFIG_JSON,
            SIZE_64K_SEMI_FAST_CONFIG_JSON, SIZE_64K_WASM_CONFIG_JSON,
        },
        records::{self, detect_records},
        Encoder,
    },
    report::{
        is_text,
        source_map::{FileMapping, MapFile, SourceMapping},
        wasm::{self, WasmDebug},
        ReportData, ReportOptions, Section, View, REPORT_DIR,
    },
};
use crate::compressor::search::{
    cost, demo_eval, search, stream_size, Demo, SearchOptions, MAX_DECODER_MEMORY,
};
use output_generator::{
    render_output, BundledFile, Decoder, Loader, OutputGenerationOptions, WebLayout,
};
use wasm_decoder::WasmOptions;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Javascript file being evaluated after decompression
    #[arg(short, long)]
    pub js_main: String,

    /// File to be included and packed into the output, with compression. Can be given multiple times.
    /// Order matters, so files of similar content should be ordered together.
    #[arg(short, long = "file", value_name = "FILE")]
    pub files: Vec<String>,

    /// File that is already compressed, included and packed into the output as it is.
    /// Can be given multiple times.
    #[arg(short = 'p', long = "pre-compressed-file", value_name = "FILE")]
    pub pre_compressed_files: Vec<String>,

    /// Output directory
    #[arg(short, long)]
    pub output_directory: String,

    /// Target platform for the output
    #[arg(short, long, default_value = "web")]
    pub target: output_generator::Target,

    /// Write a compression report to the report directory in the output directory:
    /// report.json (summary for analysis, e.g. by an LLM), report.html (viewer) and data.bin
    /// (the predictions, queried with `rootsqz report`)
    #[arg(short, long)]
    pub report: bool,

    /// Also record the predictions of every submodel in the report (implies --report).
    /// Needs memory: 32 bytes per input byte and submodel
    #[arg(long)]
    pub report_models: bool,

    /// Also write these views of the report as TSV files (implies --report), e.g. lines,tokens
    #[arg(long, value_enum, value_delimiter = ',', value_name = "VIEW")]
    pub report_detail: Vec<View>,

    /// Source map of the main JavaScript, to tell in the report which original source files and
    /// lines its bytes come from (implies --report)
    #[arg(long, value_name = "FILE")]
    pub report_source_map: Option<PathBuf>,

    /// Name of a file in the report, e.g. the module a bundler made it from (implies --report).
    /// FILE is the path as given with --js-main or --file. Can be given multiple times.
    #[arg(long, value_name = "FILE=LABEL", value_parser = parse_label)]
    pub report_label: Vec<(String, String)>,

    /// Debug info of a wasm file given with --file, to name its functions and tell which source
    /// lines its bytes come from (implies --report). DEBUG is the wasm file before stripping, with
    /// its name section and DWARF, or a wasm source map (.wasm.map). Can be given multiple times.
    /// Without it, x.debug.wasm and x.wasm.map next to x.wasm are used if they exist
    #[arg(long, value_name = "FILE=DEBUG", value_parser = parse_label)]
    pub report_wasm_debug: Vec<(String, String)>,

    /// Compression model config JSON (uses the embedded default when omitted)
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Embedded model preset, used when --config is omitted (default: 64k)
    #[arg(long, value_enum)]
    pub size_profile: Option<SizeProfile>,

    /// Pack a web input as a Brotli stream (Firefox 147+ / Safari 18.4+)
    #[arg(long)]
    pub brotli: bool,

    /// Built-in loader shown while the web output decompresses
    #[arg(long, value_enum, value_name = "PRESET", conflicts_with_all = ["loader_html", "loader_js"])]
    pub loader: Option<LoaderPreset>,

    /// HTML (with CSS) shown while the web output decompresses. Packed as it is, without minification
    #[arg(long, value_name = "FILE")]
    pub loader_html: Option<PathBuf>,

    /// JavaScript run before the web output decompresses. It sets `rsqz.progress = (done, total) => ...`,
    /// which is called with the number of decoded bytes while decoding runs in slices or in a worker
    #[arg(long, value_name = "FILE")]
    pub loader_js: Option<PathBuf>,

    /// Keep the loader when the main JavaScript runs, instead of clearing the page.
    /// `rsqz.progress` stays, so the demo can continue the progress display
    #[arg(long)]
    pub loader_keep: bool,

    /// How the web output decodes. By default `sliced` with a loader script, otherwise `serial`
    #[arg(long, value_enum, value_name = "DECODER")]
    pub decoder: Option<Decoder>,

    /// Decode with a WebAssembly decoder generated for the model config, which decodes several
    /// times as fast as the JavaScript one but takes more bytes. Works with every --decoder
    #[arg(long)]
    pub wasm_decoder: bool,

    /// Title of the web page, set before the web output decompresses
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,

    /// JavaScript run first in the web output, before the loader and decompression, e.g. to set
    /// up the page. It is minified with the boot code
    #[arg(long, value_name = "FILE")]
    pub boot_js: Option<PathBuf>,

    /// Search for the model config that packs the input smallest, starting from --config or the
    /// --size-profile preset, before packing. Takes minutes to hours; the decoder is generated
    /// from the config found. The best config so far is written to search_config.json in the
    /// output directory after each improvement, and running the same command again resumes a
    /// stopped search
    #[arg(long)]
    pub search: bool,

    /// Number of configs --search evaluates at the same time (default: the number of CPUs)
    #[arg(long, value_name = "N")]
    pub search_threads: Option<usize>,

    /// Directory where --search keeps the config it found for an input and start config, to
    /// reuse it when the same input is packed again
    #[arg(long, value_name = "DIR")]
    pub search_cache: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderPreset {
    /// Spinning ring, with the decoder running in one go by default
    Spinner,
    /// Progress bar, with the decoder running in slices by default
    Bar,
    /// The browser's own progress bar (a <progress> element), with the decoder running in slices by default
    Progress,
}

impl Args {
    /// The decoder given by --decoder, or the default for `loader`.
    fn decoder(&self, loader: Option<&Loader>) -> Result<Decoder> {
        match self.decoder {
            Some(decoder) => {
                if self.target != output_generator::Target::Web || self.brotli {
                    bail!("--decoder is only supported for the web target without --brotli");
                }
                Ok(decoder)
            }
            None if loader.is_some_and(|loader| loader.js.is_some()) => Ok(Decoder::Sliced),
            None => Ok(Decoder::Serial),
        }
    }

    /// The script that runs first in the web output: --title, then --boot-js.
    fn boot_js(&self) -> Result<Option<String>> {
        let mut js = String::new();
        if let Some(title) = &self.title {
            js += &format!("document.title = {};\n", serde_json::to_string(title)?);
        }
        if let Some(path) = &self.boot_js {
            js += &std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read boot script: {}", path.display()))?;
        }
        if js.is_empty() {
            return Ok(None);
        }
        if self.target != output_generator::Target::Web {
            bail!("--title and --boot-js are only supported for the web target");
        }
        Ok(Some(js))
    }

    /// The loader given by the options, or `None` without loader options.
    fn loader(&self) -> Result<Option<Loader>> {
        let read = |path: &Option<PathBuf>| -> Result<Option<String>> {
            path.as_ref()
                .map(|path| {
                    std::fs::read_to_string(path)
                        .with_context(|| format!("Failed to read loader file: {}", path.display()))
                })
                .transpose()
        };
        let (html, js) = match self.loader {
            Some(LoaderPreset::Spinner) => (
                Some(include_str!("web/templates/web/loaders/spinner.html").to_owned()),
                None,
            ),
            Some(LoaderPreset::Bar) => (
                Some(include_str!("web/templates/web/loaders/bar.html").to_owned()),
                Some(include_str!("web/templates/web/loaders/bar.js").to_owned()),
            ),
            Some(LoaderPreset::Progress) => (
                Some(include_str!("web/templates/web/loaders/progress.html").to_owned()),
                Some(include_str!("web/templates/web/loaders/progress.js").to_owned()),
            ),
            None => (read(&self.loader_html)?, read(&self.loader_js)?),
        };
        if html.is_none() && js.is_none() {
            if self.loader_keep {
                bail!("--loader-keep requires --loader, --loader-html or --loader-js");
            }
            return Ok(None);
        }
        if self.target != output_generator::Target::Web || self.brotli {
            bail!("Loaders are only supported for the web target without --brotli");
        }
        Ok(Some(Loader {
            html,
            js,
            keep: self.loader_keep,
        }))
    }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeProfile {
    #[value(name = "4k")]
    FourK,
    #[value(name = "64k")]
    SixtyFourK,
    /// The 64k preset with context models for WebAssembly files
    #[value(name = "64k-wasm")]
    SixtyFourKWasm,
    /// The 64k preset with fewer models, whose decoder runs about 1.7 times as fast
    #[value(name = "64k-semi-fast")]
    SixtyFourKSemiFast,
    /// The 64k preset with even fewer models, whose decoder runs about twice as fast
    #[value(name = "64k-fast")]
    SixtyFourKFast,
}

impl SizeProfile {
    /// The JSON of the embedded config
    pub fn config_json(self) -> &'static str {
        match self {
            SizeProfile::FourK => DEFAULT_CONFIG_JSON,
            SizeProfile::SixtyFourK => SIZE_64K_CONFIG_JSON,
            SizeProfile::SixtyFourKWasm => SIZE_64K_WASM_CONFIG_JSON,
            SizeProfile::SixtyFourKSemiFast => SIZE_64K_SEMI_FAST_CONFIG_JSON,
            SizeProfile::SixtyFourKFast => SIZE_64K_FAST_CONFIG_JSON,
        }
    }
}

impl Args {
    fn wants_report(&self) -> bool {
        self.report
            || self.report_models
            || !self.report_detail.is_empty()
            || self.report_source_map.is_some()
            || !self.report_label.is_empty()
            || !self.report_wasm_debug.is_empty()
    }
}

fn parse_label(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(file, label)| (file.to_owned(), label.to_owned()))
        .ok_or_else(|| "expected FILE=LABEL".to_owned())
}

/// Source positions of the stream and the debug info of its wasm files, for the report.
fn report_debug_info(
    args: &Args,
    main_js_bytes: &[u8],
    base64_runs: Option<&base64::Base64Runs>,
) -> Result<(Option<SourceMapping>, Vec<WasmDebug>)> {
    let mut parts: Vec<(usize, MapFile, FileMapping)> = Vec::new();
    if let Some(path) = &args.report_source_map {
        let mut mapping = FileMapping::read(path, main_js_bytes)?;
        if let Some(runs) = base64_runs {
            mapping.positions = runs.arrange(&mapping.positions);
        }
        parts.push((
            0,
            MapFile {
                section: 0,
                file: path.display().to_string(),
                kind: "source map".to_owned(),
            },
            mapping,
        ));
    }
    let mut wasm_debug = Vec::new();
    // The decoded base64 runs are a section after the main JavaScript
    let mut start = main_js_bytes.len();
    let mut first_file_section = 1;
    if let Some(runs) = base64_runs {
        start = runs.js.len() + runs.decoded.iter().map(Vec::len).sum::<usize>();
        first_file_section = 2;
    }
    for (i, file) in args.files.iter().enumerate() {
        let section = i + first_file_section;
        let bytes =
            std::fs::read(file).with_context(|| format!("Failed to read additional file: {file}"))?;
        let given: Vec<PathBuf> = args
            .report_wasm_debug
            .iter()
            .filter(|(f, _)| f == file)
            .map(|(_, debug)| PathBuf::from(debug))
            .collect();
        if wasm::is_wasm(&bytes) {
            let strict = !given.is_empty();
            let debug_files = if strict {
                given
            } else {
                wasm::discover_debug_files(Path::new(file))
            };
            match wasm::debug_info(section, &bytes, &debug_files, strict) {
                Ok((debug, mappings)) => {
                    for (map_file, kind, mapping) in mappings {
                        parts.push((
                            start,
                            MapFile {
                                section,
                                file: map_file,
                                kind,
                            },
                            mapping,
                        ));
                    }
                    if !debug.files.is_empty() || !debug.notes.is_empty() {
                        for note in &debug.notes {
                            println!("Report: {file}: {note}");
                        }
                        wasm_debug.push(debug);
                    }
                }
                Err(err) if strict => {
                    return Err(err.context(format!("--report-wasm-debug for {file}")))
                }
                // Not a module that wasmparser reads: the report shows it as a binary file.
                Err(_) => {}
            }
        } else if !given.is_empty() {
            bail!("--report-wasm-debug: '{file}' is not a WebAssembly file");
        }
        start += bytes.len();
    }
    if parts.is_empty() {
        return Ok((None, wasm_debug));
    }
    let mut mapping = SourceMapping::new(start);
    for (offset, map, part) in parts {
        mapping.add(offset, map, part);
    }
    Ok((Some(mapping), wasm_debug))
}

/// The stream of a demo as `run` packs it: the main JavaScript with its base64 runs taken out,
/// the decoded runs and the files. Its layout has what in the files may get their own models: the
/// WebAssembly modules, and the ranges that may hold fixed-size records, of which
/// `choose_layout` chooses.
pub fn read_demo(js_main: &str, files: &[String]) -> Result<Demo> {
    let main_js_bytes =
        std::fs::read(js_main).with_context(|| format!("Failed to read JS main file: {js_main}"))?;
    let base64_runs = base64::extract(&main_js_bytes, base64::MIN_RUN);
    let mut stream = base64_runs
        .as_ref()
        .map_or(main_js_bytes.clone(), |runs| runs.js.clone());
    let js_main_len = stream.len();
    if let Some(runs) = &base64_runs {
        stream.extend(runs.decoded.concat());
    }
    let files_start = stream.len();
    let mut wasm_ranges = Vec::new();
    let mut layout_files = Vec::new();
    for file in files {
        let bytes =
            std::fs::read(file).with_context(|| format!("Failed to read additional file: {file}"))?;
        let start = stream.len() as u32;
        if wasm::is_wasm(&bytes) {
            wasm_ranges.push((start, start + bytes.len() as u32));
        }
        let name = Path::new(file)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(file)
            .to_owned();
        layout_files.push((name, start, bytes.len() as u32));
        stream.extend(bytes);
    }
    Ok(Demo {
        name: std::iter::once(js_main).chain(files.iter().map(String::as_str)).collect::<Vec<_>>().join(","),
        layout: WebLayout {
            stream: StreamLayout {
                wasm: wasm_ranges,
                records: detect_records(&stream, files_start),
            },
            base64: base64_runs.map(|runs| output_generator::Base64Runs {
                marker: runs.marker,
                lengths: runs.decoded.iter().map(Vec::len).collect(),
            }),
            js_main_len,
            decoded_len: stream.len(),
            // A guess, made better by `estimate_encoded_len`
            encoded_len: stream.len() / 6,
            files: layout_files,
        },
        stream,
    })
}

/// The layout whose models `demo` is packed with: the layout `read_demo` detected, with the
/// record ranges that `choose_record_ranges` chooses. Its WebAssembly modules get the models of
/// the config, if it has any.
pub fn choose_layout(config: &CompressConfig, demo: &Demo) -> Result<StreamLayout> {
    let detected = &demo.layout.stream;
    let mut layout = StreamLayout {
        wasm: detected.wasm.clone(),
        records: Vec::new(),
    };
    // Without record candidates there is nothing to choose, so no trial compression
    if detected.records.is_empty() || config.static_model_params.records.models.is_empty() {
        return Ok(layout);
    }
    layout.records = detected.records.clone();
    layout.records = choose_record_ranges(config, demo, &layout)?;
    Ok(layout)
}

/// The record ranges, out of the `layout.records` candidates, that the record models of the config
/// get: the ranges where they save bits in a trial compression, or none if they don't make the
/// web output smaller.
fn choose_record_ranges(
    config: &CompressConfig,
    demo: &Demo,
    layout: &StreamLayout,
) -> Result<Vec<[u32; 3]>> {
    let size = |layout: &StreamLayout| -> Result<(Vec<f64>, f64)> {
        let costs = records::byte_costs(config.create_model_for(layout)?, &demo.stream);
        let overhead = output_generator::web_overhead(
            &config.with_stream_models(layout),
            &config.static_model_params,
            &WebLayout {
                stream: layout.clone(),
                ..demo.layout.clone()
            },
        )?;
        let size = costs.iter().sum::<f64>() / 8. + overhead as f64;
        Ok((costs, size))
    };
    let without_layout = StreamLayout {
        records: Vec::new(),
        ..layout.clone()
    };
    let (without, without_size) = size(&without_layout)?;
    println!("  no record ranges: {without_size:.0} bytes");
    let mut best = (Vec::new(), without_size);
    let mut candidates = layout.records.clone();
    for _ in 0..3 {
        let (with, with_size) = size(&StreamLayout {
            records: candidates.clone(),
            ..layout.clone()
        })?;
        println!("  {} record ranges: {with_size:.0} bytes", candidates.len());
        if with_size < best.1 {
            best = (candidates.clone(), with_size);
        }
        let selected = records::select_records(&candidates, &without, &with);
        if selected == candidates || selected.is_empty() {
            break;
        }
        candidates = selected;
    }
    Ok(best.0)
}

/// The config that `search` finds for the input of `args`, or the one kept in --search-cache.
fn search_config(args: &Args, start: CompressConfig, start_json: &str) -> Result<CompressConfig> {
    let mut demo = read_demo(&args.js_main, &args.files)?;
    demo.layout.stream = choose_layout(&start, &demo)?;
    // FNV-1a over the parts and their lengths, which unlike std's hashers is the same in every
    // build of rootsqz
    let mut hash: u64 = 0xcbf29ce484222325;
    let file_lens: Vec<u8> =
        demo.layout.files.iter().flat_map(|(_, _, len)| len.to_le_bytes()).collect();
    let parts: [&[u8]; 4] = [
        env!("CARGO_PKG_VERSION").as_bytes(),
        start_json.as_bytes(),
        &file_lens,
        &demo.stream,
    ];
    for part in parts {
        for &byte in (part.len() as u64).to_le_bytes().iter().chain(part) {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
    }
    let cache_file = args
        .search_cache
        .as_ref()
        .map(|dir| dir.join(format!("{hash:016x}.json")));
    if let Some(file) = cache_file.as_ref().filter(|file| file.exists()) {
        println!("Using the config found before: {}", file.display());
        return serde_json::from_str(&std::fs::read_to_string(file)?)
            .with_context(|| format!("Failed to parse {}", file.display()));
    }
    // The best config so far and the state of the search, kept while it runs, so that a stopped
    // search resumes where it was
    let output_directory = Path::new(&args.output_directory);
    let progress_dir = args.search_cache.as_deref().unwrap_or(output_directory);
    std::fs::create_dir_all(progress_dir)?;
    std::fs::create_dir_all(output_directory)?;
    let progress_file = progress_dir.join(format!("{hash:016x}.progress.json"));
    let state_file = progress_dir.join(format!("{hash:016x}.state"));
    let mut start = start;
    if progress_file.exists() && state_file.exists() {
        println!("Resuming the search that stopped before: {}", progress_file.display());
        start = serde_json::from_str(&std::fs::read_to_string(&progress_file)?)
            .with_context(|| format!("Failed to parse {}", progress_file.display()))?;
    }
    let threads = args
        .search_threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));
    println!("Searching for the best config ({threads} threads)...");
    demo.layout.encoded_len = stream_size(&start, &demo)?;
    let started = std::time::Instant::now();
    let options = SearchOptions {
        threads,
        batch: 12,
        max_evals: usize::MAX,
        max_memory: MAX_DECODER_MEMORY,
        state: Some(state_file.clone()),
    };
    let (config, _) = search(start, std::slice::from_ref(&demo), &options, |line, config| {
        println!("[{:>6.0}s] {line}", started.elapsed().as_secs_f64());
        // The best config so far, to build with if the search is stopped
        if let Ok(json) = serde_json::to_string_pretty(config) {
            let _ = std::fs::write(&progress_file, json.clone() + "\n");
            let _ = std::fs::write(output_directory.join("search_config.json"), json + "\n");
        }
    })?;
    if let Some(file) = cache_file {
        std::fs::write(&file, serde_json::to_string_pretty(&config)? + "\n")?;
    }
    let _ = std::fs::remove_file(&progress_file);
    let _ = std::fs::remove_file(&state_file);
    Ok(config)
}

pub fn run(args: Args) -> Result<()> {
    let loader = args.loader()?;
    let decoder = args.decoder(loader.as_ref())?;
    let boot_js = args.boot_js()?;
    let main_js_bytes = std::fs::read(&args.js_main)
        .with_context(|| format!("Failed to read JS main file: {}", args.js_main))?;
    if args.brotli {
        let files: Result<Vec<_>> = args
            .files
            .iter()
            .chain(args.pre_compressed_files.iter())
            .map(|path| {
                Ok(output_generator::FileWithContent {
                    path: PathBuf::from(path),
                    content: std::fs::read(path)
                        .with_context(|| format!("Failed to read additional file: {path}"))?,
                })
            })
            .collect();
        let files = files?;
        let source_len =
            main_js_bytes.len() + files.iter().map(|file| file.content.len()).sum::<usize>();
        if args.target != output_generator::Target::Web {
            bail!("--brotli requires the web target");
        }
        let profile_64k = matches!(args.size_profile, Some(profile) if profile != SizeProfile::FourK);
        if args.config.is_some() || profile_64k || args.wants_report() {
            bail!("--brotli cannot be combined with --config, a 64k --size-profile, or --report");
        }
        if args.wasm_decoder {
            bail!("--wasm-decoder cannot be combined with --brotli, which needs no decoder");
        }
        println!("Packing {} source bytes with Brotli", source_len);
        return output_generator::render_brotli_output(
            Path::new(&args.output_directory),
            &main_js_bytes,
            &files,
            boot_js.as_deref(),
        );
    }

    if args.report_detail.contains(&View::Summary) {
        bail!("--report-detail summary: the summary is report.json, which --report always writes");
    }

    let config_json = match &args.config {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?,
        None => args.size_profile.unwrap_or(SizeProfile::SixtyFourK).config_json().to_owned(),
    };
    let mut model_config: CompressConfig =
        serde_json::from_str(&config_json).with_context(|| {
            format!(
                "Failed to parse config: {}",
                args.config
                    .as_deref()
                    .unwrap_or(Path::new("(embedded)"))
                    .display()
            )
        })?;
    let mut config_json = config_json;
    if args.search {
        if args.target != output_generator::Target::Web && args.search_cache.is_none() {
            println!("Note: the search finds the same config for a web and a node build, so --search-cache can reuse it");
        }
        model_config = search_config(&args, model_config, &config_json)?;
        config_json = serde_json::to_string_pretty(&model_config)? + "\n";
        std::fs::create_dir_all(&args.output_directory)?;
        std::fs::write(
            Path::new(&args.output_directory).join("search_config.json"),
            &config_json,
        )?;
    }

    // Check the report options first, so that they fail before compressing.
    for (file, _) in &args.report_label {
        if *file != args.js_main
            && !args.files.contains(file)
            && !args.pre_compressed_files.contains(file)
        {
            bail!("--report-label: '{file}' is not given with --js-main, --file or --pre-compressed-file");
        }
    }
    for (file, _) in &args.report_wasm_debug {
        if !args.files.contains(file) {
            bail!("--report-wasm-debug: '{file}' is not given with --file");
        }
    }
    let base64_runs = base64::extract(&main_js_bytes, base64::MIN_RUN);
    let (source_mapping, wasm_debug) = if args.wants_report() {
        report_debug_info(&args, &main_js_bytes, base64_runs.as_ref())?
    } else {
        (None, Vec::new())
    };

    println!(
        "Starting compression (rootsqz v{})",
        env!("CARGO_PKG_VERSION")
    );
    let stream_js = base64_runs.as_ref().map_or(&main_js_bytes, |runs| &runs.js);

    // The WebAssembly modules and the records among the files get their own models, if they pay
    // for their decoder
    let demo = read_demo(&args.js_main, &args.files)?;
    let layout = choose_layout(&model_config, &demo)?;
    drop(demo);
    let packed_model = model_config.with_stream_models(&layout);

    println!("Initializing hash table...");
    let model = model_config
        .create_model_for(&layout)
        .context("Failed to create model from config")?;

    let mut encoded_data: Vec<u8> = Vec::new();
    let mut encoder = Encoder::new(model, &mut encoded_data)?;
    if args.wants_report() {
        encoder.record(args.report_models);
    }

    println!("Compressing input data ({} bytes)", stream_js.len());
    encoder.encode_section(stream_js.as_slice())?;

    let mut sections = vec![Section::new(&args.js_main, "main", 0, stream_js)];
    let mut offset = stream_js.len() as u32;
    if let Some(runs) = &base64_runs {
        let decoded = runs.decoded.concat();
        println!(
            "Compressing {} base64 runs of the main JavaScript, decoded ({} bytes)",
            runs.decoded.len(),
            decoded.len()
        );
        encoder.encode_section(decoded.as_slice())?;
        sections.push(Section::new(
            &format!("{} (base64)", args.js_main),
            "base64",
            offset as usize,
            &decoded,
        ));
        offset += decoded.len() as u32;
    }
    let mut bundled_files = Vec::new();
    for file in &args.files {
        let mut byte_stream =
            File::open(file).context(format!("Failed to open additional file: {}", file))?;
        let file_len = byte_stream.metadata()?.len() as u32;
        println!("Compressing additional file ({} bytes): {}", file_len, file);
        encoder.encode_section(&mut byte_stream)?;

        sections.push(Section {
            path: file.clone(),
            label: None,
            kind: "file".to_owned(),
            start: offset as usize,
            length: file_len as usize,
            text: false,
        });
        bundled_files.push(BundledFile {
            path: PathBuf::from(file),
            start_offset: offset,
            length: file_len,
        });

        offset += file_len;
    }

    let stream_js_len = stream_js.len();
    let recording = encoder.take_recording();
    let size_before_compression = encoder.finish().context("Failed to finish compressing")?;
    println!(
        "Finished compressing input data ({} bytes)",
        encoded_data.len()
    );

    let pre_compressed_files: Result<Vec<output_generator::FileWithContent>> = args
        .pre_compressed_files
        .iter()
        .map(|path| {
            let content = std::fs::read(path)
                .context(format!("Failed to read pre-compressed file: {}", path))?;
            Ok(output_generator::FileWithContent {
                path: PathBuf::from(path),
                content,
            })
        })
        .collect();

    println!("Rendering output...");

    let mut outputs = render_output(
        OutputGenerationOptions {
            output_dir: Path::new(&args.output_directory).to_owned(),
            target: args.target,
            model_config: packed_model.clone(),
            static_model_params: model_config.static_model_params.clone(),
            layout,
            loader,
            decoder,
            boot_js,
            base64: base64_runs.map(|runs| output_generator::Base64Runs {
                marker: runs.marker,
                lengths: runs.decoded.iter().map(Vec::len).collect(),
            }),
            wasm: args.wasm_decoder.then(WasmOptions::default),
        },
        size_before_compression,
        encoded_data,
        stream_js_len,
        bundled_files,
        pre_compressed_files?,
    )
    .context("Failed to render output")?;

    if let Some(recording) = recording {
        println!("Writing compression report...");
        for section in &mut sections {
            section.text = is_text(&recording.input[section.start..section.end()]);
        }
        for (file, label) in &args.report_label {
            if let Some(section) = sections.iter_mut().find(|s| s.path == *file) {
                section.label = Some(label.clone());
            }
            let part_name = format!("pre-compressed file {file}");
            for part in outputs.iter_mut().flat_map(|f| &mut f.parts) {
                if part.name == part_name {
                    part.name = format!("{part_name} ({label})");
                }
            }
        }
        let report_dir = Path::new(&args.output_directory).join(REPORT_DIR);
        ReportData {
            model_names: if args.report_models {
                packed_model.submodel_names()
            } else {
                Vec::new()
            },
            config_json: serde_json::from_str(&config_json)?,
            config: model_config,
            sections,
            outputs,
            recording,
            source_mapping,
            wasm_debug,
        }
        .write(
            &report_dir,
            &ReportOptions {
                detail: args.report_detail.clone(),
            },
        )
        .context("Failed to write the compression report")?;
        println!(
            "Report written to '{}': report.json (summary), report.html, AGENTS.md (a guide for coding agents); query it with 'rootsqz report {}'",
            report_dir.display(),
            report_dir.display()
        );
    }

    println!(
        "Output rendered successfully to '{}'",
        args.output_directory
    );

    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct SearchArgs {
    /// Files of one demo, separated by commas, in the order they are packed: the main JavaScript
    /// first, then the files given with --file. Can be given multiple times: the search then
    /// minimises the total of all demos, each packed on its own
    #[arg(short, long = "input", value_name = "FILES", required = true)]
    pub inputs: Vec<String>,

    /// Config JSON to start from (the embedded preset of --size-profile when omitted)
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Embedded model preset to start from, used when --config is omitted
    #[arg(long, value_enum, default_value_t = SizeProfile::SixtyFourK)]
    pub size_profile: SizeProfile,

    /// Config JSON written with the best config found, after every improvement
    #[arg(short, long)]
    pub output: PathBuf,

    /// Number of configs evaluated at the same time (default: the number of CPUs)
    #[arg(long)]
    pub threads: Option<usize>,

    /// Number of changed configs evaluated before the best of them is taken
    #[arg(long, default_value_t = 12)]
    pub batch: usize,

    /// Stop after this many evaluations
    #[arg(long, default_value_t = usize::MAX)]
    pub max_evals: usize,

    /// Skip configs whose decoder would take more memory than this, in MB, by an estimate
    #[arg(long, value_name = "MB", default_value_t = MAX_DECODER_MEMORY / 1_000_000)]
    pub max_memory_mb: usize,

    /// File where the search keeps the moves it tried and the sizes it packed, after every batch
    /// of moves. To resume a stopped search, start it again with the same --state and with
    /// --config set to its --output
    #[arg(long, value_name = "FILE")]
    pub state: Option<PathBuf>,

    /// Only print the size of the web output of each input with the start config
    #[arg(long)]
    pub eval: bool,
}

fn read_config(config: &Option<PathBuf>, size_profile: SizeProfile) -> Result<CompressConfig> {
    let json = match config {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?,
        None => size_profile.config_json().to_owned(),
    };
    serde_json::from_str(&json).context("Failed to parse config")
}

pub fn run_search(args: SearchArgs) -> Result<()> {
    let start = read_config(&args.config, args.size_profile)?;
    let mut demos = Vec::new();
    for files in &args.inputs {
        let files: Vec<String> = files.split(',').map(str::to_owned).collect();
        let mut demo = read_demo(&files[0], &files[1..])?;
        demo.layout.stream = choose_layout(&start, &demo)?;
        demo.layout.encoded_len = stream_size(&start, &demo)?;
        demos.push(demo);
    }
    if args.eval {
        let mut total = 0;
        for demo in &demos {
            let (size, memory) = demo_eval(&start, demo)?;
            println!("{size}\t{}\t(decoder memory about {} MB)", demo.name, memory / 1_000_000);
            total += size;
        }
        println!("{total}\ttotal");
        return Ok(());
    }
    let options = SearchOptions {
        threads: args
            .threads
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get())),
        batch: args.batch,
        max_evals: args.max_evals,
        max_memory: args.max_memory_mb * 1_000_000,
        state: args.state.clone(),
    };
    let started = std::time::Instant::now();
    let output = args.output.clone();
    let (best, best_cost) = search(start, &demos, &options, |line, config| {
        println!("[{:>6.0}s] {line}", started.elapsed().as_secs_f64());
        if let Ok(json) = serde_json::to_string_pretty(config) {
            let _ = std::fs::write(&output, json + "\n");
        }
    })?;
    std::fs::write(&output, serde_json::to_string_pretty(&best)? + "\n")?;
    let total = cost(&best, &demos)?;
    println!("Best cost {best_cost} ({total}), config written to {}", output.display());
    Ok(())
}
