//! Maps the bytes of the stream to positions in their original sources: the main JavaScript with
//! its source map, wasm files with theirs or with their debug info.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

/// An original source file named by the source map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFile {
    /// Path relative to the working directory if possible, otherwise as the source map names it.
    pub path: String,
    /// Content from `sourcesContent`, if the source map has it.
    pub content: Option<String>,
}

/// Position in an original source: index in `sources`, 1-based line and column.
/// Column 0 means that the generated line is the same as the source line, so a byte is at
/// the same column in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePos {
    pub source: u32,
    pub line: u32,
    pub column: u32,
}

/// Where each byte of one file came from.
#[derive(Debug, Clone, Default)]
pub struct FileMapping {
    pub sources: Vec<SourceFile>,
    /// One per byte of the file; `None` where nothing maps it.
    pub positions: Vec<Option<SourcePos>>,
}

/// A file that source positions came from, e.g. a source map.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MapFile {
    /// Index of the section of the stream that it maps.
    pub section: usize,
    pub file: String,
    /// `source map`, `wasm source map` or `dwarf`.
    pub kind: String,
}

/// Where each byte of the stream came from.
#[derive(Debug, Clone)]
pub struct SourceMapping {
    pub maps: Vec<MapFile>,
    pub sources: Vec<SourceFile>,
    /// One per byte of the stream; `None` where nothing maps it.
    pub positions: Vec<Option<SourcePos>>,
}

impl SourceMapping {
    /// No mappings yet, for a stream of `len` bytes.
    pub fn new(len: usize) -> Self {
        Self {
            maps: Vec::new(),
            sources: Vec::new(),
            positions: vec![None; len],
        }
    }

    /// Adds the mapping of the file at `start` in the stream. Sources with the same path are
    /// merged; sources without content get it from the file if it can be read.
    pub fn add(&mut self, start: usize, map: MapFile, part: FileMapping) {
        let index: Vec<u32> = part
            .sources
            .into_iter()
            .map(|source| {
                let found = self.sources.iter().position(|s| s.path == source.path);
                let i = found.unwrap_or_else(|| {
                    self.sources.push(SourceFile {
                        path: source.path.clone(),
                        content: None,
                    });
                    self.sources.len() - 1
                });
                if self.sources[i].content.is_none() {
                    self.sources[i].content = source.content.or_else(|| read_source(&source.path));
                }
                i as u32
            })
            .collect();
        for (k, pos) in part.positions.into_iter().enumerate() {
            if let (Some(pos), Some(slot)) = (pos, self.positions.get_mut(start + k)) {
                *slot = Some(SourcePos {
                    source: index[pos.source as usize],
                    ..pos
                });
            }
        }
        self.maps.push(map);
    }
}

