use std::io::{Read, Write};

use anyhow::{Context, Result};

use super::coder::ArithmeticEncoder;
use super::model::Model;
use super::utils::prob_squash;

pub struct Encoder<W: Write> {
    coder: ArithmeticEncoder<W>,
    model: Box<dyn Model>,
    size_before_compression: usize,
}

impl<W: Write> Encoder<W> {
    pub fn new(model: Box<dyn Model>, output: W) -> Result<Self> {
        Ok(Self {
            coder: ArithmeticEncoder::new(output)?,
            model,
            size_before_compression: 0,
        })
    }

    /// Compresses a section using the provided model.
    /// Similar type of data should be encoded in the same section.
    pub fn encode_section(&mut self, mut byte_stream: impl Read) -> Result<()> {
        let mut bytes = Vec::<u8>::new();
        byte_stream.read_to_end(&mut bytes)?;

        let mut b_idx = 0;
        while b_idx < bytes.len() {
            let b = bytes[b_idx];
            for i in 0..8 {
                let prob = prob_squash(self.model.pred());
                let bit = (b >> (7 - i)) & 1;
                self.coder.encode(bit, prob)?;
                self.model.learn(bit);
            }

            b_idx += 1;
        }

        self.size_before_compression += bytes.len();
        Ok(())
    }

    pub fn finish(self) -> Result<usize> {
        self.coder.finish().context("Failed to finish encoding")?;
        Ok(self.size_before_compression)
    }

    /// Warms up the model by reading a byte stream and learning from it.
    #[allow(dead_code)]
    pub fn warm_up(&mut self, mut byte_stream: impl Read) -> Result<()> {
        let mut bytes = Vec::<u8>::new();
        byte_stream.read_to_end(&mut bytes)?;
        for b in bytes {
            for i in 0..8 {
                let bit = (b >> (7 - i)) & 1;
                self.model.learn(bit);
            }
        }

        Ok(())
    }
}
