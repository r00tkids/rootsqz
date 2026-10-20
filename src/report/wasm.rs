//! The structure of WebAssembly files in the compressed stream, and their debug info.

use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use wasmparser::{KnownCustom, Name, Parser, Payload, TypeRef};

use super::source_map::{resolve_source, FileMapping, SourceFile, SourcePos};

pub fn is_wasm(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\0asm")
}

/// A section of a module: its id byte, size and contents. Offsets are in the file.
#[derive(Debug, Clone)]
pub struct WasmSection {
    /// `type`, `import`, ..., or the name of a custom section.
    pub name: String,
    pub custom: bool,
    /// Offset of the id byte.
    pub start: usize,
    pub end: usize,
    /// Bytes that the size of the section uses more than needed.
    pub padding: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Function,
    Data,
}

impl ItemKind {
    pub fn name(self) -> &'static str {
        match self {
            ItemKind::Function => "function",
            ItemKind::Data => "data",
        }
    }
}

/// A function body or a data segment.
#[derive(Debug, Clone)]
pub struct WasmItem {
    pub kind: ItemKind,
    /// Index in the function index space (imported functions first), or of the data segment.
    pub index: u32,
    /// The whole entry: for a function its size, locals and code.
    pub start: usize,
    pub end: usize,
    /// For a function, where the body starts after its size.
    pub body_start: usize,
    /// Bytes that the size of a function uses more than needed.
    pub padding: usize,
}

/// One instruction of a function body; the locals declaration counts as one, `(locals)`.
#[derive(Debug, Clone)]
pub struct Instruction {
    pub start: usize,
    pub end: usize,
    /// Index in `WasmModule::items`.
    pub item: usize,
    /// Opcode, e.g. `I32Const`, as wasmparser names it.
    pub op: String,
    pub operands: String,
    /// Bytes that the LEB128 numbers of the immediates use more than needed.
    pub padding: usize,
}

#[derive(Debug, Clone, Default)]
pub struct WasmModule {
    pub sections: Vec<WasmSection>,
    pub items: Vec<WasmItem>,
    pub instructions: Vec<Instruction>,
    /// Names from the module's own name section.
    pub function_names: BTreeMap<u32, String>,
    pub data_names: BTreeMap<u32, String>,
    pub imported_functions: u32,
    /// Offset of the contents of the code section; DWARF addresses are relative to it.
    pub code_start: Option<usize>,
    /// Custom sections by name: contents range.
    pub custom: Vec<(String, Range<usize>)>,
    /// Bytes of LEB128 numbers that are not needed: section sizes, function sizes, immediates.
    pub padded: Vec<Range<usize>>,
}

/// Length of the LEB128 number at the start of `bytes`, and how many of its bytes are not needed.
pub fn leb_padding(bytes: &[u8], signed: bool) -> Option<(usize, usize)> {
    let len = bytes.iter().take(10).position(|b| b & 0x80 == 0)? + 1;
    let mut padding = 0;
    let mut k = len;
    while k > 1 {
        let (last, prev) = (bytes[k - 1] & 0x7f, bytes[k - 2]);
        let redundant = if signed {
            (last == 0 && prev & 0x40 == 0) || (last == 0x7f && prev & 0x40 != 0)
        } else {
            last == 0
        };
        if !redundant {
            break;
        }
        padding += 1;
        k -= 1;
    }
    Some((len, padding))
}

/// The bytes of the LEB128 numbers in the instruction `bytes` that are not needed, as ranges.
fn instruction_padding(bytes: &[u8]) -> Vec<Range<usize>> {
    let Some(&op) = bytes.first() else {
        return Vec::new();
    };
    let mut padded = Vec::new();
    let mut add = |i: usize, (len, pad): (usize, usize)| {
        if pad > 0 {
            padded.push(i + len - pad..i + len);
        }
        i + len
    };
    let mut i = 1;
    match op {
        // Raw floats.
        0x43 | 0x44 => return padded,
        // SIMD: the opcode is a LEB128 number, then raw lanes may follow.
        0xfd => {
            if let Some(leb) = leb_padding(&bytes[1..], false) {
                add(1, leb);
            }
            return padded;
        }
        // Other prefixed opcodes: the opcode is a LEB128 number.
        0xfc | 0xfe => match leb_padding(&bytes[1..], false) {
            Some(leb) => i = add(1, leb),
            None => return padded,
        },
        _ => {}
    }
    // i32.const, i64.const and block types are signed; all other immediates unsigned.
    let mut signed = matches!(op, 0x41 | 0x42 | 0x02 | 0x03 | 0x04 | 0x06);
    while i < bytes.len() {
        match leb_padding(&bytes[i..], signed) {
            Some(leb) => i = add(i, leb),
            None => break,
        }
        signed = false;
    }
    padded
}

