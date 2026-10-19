//! Maps the bytes of the main JavaScript to positions in its original sources, with a source map.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePos {
    pub source: u32,
    pub line: u32,
    pub column: u32,
}

/// Where each byte of the main JavaScript came from.
#[derive(Debug, Clone)]
pub struct SourceMapping {
    /// The source map file, as given.
    pub map_path: String,
    pub sources: Vec<SourceFile>,
    /// One per byte of the main JavaScript; `None` where the source map has no mapping.
    pub positions: Vec<Option<SourcePos>>,
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

impl SourceMapping {
    /// Reads the source map at `map_path` for the JavaScript `js`.
    pub fn read(map_path: &Path, js: &[u8]) -> Result<Self> {
        let text = std::fs::read_to_string(map_path)
            .with_context(|| format!("Failed to read source map: {}", map_path.display()))?;
        Self::parse(&text, map_path, js)
            .with_context(|| format!("Failed to parse source map: {}", map_path.display()))
    }

    fn parse(text: &str, map_path: &Path, js: &[u8]) -> Result<Self> {
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

        let segments = decode_mappings(&raw.mappings)?;
        let mut positions = vec![None; js.len()];
        let mut line_start = 0;
        for (line, line_segments) in js.split(|&b| b == b'\n').enumerate() {
            let segments: &[Segment] = segments.get(line).map_or(&[], |s| s.as_slice());
            // Source map columns count UTF-16 code units.
            let mut column = 0u32;
            let mut next = 0;
            let mut current: Option<&Segment> = None;
            let mut i = 0;
            while i < line_segments.len() {
                let b = line_segments[i];
                let char_len = match b {
                    0x00..=0x7f => 1,
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xff => 4,
                    _ => 1,
                }
                .min(line_segments.len() - i);
                while next < segments.len() && segments[next].column <= column {
                    current = Some(&segments[next]);
                    next += 1;
                }
                let pos = current
                    .and_then(|s| s.source)
                    .map(|(source, l, c)| SourcePos {
                        source,
                        line: l + 1,
                        column: c + 1,
                    });
                for k in 0..char_len {
                    positions[line_start + i + k] = pos;
                }
                column += if char_len == 4 { 2 } else { 1 };
                i += char_len;
            }
            // The newline belongs to the last segment of the line.
            if line_start + line_segments.len() < js.len() {
                positions[line_start + line_segments.len()] =
                    current
                        .and_then(|s| s.source)
                        .map(|(source, l, c)| SourcePos {
                            source,
                            line: l + 1,
                            column: c + 1,
                        });
            }
            line_start += line_segments.len() + 1;
        }
        for pos in positions.iter().flatten() {
            ensure!(
                (pos.source as usize) < sources.len(),
                "a mapping refers to source {}, but there are only {}",
                pos.source,
                sources.len()
            );
        }

        Ok(Self {
            map_path: map_path.display().to_string(),
            sources,
            positions,
        })
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
    if let Some(cwd) = cwd {
        if let Ok(relative) = normal.strip_prefix(cwd) {
            return relative.display().to_string();
        }
    }
    normal.display().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{decode_vlq, SourceMapping, SourcePos};

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
        let m = SourceMapping::parse(map, Path::new("dist/out.js.map"), js).unwrap();
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
}
