//! Compression report: what the model predicted for every bit of the compressed stream,
//! saved next to the output so that it can be summarized, queried and viewed later.

mod analysis;
mod html;
pub mod query;
pub mod source_map;
mod table;

use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    compressor::{compress_config::CompressConfig, Recording},
    web::output_generator::OutputFile,
};

pub use analysis::View;
use source_map::{SourceFile, SourceMapping, SourcePos};

/// Directory in the output directory that the report is written to.
pub const REPORT_DIR: &str = "report";
const META_FILE: &str = "report.json";
const DATA_FILE: &str = "data.bin";
const SOURCES_FILE: &str = "sources.json";

/// One input file in the compressed stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    /// Path as given on the command line.
    pub path: String,
    /// Name given with --report-label, e.g. the module the plugin made the file from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `main` for the main JavaScript, `file` for a `--file`.
    pub kind: String,
    /// Offset of the first byte in the decompressed stream.
    pub start: usize,
    pub length: usize,
    /// Whether the file is UTF-8 text, analyzed by lines and tokens instead of blocks.
    pub text: bool,
}

impl Section {
    pub fn new(path: &str, kind: &str, start: usize, content: &[u8]) -> Self {
        Self {
            path: path.to_owned(),
            label: None,
            kind: kind.to_owned(),
            start,
            length: content.len(),
            text: is_text(content),
        }
    }

    pub fn end(&self) -> usize {
        self.start + self.length
    }

    /// The label if there is one, otherwise the path.
    pub fn name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.path)
    }
}

pub fn is_text(content: &[u8]) -> bool {
    std::str::from_utf8(content).is_ok()
        && content
            .iter()
            .all(|&b| b >= 0x20 || b == b'\n' || b == b'\r' || b == b'\t')
}

/// Everything the report is made from.
pub struct ReportData {
    pub config: CompressConfig,
    /// The config as it was given; `CompressConfig` cannot be serialized.
    pub config_json: serde_json::Value,
    pub sections: Vec<Section>,
    pub outputs: Vec<OutputFile>,
    /// Names of the submodels, empty if their predictions were not recorded.
    pub model_names: Vec<String>,
    pub recording: Recording,
    /// Original positions of the main JavaScript, from --report-source-map.
    pub source_mapping: Option<SourceMapping>,
}

/// What `--report` writes, besides the summary.
#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    /// Views written as TSV files at compress time (`--report-detail`).
    pub detail: Vec<View>,
}

impl ReportData {
    /// Writes `report.json`, `data.bin`, `report.html` and the detail files into `dir`.
    pub fn write(&self, dir: &Path, options: &ReportOptions) -> Result<()> {
        fs::create_dir_all(dir).context("Failed to create the report directory")?;
        self.write_data(&dir.join(DATA_FILE))?;
        let sources_path = dir.join(SOURCES_FILE);
        match &self.source_mapping {
            Some(mapping) => fs::write(&sources_path, serde_json::to_string(&mapping.sources)?)
                .context("Failed to write sources.json")?,
            None if sources_path.exists() => fs::remove_file(&sources_path)?,
            None => {}
        }

        let analysis = analysis::Analysis::new(self);
        let mut detail_files = Vec::new();
        for &view in &options.detail {
            let table = analysis.view(view, &query::Filter::default());
            let file = format!("{}.tsv", view.name());
            let mut out = BufWriter::new(
                fs::File::create(dir.join(&file))
                    .with_context(|| format!("Failed to create {file}"))?,
            );
            table.write_tsv(&mut out)?;
            out.flush()?;
            detail_files.push((file, view, table.columns.clone()));
        }

        let summary = analysis.summary(&detail_files);
        fs::write(dir.join(META_FILE), serde_json::to_string_pretty(&summary)?)
            .context("Failed to write report.json")?;

        html::write(&analysis, &summary, &dir.join("report.html"))
            .context("Failed to write report.html")?;
        Ok(())
    }

    fn write_data(&self, path: &Path) -> Result<()> {
        let r = &self.recording;
        let mut out = BufWriter::new(fs::File::create(path).context("Failed to create data.bin")?);
        out.write_all(&r.input)?;
        for v in &r.encoded_pos {
            out.write_all(&v.to_le_bytes())?;
        }
        for v in r.preds.iter().chain(&r.submodel_preds) {
            out.write_all(&v.to_le_bytes())?;
        }
        if let Some(mapping) = &self.source_mapping {
            for pos in &mapping.positions {
                let (source, line, column) =
                    pos.map_or((-1, 0, 0), |p| (p.source as i32, p.line, p.column));
                out.write_all(&source.to_le_bytes())?;
                out.write_all(&line.to_le_bytes())?;
                out.write_all(&column.to_le_bytes())?;
            }
        }
        out.flush()?;
        Ok(())
    }