/// Short text of an operator: its name and its fields.
fn describe(op: &wasmparser::Operator) -> (String, String) {
    use wasmparser::Operator;
    match op {
        Operator::F32Const { value } => {
            return ("F32Const".to_owned(), format!("{}", f32::from_bits(value.bits())))
        }
        Operator::F64Const { value } => {
            return ("F64Const".to_owned(), format!("{}", f64::from_bits(value.bits())))
        }
        _ => {}
    }
    let mut debug = format!("{op:?}");
    // `memarg: MemArg { align: 2, max_align: 2, offset: 8, memory: 0 }` -> `align: 2, offset: 8`
    if let Some(at) = debug.find("memarg: MemArg { ") {
        if let Some(end) = debug[at..].find(" }") {
            let fields: Vec<&str> = debug[at + 17..at + end]
                .split(", ")
                .filter(|f| !f.starts_with("max_align") && *f != "memory: 0")
                .collect();
            debug = format!("{}{}{}", &debug[..at], fields.join(", "), &debug[at + end + 2..]);
        }
    }
    let split = debug.find([' ', '{', '(']).unwrap_or(debug.len());
    let name = debug[..split].to_owned();
    let mut operands = debug[split..]
        .trim()
        .trim_start_matches(['{', '('])
        .trim_end_matches(['}', ')'])
        .trim()
        .to_owned();
    if operands.len() > 80 {
        let mut cut = 77;
        while !operands.is_char_boundary(cut) {
            cut -= 1;
        }
        operands.truncate(cut);
        operands += "...";
    }
    (name, operands)
}

const SECTION_NAMES: [&str; 14] = [
    "custom", "type", "import", "function", "table", "memory", "global", "export", "start",
    "element", "code", "data", "datacount", "tag",
];

impl WasmModule {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if !is_wasm(bytes) || bytes.len() < 8 {
            bail!("not a WebAssembly module");
        }
        let mut module = WasmModule::default();

        // Section headers.
        let mut pos = 8;
        while pos < bytes.len() {
            let id = bytes[pos];
            let (len, padding) =
                leb_padding(&bytes[pos + 1..], false).context("truncated section header")?;
            let size = read_leb(&bytes[pos + 1..pos + 1 + len]) as usize;
            if padding > 0 {
                module.padded.push(pos + 1 + len - padding..pos + 1 + len);
            }
            let contents = pos + 1 + len;
            let end = contents + size;
            if end > bytes.len() {
                bail!("section at {pos} ends after the file");
            }
            let (name, custom) = if id == 0 {
                let (n_len, _) = leb_padding(&bytes[contents..end], false).unwrap_or((0, 0));
                let n = read_leb(&bytes[contents..contents + n_len]) as usize;
                let name_start = contents + n_len;
                let name = std::str::from_utf8(bytes.get(name_start..name_start + n).unwrap_or(&[]))
                    .unwrap_or("?")
                    .to_owned();
                module
                    .custom
                    .push((name.clone(), (name_start + n).min(end)..end));
                (name, true)
            } else {
                (
                    SECTION_NAMES
                        .get(id as usize)
                        .map_or_else(|| format!("unknown {id}"), |s| s.to_string()),
                    false,
                )
            };
            module.sections.push(WasmSection {
                name,
                custom,
                start: pos,
                end,
                padding,
            });
            pos = end;
        }

