use std::{collections::HashMap, path::Path};

use serde_json::{json, Value};

use super::{
    query::{Filter, Sort},
    source_map::SourcePos,
    table::{escape, Cell, Table},
    wasm::{ItemKind, WasmDebug, WasmModule},
    ReportData, Section,
};
use crate::compressor::compress_config::ModelConfig;

/// Views of the report, printed by `rootsqz report --view` and written by `--report-detail`.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Totals, output layout, sections, top lists and suggestions (JSON)
    Summary,
    /// One row per input file
    Sections,
    /// One row per line of the text sections
    Lines,
    /// One row per block of the binary sections
    Blocks,
    /// The most expensive windows of bytes, not overlapping
    Hotspots,
    /// One row per token of the text sections
    Tokens,
    /// Tokens grouped by their text
    TokenTotals,
    /// One row per byte
    Bytes,
    /// One row per bit, with the predicted probability
    Bits,
    /// Repeated byte strings and what their repetition costs
    Repeats,
    /// One row per submodel: what it alone would have cost (needs --report-models)
    Models,
    /// One row per original source file (needs a source map or wasm debug info)
    Sources,
    /// One row per line of an original source file (needs a source map or wasm debug info)
    SourceLines,
    /// Candidates worth looking into
    Suggestions,
    /// One row per section of each wasm file
    WasmSections,
    /// One row per function and data segment of each wasm file
    WasmFunctions,
    /// One row per instruction of each wasm file
    WasmInstructions,
    /// Wasm instructions grouped by opcode
    WasmOpcodeTotals,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Summary => "summary",
            View::Sections => "sections",
            View::Lines => "lines",
            View::Blocks => "blocks",
            View::Hotspots => "hotspots",
            View::Tokens => "tokens",
            View::TokenTotals => "token-totals",
            View::Bytes => "bytes",
            View::Bits => "bits",
            View::Repeats => "repeats",
            View::Models => "models",
            View::Sources => "sources",
            View::SourceLines => "source-lines",
            View::Suggestions => "suggestions",
            View::WasmSections => "wasm-sections",
            View::WasmFunctions => "wasm-functions",
            View::WasmInstructions => "wasm-instructions",
            View::WasmOpcodeTotals => "wasm-opcode-totals",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            View::Summary => "this summary",
            View::Sections => "one row per input file (section) of the compressed stream",
            View::Lines => "one row per line of the text sections; line and column are 1-based",
            View::Blocks => "one row per block (default 64 bytes) of the binary sections",
            View::Hotspots => "the most expensive windows of --window bytes (default 32), not overlapping, largest cost first",
            View::Tokens => "one row per token of the text sections (kinds: ident, number, space, punct, other)",
            View::TokenTotals => "tokens grouped by kind and text: count and total cost",
            View::Bytes => "one row per byte",
            View::Bits => "one row per bit: the bit, the predicted p(1) and its cost; bit 0 is the most significant",
            View::Repeats => "byte strings of 8 or more bytes that occurred before, with what they cost again",
            View::Models => "one row per submodel: bits it alone would have cost, and for how many bytes it was the best",
            View::Sources => "one row per original source file: the bytes of the stream that come from it and their cost (needs --report-source-map for the main JavaScript, debug info for wasm files)",
            View::SourceLines => "one row per line of an original source file: the bytes of the stream that come from it and their cost (needs --report-source-map or wasm debug info)",
            View::Suggestions => "candidates worth looking into, largest cost first",
            View::WasmSections => "one row per section of each wasm file (type, import, code, data, custom sections such as name), with the bytes that the LEB128 size uses more than needed",
            View::WasmFunctions => "one row per function and data segment of each wasm file (--kind function or data), with its name if known",
            View::WasmInstructions => "one row per instruction of the wasm functions (--kind <opcode>, e.g. I32Const), with its function and the bytes its LEB128 immediates use more than needed",
            View::WasmOpcodeTotals => "wasm instructions grouped by opcode: count and total cost",
        }
    }

    pub const ALL: [View; 18] = [
        View::Summary,
        View::Sections,
        View::Lines,
        View::Blocks,
        View::Hotspots,
        View::Tokens,
        View::TokenTotals,
        View::Bytes,
        View::Bits,
        View::Repeats,
        View::Models,
        View::Sources,
        View::SourceLines,
        View::Suggestions,
        View::WasmSections,
        View::WasmFunctions,
        View::WasmInstructions,
        View::WasmOpcodeTotals,
    ];
}

/// Cost in bits of coding `bit` with the stretched prediction `s` (logit of p(1)).
fn bit_cost(s: f32, bit: u8) -> f64 {
    let x = if bit == 1 { -s as f64 } else { s as f64 };
    let softplus = if x > 30. { x } else { x.exp().ln_1p() };
    softplus / std::f64::consts::LN_2
}

fn squash(s: f32) -> f64 {
    1. / (1. + (-(s as f64)).exp())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenKind {
    Ident,
    Number,
    Space,
    Punct,
    Other,
}

impl TokenKind {
    fn name(self) -> &'static str {
        match self {
            TokenKind::Ident => "ident",
            TokenKind::Number => "number",
            TokenKind::Space => "space",
            TokenKind::Punct => "punct",
            TokenKind::Other => "other",
        }
    }
}

/// A byte range of the decompressed stream that a row describes.
#[derive(Debug, Clone)]
struct Unit {
    section: usize,
    start: usize,
    end: usize,
}

/// Splits text into tokens, roughly as in JavaScript or GLSL.
fn tokenize(bytes: &[u8], base: usize) -> Vec<(usize, usize, TokenKind)> {
    let is_ident_start = |b: u8| b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80;
    let is_ident = |b: u8| is_ident_start(b) || b.is_ascii_digit();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let start = i;
        let kind = if is_ident_start(b) {
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            TokenKind::Ident
        } else if b.is_ascii_digit()
            || (b == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())
        {
            let hex = b == b'0' && i + 1 < bytes.len() && matches!(bytes[i + 1], b'x' | b'X');
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                let exponent_sign =
                    !hex && matches!(c, b'+' | b'-') && matches!(bytes[i - 1], b'e' | b'E');
                if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || exponent_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            TokenKind::Number
        } else if b.is_ascii_whitespace() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            TokenKind::Space
        } else if b.is_ascii_punctuation() {
            i += 1;
            TokenKind::Punct
        } else {
            i += 1;
            TokenKind::Other
        };
        tokens.push((base + start, base + i, kind));
    }
    tokens
}

pub struct Analysis<'a> {
    data: &'a ReportData,
    /// Cost in bits of every byte.
    byte_costs: Vec<f64>,
    /// Number of submodels, 0 if not recorded.
    m: usize,
    /// Cost in bits of every byte for every submodel alone, `m` per byte.
    model_byte_costs: Vec<f32>,
    /// Offsets of the line starts of each text section (empty for binary sections).
    line_starts: Vec<Vec<usize>>,
    tokens: Vec<Vec<(usize, usize, TokenKind)>>,
    /// The structure of each section that is a wasm file.
    wasm: Vec<Option<WasmModule>>,
}

