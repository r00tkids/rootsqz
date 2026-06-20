use anyhow::Result;
use clap::{Parser, Subcommand};
use human_panic::{setup_panic, Metadata};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod compressor;
mod report;
mod web;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[command(flatten)]
    default_args: web::Args,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Pack and compress JS/web assets into a self-contained output
    Web(web::Args),
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

    let command = args
        .command
        .unwrap_or_else(|| Commands::Web(args.default_args));
    match command {
        Commands::Web(web_args) => web::run(web_args),
    }
}

#[cfg(test)]
mod node_tests {
    use std::path::PathBuf;
    use std::process::Command;
    use std::{fs::File, io::Read, path::Path};

    use crate::compressor::model_finder::create_default_compress_config;
    use crate::compressor::{compress_config::CompressConfig, Encoder};
    use crate::web::output_generator::{
        self, render_output, FileWithContent, OutputGenerationOptions,
    };

    #[test]
    pub fn round_trip() {
        let model_config = serde_json::de::from_reader::<_, CompressConfig>(
            File::open("tests/compress.json").expect("Failed to open tests/compress.json"),
        )
        .expect("Failed to parse tests/compress.json");

        let model = model_config
            .create_model()
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
                model_config: model_config.model.clone(),
                static_model_params: model_config.static_model_params.clone(),
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

        let model = model_config
            .create_model()
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
                model_config: model_config.model.clone(),
                static_model_params: model_config.static_model_params.clone(),
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

        let model_config = create_default_compress_config();

        let model = model_config
            .create_model()
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
                model_config: model_config.model.clone(),
                static_model_params: model_config.static_model_params.clone(),
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