        // Functions, data segments and names.
        let mut functions = 0;
        let mut entry_start = 0;
        for payload in Parser::new(0).parse_all(bytes) {
            match payload? {
                Payload::ImportSection(reader) => {
                    for imports in reader {
                        let is_func = |ty: &TypeRef| matches!(ty, TypeRef::Func(_) | TypeRef::FuncExact(_));
                        match imports? {
                            wasmparser::Imports::Single(_, import) => {
                                module.imported_functions += is_func(&import.ty) as u32
                            }
                            wasmparser::Imports::Compact1 { items, .. } => {
                                for item in items {
                                    module.imported_functions += is_func(&item?.ty) as u32;
                                }
                            }
                            wasmparser::Imports::Compact2 { ty, names, .. } => {
                                if is_func(&ty) {
                                    module.imported_functions += names.count();
                                }
                            }
                        }
                    }
                }
                Payload::CodeSectionStart {
                    unchecked_range, ..
                } => {
                    let start = unchecked_range.start as usize;
                    module.code_start = Some(start);
                    let (count_len, _) = leb_padding(&bytes[start..], false).unwrap_or((0, 0));
                    entry_start = start + count_len;
                }
                Payload::CodeSectionEntry(body) => {
                    let range = body.range();
                    let (body_start, end) = (range.start as usize, range.end as usize);
                    let padding = leb_padding(&bytes[entry_start..body_start], false)
                        .map_or(0, |(_, p)| p);
                    if padding > 0 {
                        module.padded.push(body_start - padding..body_start);
                    }
                    let item = module.items.len();
                    module.items.push(WasmItem {
                        kind: ItemKind::Function,
                        index: module.imported_functions + functions,
                        start: entry_start,
                        end,
                        body_start,
                        padding,
                    });
                    functions += 1;
                    entry_start = end;

                    let mut reader = body.get_operators_reader()?;
                    let code_start = reader.original_position() as usize;
                    if code_start > body_start {
                        module.instructions.push(Instruction {
                            start: body_start,
                            end: code_start,
                            item,
                            op: "(locals)".to_owned(),
                            operands: String::new(),
                            padding: 0,
                        });
                    }
                    while !reader.eof() {
                        let (op, offset) = reader.read_with_offset()?;
                        let (start, end) = (offset as usize, reader.original_position() as usize);
                        let (name, operands) = describe(&op);
                        let padded = instruction_padding(&bytes[start..end]);
                        let padding = padded.iter().map(|r| r.len()).sum();
                        module
                            .padded
                            .extend(padded.into_iter().map(|r| start + r.start..start + r.end));
                        module.instructions.push(Instruction {
                            start,
                            end,
                            item,
                            op: name,
                            operands,
                            padding,
                        });
                    }
                }
                Payload::DataSection(reader) => {
                    for (index, data) in reader.into_iter().enumerate() {
                        let data = data?;
                        module.items.push(WasmItem {
                            kind: ItemKind::Data,
                            index: index as u32,
                            start: data.range.start as usize,
                            end: data.range.end as usize,
                            body_start: data.range.end as usize - data.data.len(),
                            padding: 0,
                        });
                    }
                }
                Payload::CustomSection(reader) => {
                    if let KnownCustom::Name(names) = reader.as_known() {
                        // A broken name section only loses the names.
                        let _ = read_names(names, &mut module);
                    }
                }
                _ => {}
            }
        }
        module.items.sort_by_key(|i| i.start);
        module.padded.sort_by_key(|r| r.start);
        Ok(module)
    }

    /// The item and instruction at `offset` (in the file).
    pub fn item_at(&self, offset: usize) -> Option<usize> {
        let i = self.items.partition_point(|it| it.start <= offset);
        (i > 0 && offset < self.items[i - 1].end).then(|| i - 1)
    }

    pub fn instruction_at(&self, offset: usize) -> Option<&Instruction> {
        let i = self.instructions.partition_point(|ins| ins.start <= offset);
        (i > 0 && offset < self.instructions[i - 1].end).then(|| &self.instructions[i - 1])
    }

    pub fn section_at(&self, offset: usize) -> Option<&WasmSection> {
        self.sections
            .iter()
            .find(|s| s.start <= offset && offset < s.end)
    }

    /// Bytes of the function `index` after its size.
    pub fn body(&self, bytes: &[u8], index: u32) -> Option<Range<usize>> {
        self.items
            .iter()
            .find(|i| i.kind == ItemKind::Function && i.index == index)
            .map(|i| i.body_start..i.end)
            .filter(|r| r.end <= bytes.len())
    }
}

