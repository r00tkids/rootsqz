use std::io::Write;

use anyhow::Result;
use serde_json::{Map, Value};

/// A value in a table. Floats carry the number of decimals they are printed with.
#[derive(Debug, Clone)]
pub enum Cell {
    Int(i64),
    Float(f64, usize),
    Str(String),
    Bool(bool),
    Empty,
}

impl Cell {
    pub fn bits(value: f64) -> Self {
        Cell::Float(value, 2)
    }

    fn json(&self) -> Value {
        match self {
            Cell::Int(v) => Value::from(*v),
            Cell::Float(v, decimals) => {
                let factor = 10f64.powi(*decimals as i32);
                Value::from((v * factor).round() / factor)
            }
            Cell::Str(s) => Value::from(s.as_str()),
            Cell::Bool(b) => Value::from(*b),
            Cell::Empty => Value::Null,
        }
    }

    fn tsv(&self) -> String {
        match self {
            Cell::Int(v) => v.to_string(),
            Cell::Float(v, decimals) => format!("{v:.decimals$}"),
            Cell::Str(s) => s.clone(),
            Cell::Bool(b) => b.to_string(),
            Cell::Empty => String::new(),
        }
    }
}

impl From<usize> for Cell {
    fn from(v: usize) -> Self {
        Cell::Int(v as i64)
    }
}

impl From<String> for Cell {
    fn from(v: String) -> Self {
        Cell::Str(v)
    }
}

impl From<&str> for Cell {
    fn from(v: &str) -> Self {
        Cell::Str(v.to_owned())
    }
}

/// Rows with named columns, printed as TSV or put into the summary as JSON objects.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

impl Table {
    pub fn new(columns: &[&str]) -> Self {
        Self {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, row: Vec<Cell>) {
        debug_assert_eq!(row.len(), self.columns.len());
        self.rows.push(row);
    }

    /// Sorts by the value of a numeric column, largest first.
    pub fn sort_desc(&mut self, column: &str) {
        if let Some(i) = self.columns.iter().position(|c| c == column) {
            let key = |row: &Vec<Cell>| match row[i] {
                Cell::Int(v) => v as f64,
                Cell::Float(v, _) => v,
                _ => f64::NEG_INFINITY,
            };
            self.rows.sort_by(|a, b| {
                key(b)
                    .partial_cmp(&key(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    pub fn truncate(&mut self, n: usize) {
        self.rows.truncate(n);
    }

    pub fn write_tsv(&self, out: &mut impl Write) -> Result<()> {
        writeln!(out, "{}", self.columns.join("\t"))?;
        for row in &self.rows {
            let cells: Vec<String> = row.iter().map(Cell::tsv).collect();
            writeln!(out, "{}", cells.join("\t"))?;
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        Value::Array(
            self.rows
                .iter()
                .map(|row| {
                    let mut object = Map::new();
                    for (column, cell) in self.columns.iter().zip(row) {
                        object.insert(column.clone(), cell.json());
                    }
                    Value::Object(object)
                })
                .collect(),
        )
    }
}

/// Text of bytes for one TSV cell: printable ASCII as it is, `\n`, `\t`, `\r`, `\\`
/// and other bytes as `\xNN`, shortened to `max` characters with a trailing `…`.
pub fn escape(bytes: &[u8], max: usize) -> String {
    let mut s = String::new();
    let mut count = 0;
    for &b in bytes {
        if count >= max {
            s.push('…');
            break;
        }
        match b {
            b'\n' => s.push_str("\\n"),
            b'\t' => s.push_str("\\t"),
            b'\r' => s.push_str("\\r"),
            b'\\' => s.push_str("\\\\"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02X}")),
        }
        count += 1;
    }
    s
}
