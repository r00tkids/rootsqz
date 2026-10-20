use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

pub mod output_generator;

use crate::{
    compressor::{
        compress_config::CompressConfig,
        model_finder::{DEFAULT_CONFIG_JSON, SIZE_64K_CONFIG_JSON},
        Encoder,
    },
    report::{
        is_text,
        source_map::{FileMapping, MapFile, SourceMapping},
        wasm::{self, WasmDebug},
        ReportData, ReportOptions, Section, View, REPORT_DIR,
    },
};
use output_generator::{render_output, BundledFile, Loader, OutputGenerationOptions};

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

    /// Embedded model preset, used when --config is omitted
    #[arg(long, value_enum, default_value_t = SizeProfile::FourK)]
    pub size_profile: SizeProfile,

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
    /// which is called with the number of decoded bytes while decoding runs in slices
    #[arg(long, value_name = "FILE")]
    pub loader_js: Option<PathBuf>,

    /// Keep the loader when the main JavaScript runs, instead of clearing the page.
    /// `rsqz.progress` stays, so the demo can continue the progress display
    #[arg(long)]
    pub loader_keep: bool,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderPreset {
    /// Spinning ring, with the decoder running in one go
    Spinner,
    /// Progress bar, with the decoder running in slices
    Bar,
    /// The browser's own progress bar (a <progress> element), with the decoder running in slices
    Progress,
}

impl Args {
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
) -> Result<(Option<SourceMapping>, Vec<WasmDebug>)> {
    let mut parts: Vec<(usize, MapFile, FileMapping)> = Vec::new();
    if let Some(path) = &args.report_source_map {
        parts.push((
            0,
            MapFile {
                section: 0,
                file: path.display().to_string(),
                kind: "source map".to_owned(),
            },
            FileMapping::read(path, main_js_bytes)?,
        ));
    }
    let mut wasm_debug = Vec::new();
    let mut start = main_js_bytes.len();
    for (i, file) in args.files.iter().enumerate() {
        let section = i + 1;
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

pub fn run(args: Args) -> Result<()> {
    let loader = args.loader()?;
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
        if args.config.is_some() || args.size_profile != SizeProfile::FourK || args.wants_report() {
            bail!("--brotli cannot be combined with --config, --size-profile 64k, or --report");
        }
        println!("Packing {} source bytes with Brotli", source_len);
        return output_generator::render_brotli_output(
            Path::new(&args.output_directory),
            &main_js_bytes,
            &files,
        );
    }

    if args.report_detail.contains(&View::Summary) {
        bail!("--report-detail summary: the summary is report.json, which --report always writes");
    }

    let config_json = match &args.config {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?,
        None => match args.size_profile {
            SizeProfile::FourK => DEFAULT_CONFIG_JSON,
            SizeProfile::SixtyFourK => SIZE_64K_CONFIG_JSON,
        }
        .to_owned(),
    };
    let model_config: CompressConfig = serde_json::from_str(&config_json).with_context(|| {
        format!(
            "Failed to parse config: {}",
            args.config
                .as_deref()
                .unwrap_or(Path::new("(embedded)"))
                .display()
        )
    })?;

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
    let (source_mapping, wasm_debug) = if args.wants_report() {
        report_debug_info(&args, &main_js_bytes)?
    } else {
        (None, Vec::new())
    };

    println!(
        "Starting compression (rootsqz v{})",
        env!("CARGO_PKG_VERSION")
    );
    println!("Initializing hash table...");
    let model = model_config
        .create_model()
        .context("Failed to create model from config")?;

    let mut encoded_data: Vec<u8> = Vec::new();
    let mut encoder = Encoder::new(model, &mut encoded_data)?;
    if args.wants_report() {
        encoder.record(args.report_models);
    }

    println!("Compressing input data ({} bytes)", main_js_bytes.len());
    encoder.encode_section(main_js_bytes.as_slice())?;

    let mut sections = vec![Section::new(&args.js_main, "main", 0, &main_js_bytes)];
    let mut bundled_files = Vec::new();
    let mut offset = main_js_bytes.len() as u32;
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
            model_config: model_config.model.clone(),
            static_model_params: model_config.static_model_params.clone(),
            loader,
        },
        size_before_compression,
        encoded_data,
        main_js_bytes.len(),
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
                model_config.model.submodel_names()
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
