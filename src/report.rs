use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::Path,
};

use crate::compressor::{model::Model, utils::prob_squash};
use anyhow::Result;

pub struct ReportGenerator {}

impl ReportGenerator {
    pub fn create(
        mut byte_stream: impl Read,
        mut model: Box<dyn Model>,
        output_dir: &Path,
    ) -> Result<()> {
        const ONE_OVER_8: f64 = 1. / 8.;
        let mut bytes = Vec::<u8>::new();
        byte_stream.read_to_end(&mut bytes)?;

        let output_file = File::create(output_dir.join("report.html"))?;
        let mut output = BufWriter::new(output_file);

        write!(output, "<html><body>")?;

        for b in bytes {
            let mut avg_pred_err_byte = 0.;
            for i in 0..8 {
                let prob = prob_squash(model.pred());
                let bit = (b >> (7 - i)) & 1;
                let pred_err = bit as f64 - prob;
                avg_pred_err_byte += pred_err.abs();
                model.learn(bit);
            }

            avg_pred_err_byte *= ONE_OVER_8;

            let ch = b as char;
            let display = if ch.is_ascii_graphic() || ch == ' ' {
                ch.to_string()
            } else {
                format!("\\x{:02X}", b)
            };
            write!(
                output,
                "<i style=\"background: hsl({}, 100%, 40%);\">{}</i>",
                ((1. - avg_pred_err_byte) * 75.) as u8,
                display
            )?;
        }
        write!(output, "</body></html>")?;

        Ok(())
    }
}