    /// Reads a report written by `write`. `dir` is the report directory or the output directory.
    pub fn read(dir: &Path) -> Result<Self> {
        let dir = resolve_dir(dir)?;
        let summary: serde_json::Value = serde_json::from_slice(
            &fs::read(dir.join(META_FILE)).context("Failed to read report.json")?,
        )
        .context("Failed to parse report.json")?;
        let field = |name: &str, value: &serde_json::Value| -> Result<serde_json::Value> {
            ensure!(
                !value.is_null(),
                "report.json has no \"{name}\"; was it written by another version?"
            );
            Ok(value.clone())
        };
        let config_json = field("config", &summary["config"])?;
        let config: CompressConfig = serde_json::from_value(config_json.clone())?;
        let sections: Vec<Section> =
            serde_json::from_value(field("sections", &summary["sections"])?)?;
        let outputs: Vec<OutputFile> =
            serde_json::from_value(field("outputs", &summary["outputs"])?)?;
        let model_names: Vec<String> =
            serde_json::from_value(field("models.names", &summary["models"]["names"])?)?;

        let data = fs::read(dir.join(DATA_FILE)).context("Failed to read data.bin")?;
        let n = sections.iter().map(|s| s.length).sum::<usize>();
        let m = model_names.len();
        let map_path = summary["source_map"]["file"].as_str().map(str::to_owned);
        let mapped = if map_path.is_some() {
            sections[0].length
        } else {
            0
        };
        let expected = n + 4 * n + 4 * 8 * n + 4 * 8 * n * m + 12 * mapped;
        ensure!(
            data.len() == expected,
            "data.bin has {} bytes, report.json describes {expected}",
            data.len()
        );
        let (input, rest) = data.split_at(n);
        let (encoded_pos, rest) = rest.split_at(4 * n);
        let (preds, rest) = rest.split_at(4 * 8 * n);
        let (submodel_preds, positions) = rest.split_at(4 * 8 * n * m);
        let u32s = |b: &[u8]| -> Vec<u32> {
            b.chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect()
        };
        let f32s = |b: &[u8]| -> Vec<f32> {
            b.chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect()
        };

        let source_mapping = match map_path {
            None => None,
            Some(map_path) => {
                let sources: Vec<SourceFile> = serde_json::from_slice(
                    &fs::read(dir.join(SOURCES_FILE)).context("Failed to read sources.json")?,
                )
                .context("Failed to parse sources.json")?;
                let positions = positions
                    .chunks_exact(12)
                    .map(|c| {
                        let source = i32::from_le_bytes(c[0..4].try_into().unwrap());
                        (source >= 0).then(|| SourcePos {
                            source: source as u32,
                            line: u32::from_le_bytes(c[4..8].try_into().unwrap()),
                            column: u32::from_le_bytes(c[8..12].try_into().unwrap()),
                        })
                    })
                    .collect();
                Some(SourceMapping {
                    map_path,
                    sources,
                    positions,
                })
            }
        };

        Ok(Self {
            source_mapping,
            config,
            config_json,
            sections,
            outputs,
            model_names,
            recording: Recording {
                input: input.to_vec(),
                encoded_pos: u32s(encoded_pos),
                preds: f32s(preds),
                submodel_preds: f32s(submodel_preds),
                submodel_count: m,
            },
        })
    }
}