impl<'a> Analysis<'a> {
    pub fn new(data: &'a ReportData) -> Self {
        let r = &data.recording;
        let n = r.input.len();
        let m = data.model_names.len();
        let mut byte_costs = vec![0.; n];
        let mut model_byte_costs = vec![0f32; n * m];
        for (i, &byte) in r.input.iter().enumerate() {
            for k in 0..8 {
                let bit = (byte >> (7 - k)) & 1;
                let b = i * 8 + k;
                byte_costs[i] += bit_cost(r.preds[b], bit);
                for j in 0..m {
                    model_byte_costs[i * m + j] +=
                        bit_cost(r.submodel_preds[b * m + j], bit) as f32;
                }
            }
        }

        let mut line_starts = Vec::new();
        let mut tokens = Vec::new();
        let mut wasm = Vec::new();
        for s in &data.sections {
            let bytes = &r.input[s.start..s.end()];
            wasm.push(
                (!s.text && super::wasm::is_wasm(bytes))
                    .then(|| WasmModule::parse(bytes).ok())
                    .flatten(),
            );
            let bytes = &r.input[s.start..s.end()];
            if s.text {
                let mut starts = vec![s.start];
                starts.extend(
                    bytes
                        .iter()
                        .enumerate()
                        .filter(|(i, &b)| b == b'\n' && i + 1 < bytes.len())
                        .map(|(i, _)| s.start + i + 1),
                );
                line_starts.push(starts);
                tokens.push(tokenize(bytes, s.start));
            } else {
                line_starts.push(Vec::new());
                tokens.push(Vec::new());
            }
        }

        Self {
            data,
            byte_costs,
            m,
            model_byte_costs,
            line_starts,
            tokens,
            wasm,
        }
    }

    /// Debug info of a wasm section, if there is some.
    fn wasm_debug(&self, section: usize) -> Option<&WasmDebug> {
        self.data.wasm_debug.iter().find(|d| d.section == section)
    }

    /// A function or data segment, e.g. `function 3 render`.
    fn item_label(&self, section: usize, item: usize) -> String {
        let module = self.wasm[section].as_ref().unwrap();
        let it = &module.items[item];
        let names = match (self.wasm_debug(section), it.kind) {
            (Some(d), ItemKind::Function) => &d.function_names,
            (Some(d), ItemKind::Data) => &d.data_names,
            (None, ItemKind::Function) => &module.function_names,
            (None, ItemKind::Data) => &module.data_names,
        };
        match names.get(&it.index) {
            Some(name) => format!("{} {} {name}", it.kind.name(), it.index),
            None => format!("{} {}", it.kind.name(), it.index),
        }
    }

    /// The function or data segment (or else the section) and the instruction at `offset` of a
    /// wasm section.
    fn wasm_place(&self, section: usize, offset: usize) -> Option<(String, String)> {
        let module = self.wasm[section].as_ref()?;
        let local = offset - self.data.sections[section].start;
        let item = match module.item_at(local) {
            Some(i) => self.item_label(section, i),
            None => match module.section_at(local) {
                Some(s) if s.custom => format!("custom section {}", s.name),
                Some(s) => format!("{} section", s.name),
                None => "header".to_owned(),
            },
        };
        let instruction = module
            .instruction_at(local)
            .map_or(String::new(), |i| format!("{} {}", i.op, i.operands).trim().to_owned());
        Some((item, instruction))
    }

    fn has_wasm(&self) -> bool {
        self.wasm.iter().any(Option::is_some)
    }

    fn wasm_cells(&self, unit: &Unit) -> Vec<Cell> {
        if !self.has_wasm() {
            return Vec::new();
        }
        match self.wasm_place(unit.section, unit.start) {
            Some((item, instruction)) => vec![
                item.into(),
                if instruction.is_empty() {
                    Cell::Empty
                } else {
                    instruction.into()
                },
            ],
            None => vec![Cell::Empty, Cell::Empty],
        }
    }

