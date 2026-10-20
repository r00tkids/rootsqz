//! `rootsqz report`: prints a view of a report written with `--report`.

use std::{io::Write, path::PathBuf};

use anyhow::{bail, Context, Result};

use super::{analysis::Analysis, ReportData, View};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Report directory, or the output directory that contains it
    pub dir: PathBuf,

    /// What to print: tab separated rows with a header line, or JSON for summary
    #[arg(short, long, value_enum, default_value_t = View::Summary)]
    pub view: View,

    /// Only this section: its index, or its path or the end of the path
    #[arg(short, long)]
    pub section: Option<String>,

    /// Only rows that overlap this byte range of the decompressed stream, e.g. 1200..1400
    #[arg(short, long, value_parser = parse_range)]
    pub range: Option<(usize, usize)>,

    /// Only rows whose first byte comes from this original source: its index, or its path or the
    /// end of the path (needs --report-source-map)
    #[arg(long)]
    pub source: Option<String>,

    /// Only tokens of this kind (ident, number, space, punct, other); for wasm-functions only
    /// function or data; for the wasm instruction views only this opcode, e.g. I32Const
    #[arg(long)]
    pub kind: Option<String>,

    /// Sort order, by default by offset (by bits with --top, and for aggregated views)
    #[arg(long, value_enum)]
    pub sort: Option<Sort>,

    /// Print only the first N rows, sorted by bits unless --sort is given
    #[arg(short, long)]
    pub top: Option<usize>,

    /// Only rows that cost at least this many bits
    #[arg(long)]
    pub min_bits: Option<f64>,

    /// Add a column per submodel: the bits it alone would have cost (bits view: its p(1)).
    /// Needs a report written with --report-models.
    #[arg(short, long)]
    pub models: bool,

    /// Size of the blocks of the blocks view
    #[arg(long, default_value_t = 64)]
    pub block_size: usize,

    /// Size of the windows of the hotspots view
    #[arg(long, default_value_t = 32)]
    pub window: usize,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Offset,
    Bits,
    BitsPerByte,
}

/// Which rows a view contains.
#[derive(Debug, Clone)]
pub struct Filter {
    pub section: Option<usize>,
    pub source: Option<usize>,
    pub range: Option<(usize, usize)>,
    pub kind: Option<String>,
    pub sort: Option<Sort>,
    pub top: Option<usize>,
    pub min_bits: Option<f64>,
    pub models: bool,
    pub block_size: usize,
    pub window: usize,
    /// Only tokens of code: the main JavaScript and files with a code extension such as .glsl.
    pub code_only: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            section: None,
            source: None,
            range: None,
            kind: None,
            sort: None,
            top: None,
            min_bits: None,
            models: false,
            block_size: 64,
            window: 32,
            code_only: false,
        }
    }
}

fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let (a, b) = s
        .split_once("..")
        .ok_or_else(|| "expected START..END".to_owned())?;
    let a: usize = a.trim().parse().map_err(|e| format!("{e}"))?;
    let b: usize = b.trim().parse().map_err(|e| format!("{e}"))?;
    if b <= a {
        return Err("END must be larger than START".to_owned());
    }
    Ok((a, b))
}

pub fn run(args: Args) -> Result<()> {
    let data = ReportData::read(&args.dir)?;
    if args.models && data.model_names.is_empty() {
        bail!("The report has no submodel predictions. Compress with --report-models.");
    }
    if args.view == View::Models && data.model_names.is_empty() {
        bail!("The models view needs submodel predictions. Compress with --report-models.");
    }
    let section = match &args.section {
        None => None,
        Some(s) => Some(match s.parse::<usize>() {
            Ok(i) if i < data.sections.len() => i,
            _ => data
                .sections
                .iter()
                .position(|sec| {
                    [Some(&sec.path), sec.label.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|name| name == s || name.ends_with(s.as_str()))
                })
                .with_context(|| format!("No section '{s}'; see the sections view"))?,
        }),
    };
    let source = match &args.source {
        None => None,
        Some(s) => {
            let mapping = data
                .source_mapping
                .as_ref()
                .context("The report has no source positions. Compress with --report-source-map, or give wasm files debug info.")?;
            Some(match s.parse::<usize>() {
                Ok(i) if i < mapping.sources.len() => i,
                _ => mapping
                    .sources
                    .iter()
                    .position(|src| src.path == *s || src.path.ends_with(s.as_str()))
                    .with_context(|| format!("No source '{s}'; see the sources view"))?,
            })
        }
    };
    if matches!(args.view, View::Sources | View::SourceLines) && data.source_mapping.is_none() {
        bail!(
            "The {} view needs source positions. Compress with --report-source-map, or give wasm files debug info (--report-wasm-debug).",
            args.view.name()
        );
    }
    let filter = Filter {
        section,
        source,
        range: args.range,
        kind: args.kind,
        sort: args.sort,
        top: args.top,
        min_bits: args.min_bits,
        models: args.models,
        block_size: args.block_size.max(1),
        window: args.window.max(1),
        code_only: false,
    };

    let analysis = Analysis::new(&data);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if args.view == View::Summary {
        let summary = analysis.summary(&[]);
        writeln!(out, "{}", serde_json::to_string_pretty(&summary)?)?;
    } else {
        analysis.view(args.view, &filter).write_tsv(&mut out)?;
    }
    Ok(())
}