/// The report directory: `dir` itself or its `report` subdirectory.
fn resolve_dir(dir: &Path) -> Result<PathBuf> {
    if dir.join(META_FILE).is_file() {
        Ok(dir.to_owned())
    } else if dir.join(REPORT_DIR).join(META_FILE).is_file() {
        Ok(dir.join(REPORT_DIR))
    } else {
        bail!(
            "No report in '{}'. Compress with --report to write one.",
            dir.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{query::Filter, ReportData, ReportOptions, Section, View};
    use crate::compressor::compress_config::CompressConfig;
    use crate::compressor::{model_finder::DEFAULT_CONFIG_JSON, Encoder};

    /// A source map that maps every line of `js` to the same line of `orig.js`.
    fn line_source_map(js: &[u8]) -> super::SourceMapping {
        let lines = js.split(|&b| b == b'\n').count();
        let mappings = std::iter::once("AAAA")
            .chain(std::iter::repeat("AACA").take(lines - 1))
            .collect::<Vec<_>>()
            .join(";");
        let dir = Path::new("testout/report_source_map");
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("index.js.map");
        std::fs::write(
            &path,
            format!(r#"{{"version":3,"sources":["orig.js"],"mappings":"{mappings}"}}"#),
        )
        .unwrap();
        super::SourceMapping::read(&path, js).unwrap()
    }

    fn encode(config: &CompressConfig, sections: &[&[u8]], record: bool) -> (Vec<u8>, ReportData) {
        let mut encoded = Vec::new();
        let mut encoder = Encoder::new(config.create_model().unwrap(), &mut encoded).unwrap();
        if record {
            encoder.record(true);
        }
        let mut report_sections = Vec::new();
        let mut start = 0;
        for (i, &content) in sections.iter().enumerate() {
            encoder.encode_section(content).unwrap();
            report_sections.push(Section::new(
                &format!("s{i}"),
                if i == 0 { "main" } else { "file" },
                start,
                content,
            ));
            start += content.len();
        }
        let recording = encoder.take_recording().unwrap_or_default();
        encoder.finish().unwrap();
        let data = ReportData {
            config: config.clone(),
            config_json: serde_json::from_str(DEFAULT_CONFIG_JSON).unwrap(),
            model_names: if record {
                config.model.submodel_names()
            } else {
                Vec::new()
            },
            sections: report_sections,
            outputs: Vec::new(),
            recording,
            source_mapping: None,
        };
        (encoded, data)
    }

    #[test]
    fn report_round_trip() {
        let config: CompressConfig = serde_json::from_str(DEFAULT_CONFIG_JSON).unwrap();
        let js = std::fs::read("tests/ray_tracer/index.js").unwrap();
        let binary: Vec<u8> = (0..2000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        let sections: [&[u8]; 2] = [&js, &binary];

        let (plain, _) = encode(&config, &sections, false);
        let (encoded, mut data) = encode(&config, &sections, true);
        data.source_mapping = Some(line_source_map(&js));
        data.sections[1].label = Some("random.bin".to_owned());
        assert_eq!(plain, encoded, "recording must not change the output");

        assert_eq!(data.recording.input.len(), js.len() + binary.len());
        assert_eq!(data.recording.submodel_count, data.model_names.len());
        assert!(data.sections[0].text && !data.sections[1].text);

        let analysis = super::analysis::Analysis::new(&data);
        let cost_bytes = analysis.byte_costs().iter().sum::<f64>() / 8.;
        assert!(
            (cost_bytes - encoded.len() as f64).abs() < 8.,
            "costs {cost_bytes} should add up to the stream size {}",
            encoded.len()
        );

        let dir = Path::new("testout/report");
        let _ = std::fs::remove_dir_all(dir);
        data.write(
            dir,
            &ReportOptions {
                detail: vec![View::Lines, View::Blocks],
            },
        )
        .unwrap();
        for file in [
            "report.json",
            "report.html",
            "data.bin",
            "lines.tsv",
            "blocks.tsv",
        ] {
            assert!(dir.join(file).is_file(), "{file} missing");
        }

        let read = ReportData::read(dir.parent().unwrap().join("report").as_path()).unwrap();
        assert_eq!(read.recording.input, data.recording.input);
        assert_eq!(read.recording.preds, data.recording.preds);
        assert_eq!(read.recording.submodel_preds, data.recording.submodel_preds);
        assert_eq!(read.model_names, data.model_names);
        assert_eq!(read.sections[1].label.as_deref(), Some("random.bin"));
        assert_eq!(
            read.source_mapping.as_ref().unwrap().positions,
            data.source_mapping.as_ref().unwrap().positions
        );

        let analysis = super::analysis::Analysis::new(&read);
        let filter = Filter {
            models: true,
            ..Filter::default()
        };
        for view in View::ALL {
            if view == View::Summary {
                continue;
            }
            let table = analysis.view(view, &filter);
            assert!(!table.rows.is_empty(), "{} is empty", view.name());
            assert!(
                table.rows.iter().all(|r| r.len() == table.columns.len()),
                "{}",
                view.name()
            );
        }
        let lines = analysis.view(View::Lines, &Filter::default());
        let bits = lines.columns.iter().position(|c| c == "bits").unwrap();
        let line_bits: f64 = lines
            .rows
            .iter()
            .map(|r| match r[bits] {
                super::table::Cell::Float(v, _) => v,
                _ => 0.,
            })
            .sum();
        let section_bits: f64 = analysis.byte_costs()[..js.len()].iter().sum();
        assert!(
            (line_bits - section_bits).abs() < 1e-6,
            "lines must cover the text section"
        );

        // Every line maps to the same line of orig.js, so source lines and lines agree.
        let source_lines = analysis.view(View::SourceLines, &Filter::default());
        assert_eq!(source_lines.rows.len(), lines.rows.len());
    }
}
