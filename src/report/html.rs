use std::{fs, path::Path};

use anyhow::Result;
use serde_json::{json, Value};

use super::analysis::Analysis;

const TEMPLATE: &str = include_str!("report.html");

/// Costs in 1/256 bits, as little-endian u16, base64 encoded.
fn encode_costs<T: Copy + Into<f64>>(costs: &[T]) -> String {
    let mut bytes = Vec::with_capacity(costs.len() * 2);
    for &c in costs {
        let v = (c.into() * 256.).round().clamp(0., 65535.) as u16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
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
    let payload = json!({
        "summary": summary,
        "input": base64(&data.recording.input),
        "costs": encode_costs(analysis.byte_costs()),
        "modelCosts": encode_costs(analysis.model_byte_costs()),
        "encodedPos": data.recording.encoded_pos,
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
