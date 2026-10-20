use std::{fs, path::Path};

use anyhow::Result;
use serde_json::{json, Value};

use super::analysis::Analysis;

const TEMPLATE: &str = include_str!("report.html");

/// Values in units of `1 / scale`, as little-endian u16, base64 encoded.
fn encode_u16<T: Copy + Into<f64>>(values: &[T], scale: f64) -> String {
    let mut bytes = Vec::with_capacity(values.len() * 2);
    for &v in values {
        let v = (v.into() * scale).round().clamp(0., 65535.) as u16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    base64(&bytes)
}

/// Values in units of `1 / scale`, as u8, base64 encoded.
fn encode_u8<T: Copy + Into<f64>>(values: &[T], scale: f64) -> String {
    let bytes: Vec<u8> = values
        .iter()
        .map(|&v| (v.into() * scale).round().clamp(0., 255.) as u8)
        .collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn write(analysis: &Analysis, summary: &Value, path: &Path) -> Result<()> {
    let data = analysis.data();

    // How far the compressed stream got with each byte: small steps, so one byte each.
    let mut previous = 0u32;
    let encoded_steps: Vec<u8> = data
        .recording
        .encoded_pos
        .iter()
        .map(|&p| {
            let step = (p - previous).min(255) as u8;
            previous = p;
            step
        })
        .collect();

    // Source positions as runs: start, source (u32::MAX for none), line, column.
    let (sources, runs) = match &data.source_mapping {
        Some(m) => {
            let mut runs = Vec::new();
            let mut previous = None;
            for (offset, pos) in m.positions.iter().enumerate() {
                if previous != Some(*pos) {
                    let (source, line, column) =
                        pos.map_or((u32::MAX, 0, 0), |p| (p.source, p.line, p.column));
                    for v in [offset as u32, source, line, column] {
                        runs.extend_from_slice(&v.to_le_bytes());
                    }
                    previous = Some(*pos);
                }
            }
            (
                m.sources
                    .iter()
                    .map(|s| json!({ "path": s.path, "content": s.content }))
                    .collect::<Vec<_>>(),
                base64(&runs),
            )
        }
        None => (Vec::new(), String::new()),
    };

    // Per wasm section: its parts, and each instruction's start and opcode.
    let wasm: Vec<Value> = analysis
        .wasm_html()
        .into_iter()
        .map(|w| {
            let starts: Vec<u8> = w
                .instructions
                .iter()
                .flat_map(|i| (i.0 as u32).to_le_bytes())
                .collect();
            let ops: Vec<u8> = w.instructions.iter().flat_map(|i| i.1.to_le_bytes()).collect();
            let operands: Vec<&str> = w.instructions.iter().map(|i| i.2.as_str()).collect();
            json!({
                "section": w.section,
                "regions": w.regions,
                "opcodes": w.opcodes,
                "starts": base64(&starts),
                "ops": base64(&ops),
                "operands": operands.join("\n"),
                "padded": w.padded,
            })
        })
        .collect();

    let payload = json!({
        "summary": summary,
        "input": base64(&data.recording.input),
        "costs": encode_u16(analysis.byte_costs(), 256.),
        "modelCosts": encode_u8(analysis.model_byte_costs(), 16.),
        "encodedSteps": base64(&encoded_steps),
        "sources": sources,
        "sourceRuns": runs,
        "wasm": wasm,
    });
    let script = serde_json::to_string(&payload)?.replace("</", "<\\/");
    fs::write(path, TEMPLATE.replace("/*REPORT_DATA*/null", &script))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
