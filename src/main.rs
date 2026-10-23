use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use human_panic::{setup_panic, Metadata};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod compressor;
mod report;
mod skill;
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
    /// Search for the model config that packs the given inputs smallest, counting the decoder
    /// bytes of each change
    Search(web::SearchArgs),
    /// Install the agent skill about the compression report into a project, for coding agents
    /// such as Claude Code (.claude/skills and .agents/skills)
    InstallSkill(skill::Args),
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
        Commands::Search(search_args) => web::run_search(search_args),
        Commands::InstallSkill(skill_args) => skill::run(skill_args),
    }
}

#[cfg(test)]
mod node_tests {
    use clap::Parser;
    use std::path::PathBuf;
    use std::process::Command;
    use std::{fs::File, io::Read, path::Path};

    use crate::compressor::compress_config::{ModelConfig, StreamLayout};
    use crate::compressor::records::detect_records;
    use crate::compressor::mixer::{ApmConfig, ApmContext, Selector};
    use crate::compressor::model_finder::{
        create_default_compress_config, SIZE_64K_CONFIG_JSON, SIZE_64K_WASM_CONFIG_JSON,
    };
    use crate::compressor::utils::prob_squash;
    use crate::compressor::Encoder;
    use crate::web::output_generator::{
        self, generate_js_decompression_code, render_output, Decoder, FileWithContent, Loader, ModelRef,
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
            ModelConfig::JsContext {
                variant: 0,
                max_count: None,
            },
            ModelConfig::JsContext {
                variant: 1,
                max_count: Some(4),
            },
            ModelConfig::JsContext {
                variant: 2,
                max_count: None,
            },
            ModelConfig::JsContext {
                variant: 3,
                max_count: None,
            },
            ModelConfig::IndirectByte {
                context_bytes: 2,
                max_count: 15,
            },
            ModelConfig::Match {
                min_len: 4,
                candidates: 8,
                expected_byte_context: false,
                second: false,
            },
            ModelConfig::Match {
                min_len: 1,
                candidates: 4,
                expected_byte_context: true,
                second: true,
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

    /// The decoder must make bit for bit the predictions of the encoder. A round trip only fails
    /// when a difference happens to move a split of the arithmetic coder, which can take hundreds
    /// of kilobytes, so this compares every prediction instead, for every model type.
    #[test]
    pub fn predictions_match_js_bit_for_bit() {
        let mut mixed = create_default_compress_config();
        if let ModelConfig::Mixer { models } = &mut mixed.model {
            models.extend([
                ModelConfig::CharClass {
                    order: 3,
                    max_count: None,
                },
                ModelConfig::TokenPosition,
                ModelConfig::IndirectByte {
                    context_bytes: 2,
                    max_count: 15,
                },
                ModelConfig::MatchPredictor {
                    context_bytes: 2,
                    confidence: 0.97,
                    table_bits: 16,
                },
                ModelConfig::Match {
                    min_len: 4,
                    candidates: 8,
                    expected_byte_context: false,
                    second: false,
                },
                ModelConfig::JsContext {
                    variant: 1,
                    max_count: Some(4),
                },
            ]);
        }
        mixed.static_model_params.count_offset = 0.13;
        // Every model type in a ContextMixer, with every kind of selector and APM, and bit histories
        let mut context_mixer = mixed.clone();
        context_mixer.static_model_params.context_input_scale = 1.3;
        if let ModelConfig::Mixer { models } = &mixed.model {
            let apm = |ctx, limit, weight| ApmConfig {
                ctx,
                bits: 10,
                limit,
                weight,
            };
            let matched = models
                .iter()
                .position(|m| matches!(m, ModelConfig::Match { .. }))
                .unwrap();
            let js = models
                .iter()
                .position(|m| matches!(m, ModelConfig::JsContext { .. }))
                .unwrap();
            context_mixer.model = ModelConfig::ContextMixer {
                models: models.clone(),
                selectors: vec![
                    Selector::None,
                    Selector::Order1,
                    Selector::Known,
                    Selector::Context(js),
                    Selector::State(matched),
                    // A model other than JsContext offers the context 0
                    Selector::Context(matched),
                ],
                learning_rate: 0.01,
                final_learning_rate: 0.004,
                init_weight: 0.1,
                lr_boost: 2.,
                apms: vec![
                    apm(ApmContext::Order0, 255, 0.3),
                    apm(ApmContext::Order1, 60, 0.2),
                    apm(ApmContext::Order2, 127, 0.1),
                    apm(ApmContext::State(matched), 127, 0.15),
                    apm(ApmContext::Context(js), 127, 0.1),
                ],
            };
        }
        let size_64k: crate::compressor::compress_config::CompressConfig =
            serde_json::from_str(SIZE_64K_CONFIG_JSON).unwrap();
        let size_64k_wasm: crate::compressor::compress_config::CompressConfig =
            serde_json::from_str(SIZE_64K_WASM_CONFIG_JSON).unwrap();
        let script_input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        // The script followed by a WebAssembly module, for the WasmContext models
        let mut wasm_input = script_input.clone();
        wasm_input.extend(std::fs::read("tests/wasm/fixture.wasm").unwrap());
        let wasm_layout = StreamLayout {
            wasm: vec![(script_input.len() as u32, wasm_input.len() as u32)],
            records: Vec::new(),
        };
        // Then binary records, for the Record models
        let mut records_input = wasm_input.clone();
        records_input.extend(test_records());
        let records_layout = StreamLayout {
            records: detect_records(&records_input, wasm_input.len()),
            ..wasm_layout.clone()
        };
        assert!(records_layout.records.iter().any(|range| range[2] == 8));

        for (name, config, input, layout) in [
            ("mixed", mixed, &script_input, StreamLayout::default()),
            ("64k", size_64k.clone(), &script_input, StreamLayout::default()),
            ("64k_wasm", size_64k_wasm, &wasm_input, wasm_layout.clone()),
            ("64k_records", size_64k, &records_input, records_layout.clone()),
            ("context_mixer_records", context_mixer, &records_input, records_layout),
        ] {
            // The WebAssembly decoder too
            crate::web::wasm_decoder::tests::check_predictions(name, &config, input, &layout, false);
            let mut model = config.create_model_for(&layout).unwrap();
            let mut expected = Vec::new();
            for byte in input {
                for i in (0..8).rev() {
                    let pred = model.pred();
                    expected.extend_from_slice(&pred.to_le_bytes());
                    expected.extend_from_slice(&prob_squash(pred).to_le_bytes());
                    model.learn((byte >> i) & 1);
                }
            }

            let output_dir = PathBuf::from(format!("testout/predictions_{name}"));
            std::fs::create_dir_all(&output_dir).unwrap();
            std::fs::write(output_dir.join("input.bin"), input).unwrap();
            let mut features_used = ModelRef::None;
            let script = generate_js_decompression_code(
                &config.with_stream_models(&layout),
                &config.static_model_params,
                &layout,
                &mut features_used,
            ) + r#"
import fs from 'fs';
let input = fs.readFileSync(process.argv[2]);
let preds = new Float64Array(input.length * 16), n = 0;
for (let byte of input) {
    for (let i = 7; i >= 0; i--) {
        let pred = model.p();
        preds[n++] = pred;
        preds[n++] = probSquash(pred);
        model.l((byte >> i) & 1);
    }
}
fs.writeFileSync(process.argv[3], new Uint8Array(preds.buffer));
"#;
            let script_path = output_dir.join("predict.mjs");
            let preds_path = output_dir.join("preds.bin");
            std::fs::write(&script_path, script).unwrap();
            assert!(Command::new("node")
                .arg(&script_path)
                .arg(output_dir.join("input.bin"))
                .arg(&preds_path)
                .status()
                .expect("Failed to run node")
                .success());

            let actual = std::fs::read(&preds_path).unwrap();
            assert_eq!(actual.len(), expected.len());
            if let Some(index) = (0..expected.len() / 8)
                .find(|i| expected[i * 8..i * 8 + 8] != actual[i * 8..i * 8 + 8])
            {
                let value =
                    |data: &[u8]| f64::from_le_bytes(data[index * 8..][..8].try_into().unwrap());
                panic!(
                    "{name}: the JS {} of bit {} is {:e}, the Rust one {:e}",
                    ["prediction", "squashed prediction"][index % 2],
                    index / 2,
                    value(&actual),
                    value(&expected),
                );
            }
        }
    }

    /// Binary data with records: 8-byte records of int16 ramps, and 14-byte records of a tag and
    /// three floats.
    fn test_records() -> Vec<u8> {
        let mut data = Vec::new();
        let mut seed = 12345u32;
        let mut noise = |range: u32| {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) % range
        };
        for i in 0..600u32 {
            let x = (i * 0x21 + noise(3)) as u16;
            for word in [1, x, (i * 3) as u16 ^ noise(4) as u16, x.wrapping_add(7)] {
                data.extend_from_slice(&word.to_le_bytes());
            }
        }
        for i in 0..300u32 {
            data.extend_from_slice(&[6, 3]);
            for axis in 0..3 {
                let value = (i as f32 * 0.25 + axis as f32) * if noise(8) == 0 { -1. } else { 1. };
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
        data
    }

    /// Packs a binary file of records through the command line, which uses the record models for it.
    #[test]
    pub fn records_round_trip() {
        let output_dir = Path::new("testout/records");
        std::fs::create_dir_all(output_dir).unwrap();
        let records_path = output_dir.join("records.bin");
        std::fs::write(&records_path, test_records()).unwrap();

        let cli = crate::Cli::try_parse_from([
            "rootsqz",
            "-j",
            "tests/ray_tracer/index.js",
            "-f",
            records_path.to_str().unwrap(),
            "--size-profile",
            "64k",
            "-t",
            "node",
            "-o",
            output_dir.to_str().unwrap(),
        ])
        .unwrap();
        crate::web::run(cli.default_args.unwrap()).unwrap();

        assert!(std::fs::read_to_string(output_dir.join("index.mjs"))
            .unwrap()
            .contains("RecordModel("));
        assert!(Command::new("node")
            .arg(output_dir.join("index.mjs"))
            .status()
            .expect("Failed to run node decompressor")
            .success());
        let mut input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        input.extend_from_slice(&test_records());
        assert!(std::fs::read(output_dir.join("output.bin")).unwrap() == input);
    }

    #[test]
    pub fn base64_runs_round_trip() {
        let font: Vec<u8> = (0..3000u32).map(|i| (i * i % 251) as u8).collect();
        let b64 = crate::web::base64::tests::encode_for_test(&font);
        let mut input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        input.extend_from_slice(format!("const f=\"data:font/otf;base64,{b64}\",g=`{b64}`;").as_bytes());
        let file = b"binary file after the runs".to_vec();
        let runs = crate::web::base64::extract(&input, crate::web::base64::MIN_RUN).unwrap();
        assert_eq!(runs.decoded.len(), 2);

        let model_config = create_default_compress_config();
        let mut encoded_data: Vec<u8> = Vec::new();
        let mut encoder =
            Encoder::new(model_config.create_model().unwrap(), &mut encoded_data).unwrap();
        encoder.encode_section(runs.js.as_slice()).unwrap();
        encoder.encode_section(runs.decoded.concat().as_slice()).unwrap();
        encoder.encode_section(file.as_slice()).unwrap();
        let size = encoder.finish().unwrap();

        let output_dir = "testout/base64_runs";
        render_output(
            OutputGenerationOptions {
                output_dir: Path::new(output_dir).to_owned(),
                target: output_generator::Target::Node,
                model_config: model_config.model.clone(),
                static_model_params: model_config.static_model_params.clone(),
                loader: None,
                decoder: Decoder::Serial,
                boot_js: None,
                layout: StreamLayout::default(),
                base64: Some(output_generator::Base64Runs {
                    marker: runs.marker,
                    lengths: runs.decoded.iter().map(Vec::len).collect(),
                }),
                wasm: None,
            },
            size,
            encoded_data,
            runs.js.len(),
            vec![],
            vec![],
        )
        .expect("Failed to render output");

        assert!(Command::new("node")
            .arg(Path::new(output_dir).join("index.mjs"))
            .status()
            .expect("Failed to run node decompressor")
            .success());
        input.extend_from_slice(&file);
        assert!(std::fs::read(Path::new(output_dir).join("output.bin")).unwrap() == input);
    }

    /// Packs a main JavaScript with a base64 run and a wasm file through the command line, where
    /// the module starts after the decoded base64 bytes.
    #[test]
    pub fn wasm_after_base64_round_trip() {
        let font: Vec<u8> = (0..3000u32).map(|i| (i * i % 251) as u8).collect();
        let b64 = crate::web::base64::tests::encode_for_test(&font);
        let mut input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        input.extend_from_slice(format!("const f=\"data:font/otf;base64,{b64}\";").as_bytes());

        let output_dir = Path::new("testout/wasm_after_base64");
        std::fs::create_dir_all(output_dir).unwrap();
        let js_path = output_dir.join("main.js");
        std::fs::write(&js_path, &input).unwrap();

        let cli = crate::Cli::try_parse_from([
            "rootsqz",
            "-j",
            js_path.to_str().unwrap(),
            "-f",
            "tests/wasm/fixture.wasm",
            "--size-profile",
            "64k-wasm",
            "-t",
            "node",
            "-o",
            output_dir.to_str().unwrap(),
        ])
        .unwrap();
        crate::web::run(cli.default_args.unwrap()).unwrap();

        assert!(Command::new("node")
            .arg(output_dir.join("index.mjs"))
            .status()
            .expect("Failed to run node decompressor")
            .success());
        let wasm = std::fs::read("tests/wasm/fixture.wasm").unwrap();
        let runs = crate::web::base64::extract(&input, crate::web::base64::MIN_RUN).unwrap();
        input.extend_from_slice(&wasm);
        assert!(std::fs::read(output_dir.join("output.bin")).unwrap() == input);

        // The module is after the JavaScript and the decoded runs in the stream
        let start = (runs.js.len() + runs.decoded.concat().len()) as u32;
        let demo = crate::web::read_demo(
            js_path.to_str().unwrap(),
            &["tests/wasm/fixture.wasm".to_owned()],
        )
        .unwrap();
        assert_eq!(demo.layout.stream.wasm, vec![(start, start + wasm.len() as u32)]);
    }

    /// The search weighs a config by the size of the web output it makes, base64 runs and wasm
    /// files included, so that cost must be the size `run` writes.
    #[test]
    pub fn search_cost_is_web_output_size() {
        let font: Vec<u8> = (0..3000u32).map(|i| (i * i % 251) as u8).collect();
        let b64 = crate::web::base64::tests::encode_for_test(&font);
        let mut input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        input.extend_from_slice(format!("const f=\"data:font/otf;base64,{b64}\";").as_bytes());
        let output_dir = Path::new("testout/search_cost");
        std::fs::create_dir_all(output_dir).unwrap();
        let js_path = output_dir.join("main.js");
        std::fs::write(&js_path, &input).unwrap();
        let (js, wasm) = (js_path.to_str().unwrap(), "tests/wasm/fixture.wasm");

        let cli = crate::Cli::try_parse_from([
            "rootsqz", "-j", js, "-f", wasm, "--size-profile", "64k-wasm", "-o",
            output_dir.to_str().unwrap(),
        ])
        .unwrap();
        crate::web::run(cli.default_args.unwrap()).unwrap();

        let config = serde_json::from_str(SIZE_64K_WASM_CONFIG_JSON).unwrap();
        let mut demo = crate::web::read_demo(js, &[wasm.to_owned()]).unwrap();
        demo.layout.stream = crate::web::choose_layout(&config, &demo).unwrap();
        demo.layout.encoded_len = crate::compressor::search::stream_size(&config, &demo).unwrap();
        assert_eq!(
            crate::compressor::search::demo_cost(&config, &demo).unwrap() as u64,
            std::fs::metadata(output_dir.join("index.html")).unwrap().len()
        );
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
                layout: StreamLayout::default(),
                loader: None,
                decoder: Decoder::Serial,
                boot_js: None,
                base64: None,
                wasm: None,
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
                layout: StreamLayout::default(),
                loader: None,
                decoder: Decoder::Serial,
                boot_js: None,
                base64: None,
                wasm: None,
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
                layout: StreamLayout::default(),
                loader: None,
                decoder: Decoder::Serial,
                boot_js: None,
                base64: None,
                wasm: None,
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

    /// Renders the web output with every decoder, with and without a loader script and a boot script
    #[test]
    pub fn web_decoders() {
        let model_config = create_default_compress_config();
        let input = b"document.body.append('loaded');";
        let mut encoded_data = Vec::new();
        let mut encoder =
            Encoder::new(model_config.create_model().unwrap(), &mut encoded_data).unwrap();
        encoder.encode_section(&input[..]).unwrap();
        encoder.finish().unwrap();

        for decoder in [Decoder::Serial, Decoder::Sliced, Decoder::Worker] {
            for with_loader in [false, true] {
                let loader = with_loader.then(|| Loader {
                    html: Some(include_str!("web/templates/web/loaders/bar.html").to_owned()),
                    js: Some(include_str!("web/templates/web/loaders/bar.js").to_owned()),
                    keep: true,
                });
                render_output(
                    OutputGenerationOptions {
                        output_dir: Path::new(&format!(
                            "testout/web_decoders/{decoder:?}_{with_loader}"
                        ))
                        .to_owned(),
                        target: output_generator::Target::Web,
                        model_config: model_config.model.clone(),
                        static_model_params: model_config.static_model_params.clone(),
                        layout: StreamLayout::default(),
                        loader,
                        decoder,
                        boot_js: with_loader.then(|| "document.title = 'x';".to_owned()),
                        base64: None,
                        wasm: None,
                    },
                    input.len(),
                    encoded_data.clone(),
                    input.len(),
                    vec![],
                    vec![],
                )
                .unwrap_or_else(|e| panic!("{decoder:?}, loader {with_loader}: {e:?}"));
            }
        }
    }

    /// Decodes with the worker decoder in node, and checks the output and the progress messages
    #[test]
    pub fn worker_round_trip() {
        let model_config = create_default_compress_config();
        let input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        let mut encoded_data = Vec::new();
        let mut encoder =
            Encoder::new(model_config.create_model().unwrap(), &mut encoded_data).unwrap();
        encoder.encode_section(input.as_slice()).unwrap();
        encoder.finish().unwrap();

        let output_dir = Path::new("testout/worker_round_trip");
        std::fs::create_dir_all(output_dir).unwrap();
        std::fs::write(output_dir.join("input.pack"), &encoded_data).unwrap();
        let mut features_used = ModelRef::None;
        // postMessage stands in for the one of the worker
        let script = format!(
            r#"import fs from "node:fs";
let progress = [];
globalThis.postMessage = message => {{
    if (typeof message == "number") return progress.push(message);
    fs.writeFileSync(import.meta.dirname + "/output.bin", message);
    fs.writeFileSync(import.meta.dirname + "/progress.json", JSON.stringify(progress));
}};
{}
{}
decompressWorker(model, new Uint8Array(fs.readFileSync(import.meta.dirname + "/input.pack")), {}, {});
"#,
            generate_js_decompression_code(
                &model_config.model,
                &model_config.static_model_params,
                &StreamLayout::default(), &mut features_used
            ),
            include_str!("web/js_source/coder_worker.js"),
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

        let progress: Vec<usize> =
            serde_json::from_slice(&std::fs::read(output_dir.join("progress.json")).unwrap())
                .unwrap();
        assert!(!progress.is_empty(), "the worker should post progress");
        assert!(progress.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(progress.iter().all(|&done| done <= input.len()));
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
                &StreamLayout::default(), &mut features_used
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
                layout: StreamLayout::default(),
                loader: None,
                decoder: Decoder::Serial,
                boot_js: None,
                base64: None,
                wasm: None,
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

#[cfg(test)]
mod wasm_decoder_tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::compressor::compress_config::{CompressConfig, StreamLayout};
    use crate::compressor::model_finder::SIZE_64K_FAST_CONFIG_JSON;
    use crate::compressor::Encoder;
    use crate::web::output_generator::{
        self, insert_wasm, render_output, wasm_glue, Decoder, Loader, OutputGenerationOptions,
    };
    use crate::web::wasm_decoder::{self, WasmOptions};

    fn config() -> CompressConfig {
        let mut config: CompressConfig = serde_json::from_str(SIZE_64K_FAST_CONFIG_JSON).unwrap();
        config.static_model_params.hash_table_pow2_size = 20;
        config
    }

    fn encode(config: &CompressConfig, input: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();
        let mut encoder = Encoder::new(config.create_model().unwrap(), &mut encoded).unwrap();
        encoder.encode_section(input).unwrap();
        encoder.finish().unwrap();
        encoded
    }

    /// The node output with the WebAssembly decoder decodes the input
    #[test]
    pub fn node_round_trip() {
        let config = config();
        let input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        let encoded = encode(&config, &input);
        let output_dir = PathBuf::from("testout/wasm_node_round_trip");
        render_output(
            OutputGenerationOptions {
                output_dir: output_dir.clone(),
                target: output_generator::Target::Node,
                model_config: config.model.clone(),
                static_model_params: config.static_model_params.clone(),
                layout: StreamLayout::default(),
                loader: None,
                decoder: Decoder::Serial,
                boot_js: None,
                base64: None,
                wasm: Some(WasmOptions::default()),
            },
            input.len(),
            encoded,
            input.len(),
            vec![],
            vec![],
        )
        .unwrap();
        assert!(Command::new("node").arg(output_dir.join("index.mjs")).status().unwrap().success());
        assert_eq!(std::fs::read(output_dir.join("output.bin")).unwrap(), input);
    }

    /// Decodes with the glue of the worker and sliced decoders in node, and checks the output
    /// and the progress
    #[test]
    pub fn worker_and_sliced_round_trip() {
        let config = config();
        let input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        let encoded = encode(&config, &input);
        let module = wasm_decoder::generate(
            &config.model,
            &config.static_model_params,
            &StreamLayout::default(),
            encoded.len(),
            input.len(),
            WasmOptions::default(),
        )
        .unwrap();
        for (decoder, run) in [
            (
                Decoder::Worker,
                r#"globalThis.postMessage = message => {
    if (typeof message == "number") return progress.push(message);
    fs.writeFileSync(dir + "/output.bin", message);
    fs.writeFileSync(dir + "/progress.json", JSON.stringify(progress));
};
decompressWorker(input);"#,
            ),
            (
                Decoder::Sliced,
                r#"globalThis.rsqz = { progress: (done, total) => progress.push(done) };
decompressSliced(input, output => {
    fs.writeFileSync(dir + "/output.bin", output);
    fs.writeFileSync(dir + "/progress.json", JSON.stringify(progress));
    process.exit(0);
});"#,
            ),
        ] {
            let dir = Path::new("testout/wasm_glue").join(format!("{decoder:?}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("input.pack"), &encoded).unwrap();
            let glue = insert_wasm(&wasm_glue(&module, encoded.len(), input.len(), decoder), &module.bytes).unwrap();
            let script = format!(
                "import fs from 'node:fs';\nlet dir = import.meta.dirname, progress = [];\nlet input = new Uint8Array(fs.readFileSync(dir + '/input.pack'));\n{glue}\n{run}\n"
            );
            std::fs::write(dir.join("index.mjs"), script).unwrap();
            assert!(Command::new("node").arg(dir.join("index.mjs")).status().unwrap().success());
            assert_eq!(std::fs::read(dir.join("output.bin")).unwrap(), input, "{decoder:?}");
            let progress: Vec<usize> =
                serde_json::from_slice(&std::fs::read(dir.join("progress.json")).unwrap()).unwrap();
            assert!(!progress.is_empty(), "{decoder:?} should report progress");
            assert!(progress.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(progress.iter().all(|&done| done <= input.len()));
        }
    }

    /// Renders the web output with the WebAssembly decoder and every decoder, with and without a loader
    #[test]
    pub fn web_decoders() {
        let config = config();
        let input = b"document.body.append('loaded');";
        let encoded = encode(&config, input);
        for decoder in [Decoder::Serial, Decoder::Sliced, Decoder::Worker] {
            for with_loader in [false, true] {
                let loader = with_loader.then(|| Loader {
                    html: Some(include_str!("web/templates/web/loaders/bar.html").to_owned()),
                    js: Some(include_str!("web/templates/web/loaders/bar.js").to_owned()),
                    keep: true,
                });
                render_output(
                    OutputGenerationOptions {
                        output_dir: PathBuf::from(format!("testout/wasm_web_decoders/{decoder:?}_{with_loader}")),
                        target: output_generator::Target::Web,
                        model_config: config.model.clone(),
                        static_model_params: config.static_model_params.clone(),
                        layout: StreamLayout::default(),
                        loader,
                        decoder,
                        boot_js: None,
                        base64: None,
                        wasm: Some(WasmOptions::default()),
                    },
                    input.len(),
                    encoded.clone(),
                    input.len(),
                    vec![],
                    vec![],
                )
                .unwrap_or_else(|e| panic!("{decoder:?}, loader {with_loader}: {e:?}"));
            }
        }
    }
}
