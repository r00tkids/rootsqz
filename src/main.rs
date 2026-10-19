use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use human_panic::{setup_panic, Metadata};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod compressor;
mod report;
mod web;

#[derive(Parser, Debug)]
#[command(author, version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[command(flatten)]
    default_args: Option<web::Args>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Pack and compress JS/web assets into a self-contained output
    Web(web::Args),
    /// Print a view of a compression report written with --report
    Report(report::query::Args),
}

fn main() -> Result<()> {
    setup_panic!(
        Metadata::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            .support("https://github.com/r00tkids/rootsqz/issues")
    );

    tracing_subscriber::registry()
        .with(fmt::layer().event_format(fmt::format().compact()))
        .with(EnvFilter::from_default_env())
        .try_init()?;

    let args = Cli::parse();

    let command = match (args.command, args.default_args) {
        (Some(command), _) => command,
        (None, Some(default_args)) => Commands::Web(default_args),
        (None, None) => Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "the following required arguments were not provided: --js-main, --output-directory",
            )
            .exit(),
    };
    match command {
        Commands::Web(web_args) => web::run(web_args),
        Commands::Report(report_args) => report::query::run(report_args),
    }
}

#[cfg(test)]
mod node_tests {
    use std::path::PathBuf;
    use std::process::Command;
    use std::{fs::File, io::Read, path::Path};

    use crate::compressor::compress_config::ModelConfig;
    use crate::compressor::model_finder::create_default_compress_config;
    use crate::compressor::Encoder;
    use crate::web::output_generator::{
        self, generate_js_decompression_code, render_output, FileWithContent, Loader, ModelRef,
        OutputGenerationOptions,
    };

    #[test]
    pub fn round_trip() {
        round_trip_with_config(create_default_compress_config(), "testout/round_trip");
    }

    #[test]
    pub fn char_class_round_trip() {
        let config = serde_json::from_str(include_str!("compressor/char_class_config.json"))
            .expect("Failed to parse character class config");
        round_trip_with_config(config, "testout/char_class_round_trip");
    }

    #[test]
    pub fn optional_models_round_trip() {
        let optional = [
            ModelConfig::TokenPosition,
            ModelConfig::IndirectByte {
                context_bytes: 2,
                max_count: 15,
            },
        ];
        for (index, model) in optional.into_iter().enumerate() {
            let mut config = create_default_compress_config();
            if let ModelConfig::Mixer { models } = &mut config.model {
                models.push(model);
            }
            round_trip_with_config(config, &format!("testout/optional_model_{index}"));
        }
    }

    #[test]
    pub fn mixed_optional_models_round_trip() {
        let mut config = create_default_compress_config();
        if let ModelConfig::Mixer { models } = &mut config.model {
            models.extend([
                ModelConfig::CharClass {
                    order: 3,
                    max_count: None,
                },
                ModelConfig::TokenPosition,
                ModelConfig::MatchPredictor {
                    context_bytes: 2,
                    confidence: 0.97,
                    table_bits: 16,
                },
                ModelConfig::IndirectByte {
                    context_bytes: 2,
                    max_count: 15,
                },
            ]);
        }
        round_trip_with_config(config, "testout/mixed_optional_models");
    }

