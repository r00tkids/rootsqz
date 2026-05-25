pub mod coder;
pub mod compress_config;
pub mod model;
pub mod model_finder;
pub mod utils;

mod encoder;

pub use encoder::Encoder;

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, fs::File, io::Read, rc::Rc};

    use super::coder::tests::ArithmeticDecoder;
    use super::model::{HashTable, Model, Model4k, NOrderByteData};
    use super::model_finder::{create_default_model_config, ModelFinder};
    use super::utils::prob_squash;
    use crate::compressor::Encoder;
    use anyhow::Result;

    #[cfg(test)]
    pub struct Decoder<R: Read> {
        coder: ArithmeticDecoder<R>,
        model: Box<dyn Model>,
    }

    #[cfg(test)]
    impl<R: Read> Decoder<R> {
        pub fn new(model: Box<dyn Model>, read_stream: R) -> Result<Self> {
            Ok(Self {
                coder: ArithmeticDecoder::new(read_stream)?,
                model: model,
            })
        }

        pub fn decode(&mut self, size: usize) -> Result<Vec<u8>> {
            let mut res: Vec<u8> = vec![0; size];
            for byte_idx in 0..size {
                for _ in 0..8 {
                    let prob = prob_squash(self.model.pred());
                    let bit = self.coder.decode(prob)?;
                    self.model.learn(bit);
                    res[byte_idx] = (res[byte_idx] << 1) | bit;
                }
            }

            Ok(res)
        }

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

    #[test]
    pub fn round_trip() {
        let bootstrap_text = "for(w=c.width=185,e=c.getContext('2d'),e.drawImage(this,p=0,0),n='',d=e.getImageData(0,0,w,150).data;t=d[p+=4];)n+=String.fromCharCode(t);(1,eval)(e)";

        let mut test_data = String::new();
        File::open("tests/ray_tracer/index.js")
            .unwrap()
            .read_to_string(&mut test_data)
            .unwrap();

        let test_bytes = test_data.as_bytes();

        let model_finder = ModelFinder::new();
        let mut encoded_data: Vec<u8> = Vec::new();
        {
            let model = model_finder.default_model;
            let mut encoder = Encoder::new(model, &mut encoded_data).unwrap();
            encoder.warm_up(bootstrap_text.as_bytes()).unwrap();
            encoder.encode_section(test_bytes).unwrap();
            encoder.finish().unwrap();
        }

        println!(
            "Size of input: {}\nSize of encoded data: {}",
            test_data.len(),
            encoded_data.len()
        );

        let model_finder = ModelFinder::new();
        let model = model_finder.default_model;
        let mut decoder = Decoder::new(model, encoded_data.as_slice()).unwrap();

        decoder.warm_up(bootstrap_text.as_bytes()).unwrap();

        let decode_res = decoder.decode(test_bytes.len()).unwrap();
        assert!(String::from_utf8(decode_res).unwrap() == test_data);
    }

    #[test]
    fn model4k_matches_expanded_default_model() {
        let table_pow2 = 10;
        let hash_table = Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(table_pow2)));
        let mut expanded = create_default_model_config()
            .create_model(hash_table)
            .expect("Failed to create expanded default model");
        let mut model4k = Model4k::new(table_pow2);

        let input = (0u8..=255)
            .chain([0x00, 0xff, 0x55, 0xaa])
            .collect::<Vec<_>>();
        for byte in input {
            for bit_idx in 0..8 {
                let bit = (byte >> (7 - bit_idx)) & 1;
                let expanded_p = expanded.pred();
                let model4k_p = model4k.pred();

                assert!(
                    (expanded_p - model4k_p).abs() <= f64::EPSILON,
                    "prediction mismatch for byte {byte:#04x}, bit {bit_idx}: {expanded_p} != {model4k_p}",
                );

                expanded.learn(bit);
                model4k.learn(bit);
            }
        }
    }
}