/// The content of a source file that the source map does not include, if it is a readable text
/// file of a sensible size.
fn read_source(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 4 << 20 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSourceMap {
    version: u32,
    #[serde(default)]
    source_root: Option<String>,
    #[serde(default)]
    sources: Vec<Option<String>>,
    #[serde(default)]
    sources_content: Vec<Option<String>>,
    #[serde(default)]
    mappings: String,
    #[serde(default)]
    sections: Option<serde_json::Value>,
}

impl FileMapping {
    /// Reads the source map at `map_path` for the JavaScript `js`.
    pub fn read(map_path: &Path, js: &[u8]) -> Result<Self> {
        let text = std::fs::read_to_string(map_path)
            .with_context(|| format!("Failed to read source map: {}", map_path.display()))?;
        Self::parse(&text, map_path, js)
            .with_context(|| format!("Failed to parse source map: {}", map_path.display()))
    }

    /// Reads the source map at `map_path` of a wasm file of `len` bytes, as written by e.g.
    /// AssemblyScript or emscripten: its columns are byte offsets in the file.
    pub fn read_wasm(map_path: &Path, len: usize) -> Result<Self> {
        let text = std::fs::read_to_string(map_path)
            .with_context(|| format!("Failed to read source map: {}", map_path.display()))?;
        Self::parse_wasm(&text, map_path, len)
            .with_context(|| format!("Failed to parse source map: {}", map_path.display()))
    }

    fn parse_wasm(text: &str, map_path: &Path, len: usize) -> Result<Self> {
        let (raw, sources) = parse_raw(text, map_path)?;
        let lines = decode_mappings(&raw.mappings)?;
        ensure!(
            lines.len() <= 1,
            "it has {} lines of mappings, a wasm source map has one",
            lines.len()
        );
        let segments = lines.into_iter().next().unwrap_or_default();
        let mut positions = vec![None; len];
        for (k, segment) in segments.iter().enumerate() {
            let start = (segment.column as usize).min(len);
            let end = segments
                .get(k + 1)
                .map_or(len, |next| (next.column as usize).min(len));
            let pos = segment.source.map(|(source, l, c)| SourcePos {
                source,
                line: l + 1,
                column: c + 1,
            });
            positions[start..end].fill(pos);
        }
        check_sources(&positions, sources.len())?;
        Ok(Self { sources, positions })
    }

    fn parse(text: &str, map_path: &Path, js: &[u8]) -> Result<Self> {
        let (raw, sources) = parse_raw(text, map_path)?;
        let segments = decode_mappings(&raw.mappings)?;
        let source_lines: Vec<Option<Vec<&str>>> = sources
            .iter()
            .map(|s| s.content.as_deref().map(|c| c.lines().collect()))
            .collect();
        let mut positions = vec![None; js.len()];
        let mut line_start = 0;
        let mut carried: Option<Carried> = None;
        for (line, line_bytes) in js.split(|&b| b == b'\n').enumerate() {
            let segments: &[Segment] = segments.get(line).map_or(&[], |s| s.as_slice());

            // Bytes before the first mapping of a line, e.g. the lines of a multi-line string.
            let mut current = carried
                .as_mut()
                .and_then(|c| c.continue_to(line, line_bytes, &source_lines));

            // Source map columns count UTF-16 code units.
            let mut column = 0u32;
            let mut next = 0;
            let mut i = 0;
            while i < line_bytes.len() {
                let char_len = match line_bytes[i] {
                    0x00..=0x7f => 1,
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xff => 4,
                    _ => 1,
                }
                .min(line_bytes.len() - i);
                while next < segments.len() && segments[next].column <= column {
                    current = segments[next].source.map(|(source, l, c)| SourcePos {
                        source,
                        line: l + 1,
                        column: c + 1,
                    });
                    next += 1;
                }
                for k in 0..char_len {
                    positions[line_start + i + k] = current;
                }
                column += if char_len == 4 { 2 } else { 1 };
                i += char_len;
            }
            // The newline belongs to the last mapping of the line.
            if line_start + line_bytes.len() < js.len() {
                positions[line_start + line_bytes.len()] = current;
            }
            if let Some(last) = segments.last() {
                carried = last.source.map(|(source, l, c)| Carried {
                    source,
                    line: l,
                    column: c,
                    generated_line: line,
                    next_search: l as usize,
                    matched: false,
                });
            }
            line_start += line_bytes.len() + 1;
        }
        check_sources(&positions, sources.len())?;
        Ok(Self { sources, positions })
    }
}

fn parse_raw(text: &str, map_path: &Path) -> Result<(RawSourceMap, Vec<SourceFile>)> {
    let raw: RawSourceMap = serde_json::from_str(text)?;
    ensure!(raw.version == 3, "only version 3 source maps are supported");
    if raw.sections.is_some() {
        bail!("index source maps (with \"sections\") are not supported");
    }
    let base = map_path.parent().unwrap_or(Path::new(""));
    let cwd = std::env::current_dir().ok();
    let sources = raw
        .sources
        .iter()
        .enumerate()
        .map(|(i, source)| SourceFile {
            path: resolve_source(
                source.as_deref().unwrap_or(""),
                raw.source_root.as_deref(),
                base,
                cwd.as_deref(),
            ),
            content: raw.sources_content.get(i).cloned().flatten(),
        })
        .collect::<Vec<_>>();
    Ok((raw, sources))
}

fn check_sources(positions: &[Option<SourcePos>], sources: usize) -> Result<()> {
    for pos in positions.iter().flatten() {
        ensure!(
            (pos.source as usize) < sources,
            "a mapping refers to source {}, but there are only {}",
            pos.source,
            sources
        );
    }
    Ok(())
}

/// The last mapping of an earlier generated line, for the lines after it that have no mapping
/// at their start, such as the lines of a multi-line string.
struct Carried {
    source: u32,
    /// Line and column of the mapping, 0-based.
    line: u32,
    column: u32,
    generated_line: usize,
    /// Source line to search the next generated line from.
    next_search: usize,
    /// Whether an earlier generated line was found in the source.
    matched: bool,
}

/// A line without whitespace and `//` comments, to find minified lines in their source.
fn normalize(line: &str) -> String {
    let code = line.find("//").map_or(line, |i| &line[..i]);
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

impl Carried {
    /// Where the bytes at the start of `generated_line` come from:
    /// - the source line at the same distance from the mapping, if it is the same line, as in a
    ///   multi-line template literal;
    /// - otherwise the next source line that is the same without whitespace and comments, as in a
    ///   minified copy of the source;
    /// - otherwise the line found last, or the start of the source if the mapping is at the start
    ///   of it, as for a whole imported file;
    /// - otherwise nothing, because the line may belong to code without a mapping.
    fn continue_to(
        &mut self,
        generated_line: usize,
        line_bytes: &[u8],
        source_lines: &[Option<Vec<&str>>],
    ) -> Option<SourcePos> {
        let lines = source_lines
            .get(self.source as usize)
            .and_then(|l| l.as_ref());
        let pos = |line: usize, column: u32| SourcePos {
            source: self.source,
            line: line as u32 + 1,
            column,
        };
        if let Some(lines) = lines {
            let estimate = self.line as usize + (generated_line - self.generated_line);
            let same = lines
                .get(estimate)
                .is_some_and(|text| text.trim_end_matches('\r').as_bytes() == line_bytes);
            if same {
                self.next_search = estimate + 1;
                self.matched = true;
                return Some(pos(estimate, 0));
            }
            let wanted = normalize(&String::from_utf8_lossy(line_bytes));
            if wanted.len() >= 4 {
                let end = (self.next_search + 400).min(lines.len());
                if let Some(found) =
                    (self.next_search..end).find(|&k| normalize(lines[k]) == wanted)
                {
                    self.next_search = found + 1;
                    self.matched = true;
                    return Some(pos(found, 1));
                }
            }
            if self.matched {
                return Some(pos(self.next_search.saturating_sub(1), 1));
            }
        }
        (self.line == 0 && self.column == 0).then(|| pos(0, 1))
    }
}

struct Segment {
    column: u32,
    /// Source index, line and column, all 0-based.
    source: Option<(u32, u32, u32)>,
}

fn decode_mappings(mappings: &str) -> Result<Vec<Vec<Segment>>> {
    let mut lines = Vec::new();
    let (mut source, mut source_line, mut source_column) = (0i64, 0i64, 0i64);
    for line in mappings.split(';') {
        let mut segments = Vec::new();
        let mut column = 0i64;
        for segment in line.split(',').filter(|s| !s.is_empty()) {
            let fields = decode_vlq(segment)?;
            ensure!(
                matches!(fields.len(), 1 | 4 | 5),
                "a segment has {} fields",
                fields.len()
            );
            column += fields[0];
            let mapped = if fields.len() >= 4 {
                source += fields[1];
                source_line += fields[2];
                source_column += fields[3];
                ensure!(
                    source >= 0 && source_line >= 0 && source_column >= 0,
                    "negative position in mappings"
                );
                Some((source as u32, source_line as u32, source_column as u32))
            } else {
                None
            };
            ensure!(column >= 0, "negative column in mappings");
            segments.push(Segment {
                column: column as u32,
                source: mapped,
            });
        }
        segments.sort_by_key(|s| s.column);
        lines.push(segments);
    }
    Ok(lines)
}

fn decode_vlq(segment: &str) -> Result<Vec<i64>> {
    let mut values = Vec::new();
    let (mut value, mut shift) = (0i64, 0);
    for c in segment.bytes() {
        let digit = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("invalid character '{}' in mappings", c as char),
        } as i64;
        value += (digit & 31) << shift;
        if digit & 32 != 0 {
            shift += 5;
            ensure!(shift < 60, "too long number in mappings");
        } else {
            values.push(if value & 1 == 1 {
                -(value >> 1)
            } else {
                value >> 1
            });
            value = 0;
            shift = 0;
        }
    }
    ensure!(shift == 0, "unfinished number in mappings");
    Ok(values)
}

/// Path of a source: relative to the source map and its source root, then relative to `cwd` if inside it.
fn resolve_source(
    source: &str,
    source_root: Option<&str>,
    base: &Path,
    cwd: Option<&Path>,
) -> String {
    if source.contains("://") || source.starts_with('\0') {
        return source.to_owned();
    }
    let mut path = PathBuf::from(base);
    if let Some(root) = source_root.filter(|r| !r.is_empty() && !r.contains("://")) {
        path.push(root);
    }
    path.push(source);
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(normal.components().next_back(), Some(Component::Normal(_))) {
                    normal.pop();
                } else {
                    normal.push("..");
                }
            }
            other => normal.push(other),
        }
    }
    match cwd {
        Some(cwd) if normal.is_absolute() => relative_to(&normal, cwd).display().to_string(),
        _ => normal.display().to_string(),
    }
}