    fn round_trip_with_config(
        model_config: crate::compressor::compress_config::CompressConfig,
        output_dir: &str,
    ) {
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
                output_dir: Path::new(output_dir).to_owned(),
                target: output_generator::Target::Node,
                model_config: model_config.model.clone(),
                static_model_params: model_config.static_model_params.clone(),
                loader: None,
            },
            input_bytes.len(),
            encoded_data,
            input_bytes.len(),
            vec![],
            vec![],
        )
        .expect("Failed to render output");

        assert!(Command::new("node")
            .arg(Path::new(output_dir).join("index.mjs"))
            .status()
            .expect("Failed to run node decompressor")
            .success());

        let output_file = File::open(Path::new(output_dir).join("output.bin"))
            .expect("Failed to open output.bin");
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
    pub fn match_predictor_round_trip() {
        let mut config = create_default_compress_config();
        if let ModelConfig::Mixer { models } = &mut config.model {
            models.push(ModelConfig::MatchPredictor {
                context_bytes: 2,
                confidence: 0.97,
                table_bits: 14,
            });
        } else {
            panic!("default model must be a mixer");
        }

        let input = b"a repeated phrase with shared context; a repeated phrase with shared context; a repeated phrase with shared context;";
        let mut encoded = Vec::new();
        let mut encoder = Encoder::new(config.create_model().unwrap(), &mut encoded).unwrap();
        encoder.encode_section(&input[..]).unwrap();
        encoder.finish().unwrap();

        let output_dir = Path::new("testout/match_predictor");
        render_output(
            OutputGenerationOptions {
                output_dir: output_dir.to_owned(),
                target: output_generator::Target::Node,
                model_config: config.model,
                static_model_params: config.static_model_params,
                loader: None,
            },
            input.len(),
            encoded,
            input.len(),
            vec![],
            vec![],
        )
        .unwrap();
        assert!(Command::new("node")
            .arg(output_dir.join("index.mjs"))
            .status()
            .unwrap()
            .success());
        assert_eq!(std::fs::read(output_dir.join("output.bin")).unwrap(), input);
    }

    #[test]
    pub fn web() {
        let model_config = create_default_compress_config();

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
                loader: None,
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
    pub fn web_with_loader() {
        let model_config = create_default_compress_config();
        let input = b"document.body.append('loaded');";
        let mut encoded_data = Vec::new();
        let mut encoder =
            Encoder::new(model_config.create_model().unwrap(), &mut encoded_data).unwrap();
        encoder.encode_section(&input[..]).unwrap();
        encoder.finish().unwrap();

        render_output(
            OutputGenerationOptions {
                output_dir: Path::new("testout/web_with_loader").to_owned(),
                target: output_generator::Target::Web,
                model_config: model_config.model,
                static_model_params: model_config.static_model_params,
                loader: Some(Loader {
                    html: Some(include_str!("web/templates/web/loaders/bar.html").to_owned()),
                    js: Some(include_str!("web/templates/web/loaders/bar.js").to_owned()),
                    keep: true,
                }),
            },
            input.len(),
            encoded_data,
            input.len(),
            vec![],
            vec![],
        )
        .expect("Failed to render output");
    }

    /// Decodes with the sliced decoder of the loaders in node, and checks the output and the progress calls
    #[test]
    pub fn sliced_round_trip() {
        let model_config = create_default_compress_config();
        let input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        let mut encoded_data = Vec::new();
        let mut encoder =
            Encoder::new(model_config.create_model().unwrap(), &mut encoded_data).unwrap();
        encoder.encode_section(input.as_slice()).unwrap();
        encoder.finish().unwrap();

        let output_dir = Path::new("testout/sliced_round_trip");
        std::fs::create_dir_all(output_dir).unwrap();
        std::fs::write(output_dir.join("input.pack"), &encoded_data).unwrap();
        let mut features_used = ModelRef::None;
        let script = format!(
            r#"import fs from "node:fs";
let progress = [];
globalThis.rsqz = {{ progress: (done, total) => progress.push([done, total]) }};
{}
{}
decompressSliced(model, new Uint8Array(fs.readFileSync(import.meta.dirname + "/input.pack")), {}, {}, output => {{
    fs.writeFileSync(import.meta.dirname + "/output.bin", output);
    fs.writeFileSync(import.meta.dirname + "/progress.json", JSON.stringify(progress));
    process.exit(0);
}});
"#,
            generate_js_decompression_code(
                &model_config.model,
                &model_config.static_model_params,
                &mut features_used
            ),
            include_str!("web/js_source/coder_sliced.js"),
            encoded_data.len(),
            input.len()
        );
        std::fs::write(output_dir.join("index.mjs"), script).unwrap();

        assert!(Command::new("node")
            .arg(output_dir.join("index.mjs"))
            .status()
            .unwrap()
            .success());
        assert_eq!(std::fs::read(output_dir.join("output.bin")).unwrap(), input);

        let progress: Vec<(usize, usize)> =
            serde_json::from_slice(&std::fs::read(output_dir.join("progress.json")).unwrap())
                .unwrap();
        assert!(
            progress.len() > 1,
            "decoding should take more than one slice"
        );
        assert!(progress.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(progress.iter().all(|&(_, total)| total == input.len()));
        assert_eq!(progress.last().unwrap().0, input.len());
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
                loader: None,
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