    fn wasm_columns(&self) -> &'static [&'static str] {
        if self.has_wasm() {
            &["wasm_item", "wasm_instruction"]
        } else {
            &[]
        }
    }

    fn source_pos(&self, offset: usize) -> Option<SourcePos> {
        self.data
            .source_mapping
            .as_ref()
            .and_then(|m| m.positions.get(offset).copied().flatten())
    }

    fn source_path(&self, source: u32) -> &str {
        &self.data.source_mapping.as_ref().unwrap().sources[source as usize].path
    }

    /// `path:line` of the original source of `offset`, if known.
    fn source_place(&self, offset: usize) -> Option<String> {
        self.source_pos(offset)
            .map(|p| format!("{}:{}", self.source_path(p.source), p.line))
    }

    fn input(&self) -> &[u8] {
        &self.data.recording.input
    }

    fn cost(&self, start: usize, end: usize) -> f64 {
        self.byte_costs[start..end].iter().sum()
    }

    fn model_costs(&self, start: usize, end: usize) -> Vec<f64> {
        let mut costs = vec![0.; self.m];
        for i in start..end {
            for (j, c) in costs.iter_mut().enumerate() {
                *c += self.model_byte_costs[i * self.m + j] as f64;
            }
        }
        costs
    }

    /// Position in the compressed stream before `offset` was coded.
    fn encoded_before(&self, offset: usize) -> usize {
        if offset == 0 {
            0
        } else {
            self.data.recording.encoded_pos[offset - 1] as usize
        }
    }

    fn encoded_after(&self, offset: usize) -> usize {
        self.data.recording.encoded_pos[offset] as usize
    }

    fn section_of(&self, offset: usize) -> usize {
        self.data
            .sections
            .iter()
            .position(|s| offset < s.end())
            .unwrap_or(self.data.sections.len() - 1)
    }

    /// 1-based line and column of `offset` in a text section.
    fn line_col(&self, section: usize, offset: usize) -> Option<(usize, usize)> {
        let starts = &self.line_starts[section];
        if starts.is_empty() {
            return None;
        }
        let line = starts.partition_point(|&s| s <= offset) - 1;
        Some((line + 1, offset - starts[line] + 1))
    }

    fn sections_in(&self, filter: &Filter) -> impl Iterator<Item = (usize, &Section)> {
        let only = filter.section;
        self.data
            .sections
            .iter()
            .enumerate()
            .filter(move |(i, _)| only.map_or(true, |o| o == *i))
    }

    fn in_range(filter: &Filter, start: usize, end: usize) -> bool {
        filter.range.map_or(true, |(a, b)| start < b && end > a)
    }

    fn lines(&self, filter: &Filter) -> Vec<Unit> {
        let mut units = Vec::new();
        for (i, s) in self.sections_in(filter).filter(|(_, s)| s.text) {
            let starts = &self.line_starts[i];
            for (k, &start) in starts.iter().enumerate() {
                let end = starts.get(k + 1).copied().unwrap_or(s.end());
                units.push(Unit {
                    section: i,
                    start,
                    end,
                });
            }
        }
        units
    }

    fn blocks(&self, filter: &Filter) -> Vec<Unit> {
        let mut units = Vec::new();
        for (i, s) in self.sections_in(filter).filter(|(_, s)| !s.text) {
            let mut start = s.start;
            while start < s.end() {
                let end = (start + filter.block_size).min(s.end());
                units.push(Unit {
                    section: i,
                    start,
                    end,
                });
                start = end;
            }
        }
        units
    }

    /// Windows of `filter.window` bytes with the highest cost, chosen greedily so that they do not overlap.
    fn hotspots(&self, filter: &Filter) -> Vec<Unit> {
        let mut units = Vec::new();
        for (i, s) in self.sections_in(filter) {
            let w = filter.window.min(s.length);
            if w == 0 {
                continue;
            }
            let mut prefix = vec![0.; s.length + 1];
            for k in 0..s.length {
                prefix[k + 1] = prefix[k] + self.byte_costs[s.start + k];
            }
            let mut windows: Vec<(f64, usize)> = (0..=s.length - w)
                .map(|k| (prefix[k + w] - prefix[k], k))
                .collect();
            windows.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut covered = vec![false; s.length];
            for (_, k) in windows {
                if covered[k] || covered[k + w - 1] {
                    continue;
                }
                covered[k..k + w].iter_mut().for_each(|c| *c = true);
                units.push(Unit {
                    section: i,
                    start: s.start + k,
                    end: s.start + k + w,
                });
            }
        }
        units
    }

    /// Whether a section holds code, in which identifiers are names that can be changed.
    fn is_code(&self, section: usize) -> bool {
        const CODE: &[&str] = &[
            "js", "mjs", "cjs", "ts", "glsl", "frag", "vert", "comp", "wgsl", "hlsl",
        ];
        let s = &self.data.sections[section];
        s.kind == "main"
            || Path::new(s.name())
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| CODE.contains(&e))
    }

    fn token_list(&self, filter: &Filter) -> Vec<(Unit, TokenKind)> {
        let mut units = Vec::new();
        for (i, _) in self
            .sections_in(filter)
            .filter(|(i, _)| !filter.code_only || self.is_code(*i))
        {
            for &(start, end, kind) in &self.tokens[i] {
                if filter.kind.as_deref().map_or(true, |k| k == kind.name()) {
                    units.push((
                        Unit {
                            section: i,
                            start,
                            end,
                        },
                        kind,
                    ));
                }
            }
        }
        units
    }

    /// The bytes of a unit as escaped text, or as hex in a binary section.
    fn snippet(&self, unit: &Unit) -> String {
        let bytes = &self.input()[unit.start..unit.end];
        if self.data.sections[unit.section].text {
            escape(bytes, 120)
        } else {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }

    /// Where a unit is, for messages: `path:line:column` or `path +offset`.
    fn place(&self, unit: &Unit) -> String {
        let s = &self.data.sections[unit.section];
        match self.line_col(unit.section, unit.start) {
            Some((line, col)) => format!("{}:{line}:{col}", s.name()),
            None => match self.wasm_place(unit.section, unit.start) {
                Some((item, _)) => format!("{} +{} ({item})", s.name(), unit.start - s.start),
                None => format!("{} +{}", s.name(), unit.start - s.start),
            },
        }
    }

    /// Columns shared by all views of byte ranges.
    const UNIT_COLUMNS: [&'static str; 8] = [
        "section",
        "start",
        "end",
        "length",
        "bits",
        "bits_per_byte",
        "encoded_start",
        "encoded_end",
    ];

    fn unit_cells(&self, unit: &Unit) -> Vec<Cell> {
        let bits = self.cost(unit.start, unit.end);
        let length = unit.end - unit.start;
        vec![
            unit.section.into(),
            unit.start.into(),
            unit.end.into(),
            length.into(),
            Cell::bits(bits),
            Cell::Float(bits / length.max(1) as f64, 3),
            self.encoded_before(unit.start).into(),
            self.encoded_after(unit.end - 1).into(),
        ]
    }

    fn model_columns(&self, filter: &Filter) -> Vec<String> {
        if !filter.models {
            return Vec::new();
        }
        let mut columns = vec!["best_model".to_owned()];
        columns.extend((0..self.m).map(|j| format!("m{j}")));
        columns
    }

    fn model_cells(&self, filter: &Filter, unit: &Unit) -> Vec<Cell> {
        if !filter.models {
            return Vec::new();
        }
        let costs = self.model_costs(unit.start, unit.end);
        let best = (0..self.m)
            .min_by(|&a, &b| costs[a].total_cmp(&costs[b]))
            .map_or(Cell::Empty, |b| Cell::Int(b as i64));
        let mut cells = vec![best];
        cells.extend(costs.into_iter().map(Cell::bits));
        cells
    }

    fn unit_table(
        &self,
        filter: &Filter,
        extra: &[&str],
        units: Vec<Unit>,
        extra_cells: impl Fn(&Unit) -> Vec<Cell>,
    ) -> Table {
        let mut columns: Vec<String> = Self::UNIT_COLUMNS.iter().map(|c| c.to_string()).collect();
        columns.extend(extra.iter().map(|c| c.to_string()));
        let mapped = self.data.source_mapping.is_some();
        if mapped {
            columns.extend(["source".to_owned(), "source_line".to_owned()]);
        }
        columns.extend(self.model_columns(filter));
        let mut table = Table {
            columns,
            rows: Vec::new(),
        };
        for unit in units {
            if !Self::in_range(filter, unit.start, unit.end) {
                continue;
            }
            let pos = self.source_pos(unit.start);
            if filter.source.is_some() && pos.map(|p| p.source as usize) != filter.source {
                continue;
            }
            let mut row = self.unit_cells(&unit);
            row.extend(extra_cells(&unit));
            if mapped {
                match pos {
                    Some(p) => {
                        row.extend([self.source_path(p.source).into(), (p.line as usize).into()])
                    }
                    None => row.extend([Cell::Empty, Cell::Empty]),
                }
            }
            row.extend(self.model_cells(filter, &unit));
            table.rows.push(row);
        }
        table
    }

    fn line_col_cells(&self, unit: &Unit) -> Vec<Cell> {
        match self.line_col(unit.section, unit.start) {
            Some((line, col)) => vec![line.into(), col.into()],
            None => vec![Cell::Empty, Cell::Empty],
        }
    }

    pub fn view(&self, view: View, filter: &Filter) -> Table {
        let input = self.input();
        let (mut table, default_sort) = match view {
            View::Summary => (Table::new(&[]), Sort::Offset),
            View::Sections => (self.sections_table(filter), Sort::Offset),
            View::Lines => (
                self.unit_table(filter, &["line", "text"], self.lines(filter), |u| {
                    let (line, _) = self.line_col(u.section, u.start).unwrap();
                    let text = input[u.start..u.end]
                        .strip_suffix(b"\n")
                        .unwrap_or(&input[u.start..u.end]);
                    vec![line.into(), escape(text, 200).into()]
                }),
                Sort::Offset,
            ),
            View::Blocks => (
                self.unit_table(filter, &["hex"], self.blocks(filter), |u| {
                    let hex: Vec<String> = input[u.start..u.end]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    vec![hex.join("").into()]
                }),
                Sort::Offset,
            ),
            View::Hotspots => (
                self.unit_table(
                    filter,
                    &[&["line", "column", "text"], self.wasm_columns()].concat(),
                    self.hotspots(filter),
                    |u| {
                        let mut cells = self.line_col_cells(u);
                        cells.push(self.snippet(u).into());
                        cells.extend(self.wasm_cells(u));
                        cells
                    },
                ),
                Sort::Bits,
            ),
            View::Tokens => {
                let tokens = self.token_list(filter);
                let kinds: HashMap<usize, TokenKind> =
                    tokens.iter().map(|(u, k)| (u.start, *k)).collect();
                (
                    self.unit_table(
                        filter,
                        &["line", "column", "kind", "text"],
                        tokens.into_iter().map(|(u, _)| u).collect(),
                        |u| {
                            let mut cells = self.line_col_cells(u);
                            cells.push(kinds[&u.start].name().into());
                            cells.push(escape(&input[u.start..u.end], 80).into());
                            cells
                        },
                    ),
                    Sort::Offset,
                )
            }
            View::TokenTotals => (self.token_totals(filter), Sort::Bits),
            View::Bytes => {
                let units = self
                    .sections_in(filter)
                    .flat_map(|(i, s)| {
                        (s.start..s.end()).map(move |o| Unit {
                            section: i,
                            start: o,
                            end: o + 1,
                        })
                    })
                    .filter(|u| Self::in_range(filter, u.start, u.end))
                    .collect();
                (
                    self.unit_table(
                        filter,
                        &[&["line", "column", "byte", "char"], self.wasm_columns()].concat(),
                        units,
                        |u| {
                            let mut cells = self.line_col_cells(u);
                            cells.push(format!("{:02x}", input[u.start]).into());
                            cells.push(escape(&input[u.start..u.end], 1).into());
                            cells.extend(self.wasm_cells(u));
                            cells
                        },
                    ),
                    Sort::Offset,
                )
            }
            View::Bits => (self.bits_table(filter), Sort::Offset),
            View::Repeats => (self.repeats_table(filter), Sort::Bits),
            View::Models => (self.models_table(filter), Sort::Offset),
            View::Sources => (self.sources_table(filter, false), Sort::Bits),
            View::SourceLines => (self.sources_table(filter, true), Sort::Offset),
            View::Suggestions => (self.suggestions_table(), Sort::Bits),
            View::WasmSections => (self.wasm_sections_table(filter), Sort::Offset),
            View::WasmFunctions => (self.wasm_functions_table(filter), Sort::Offset),
            View::WasmInstructions => (self.wasm_instructions_table(filter), Sort::Offset),
            View::WasmOpcodeTotals => (self.wasm_opcode_totals(filter), Sort::Bits),
        };

        if let Some(min) = filter.min_bits {
            if let Some(i) = table.columns.iter().position(|c| c == "bits") {
                table
                    .rows
                    .retain(|row| matches!(row[i], Cell::Float(v, _) if v >= min));
            }
        }
        let sort = filter.sort.unwrap_or(if filter.top.is_some() {
            Sort::Bits
        } else {
            default_sort
        });
        match sort {
            Sort::Offset => {}
            Sort::Bits => table.sort_desc("bits"),
            Sort::BitsPerByte => table.sort_desc("bits_per_byte"),
        }
        if let Some(n) = filter.top {
            table.truncate(n);
        }
        table
    }

    fn sections_table(&self, filter: &Filter) -> Table {
        let mut table = Table::new(&[
            "index",
            "path",
            "label",
            "kind",
            "text",
            "wasm",
            "start",
            "end",
            "length",
            "bits",
            "cost_bytes",
            "bits_per_byte",
            "encoded_start",
            "encoded_end",
        ]);
        for (i, s) in self.sections_in(filter) {
            if !Self::in_range(filter, s.start, s.end()) {
                continue;
            }
            let bits = if s.length > 0 {
                self.cost(s.start, s.end())
            } else {
                0.
            };
            table.push(vec![
                i.into(),
                s.path.as_str().into(),
                s.label.as_deref().map_or(Cell::Empty, Cell::from),
                s.kind.as_str().into(),
                Cell::Bool(s.text),
                Cell::Bool(self.wasm[i].is_some()),
                s.start.into(),
                s.end().into(),
                s.length.into(),
                Cell::bits(bits),
                Cell::bits(bits / 8.),
                Cell::Float(bits / s.length.max(1) as f64, 3),
                if s.length > 0 {
                    self.encoded_before(s.start).into()
                } else {
                    Cell::Empty
                },
                if s.length > 0 {
                    self.encoded_after(s.end() - 1).into()
                } else {
                    Cell::Empty
                },
            ]);
        }
        table
    }

    /// Costs by original source file, or by line of it with `by_line`.
    fn sources_table(&self, filter: &Filter, by_line: bool) -> Table {
        let mut table = Table::new(if by_line {
            &[
                "source",
                "path",
                "line",
                "length",
                "bits",
                "bits_per_byte",
                "first_start",
                "text",
            ]
        } else {
            &[
                "source",
                "path",
                "length",
                "bits",
                "cost_bytes",
                "bits_per_byte",
                "share",
                "first_start",
            ]
        });
        let Some(mapping) = &self.data.source_mapping else {
            return table;
        };
        // Key: source index (u32::MAX for unmapped bytes) and line (0 for whole files).
        let mut totals: std::collections::BTreeMap<(u32, u32), (usize, f64, usize)> =
            Default::default();
        let total_bits: f64 = self.byte_costs.iter().sum();
        let mapped: Vec<usize> = mapping.maps.iter().map(|m| m.section).collect();
        let offsets = self
            .sections_in(filter)
            .filter(|(i, _)| mapped.contains(i))
            .flat_map(|(_, s)| s.start..s.end());
        for o in offsets {
            if !Self::in_range(filter, o, o + 1) {
                continue;
            }
            let pos = mapping.positions[o];
            if filter.source.is_some() && pos.map(|p| p.source as usize) != filter.source {
                continue;
            }
            let key = match pos {
                Some(p) => (p.source, if by_line { p.line } else { 0 }),
                None => (u32::MAX, 0),
            };
            let e = totals.entry(key).or_insert((0, 0., o));
            e.0 += 1;
            e.1 += self.byte_costs[o];
        }
        for ((source, line), (length, bits, first)) in totals {
            let (index, path) = if source == u32::MAX {
                (Cell::Empty, "(not mapped)".to_owned())
            } else {
                (
                    (source as usize).into(),
                    mapping.sources[source as usize].path.clone(),
                )
            };
            let bpb = Cell::Float(bits / length as f64, 3);
            if by_line {
                let text = (source != u32::MAX)
                    .then(|| mapping.sources[source as usize].content.as_deref())
                    .flatten()
                    .and_then(|c| c.lines().nth(line as usize - 1))
                    .map_or(Cell::Empty, |l| escape(l.trim().as_bytes(), 160).into());
                let line = if source == u32::MAX {
                    Cell::Empty
                } else {
                    (line as usize).into()
                };
                table.push(vec![
                    index,
                    path.into(),
                    line,
                    length.into(),
                    Cell::bits(bits),
                    bpb,
                    first.into(),
                    text,
                ]);
            } else {
                table.push(vec![
                    index,
                    path.into(),
                    length.into(),
                    Cell::bits(bits),
                    Cell::bits(bits / 8.),
                    bpb,
                    Cell::Float(bits / total_bits.max(1e-9), 3),
                    first.into(),
                ]);
            }
        }
        table
    }

    /// The wasm sections in the filter, with their module.
    fn wasm_in(&self, filter: &Filter) -> impl Iterator<Item = (usize, &Section, &WasmModule)> {
        self.sections_in(filter)
            .filter_map(|(i, s)| self.wasm[i].as_ref().map(|m| (i, s, m)))
    }

    fn wasm_sections_table(&self, filter: &Filter) -> Table {
        let mut units = Vec::new();
        let mut info = HashMap::new();
        for (i, s, module) in self.wasm_in(filter) {
            for ws in &module.sections {
                let unit = Unit {
                    section: i,
                    start: s.start + ws.start,
                    end: s.start + ws.end,
                };
                info.insert(unit.start, ws);
                units.push(unit);
            }
        }
        self.unit_table(filter, &["kind", "name", "padding"], units, |u| {
            let ws = info[&u.start];
            vec![
                if ws.custom { "custom" } else { "standard" }.into(),
                ws.name.as_str().into(),
                ws.padding.into(),
            ]
        })
    }

    fn wasm_functions_table(&self, filter: &Filter) -> Table {
        let mut units = Vec::new();
        let mut info = HashMap::new();
        for (i, s, module) in self.wasm_in(filter) {
            for (k, item) in module.items.iter().enumerate() {
                if filter.kind.as_deref().is_some_and(|kind| kind != item.kind.name()) {
                    continue;
                }
                let unit = Unit {
                    section: i,
                    start: s.start + item.start,
                    end: s.start + item.end,
                };
                let instructions = module.instructions.iter().filter(|x| x.item == k);
                let (count, padding) =
                    instructions.fold((0, item.padding), |(c, p), x| (c + 1, p + x.padding));
                info.insert(unit.start, (k, count, padding));
                units.push(unit);
            }
        }
        self.unit_table(
            filter,
            &["kind", "index", "name", "instructions", "padding"],
            units,
            |u| {
                let (k, count, padding) = info[&u.start];
                let module = self.wasm[u.section].as_ref().unwrap();
                let item = &module.items[k];
                let label = self.item_label(u.section, k);
                let name = label.splitn(3, ' ').nth(2).map_or(Cell::Empty, Cell::from);
                vec![
                    item.kind.name().into(),
                    (item.index as usize).into(),
                    name,
                    if item.kind == ItemKind::Function {
                        count.into()
                    } else {
                        Cell::Empty
                    },
                    padding.into(),
                ]
            },
        )
    }

    fn wasm_instruction_units(&self, filter: &Filter) -> Vec<(Unit, &super::wasm::Instruction)> {
        let mut units = Vec::new();
        for (i, s, module) in self.wasm_in(filter) {
            for ins in &module.instructions {
                if filter
                    .kind
                    .as_deref()
                    .is_some_and(|k| !k.eq_ignore_ascii_case(&ins.op))
                {
                    continue;
                }
                units.push((
                    Unit {
                        section: i,
                        start: s.start + ins.start,
                        end: s.start + ins.end,
                    },
                    ins,
                ));
            }
        }
        units
    }

    fn wasm_instructions_table(&self, filter: &Filter) -> Table {
        let units = self.wasm_instruction_units(filter);
        let info: HashMap<usize, &super::wasm::Instruction> =
            units.iter().map(|(u, ins)| (u.start, *ins)).collect();
        self.unit_table(
            filter,
            &["item", "opcode", "operands", "padding", "hex"],
            units.into_iter().map(|(u, _)| u).collect(),
            |u| {
                let ins = info[&u.start];
                let hex: String = self.input()[u.start..u.end]
                    .iter()
                    .take(24)
                    .map(|b| format!("{b:02x}"))
                    .collect();
                vec![
                    self.item_label(u.section, ins.item).into(),
                    ins.op.as_str().into(),
                    if ins.operands.is_empty() {
                        Cell::Empty
                    } else {
                        ins.operands.as_str().into()
                    },
                    ins.padding.into(),
                    hex.into(),
                ]
            },
        )
    }

    fn wasm_opcode_totals(&self, filter: &Filter) -> Table {
        struct Total {
            count: usize,
            length: usize,
            bits: f64,
            padding: usize,
            first: usize,
        }
        let mut totals: HashMap<&str, Total> = HashMap::new();
        for (u, ins) in self.wasm_instruction_units(filter) {
            if !Self::in_range(filter, u.start, u.end) {
                continue;
            }
            let t = totals.entry(ins.op.as_str()).or_insert(Total {
                count: 0,
                length: 0,
                bits: 0.,
                padding: 0,
                first: u.start,
            });
            t.count += 1;
            t.length += u.end - u.start;
            t.bits += self.cost(u.start, u.end);
            t.padding += ins.padding;
        }
        let mut table = Table::new(&[
            "opcode",
            "count",
            "length",
            "bits",
            "bits_per_occurrence",
            "bits_per_byte",
            "padding",
            "first_start",
        ]);
        let mut entries: Vec<_> = totals.into_iter().collect();
        entries.sort_by_key(|(_, t)| t.first);
        for (op, t) in entries {
            table.push(vec![
                op.into(),
                t.count.into(),
                t.length.into(),
                Cell::bits(t.bits),
                Cell::bits(t.bits / t.count as f64),
                Cell::Float(t.bits / t.length.max(1) as f64, 3),
                t.padding.into(),
                t.first.into(),
            ]);
        }
        table
    }

    /// Padding of the LEB128 numbers of a wasm section: count, bytes and their cost in bits.
    fn wasm_padding(&self, section: usize) -> (usize, usize, f64) {
        let Some(module) = &self.wasm[section] else {
            return (0, 0, 0.);
        };
        let start = self.data.sections[section].start;
        module.padded.iter().fold((0, 0, 0.), |(n, b, bits), r| {
            (
                n + 1,
                b + r.len(),
                bits + self.cost(start + r.start, start + r.end),
            )
        })
    }

    fn token_totals(&self, filter: &Filter) -> Table {
        struct Total {
            count: usize,
            bits: f64,
            length: usize,
            first: usize,
        }
        let mut totals: HashMap<(TokenKind, &[u8]), Total> = HashMap::new();
        let input = self.input();
        for (u, kind) in self.token_list(filter) {
            if !Self::in_range(filter, u.start, u.end) {
                continue;
            }
            let total = totals
                .entry((kind, &input[u.start..u.end]))
                .or_insert(Total {
                    count: 0,
                    bits: 0.,
                    length: u.end - u.start,
                    first: u.start,
                });
            total.count += 1;
            total.bits += self.cost(u.start, u.end);
        }
        let mut table = Table::new(&[
            "kind",
            "text",
            "length",
            "count",
            "bits",
            "bits_per_occurrence",
            "bits_per_byte",
            "first_start",
        ]);
        let mut entries: Vec<_> = totals.into_iter().collect();
        entries.sort_by_key(|(_, t)| t.first);
        for ((kind, text), t) in entries {
            table.push(vec![
                kind.name().into(),
                escape(text, 80).into(),
                t.length.into(),
                t.count.into(),
                Cell::bits(t.bits),
                Cell::bits(t.bits / t.count as f64),
                Cell::Float(t.bits / (t.length * t.count) as f64, 3),
                t.first.into(),
            ]);
        }
        table
    }

    fn bits_table(&self, filter: &Filter) -> Table {
        let mut columns = vec!["offset", "section", "bit", "value", "p1", "bits"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        if filter.models {
            columns.extend((0..self.m).map(|j| format!("m{j}_p1")));
        }
        let mut table = Table {
            columns,
            rows: Vec::new(),
        };
        let r = &self.data.recording;
        for (i, s) in self.sections_in(filter) {
            for offset in s.start..s.end() {
                if !Self::in_range(filter, offset, offset + 1) {
                    continue;
                }
                for k in 0..8 {
                    let b = offset * 8 + k;
                    let bit = (r.input[offset] >> (7 - k)) & 1;
                    let mut row = vec![
                        offset.into(),
                        i.into(),
                        k.into(),
                        (bit as usize).into(),
                        Cell::Float(squash(r.preds[b]), 4),
                        Cell::Float(bit_cost(r.preds[b], bit), 3),
                    ];
                    if filter.models {
                        row.extend(
                            (0..self.m)
                                .map(|j| Cell::Float(squash(r.submodel_preds[b * self.m + j]), 4)),
                        );
                    }
                    table.rows.push(row);
                }
            }
        }
        table
    }

    /// Byte strings of at least `MIN` bytes that occurred before, found greedily from the start.
    fn repeats(&self, filter: &Filter) -> Vec<(Unit, usize)> {
        const MIN: usize = 8;
        let input = self.input();
        let mut last: HashMap<&[u8], usize> = HashMap::new();
        let mut repeats = Vec::new();
        let mut i = 0;
        while i + MIN <= input.len() {
            let key = &input[i..i + MIN];
            if let Some(&earlier) = last.get(key) {
                let mut len = MIN;
                while i + len < input.len() && input[earlier + len] == input[i + len] {
                    len += 1;
                }
                let section = self.section_of(i);
                let unit = Unit {
                    section,
                    start: i,
                    end: i + len,
                };
                if filter.section.map_or(true, |s| s == section)
                    && Self::in_range(filter, unit.start, unit.end)
                {
                    repeats.push((unit, earlier));
                }
                for k in i..i + len {
                    if k + MIN <= input.len() {
                        last.insert(&input[k..k + MIN], k);
                    }
                }
                i += len;
            } else {
                last.insert(key, i);
                i += 1;
            }
        }
        repeats
    }

    fn repeats_table(&self, filter: &Filter) -> Table {
        let repeats = self.repeats(filter);
        let earlier: HashMap<usize, usize> = repeats.iter().map(|(u, e)| (u.start, *e)).collect();
        let input = self.input();
        self.unit_table(
            filter,
            &["earlier_start", "text"],
            repeats.into_iter().map(|(u, _)| u).collect(),
            |u| {
                vec![
                    earlier[&u.start].into(),
                    escape(&input[u.start..u.end], 120).into(),
                ]
            },
        )
    }

    fn models_table(&self, filter: &Filter) -> Table {
        let mut table = Table::new(&[
            "model",
            "name",
            "bits",
            "bits_per_byte",
            "bits_over_actual",
            "best_bytes",
            "best_share",
        ]);
        let mut bits = vec![0.; self.m];
        let mut best = vec![0usize; self.m];
        let mut actual = 0.;
        let mut count = 0usize;
        for (_, s) in self.sections_in(filter) {
            for o in s.start..s.end() {
                if !Self::in_range(filter, o, o + 1) {
                    continue;
                }
                count += 1;
                actual += self.byte_costs[o];
                let costs = &self.model_byte_costs[o * self.m..(o + 1) * self.m];
                let mut best_j = 0;
                for j in 0..self.m {
                    bits[j] += costs[j] as f64;
                    if costs[j] < costs[best_j] {
                        best_j = j;
                    }
                }
                if self.m > 0 {
                    best[best_j] += 1;
                }
            }
        }
        table.push(vec![
            Cell::Empty,
            "actual (the whole model)".into(),
            Cell::bits(actual),
            Cell::Float(actual / count.max(1) as f64, 3),
            Cell::bits(0.),
            Cell::Empty,
            Cell::Empty,
        ]);
        for j in 0..self.m {
            table.push(vec![
                j.into(),
                self.data.model_names[j].as_str().into(),
                Cell::bits(bits[j]),
                Cell::Float(bits[j] / count.max(1) as f64, 3),
                Cell::bits(bits[j] - actual),
                best[j].into(),
                Cell::Float(best[j] as f64 / count.max(1) as f64, 3),
            ]);
        }
        table
    }

    fn top(&self, view: View, n: usize, kind: Option<&str>) -> Table {
        self.view(
            view,
            &Filter {
                top: Some(n),
                kind: kind.map(str::to_owned),
                code_only: kind == Some("ident"),
                ..Filter::default()
            },
        )
    }

    fn suggestions_table(&self) -> Table {
        let mut table = Table::new(&["kind", "bits", "start", "end", "message", "query"]);
        let mut add = |kind: &str,
                       bits: f64,
                       range: Option<(usize, usize)>,
                       message: String,
                       query: String| {
            let (start, end) =
                range.map_or((Cell::Empty, Cell::Empty), |(a, b)| (a.into(), b.into()));
            table.push(vec![
                kind.into(),
                Cell::bits(bits),
                start,
                end,
                message.into(),
                query.into(),
            ]);
        };

        // Files that cost more than storing them as they are.
        for (i, s) in self.data.sections.iter().enumerate() {
            if s.length == 0 {
                continue;
            }
            let bits = self.cost(s.start, s.end());
            if bits > 8. * s.length as f64 && s.kind == "file" {
                add(
                    "pre-compress",
                    bits - 8. * s.length as f64,
                    Some((s.start, s.end())),
                    format!(
                        "'{}' costs {:.0} bytes, more than its {} bytes stored as they are: pass it with --pre-compressed-file instead.",
                        s.name(), bits / 8., s.length
                    ),
                    format!("--view blocks --section {i} --top 20"),
                );
            }
        }

        let get = |row: &Vec<Cell>, table: &Table, column: &str| -> Cell {
            let i = table.columns.iter().position(|c| c == column).unwrap();
            row[i].clone()
        };
        let num = |cell: Cell| match cell {
            Cell::Int(v) => v as f64,
            Cell::Float(v, _) => v,
            _ => 0.,
        };
        let text = |cell: Cell| match cell {
            Cell::Str(s) => s,
            _ => String::new(),
        };

        // Original sources that cost the most.
        let sources = self.top(View::Sources, 5, None);
        for row in &sources.rows {
            let (path, bits, share, bpb) = (
                text(get(row, &sources, "path")),
                num(get(row, &sources, "bits")),
                num(get(row, &sources, "share")),
                num(get(row, &sources, "bits_per_byte")),
            );
            if share < 0.05 {
                continue;
            }
            if text(get(row, &sources, "source")).is_empty() {
                let first = num(get(row, &sources, "first_start")) as usize;
                add(
                    "source",
                    bits,
                    Some((first, first + 1)),
                    format!(
                        "{:.0} bytes that the source map or debug info does not cover cost {:.0} bytes, {:.0}% of the stream ({bpb:.2} bits per byte), starting at offset {first}. A plugin that changes code without a source map, e.g. one that minifies imported shaders, makes them.",
                        num(get(row, &sources, "length")),
                        bits / 8.,
                        share * 100.
                    ),
                    format!("--view lines --range {first}..{}", first + 200),
                );
                continue;
            }
            add(
                "source",
                bits,
                None,
                format!(
                    "The code from {path} costs {:.0} bytes, {:.0}% of the stream ({bpb:.2} bits per byte).",
                    bits / 8.,
                    share * 100.
                ),
                format!("--view source-lines --source {path} --top 30"),
            );
        }

        // Number literals with many digits.
        let numbers = self.top(View::TokenTotals, 50, Some("number"));
        let mut added = 0;
        for row in &numbers.rows {
            let (literal, count, bits) = (
                text(get(row, &numbers, "text")),
                num(get(row, &numbers, "count")),
                num(get(row, &numbers, "bits")),
            );
            let fraction_digits = literal
                .split_once('.')
                .map_or(0, |(_, f)| f.bytes().take_while(u8::is_ascii_digit).count());
            if fraction_digits < 3 || bits < 16. || added >= 5 {
                continue;
            }
            added += 1;
            add(
                "number",
                bits,
                Some((num(get(row, &numbers, "first_start")) as usize, num(get(row, &numbers, "first_start")) as usize + literal.len())),
                format!(
                    "Number `{literal}` ({}) costs {:.1} bytes. Check whether fewer digits are precise enough, or whether a shorter form (e.g. 1e-5) exists.",
                    times(count),
                    bits / 8.
                ),
                "--view token-totals --kind number --top 30".to_owned(),
            );
        }

        // Most expensive places.
        for unit in self.hotspots(&Filter::default()).into_iter().take(8) {
            let bits = self.cost(unit.start, unit.end);
            add(
                "hotspot",
                bits,
                Some((unit.start, unit.end)),
                format!(
                    "{} bytes at {}{} cost {:.1} bytes ({:.2} bits per byte): {}",
                    unit.end - unit.start,
                    self.place(&unit),
                    self.source_place(unit.start)
                        .map_or(String::new(), |p| format!(" (from {p})")),
                    bits / 8.,
                    bits / (unit.end - unit.start) as f64,
                    self.snippet(&unit)
                ),
                format!(
                    "--view {} --range {}..{}",
                    if self.data.sections[unit.section].text {
                        "tokens"
                    } else if self.wasm[unit.section].is_some() {
                        "wasm-instructions"
                    } else {
                        "bytes"
                    },
                    unit.start,
                    unit.end
                ),
            );
        }

        // Wasm files: custom sections, padded LEB128 numbers, the most expensive functions.
        for (i, s, module) in self.wasm_in(&Filter::default()) {
            // Only sections of compilers and tools: others may be read by the intro, with
            // WebAssembly.Module.customSections.
            let custom: Vec<_> = module
                .sections
                .iter()
                .filter(|w| w.custom && is_tool_section(&w.name))
                .collect();
            if !custom.is_empty() {
                let bits: f64 = custom
                    .iter()
                    .map(|w| self.cost(s.start + w.start, s.start + w.end))
                    .sum();
                let list: Vec<String> = custom
                    .iter()
                    .map(|w| format!("{} ({} bytes)", w.name, w.end - w.start))
                    .collect();
                add(
                    "wasm-custom",
                    bits,
                    Some((s.start + custom[0].start, s.start + custom[custom.len() - 1].end)),
                    format!(
                        "'{}' has custom sections of compilers and tools, which it does not need to run: {}. They cost {:.1} bytes. Strip them, e.g. with `wasm-opt --strip-debug --strip-producers --strip-target-features` (`wasm-strip` removes all custom sections, also those the intro reads), and give the unstripped file to the report with --report-wasm-debug.",
                        s.name(),
                        list.join(", "),
                        bits / 8.
                    ),
                    format!("--view wasm-sections --section {i}"),
                );
            }

            let (count, bytes, bits) = self.wasm_padding(i);
            if bytes > 0 {
                add(
                    "wasm-leb",
                    bits,
                    None,
                    format!(
                        "{count} LEB128 numbers in '{}' are longer than needed: {bytes} bytes of padding cost {:.1} bytes. wasm-ld pads relocated numbers, such as function indices of calls; optimizing with wasm-opt (e.g. -Oz) writes them as short as possible.",
                        s.name(),
                        bits / 8.
                    ),
                    format!("--view wasm-functions --section {i} --sort bits"),
                );
            }

            let file_bits = self.cost(s.start, s.end());
            let mut items: Vec<(usize, f64)> = module
                .items
                .iter()
                .enumerate()
                .map(|(k, it)| (k, self.cost(s.start + it.start, s.start + it.end)))
                .collect();
            items.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (k, bits) in items.into_iter().take(3) {
                let it = &module.items[k];
                if bits < 64. || bits < 0.05 * file_bits {
                    continue;
                }
                let (start, end) = (s.start + it.start, s.start + it.end);
                let from = self
                    .source_place(s.start + it.body_start.min(it.end.saturating_sub(1)))
                    .map_or(String::new(), |p| format!(", from {p}"));
                let instructions = module.instructions.iter().filter(|x| x.item == k).count();
                let what = if it.kind == ItemKind::Function {
                    format!("{instructions} instructions")
                } else {
                    "data".to_owned()
                };
                add(
                    "wasm-function",
                    bits,
                    Some((start, end)),
                    format!(
                        "{} of '{}' costs {:.1} bytes, {:.0}% of the file ({} bytes, {what}, {:.2} bits per byte{from}).",
                        self.item_label(i, k),
                        s.name(),
                        bits / 8.,
                        100. * bits / file_bits.max(1e-9),
                        end - start,
                        bits / (end - start).max(1) as f64,
                    ),
                    if it.kind == ItemKind::Function {
                        format!("--view wasm-instructions --range {start}..{end} --top 30")
                    } else {
                        format!("--view bytes --range {start}..{end}")
                    },
                );
            }
        }

        // Repetitions that the model does not predict well.
        let has_match = config_has(&self.data.config.model, |m| {
            matches!(m, ModelConfig::MatchPredictor { .. })
        });
        let repeats = self.top(View::Repeats, 5, None);
        for row in &repeats.rows {
            let (start, end, bits, bpb, earlier) = (
                num(get(row, &repeats, "start")) as usize,
                num(get(row, &repeats, "end")) as usize,
                num(get(row, &repeats, "bits")),
                num(get(row, &repeats, "bits_per_byte")),
                num(get(row, &repeats, "earlier_start")) as usize,
            );
            if bpb < 1. || bits < 32. {
                continue;
            }
            let hint = if has_match {
                "The match predictor does not catch it; a larger context or table may help."
            } else {
                "The config has no MatchPredictor; adding one (e.g. the 64k size profile) may help."
            };
            add(
                "repeat",
                bits,
                Some((start, end)),
                format!(
                    "{} bytes at {start} repeat the bytes at {earlier} but still cost {:.1} bytes ({bpb:.2} bits per byte). {hint}",
                    end - start,
                    bits / 8.
                ),
                format!("--view bytes --range {start}..{end}"),
            );
        }

        // Submodels that are rarely the best.
        if self.m > 0 {
            let models = self.models_table(&Filter::default());
            for row in models.rows.iter().skip(1) {
                let share = num(get(row, &models, "best_share"));
                if share < 0.01 {
                    add(
                        "model",
                        0.,
                        None,
                        format!(
                            "Submodel {} was the best for {:.1}% of the bytes. Removing it from the config may cost little and makes the decompressor smaller and faster; compress again to check.",
                            text(get(row, &models, "name")),
                            share * 100.
                        ),
                        "--view models".to_owned(),
                    );
                }
            }
        }

        table.sort_desc("bits");
        table
    }

    pub fn summary(&self, detail_files: &[(String, View, Vec<String>)]) -> Value {
        let n = self.input().len();
        let bits: f64 = self.byte_costs.iter().sum();
        let mut by_kind: HashMap<TokenKind, (usize, usize, f64)> = HashMap::new();
        for tokens in &self.tokens {
            for &(start, end, kind) in tokens {
                let e = by_kind.entry(kind).or_default();
                e.0 += 1;
                e.1 += end - start;
                e.2 += self.cost(start, end);
            }
        }
        let mut kinds: Vec<_> = by_kind.into_iter().collect();
        kinds.sort_by(|a, b| b.1 .2.total_cmp(&a.1 .2));
        let token_kinds: Vec<Value> = kinds
            .into_iter()
            .map(|(k, (count, length, bits))| {
                json!({
                    "kind": k.name(),
                    "count": count,
                    "length": length,
                    "bits": (bits * 100.).round() / 100.,
                    "bits_per_byte": (bits / length.max(1) as f64 * 1000.).round() / 1000.,
                })
            })
            .collect();

        let compressed = self
            .data
            .outputs
            .iter()
            .flat_map(|f| &f.parts)
            .find(|p| p.name == "compressed stream")
            .map(|p| p.length);
        let output_size: usize = self.data.outputs.iter().map(|f| f.size).sum();

        let models = if self.m > 0 {
            json!({
                "names": self.data.model_names,
                "summary": self.models_table(&Filter::default()).to_json(),
            })
        } else {
            json!({
                "names": [],
                "note": "Submodel predictions were not recorded. Compress with --report-models to get them.",
            })
        };

        json!({
            "about": [
                "Compression report of rootsqz for analysis. The compressed stream holds the main JavaScript and then each --file, in this order, as sections of one decompressed stream; all offsets (start, end, offset) are byte offsets in that decompressed stream, end is exclusive.",
                "The model predicts every bit. A bit with predicted probability p costs -log2(p) bits; bits is the sum of that over a range, cost_bytes = bits / 8. The costs add up to the size of the compressed stream, give or take a few bytes.",
                "The model learns from all bytes before a byte, also across sections, so a byte costs less if similar bytes came before. Reordering or changing earlier bytes changes the cost of later ones.",
                "encoded_start and encoded_end are positions in the compressed stream before and after a range was coded; arithmetic coding spreads bits over bytes, so they are approximate. Add the offset of the 'compressed stream' part in outputs to get a position in the output file.",
                "With --report-source-map, 'sources' and 'top.source_lines' tell which original source files and lines the bytes of the main JavaScript come from, and the views of byte ranges have the columns source and source_line (of their first byte). Debug info of wasm files (--report-wasm-debug) adds their source lines.",
                "'wasm' describes each wasm file: its sections, most expensive functions and data segments, opcodes, and the bytes its LEB128 numbers use more than needed (padding). 'errors' lists what could not be decoded; such a function ends in an instruction '(not decoded)'. 'wasm_debug' tells where the names and source lines of a wasm file came from. The views wasm-sections, wasm-functions, wasm-instructions and wasm-opcode-totals have the details.",
                "Submodel names are paths in config.model. With --report-models, 'models' tells what each submodel alone would have cost: a submodel can be useful in the mix even if it is bad alone.",
                "More detail: 'rootsqz report <this directory> --view <view>' prints a view as tab separated rows; see 'views' and 'rootsqz report --help' for filters (--section, --range, --kind, --top, --sort, --min-bits, --models). The suggestions have such a query each.",
            ],
            "rootsqz_version": env!("CARGO_PKG_VERSION"),
            "totals": {
                "input_bytes": n,
                "bits": (bits * 100.).round() / 100.,
                "cost_bytes": (bits / 8. * 100.).round() / 100.,
                "bits_per_byte": (bits / n.max(1) as f64 * 1000.).round() / 1000.,
                "compressed_stream_bytes": compressed,
                "output_bytes": output_size,
            },
            "outputs": self.data.outputs,
            "sections": self.sections_table(&Filter::default()).to_json(),
            "token_kinds": token_kinds,
            "source_map": self.data.source_mapping.as_ref().map(|m| json!({
                "maps": m.maps,
                "sources": m.sources.len(),
                "mapped_bytes": m.positions.iter().filter(|p| p.is_some()).count(),
            })),
            "wasm": self.wasm_summary(),
            "wasm_debug": self.data.wasm_debug,
            "sources": self.top(View::Sources, 30, None).to_json(),
            "suggestions": self.view(View::Suggestions, &Filter { top: Some(25), ..Filter::default() }).to_json(),
            "top": {
                "hotspots": self.top(View::Hotspots, 15, None).to_json(),
                "blocks": self.top(View::Blocks, 10, None).to_json(),
                "identifiers": self.top(View::TokenTotals, 15, Some("ident")).to_json(),
                "numbers": self.top(View::TokenTotals, 10, Some("number")).to_json(),
                "repeats": self.top(View::Repeats, 10, None).to_json(),
                "source_lines": self.top(View::SourceLines, 15, None).to_json(),
            },
            "models": models,
            "views": View::ALL.iter().map(|v| json!({ "view": v.name(), "description": v.description() })).collect::<Vec<_>>(),
            "detail_files": detail_files.iter().map(|(file, view, columns)| json!({
                "file": file,
                "view": view.name(),
                "columns": columns,
            })).collect::<Vec<_>>(),
            "config": self.data.config_json,
        })
    }

    fn wasm_summary(&self) -> Vec<Value> {
        self.wasm_in(&Filter::default())
            .map(|(i, s, module)| {
                let filter = |top: usize| Filter {
                    section: Some(i),
                    top: Some(top),
                    ..Filter::default()
                };
                let (count, bytes, bits) = self.wasm_padding(i);
                json!({
                    "section": i,
                    "name": s.name(),
                    "sections": self.view(View::WasmSections, &Filter { section: Some(i), ..Filter::default() }).to_json(),
                    "functions": self.view(View::WasmFunctions, &filter(20)).to_json(),
                    "opcodes": self.view(View::WasmOpcodeTotals, &filter(20)).to_json(),
                    "padding": {
                        "numbers": count,
                        "bytes": bytes,
                        "bits": (bits * 100.).round() / 100.,
                    },
                    "errors": module.errors,
                })
            })
            .collect()
    }

    /// What report.html needs to name the parts of each wasm section: offsets are in the stream.
    pub fn wasm_html(&self) -> Vec<WasmHtml> {
        self.wasm_in(&Filter::default())
            .map(|(i, s, module)| {
                let mut regions: Vec<(usize, usize, String)> = module
                    .sections
                    .iter()
                    .map(|ws| {
                        let name = if ws.custom {
                            format!("custom section {}", ws.name)
                        } else {
                            format!("{} section", ws.name)
                        };
                        (s.start + ws.start, s.start + ws.end, name)
                    })
                    .collect();
                regions.extend(module.items.iter().enumerate().map(|(k, it)| {
                    (s.start + it.start, s.start + it.end, self.item_label(i, k))
                }));
                let mut opcodes: Vec<String> = Vec::new();
                let mut index = HashMap::new();
                let mut instructions = Vec::with_capacity(module.instructions.len());
                for ins in &module.instructions {
                    let op = *index.entry(ins.op.clone()).or_insert_with(|| {
                        opcodes.push(ins.op.clone());
                        opcodes.len() - 1
                    });
                    instructions.push((s.start + ins.start, op as u16, ins.operands.clone()));
                }
                WasmHtml {
                    section: i,
                    regions,
                    opcodes,
                    instructions,
                    padded: module
                        .padded
                        .iter()
                        .map(|r| (s.start + r.start, s.start + r.end))
                        .collect(),
                }
            })
            .collect()
    }

    // Accessors for the HTML report.
    pub fn data(&self) -> &ReportData {
        self.data
    }

    pub fn byte_costs(&self) -> &[f64] {
        &self.byte_costs
    }

    pub fn model_byte_costs(&self) -> &[f32] {
        &self.model_byte_costs
    }
}

/// See [`Analysis::wasm_html`].
pub struct WasmHtml {
    pub section: usize,
    /// Sections, then functions and data segments: start, end, name.
    pub regions: Vec<(usize, usize, String)>,
    pub opcodes: Vec<String>,
    /// Start, index into `opcodes`, operands.
    pub instructions: Vec<(usize, u16, String)>,
    /// Numbers longer than they need to be.
    pub padded: Vec<(usize, usize)>,
}

/// Custom sections that compilers and tools write, not the program.
fn is_tool_section(name: &str) -> bool {
    matches!(
        name,
        "name"
            | "producers"
            | "target_features"
            | "sourceMappingURL"
            | "external_debug_info"
            | "linking"
            | "dylink"
            | "dylink.0"
    ) || name.starts_with(".debug_")
        || name.starts_with("reloc.")
}

fn config_has(config: &ModelConfig, f: impl Fn(&ModelConfig) -> bool + Copy) -> bool {
    f(config)
        || match config {
            ModelConfig::Mixer { models } | ModelConfig::ContextMixer { models, .. } => {
                models.iter().any(|m| config_has(m, f))
            }
            ModelConfig::AdaptiveProbabilityMap(m) => config_has(m, f),
            _ => false,
        }
}

fn times(count: f64) -> String {
    if count == 1. {
        "once".to_owned()
    } else {
        format!("{count} times")
    }
}

#[cfg(test)]
mod tests {
    use super::{tokenize, TokenKind::*};

    #[test]
    fn tokenize_js() {
        let text = b"a1=0x1F+1e-5*.5;  $b";
        let tokens: Vec<_> = tokenize(text, 10)
            .into_iter()
            .map(|(s, e, k)| (std::str::from_utf8(&text[s - 10..e - 10]).unwrap(), k))
            .collect();
        assert_eq!(
            tokens,
            [
                ("a1", Ident),
                ("=", Punct),
                ("0x1F", Number),
                ("+", Punct),
                ("1e-5", Number),
                ("*", Punct),
                (".5", Number),
                (";", Punct),
                ("  ", Space),
                ("$b", Ident),
            ]
        );
    }
}
