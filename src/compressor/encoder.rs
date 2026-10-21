use std::io::{Read, Write};

use anyhow::{Context, Result};

use super::coder::ArithmeticEncoder;
use super::model::Model;
use super::utils::prob_squash;

/// What the encoder predicted for every bit, recorded for the report.
#[derive(Debug, Default, Clone)]
pub struct Recording {
    /// The encoded bytes.
    pub input: Vec<u8>,
    /// Stretched prediction (logit of p(1)) of the whole model, one per bit.
    pub preds: Vec<f32>,
    /// Stretched predictions of the submodels, `submodel_count` per bit. Empty unless requested.
    pub submodel_preds: Vec<f32>,
    pub submodel_count: usize,
    /// Number of bytes the coder had written after each input byte.
    pub encoded_pos: Vec<u32>,
}

pub struct Encoder<W: Write> {
    coder: ArithmeticEncoder<W>,
    model: Box<dyn Model>,
    size_before_compression: usize,
    recording: Option<(Recording, bool)>,
}

impl<W: Write> Encoder<W> {
    pub fn new(model: Box<dyn Model>, output: W) -> Result<Self> {
        Ok(Self {
            coder: ArithmeticEncoder::new(output)?,
            model,
            size_before_compression: 0,
            recording: None,
        })
    }

    pub fn model(&self) -> &dyn Model {
        self.model.as_ref()
    }

    /// Records the predictions for the report, with those of the submodels if `submodels` is set.
    pub fn record(&mut self, submodels: bool) {
        self.recording = Some((Recording::default(), submodels));
    }

    /// Compresses a section using the provided model.
    /// Similar type of data should be encoded in the same section.
    pub fn encode_section(&mut self, mut byte_stream: impl Read) -> Result<()> {
        let mut bytes = Vec::<u8>::new();
        byte_stream.read_to_end(&mut bytes)?;

        match &mut self.recording {
            None => {
                for &b in &bytes {
                    for i in 0..8 {
                        let prob = prob_squash(self.model.pred());
                        let bit = (b >> (7 - i)) & 1;
                        self.coder.encode(bit, prob)?;
                        self.model.learn(bit);
                    }
                }
            }
            Some((recording, submodels)) => {
                for &b in &bytes {
                    for i in 0..8 {
                        let pred = self.model.pred();
                        recording.preds.push(pred as f32);
                        if *submodels {
                            let before = recording.submodel_preds.len();
                            self.model.submodel_preds(&mut recording.submodel_preds);
                            recording.submodel_count = recording.submodel_preds.len() - before;
                        }
                        let bit = (b >> (7 - i)) & 1;
                        self.coder.encode(bit, prob_squash(pred))?;
                        self.model.learn(bit);
                    }
                    recording
                        .encoded_pos
                        .push(self.coder.bytes_written() as u32);
                }
                recording.input.extend_from_slice(&bytes);
            }
        }

        self.size_before_compression += bytes.len();
        Ok(())
    }

    /// Takes what was recorded since `record`.
    pub fn take_recording(&mut self) -> Option<Recording> {
        self.recording.take().map(|(recording, _)| recording)
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