fn read_names(names: wasmparser::NameSectionReader, module: &mut WasmModule) -> Result<()> {
    for name in names {
        match name? {
            Name::Function(map) => {
                for naming in map {
                    let naming = naming?;
                    module
                        .function_names
                        .insert(naming.index, naming.name.to_owned());
                }
            }
            Name::Data(map) => {
                for naming in map {
                    let naming = naming?;
                    module.data_names.insert(naming.index, naming.name.to_owned());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn read_leb(bytes: &[u8]) -> u64 {
    let mut value = 0u64;
    for (k, b) in bytes.iter().enumerate().take(10) {
        value |= ((b & 0x7f) as u64) << (7 * k);
    }
    value
}

/// A file that the debug info of a wasm file came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DebugFile {
    pub path: String,
    /// `debug wasm`, `source map` or `shipped` (debug info left in the file itself).
    pub kind: String,
    /// What it gave: e.g. `names`, `lines`.
    pub gave: Vec<String>,
}

/// Debug info of a wasm file in the stream, kept in report.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WasmDebug {
    /// Index of the section of the stream.
    pub section: usize,
    pub files: Vec<DebugFile>,
    pub function_names: BTreeMap<u32, String>,
    pub data_names: BTreeMap<u32, String>,
    /// Functions whose code differs from the debug wasm, so its info does not apply to them.
    pub unmatched_functions: Vec<u32>,
    /// Problems with the debug files that did not stop the report.
    pub notes: Vec<String>,
}

/// Debug files next to a wasm file: `x.debug.wasm` and `x.wasm.map` for `x.wasm`.
pub fn discover_debug_files(path: &Path) -> Vec<PathBuf> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Some(stem) = name.strip_suffix(".wasm") else {
        return Vec::new();
    };
    [format!("{stem}.debug.wasm"), format!("{name}.map")]
        .into_iter()
        .map(|n| path.with_file_name(n))
        .filter(|p| p.is_file())
        .collect()
}

/// Source positions of a wasm file from one debug file: the file, its kind and the positions.
pub type DebugMapping = (String, String, FileMapping);

/// Debug info for the wasm file `bytes` (section `section` of the stream) from `debug_files` and
/// from the file itself, and the source positions of its bytes. With `strict`, a debug file that
/// cannot be used is an error, otherwise a note.
pub fn debug_info(
    section: usize,
    bytes: &[u8],
    debug_files: &[PathBuf],
    strict: bool,
) -> Result<(WasmDebug, Vec<DebugMapping>)> {
    let shipped = WasmModule::parse(bytes)?;
    let mut debug = WasmDebug {
        section,
        ..WasmDebug::default()
    };
    let mut mappings = Vec::new();

    if !shipped.function_names.is_empty() || !shipped.data_names.is_empty() {
        debug.function_names = shipped.function_names.clone();
        debug.data_names = shipped.data_names.clone();
        debug.files.push(DebugFile {
            path: "(name section of the file)".to_owned(),
            kind: "shipped".to_owned(),
            gave: vec!["names".to_owned()],
        });
    }

    for path in debug_files {
        if let Err(err) = add_debug_file(&mut debug, &mut mappings, &shipped, bytes, path) {
            if strict {
                return Err(err);
            }
            debug.notes.push(format!("{err:#}"));
        }
    }

    // DWARF left in the file, if no debug file gave lines.
    if mappings.is_empty() && shipped.custom.iter().any(|(n, _)| n == ".debug_line") {
        let matched = match_functions(&shipped, bytes, &shipped, bytes);
        match dwarf_mapping(&shipped, bytes, &shipped, bytes, &matched) {
            Ok(Some(mapping)) => {
                let path = "(DWARF of the file)".to_owned();
                debug.files.push(DebugFile {
                    path: path.clone(),
                    kind: "shipped".to_owned(),
                    gave: vec![lines_gave(&mapping)],
                });
                mappings.push((path, "dwarf".to_owned(), mapping));
            }
            Ok(None) => {}
            Err(err) => debug.notes.push(format!("Failed to read the DWARF of the file: {err}")),
        }
    }
    debug.unmatched_functions.sort();
    debug.unmatched_functions.dedup();
    Ok((debug, mappings))
}

fn add_debug_file(
    debug: &mut WasmDebug,
    mappings: &mut Vec<DebugMapping>,
    shipped: &WasmModule,
    bytes: &[u8],
    path: &Path,
) -> Result<()> {
    let shown = path.display().to_string();
    if path.extension().is_some_and(|e| e == "map") {
        let part = FileMapping::read_wasm(path, bytes.len())?;
        debug.files.push(DebugFile {
            path: shown.clone(),
            kind: "source map".to_owned(),
            gave: vec![lines_gave(&part)],
        });
        mappings.push((shown, "wasm source map".to_owned(), part));
        return Ok(());
    }

    {
        let debug_bytes = std::fs::read(path)
            .with_context(|| format!("Failed to read the debug wasm: {}", path.display()))?;
        let module = WasmModule::parse(&debug_bytes)
            .with_context(|| format!("Failed to parse the debug wasm: {}", path.display()))?;
        let matched = match_functions(shipped, bytes, &module, &debug_bytes);
        let mut unmatched: Vec<u32> = matched
            .iter()
            .filter(|(_, m)| m.is_none())
            .map(|(&i, _)| i)
            .collect();
        unmatched.sort();
        let mut gave = Vec::new();
        let names = matched
            .iter()
            .filter(|(_, m)| m.is_some())
            .filter_map(|(i, _)| module.function_names.get(i).map(|n| (*i, n.clone())))
            .collect::<BTreeMap<_, _>>();
        if !names.is_empty() {
            gave.push(format!("names of {} functions", names.len()));
            debug.function_names.extend(names);
        }
        // Data segments keep their order; their names apply if there are as many.
        let data_count = |m: &WasmModule| m.items.iter().filter(|i| i.kind == ItemKind::Data).count();
        if !module.data_names.is_empty() && data_count(&module) == data_count(shipped) {
            gave.push(format!("names of {} data segments", module.data_names.len()));
            debug.data_names.extend(module.data_names.clone());
        }
        match dwarf_mapping(shipped, bytes, &module, &debug_bytes, &matched) {
            Ok(Some(mapping)) => {
                gave.push(lines_gave(&mapping));
                mappings.push((shown.clone(), "dwarf".to_owned(), mapping));
            }
            Ok(None) => {}
            Err(err) => debug
                .notes
                .push(format!("Failed to read the DWARF of {shown}: {err}")),
        }
        if !unmatched.is_empty() {
            debug.notes.push(format!(
                "{} of {} functions differ from {}, so its debug info does not apply to them: was the shipped file optimized after it, e.g. with wasm-opt?",
                unmatched.len(),
                matched.len(),
                shown
            ));
        }
        debug.unmatched_functions.extend(unmatched);
        debug.files.push(DebugFile {
            path: shown,
            kind: "debug wasm".to_owned(),
            gave,
        });
    }
    Ok(())
}

fn lines_gave(mapping: &FileMapping) -> String {
    let mapped = mapping.positions.iter().filter(|p| p.is_some()).count();
    format!("lines of {mapped} bytes")
}

/// Source positions of the shipped file from the DWARF line info of `debug` (which can be the
/// shipped file itself), through the functions whose code is the same. `None` without line info.
fn dwarf_mapping(
    shipped: &WasmModule,
    bytes: &[u8],
    debug: &WasmModule,
    debug_bytes: &[u8],
    matched: &BTreeMap<u32, Option<usize>>,
) -> Result<Option<FileMapping>> {
    let Some(code_start) = debug.code_start else {
        return Ok(None);
    };
    let rows = dwarf_lines(debug, debug_bytes)?;
    if rows.is_empty() {
        return Ok(None);
    }
    // Matched function bodies: start and end in the debug file's code addresses, shipped start.
    let mut bodies: Vec<(u64, u64, usize)> = shipped
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Function)
        .filter_map(|item| {
            let start = (*matched.get(&item.index)?)?;
            let address = (start - code_start) as u64;
            Some((address, address + (item.end - item.body_start) as u64, item.body_start))
        })
        .collect();
    bodies.sort();

    let cwd = std::env::current_dir().ok();
    let mut sources: Vec<SourceFile> = Vec::new();
    let mut source_index: HashMap<String, u32> = HashMap::new();
    let mut positions = vec![None; bytes.len()];
    for row in rows {
        let k = bodies.partition_point(|b| b.1 <= row.start);
        let Some(&(body_start, body_end, shipped_start)) = bodies.get(k) else {
            continue;
        };
        let (start, end) = (row.start.max(body_start), row.end.min(body_end));
        if start >= end {
            continue;
        }
        let source = *source_index.entry(row.file.clone()).or_insert_with(|| {
            sources.push(SourceFile {
                path: resolve_source(&row.file, None, Path::new(""), cwd.as_deref()),
                content: None,
            });
            sources.len() as u32 - 1
        });
        let at = |a: u64| shipped_start + (a - body_start) as usize;
        positions[at(start)..at(end)].fill(Some(SourcePos {
            source,
            line: row.line,
            column: row.column,
        }));
    }
    Ok(Some(FileMapping { sources, positions }))
}

/// A row of a DWARF line program: code addresses `start..end` (relative to the contents of the
/// code section) come from a line of a file.
struct LineRow {
    start: u64,
    end: u64,
    file: String,
    line: u32,
    column: u32,
}

fn dwarf_lines(module: &WasmModule, bytes: &[u8]) -> Result<Vec<LineRow>> {
    let dwarf = gimli::Dwarf::load(|id: gimli::SectionId| -> Result<_, gimli::Error> {
        let data = module
            .custom
            .iter()
            .find(|(name, _)| name == id.name())
            .map_or(&[][..], |(_, r)| &bytes[r.clone()]);
        Ok(gimli::EndianSlice::new(data, gimli::LittleEndian))
    })?;
    let mut out = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next()? {
        let unit = dwarf.unit(header)?;
        let Some(program) = unit.line_program.clone() else {
            continue;
        };
        let comp_dir = unit
            .comp_dir
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut paths: HashMap<u64, String> = HashMap::new();
        let mut rows = program.rows();
        let mut previous: Option<LineRow> = None;
        while let Some((header, row)) = rows.next_row()? {
            if let Some(mut p) = previous.take() {
                if row.address() > p.start {
                    p.end = row.address();
                    out.push(p);
                }
            }
            if row.end_sequence() {
                continue;
            }
            let Some(line) = row.line() else { continue };
            let file = match paths.get(&row.file_index()) {
                Some(path) => path.clone(),
                None => {
                    let Some(entry) = row.file(header) else { continue };
                    let mut path = PathBuf::from(&comp_dir);
                    if let Some(dir) = entry.directory(header) {
                        path.push(dwarf.attr_string(&unit, dir)?.to_string_lossy().as_ref());
                    }
                    path.push(dwarf.attr_string(&unit, entry.path_name())?.to_string_lossy().as_ref());
                    let path = path.to_string_lossy().into_owned();
                    paths.insert(row.file_index(), path.clone());
                    path
                }
            };
            let column = match row.column() {
                gimli::ColumnType::LeftEdge => 1,
                gimli::ColumnType::Column(c) => c.get() as u32,
            };
            previous = Some(LineRow {
                start: row.address(),
                end: 0,
                file,
                line: line.get() as u32,
                column,
            });
        }
    }
    Ok(out)
}

/// For each function of the shipped module: the start of the same body in the debug module, if
/// the bytes are the same.
fn match_functions(
    shipped: &WasmModule,
    bytes: &[u8],
    debug: &WasmModule,
    debug_bytes: &[u8],
) -> BTreeMap<u32, Option<usize>> {
    shipped
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Function)
        .map(|item| {
            let body = &bytes[item.body_start..item.end];
            let found = debug
                .body(debug_bytes, item.index)
                .filter(|r| &debug_bytes[r.clone()] == body)
                .map(|r| r.start);
            (item.index, found)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new("tests/wasm").join(name)).unwrap()
    }

    #[test]
    fn padding() {
        assert_eq!(leb_padding(&[0x05], false), Some((1, 0)));
        assert_eq!(leb_padding(&[0x85, 0x80, 0x80, 0x80, 0x00], false), Some((5, 4)));
        assert_eq!(leb_padding(&[0x80, 0x01], false), Some((2, 0)));
        // Signed: 64 needs two bytes, -1 one.
        assert_eq!(leb_padding(&[0xc0, 0x00], true), Some((2, 0)));
        assert_eq!(leb_padding(&[0xff, 0x7f], true), Some((2, 1)));
        assert_eq!(leb_padding(&[0x81, 0x80, 0x00], true), Some((3, 2)));
        // call 5, padded to five bytes as wasm-ld does.
        assert_eq!(instruction_padding(&[0x10, 0x85, 0x80, 0x80, 0x80, 0x00]), [2..6]);
        assert!(instruction_padding(&[0x43, 0x00, 0x00, 0x80, 0x00]).is_empty());
        // i32.load with a padded offset: align 2, offset 8.
        assert_eq!(instruction_padding(&[0x28, 0x02, 0x88, 0x80, 0x00]), [3..5]);
    }

    #[test]
    fn structure() {
        let bytes = fixture("fixture.debug.wasm");
        let m = WasmModule::parse(&bytes).unwrap();
        let names: Vec<_> = m.sections.iter().map(|s| s.name.as_str()).collect();
        assert!(names.starts_with(&["type", "function", "table", "memory", "global", "export", "code", "data"]));
        assert!(names.contains(&".debug_line") && names.contains(&"name"));
        assert_eq!(m.sections.last().unwrap().end, bytes.len());
        // The sections cover the file after the header.
        assert_eq!(m.sections[0].start, 8);
        for w in m.sections.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
        let funcs: Vec<_> = m.items.iter().filter(|i| i.kind == ItemKind::Function).collect();
        assert!(!funcs.is_empty());
        assert!(m.items.iter().any(|i| i.kind == ItemKind::Data));
        let named: Vec<_> = funcs.iter().filter_map(|f| m.function_names.get(&f.index)).collect();
        assert!(named.iter().any(|n| *n == "render"), "{named:?}");
        // Instructions cover each function body.
        for (i, f) in m.items.iter().enumerate().filter(|(_, i)| i.kind == ItemKind::Function) {
            let ins: Vec<_> = m.instructions.iter().filter(|x| x.item == i).collect();
            assert_eq!(ins.first().unwrap().start, f.body_start);
            assert_eq!(ins.last().unwrap().end, f.end);
            assert_eq!(ins.last().unwrap().op, "End");
        }
        assert!(m.instructions.iter().any(|i| i.op == "F32Mul"));
    }

    #[test]
    fn debug_wasm_names() {
        let shipped = fixture("fixture.wasm");
        let (debug, mappings) =
            debug_info(1, &shipped, &[PathBuf::from("tests/wasm/fixture.debug.wasm")], true).unwrap();
        assert!(debug.unmatched_functions.is_empty(), "{debug:?}");
        assert!(debug.function_names.values().any(|n| n == "render"));
        assert_eq!(debug.files[0].kind, "debug wasm");

        // DWARF lines, through the matched functions: render is at lines 13 to 18 of fixture.c
        // (with wave inlined, lines 7 to 10), mix at 21 to 23.
        assert_eq!(mappings.len(), 1);
        let (_, kind, mapping) = &mappings[0];
        assert_eq!(kind, "dwarf");
        assert_eq!(mapping.sources.len(), 1);
        assert!(mapping.sources[0].path.ends_with("fixture.c"), "{:?}", mapping.sources);
        let m = WasmModule::parse(&shipped).unwrap();
        let lines = |index: u32| -> Vec<u32> {
            let f = m.items.iter().find(|i| i.kind == ItemKind::Function && i.index == index).unwrap();
            mapping.positions[f.body_start..f.end].iter().flatten().map(|p| p.line).collect()
        };
        let render = lines(0);
        assert!(render.contains(&16) && render.iter().all(|l| (7..=18).contains(l)), "{render:?}");
        let mix = lines(1);
        assert!(!mix.is_empty() && mix.iter().all(|l| (21..=23).contains(l)), "{mix:?}");
        assert!(mapping.positions.iter().flatten().all(|p| p.column >= 1));
        // Nothing outside the code.
        let code = m.items.iter().filter(|i| i.kind == ItemKind::Function);
        let in_code: usize = code.map(|f| mapping.positions[f.body_start..f.end].iter().flatten().count()).sum();
        assert_eq!(in_code, mapping.positions.iter().flatten().count());

        // A function whose code differs gets no name.
        let mut changed = shipped.clone();
        let m = WasmModule::parse(&shipped).unwrap();
        let f = m.items.iter().find(|i| i.kind == ItemKind::Function).unwrap();
        let ins = m
            .instructions
            .iter()
            .find(|i| i.start >= f.body_start && i.op == "F32Const")
            .unwrap();
        changed[ins.end - 1] ^= 0x40;
        let (debug, mappings) =
            debug_info(1, &changed, &[PathBuf::from("tests/wasm/fixture.debug.wasm")], true).unwrap();
        assert_eq!(debug.unmatched_functions, [f.index]);
        assert!(mappings[0].2.positions[f.body_start..f.end].iter().all(Option::is_none));
        assert!(!debug.function_names.contains_key(&f.index));
        assert_eq!(debug.notes.len(), 1);

        // A missing debug file is a note unless it was asked for.
        let missing = [PathBuf::from("tests/wasm/missing.debug.wasm")];
        assert!(debug_info(1, &shipped, &missing, true).is_err());
        let (debug, _) = debug_info(1, &shipped, &missing, false).unwrap();
        assert_eq!(debug.notes.len(), 1);
    }

    #[test]
    fn shipped_dwarf() {
        // A file that still has its debug info needs no debug file.
        let bytes = fixture("fixture.debug.wasm");
        let (debug, mappings) = debug_info(1, &bytes, &[], true).unwrap();
        assert!(debug.function_names.values().any(|n| n == "render"));
        let kinds: Vec<_> = debug.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(kinds, ["(name section of the file)", "(DWARF of the file)"]);
        assert_eq!(mappings.len(), 1);
        assert!(mappings[0].2.positions.iter().flatten().any(|p| p.line == 16));
    }

    #[test]
    fn wasm_source_map() {
        let shipped = fixture("fixture.wasm");
        let m = WasmModule::parse(&shipped).unwrap();
        let f = m.items.iter().find(|i| i.kind == ItemKind::Function).unwrap();
        // One mapping at the start of the first function body: fixture.c line 15.
        let vlq = |v: i64| {
            let mut v = if v < 0 { ((-v) << 1) | 1 } else { v << 1 };
            let mut out = String::new();
            loop {
                let mut digit = v & 31;
                v >>= 5;
                if v > 0 {
                    digit |= 32;
                }
                out.push(
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[digit as usize]
                        as char,
                );
                if v == 0 {
                    return out;
                }
            }
        };
        let mappings = format!("{}AcA", vlq(f.body_start as i64));
        let dir = Path::new("testout/wasm_map");
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("fixture.wasm.map");
        std::fs::write(
            &path,
            format!(r#"{{"version":3,"sources":["fixture.c"],"mappings":"{mappings}"}}"#),
        )
        .unwrap();
        let (debug, mappings) = debug_info(1, &shipped, &[path], true).unwrap();
        assert_eq!(debug.files[0].kind, "source map");
        let (_, kind, mapping) = &mappings[0];
        assert_eq!(kind, "wasm source map");
        assert_eq!(mapping.positions[f.body_start - 1], None);
        assert_eq!(mapping.positions[f.body_start].unwrap().line, 15);
        assert_eq!(mapping.positions[shipped.len() - 1].unwrap().line, 15);
    }

    #[test]
    fn discovery() {
        let found = discover_debug_files(Path::new("tests/wasm/fixture.wasm"));
        assert_eq!(found, [PathBuf::from("tests/wasm/fixture.debug.wasm")]);
        assert!(discover_debug_files(Path::new("tests/wasm/fixture.c")).is_empty());
    }
}
