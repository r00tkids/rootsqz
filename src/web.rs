use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

pub mod output_generator;

use crate::{
    compressor::{
        compress_config::CompressConfig,
        model_finder::{create_64k_compress_config, create_default_compress_config},
        Encoder,
    },
    report::ReportGenerator,
};
use output_generator::{render_output, BundledFile, OutputGenerationOptions};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Javascript file being evaluated after decompression
    #[arg(short, long)]
    pub js_main: String,

    /// Files to be included and packed into the output, with compression.
    /// Order matters, so files of similar content should be ordered together.
    #[arg(short, long, value_delimiter = ',')]
    pub files: Vec<String>,

    /// Files to be included and packed into the output, without compression
    #[arg(short, long, value_delimiter = ',')]
    pub pre_compressed_files: Vec<String>,

    /// Output directory
    #[arg(short, long)]
    pub output_directory: String,

    /// Target platform for the output
    #[arg(short, long, default_value = "web")]
    pub target: output_generator::Target,

    /// If set, reports detailed compression statistics to websqz-report.html
    #[arg(short, long)]
    pub report: bool,

    /// Compression model config JSON (uses the embedded default when omitted)
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Embedded model preset, used when --config is omitted
    #[arg(long, value_enum, default_value_t = SizeProfile::FourK)]
    pub size_profile: SizeProfile,

    /// Pack a web input as a Brotli stream (Firefox 147+ / Safari 18.4+)
    #[arg(long)]
    pub brotli: bool,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeProfile {
    #[value(name = "4k")]
    FourK,
    #[value(name = "64k")]
    SixtyFourK,
}

pub fn run(args: Args) -> Result<()> {
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
        if args.config.is_some() || args.size_profile != SizeProfile::FourK || args.report {
            bail!("--brotli cannot be combined with --config, --size-profile 64k, or --report");
        }
        println!("Packing {} source bytes with Brotli", source_len);
        return output_generator::render_brotli_output(
            Path::new(&args.output_directory),
            &main_js_bytes,
            &files,
        );
    }

    let model_config: CompressConfig = match &args.config {
        Some(path) => serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("Failed to read config: {}", path.display()))?,
        )
        .with_context(|| format!("Failed to parse config: {}", path.display()))?,
        None => match args.size_profile {
            SizeProfile::FourK => create_default_compress_config(),
            SizeProfile::SixtyFourK => create_64k_compress_config(),
        },
    };

    println!(
        "Starting compression (websqz v{})",
        env!("CARGO_PKG_VERSION")
    );
    println!("Initializing hash table...");
    let model = model_config
        .create_model()
        .context("Failed to create model from config")?;

    let mut encoded_data: Vec<u8> = Vec::new();
    let mut encoder = Encoder::new(model, &mut encoded_data)?;

    println!("Compressing input data ({} bytes)", main_js_bytes.len());
    encoder.encode_section(main_js_bytes.as_slice())?;

    let mut bundled_files = Vec::new();
    let mut offset = main_js_bytes.len() as u32;
    for file in &args.files {
        let mut byte_stream =
            File::open(file).context(format!("Failed to open additional file: {}", file))?;
        let file_len = byte_stream.metadata()?.len() as u32;
        println!("Compressing additional file ({} bytes): {}", file_len, file);
        encoder.encode_section(&mut byte_stream)?;

        bundled_files.push(BundledFile {
            path: PathBuf::from(file),
            start_offset: offset,
            length: file_len,
        });

        offset += file_len;
    }

    let size_before_compression = encoder.finish().context("Failed to finish compressing")?;
    println!(
        "Finished compressing input data ({} bytes)",
        encoded_data.len()
    );

    let pre_compressed_files: Result<Vec<output_generator::FileWithContent>> = args
        .pre_compressed_files
        .into_iter()
        .map(|path| {
            let content = std::fs::read(&path)
                .context(format!("Failed to read pre-compressed file: {}", path))?;
            Ok(output_generator::FileWithContent {
                path: PathBuf::from(&path),
                content,
            })
        })
        .collect();

    println!("Rendering output...");

    render_output(
        OutputGenerationOptions {
            output_dir: Path::new(&args.output_directory).to_owned(),
            target: args.target,
            model_config: model_config.model.clone(),
            static_model_params: model_config.static_model_params.clone(),
        },
        size_before_compression,
        encoded_data,
        main_js_bytes.len(),
        bundled_files,
        pre_compressed_files?,
    )
    .context("Failed to render output")?;

    if args.report {
        println!("Generating compression report...");
        let model = model_config
            .create_model()
            .context("Failed to create model from config")?;

        ReportGenerator::create(
            main_js_bytes.as_slice(),
            model,
            Path::new(&args.output_directory),
        )
        .context("Failed to generate compression report")?;

        println!(
            "Report generated at '{}/report.html'",
            args.output_directory
        );
    }

    println!(
        "Output rendered successfully to '{}'",
        args.output_directory
    );

    Ok(())
}
