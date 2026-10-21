//! Decodes long base64 runs of the main JavaScript, such as fonts or wasm modules in data URLs,
//! so that the model sees the bytes they encode instead of base64 text. The decoder puts them back
//! with `btoa`.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Shortest run that is decoded. Shorter runs do not pay for their entry in the decoder.
pub const MIN_RUN: usize = 64;

/// The main JavaScript with its base64 runs taken out.
pub struct Base64Runs {
    /// The JavaScript with each run replaced by `marker`
    pub js: Vec<u8>,
    /// A byte that the JavaScript does not have
    pub marker: u8,
    /// Decoded bytes of each run, in order
    pub decoded: Vec<Vec<u8>>,
    /// Where each run was in the JavaScript
    pub spans: Vec<std::ops::Range<usize>>,
}

impl Base64Runs {
    /// Arranges `items`, one per byte of the original JavaScript, like the bytes of the stream: the
    /// JavaScript with the markers, then the decoded bytes. A marker and a decoded byte get the item
    /// of the base64 character they come from.
    pub fn arrange<T: Clone>(&self, items: &[T]) -> Vec<T> {
        let mut js = Vec::with_capacity(self.js.len());
        let mut last = 0;
        for span in &self.spans {
            js.extend_from_slice(&items[last..span.start]);
            js.push(items[span.start].clone());
            last = span.end;
        }
        js.extend_from_slice(&items[last..]);
        for (span, decoded) in self.spans.iter().zip(&self.decoded) {
            js.extend((0..decoded.len()).map(|i| items[span.start + i * 4 / 3].clone()));
        }
        js
    }
}

fn sextet(c: u8) -> Option<u32> {
    ALPHABET.iter().position(|&a| a == c).map(|i| i as u32)
}

fn decode(run: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(run.len() / 4 * 3);
    for chunk in run.chunks(4) {
        let padding = chunk.iter().filter(|&&c| c == b'=').count();
        let mut n = 0;
        for &c in &chunk[..4 - padding] {
            n = n << 6 | sextet(c)?;
        }
        n <<= 6 * padding;
        out.extend_from_slice(&n.to_be_bytes()[1..4 - padding]);
    }
    Some(out)
}

/// Base64 with padding, as `btoa` writes it
fn encode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut n = 0;
        for (i, &b) in chunk.iter().enumerate() {
            n |= (b as u32) << (16 - 8 * i);
        }
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ALPHABET[(n >> (18 - 6 * i)) as usize & 63]
            } else {
                b'='
            });
        }
    }
    out
}

/// Takes the base64 runs of at least `min_run` bytes out of `js`. A run is kept as text unless
/// `btoa` gives it back exactly. None if there is no such run, or no byte for the marker.
pub fn extract(js: &[u8], min_run: usize) -> Option<Base64Runs> {
    let marker = (1..9).find(|b| !js.contains(b))?;
    let mut out = Vec::with_capacity(js.len());
    let mut decoded = Vec::new();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < js.len() {
        let mut end = i;
        while end < js.len() && sextet(js[end]).is_some() {
            end += 1;
        }
        if end == i {
            out.push(js[i]);
            i += 1;
            continue;
        }
        let mut padded = end;
        while padded < js.len() && padded - end < 2 && js[padded] == b'=' {
            padded += 1;
        }
        // Without padding, the run ends at a whole number of 4 byte groups
        let run_end = if padded > end && (padded - i) % 4 == 0 {
            padded
        } else {
            i + (end - i) / 4 * 4
        };
        let run = &js[i..run_end];
        // Runs of only some classes of characters, such as long numbers or names, are text
        let mixed = run.iter().any(u8::is_ascii_uppercase)
            && run.iter().any(u8::is_ascii_lowercase)
            && run.iter().any(u8::is_ascii_digit);
        match decode(run).filter(|bytes| mixed && run.len() >= min_run && encode(bytes) == run) {
            Some(bytes) => {
                out.push(marker);
                decoded.push(bytes);
                spans.push(i..run_end);
                i = run_end;
            }
            None => {
                out.extend_from_slice(&js[i..end]);
                i = end;
            }
        }
    }
    (!decoded.is_empty()).then_some(Base64Runs {
        js: out,
        marker,
        decoded,
        spans,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn encode_for_test(bytes: &[u8]) -> String {
        String::from_utf8(encode(bytes)).unwrap()
    }

    #[test]
    fn decodes_runs_that_btoa_gives_back() {
        let data: Vec<u8> = (0..=255).collect();
        for len in [48, 49, 50] {
            let b64 = String::from_utf8(encode(&data[..len])).unwrap();
            let js = format!("a=\"data:font/otf;base64,{b64}\",b=`{b64}`;");
            let runs = extract(js.as_bytes(), 64).unwrap();
            assert_eq!(runs.marker, 1);
            assert_eq!(runs.js, b"a=\"data:font/otf;base64,\x01\",b=`\x01`;");
            assert_eq!(runs.decoded, vec![data[..len].to_vec(), data[..len].to_vec()]);
            let offsets: Vec<usize> = (0..js.len()).collect();
            let arranged = runs.arrange(&offsets);
            assert_eq!(arranged.len(), runs.js.len() + 2 * len);
            assert_eq!(arranged[24], 24);
            assert_eq!(arranged[25], 24 + b64.len());
            assert_eq!(&arranged[runs.js.len()..][..3], &[24, 25, 26]);
        }
    }

    #[test]
    fn keeps_short_and_inexact_runs() {
        assert!(extract(b"x=\"QUJD\"", 64).is_none());
        // Unused bits that are set do not come back from btoa
        let mut b64 = encode(&(0..49).collect::<Vec<u8>>());
        let last = b64.len() - 3;
        b64[last] = b'B';
        assert!(extract(&b64, 64).is_none());
        // Nor do runs without upper case letters, lower case letters and digits
        assert!(extract(&[b'3'; 80], 64).is_none());
        assert!(extract(&encode(&[7; 60]), 64).is_none());
        // A marker is needed
        let mut js = encode(&(0..60).collect::<Vec<u8>>());
        js.extend(1..9);
        assert!(extract(&js, 64).is_none());
    }

    #[test]
    fn decodes_the_whole_groups_of_an_unpadded_run() {
        let data: Vec<u8> = (100..160).collect();
        let mut js = encode(&data);
        js.extend_from_slice(b"xy");
        let runs = extract(&js, 64).unwrap();
        assert_eq!(runs.js, b"\x01xy");
        assert_eq!(runs.decoded, vec![data]);
    }
}
