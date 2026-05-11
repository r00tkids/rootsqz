use std::{
    cell::RefCell,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use compressor::Encoder;
use human_panic::{setup_panic, Metadata};
use model::{HashTable, NOrderByteData};
use output_generator::{render_output, OutputGenerationOptions};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::{
    model_finder::create_default_model_config, output_generator::BundledFile,
    report::ReportGenerator,
};

mod coder;
mod compress_config;
mod compressor;
mod model;
mod model_finder;
mod output_generator;
mod report;
mod utils;

/// Command-line arguments
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Javascript file being evaluated after decompression
    #[arg(short, long, required = true)]
    js_main: String,

    /// Files to be included and packed into the output, with compression.
    /// Order matters, so files of similar content should be ordered together.
    #[arg(short, long, value_delimiter = ',')]
    files: Vec<String>,

    /// Files to be included and packed into the output, without compression
    #[arg(short, long, value_delimiter = ',')]
    pre_compressed_files: Vec<String>,

    /// Output directory
    #[arg(short, long)]
    output_directory: String,

    /// Target platform for the output
    #[arg(short, long, default_value = "web")]
    target: output_generator::Target,

    /// If set, reports detailed compression statistics to websqz-report.html
    #[arg(short, long)]
    report: bool,
}

fn main() -> Result<()> {
    setup_panic!(
        Metadata::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            .support("https://github.com/r00tkids/websqz/issues")
    );

    tracing_subscriber::registry()
        .with(fmt::layer().event_format(fmt::format().compact()))
        .with(EnvFilter::from_default_env())
        .try_init()?;

    let args = Cli::parse();

    if args.js_main.is_empty() {
        bail!("No JS main file specified");
    }

    let model_config = create_default_model_config();

    println!(
        "Starting compression (websqz v{})",
        env!("CARGO_PKG_VERSION")
    );
    println!("Initializing hash table...");
    let model = model_config
        .create_model(Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(26))))
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
            return Ok(output_generator::FileWithContent {
                path: PathBuf::from(&path),
                content,
            });
        })
        .collect();

    println!("Rendering output...");

    render_output(
        OutputGenerationOptions {
            output_dir: Path::new(&args.output_directory).to_owned(),
            target: args.target,
            model_config: model_config.clone(),
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
            .create_model(Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(26))))
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

#[cfg(test)]
mod node_tests {
    use std::path::PathBuf;
    use std::process::Command;
    use std::{cell::RefCell, fs::File, io::Read, path::Path, rc::Rc};

    use crate::model_finder::create_default_model_config;
    use crate::output_generator::{FileWithContent, OutputGenerationOptions};
    use crate::{
        compress_config::CompressConfig,
        compressor::Encoder,
        model::{HashTable, NOrderByteData},
        output_generator::{self, render_output},
    };

    #[test]
    pub fn round_trip() {
        let model_config = serde_json::de::from_reader::<_, CompressConfig>(
            File::open("tests/compress.json").expect("Failed to open tests/compress.json"),
        )
        .expect("Failed to parse tests/compress.json");

        let hash_table = HashTable::<NOrderByteData>::new(26);
        let model = model_config
            .model
            .create_model(Rc::new(RefCell::new(hash_table)))
            .expect("Failed to create model from config");

        let mut input = String::new();
        File::open("tests/ray_tracer/index.js")
            .unwrap()
            .read_to_string(&mut input)
            .unwrap();

        let input_bytes = input.as_bytes();

        let mut encoded_data: Vec<u8> = Vec::new();
        let mut encoder = Encoder::new(model, &mut encoded_data).unwrap();
        encoder.encode_section(input_bytes).unwrap();
        encoder.finish().unwrap();

        render_output(
            OutputGenerationOptions {
                output_dir: Path::new("testout/round_trip").to_owned(),
                target: output_generator::Target::Node,
                model_config: model_config.model,
            },
            input_bytes.len(),
            encoded_data,
            input_bytes.len(),
            vec![],
            vec![],
        )
        .expect("Failed to render output");

        Command::new("node")
            .arg("testout/round_trip/index.mjs")
            .status()
            .expect("Failed to run node decompressor");

        let output_path = Path::new("testout/round_trip/output.bin");
        let output_file = File::open(output_path).expect("Failed to open output.bin");
        let mut output_data = Vec::new();
        output_file
            .take(usize::MAX as u64)
            .read_to_end(&mut output_data)
            .expect("Failed to read output.bin");

        assert_eq!(
            input_bytes,
            output_data.as_slice(),
            "Decompressed data does not match original input"
        );
    }

    #[test]
    pub fn web() {
        let model_config = serde_json::de::from_reader::<_, CompressConfig>(
            File::open("tests/compress.json").expect("Failed to open tests/compress.json"),
        )
        .expect("Failed to parse tests/compress.json");

        let hash_table = HashTable::<NOrderByteData>::new(26);
        let model = model_config
            .model
            .create_model(Rc::new(RefCell::new(hash_table)))
            .expect("Failed to create model from config");

        let mut input = String::new();
        File::open(
            "tests/ray_tracer/index.js", /*"tests/reore/reore_decompressed.bin"*/
        )
        .unwrap()
        .read_to_string(&mut input)
        .unwrap();

        let input_bytes = input.as_bytes();

        let mut encoded_data: Vec<u8> = Vec::new();
        let mut encoder = Encoder::new(model, &mut encoded_data).unwrap();
        encoder.encode_section(input_bytes).unwrap();
        encoder.finish().unwrap();

        render_output(
            OutputGenerationOptions {
                output_dir: Path::new("testout/web").to_owned(),
                target: output_generator::Target::Web,
                model_config: model_config.model,
            },
            input_bytes.len(),
            encoded_data,
            input_bytes.len(),
            vec![],
            vec![FileWithContent {
                path: PathBuf::from("Cargo.toml"),
                content: std::fs::read("Cargo.toml").expect("Failed to read Cargo.toml"),
            }],
        )
        .expect("Failed to render output");
    }

    #[test]
    pub fn round_trip_random_data() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let model_config = create_default_model_config();

        let hash_table = HashTable::<NOrderByteData>::new(26);
        let model = model_config
            .create_model(Rc::new(RefCell::new(hash_table)))
            .expect("Failed to create model from config");

        let mut rng = StdRng::seed_from_u64(1337);
        let mut input_bytes: Vec<u8> = vec![0u8; 1 * 1024 * 1024];
        rng.fill(&mut input_bytes[..]);

        let mut encoded_data: Vec<u8> = Vec::new();
        let mut encoder = Encoder::new(model, &mut encoded_data).unwrap();
        encoder.encode_section(&input_bytes[..]).unwrap();
        encoder.finish().unwrap();

        render_output(
            OutputGenerationOptions {
                output_dir: Path::new("testout/round_trip_rand").to_owned(),
                target: output_generator::Target::Node,
                model_config: model_config,
            },
            input_bytes.len(),
            encoded_data,
            input_bytes.len(),
            vec![],
            vec![],
        )
        .expect("Failed to render output");

        Command::new("node")
            .arg("testout/round_trip_rand/index.mjs")
            .status()
            .expect("Failed to run node decompressor");

        let output_path = Path::new("testout/round_trip_rand/output.bin");
        let output_file = File::open(output_path).expect("Failed to open output.bin");
        let mut output_data = Vec::new();
        output_file
            .take(usize::MAX as u64)
            .read_to_end(&mut output_data)
            .expect("Failed to read output.bin");

        assert_eq!(
            input_bytes,
            output_data.as_slice(),
            "Decompressed data does not match original input"
        );
    }
}