/// `path` relative to `base`, with `..` where needed. Both are absolute and normalized.
fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let path: Vec<_> = path.components().collect();
    let base: Vec<_> = base.components().collect();
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return path.iter().collect();
    }
    let mut relative = PathBuf::new();
    for _ in common..base.len() {
        relative.push("..");
    }
    relative.extend(&path[common..]);
    relative
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{decode_vlq, relative_to, FileMapping, MapFile, SourceFile, SourceMapping, SourcePos};

    #[test]
    fn relative_paths() {
        let rel = |p: &str, b: &str| {
            relative_to(Path::new(p), Path::new(b))
                .display()
                .to_string()
        };
        assert_eq!(rel("/a/b/c.ts", "/a/b"), "c.ts");
        assert_eq!(rel("/a/x/c.ts", "/a/b/d"), "../../x/c.ts");
    }

    #[test]
    fn template_literal_lines() {
        // Line 1 maps "f(`" to t.ts line 1. The literal is the same as in t.ts on lines 2 and 4,
        // minified on line 3, and changed on line 5, which is too short to search for.
        let map = r#"{"version":3,"sources":["t.ts"],
            "sourcesContent":["f(`\nstruct A\n  let  y = 1; // c\n}\n`)"],"mappings":"AAAA"}"#;
        let js = "f(`\nstruct A\nlet y=1;\n}\n`);".as_bytes();
        let m = FileMapping::parse(map, Path::new("t.js.map"), js).unwrap();
        let pos = |line, column| {
            Some(SourcePos {
                source: 0,
                line,
                column,
            })
        };
        assert_eq!(m.positions[4], pos(2, 0), "same line");
        assert_eq!(
            m.positions[13],
            pos(3, 1),
            "same without whitespace and comments"
        );
        assert_eq!(m.positions[22], pos(4, 0), "same line");
        assert_eq!(
            m.positions[24],
            pos(4, 1),
            "short line: the line found last"
        );
    }

    #[test]
    fn code_without_mapping() {
        // Line 2 is code that the source map does not cover, e.g. an imported file without a map.
        let map = r#"{"version":3,"sources":["t.ts"],"sourcesContent":["a();\n}"],"mappings":"AAAA,IACA"}"#;
        let js = "a();}\nconst shader = 1;".as_bytes();
        let m = FileMapping::parse(map, Path::new("t.js.map"), js).unwrap();
        assert_eq!(m.positions[6], None);
    }

    #[test]
    fn vlq() {
        assert_eq!(decode_vlq("AAAA").unwrap(), [0, 0, 0, 0]);
        assert_eq!(decode_vlq("SAAQ").unwrap(), [9, 0, 0, 8]);
        assert_eq!(decode_vlq("D").unwrap(), [-1]);
        assert_eq!(decode_vlq("2HAAC").unwrap(), [123, 0, 0, 1]);
    }

    #[test]
    fn positions() {
        // "ab;\ncd": a and b from a.js line 1, ";" unmapped, c from b.js line 3 column 5.
        let map = r#"{"version":3,"sources":["src/a.js","b.js"],"sourcesContent":["x",null],
            "mappings":"AAAA,EAAE;ACEE"}"#;
        let js = "ab;\ncd".as_bytes();
        let m = FileMapping::parse(map, Path::new("dist/out.js.map"), js).unwrap();
        assert_eq!(m.sources[0].path, "dist/src/a.js");
        assert_eq!(m.sources[0].content.as_deref(), Some("x"));
        let pos = |source, line, column| {
            Some(SourcePos {
                source,
                line,
                column,
            })
        };
        assert_eq!(
            m.positions,
            [
                pos(0, 1, 1),
                pos(0, 1, 1),
                pos(0, 1, 3),
                pos(0, 1, 3),
                pos(1, 3, 5),
                pos(1, 3, 5)
            ]
        );
    }

    #[test]
    fn wasm_map() {
        // Bytes 10..14 from a.c line 3, 14..20 from line 4 column 2, the rest unmapped.
        let map = r#"{"version":3,"sources":["a.c"],"mappings":"UAEA,IACC,MAAC,M"}"#;
        let m = FileMapping::parse_wasm(map, Path::new("x.wasm.map"), 30).unwrap();
        let at = |o: usize| m.positions[o].map(|p| (p.line, p.column));
        assert_eq!(at(9), None);
        assert_eq!(at(10), Some((3, 1)));
        assert_eq!(at(13), Some((3, 1)));
        assert_eq!(at(14), Some((4, 2)));
        assert_eq!(at(20), Some((4, 3)));
        assert_eq!(at(25), Some((4, 3)));
        assert_eq!(at(26), None);
    }

    #[test]
    fn merge() {
        let part = |path: &str| FileMapping {
            sources: vec![SourceFile {
                path: path.to_owned(),
                content: Some("x".to_owned()),
            }],
            positions: vec![Some(SourcePos {
                source: 0,
                line: 1,
                column: 1,
            })],
        };
        let map = |section| MapFile {
            section,
            file: "m".to_owned(),
            kind: "source map".to_owned(),
        };
        let mut all = SourceMapping::new(4);
        all.add(0, map(0), part("a.ts"));
        all.add(2, map(1), part("b.c"));
        all.add(3, map(2), part("a.ts"));
        assert_eq!(all.sources.len(), 2);
        let sources: Vec<_> = all.positions.iter().map(|p| p.map(|p| p.source)).collect();
        assert_eq!(sources, [Some(0), None, Some(1), Some(0)]);
    }
}
