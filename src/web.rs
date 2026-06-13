use std::{
    cell::RefCell,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result};

pub mod output_generator;

use crate::{
    compressor::{
        model::{HashTable, NOrderByteData},
        model_finder::create_default_compress_config,
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

    /// If set, reports detailed compression statistics to rootsqz-report.html
    #[arg(short, long)]
    pub report: bool,
}

pub fn run(args: Args) -> Result<()> {
    let model_config = create_default_compress_config();

    println!(
        "Starting compression (rootsqz v{})",
        env!("CARGO_PKG_VERSION")
    );
    println!("Initializing hash table...");
    let model = model_config
        .create_model()
        .context("Failed to create model from config")?;

    let mut main_js_bytes = Vec::new();
    File::open(&args.js_main)
        .context(format!("Failed to open JS main file: {}", args.js_main))?
        .read_to_end(&mut main_js_bytes)?;

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
