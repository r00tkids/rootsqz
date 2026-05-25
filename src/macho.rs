use std::{fs, path::PathBuf};

use anyhow::{Context, Result};

mod assembly;
mod build;
mod fixups;
mod model;
mod pack;
mod parser;
mod payload;

#[cfg(test)]
mod tests;

use crate::compressor::model::{Model4k, MODEL4K_TABLE_POW2};

const DEFAULT_NORDER_TABLE_POW2: u32 = MODEL4K_TABLE_POW2;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Input Mach-O binary executable to compress
    #[arg(short, long)]
    pub input: PathBuf,

    /// Output directory
    #[arg(short, long)]
    pub output_directory: String,

    /// Enable verbose diagnostics in the generated decompressor
    #[arg(long)]
    pub diagnostics: bool,

    /// Export the decompressor as a self-extracting shell script
    #[arg(long)]
    pub wrapper_script: bool,
}

pub fn run(args: Args) -> Result<()> {
    let binary = fs::read(&args.input)
        .with_context(|| format!("Failed to read {}", args.input.display()))?;

    let model = Box::new(Model4k::default());
    let compressed_macho = pack::compress_binary_with_model(&binary, model)?;
    let total_uncompressed = compressed_macho.uncompressed.len();

    println!(
        "Found {} segment(s) to compress ({} bytes total):",
        compressed_macho.segments.len(),
        total_uncompressed
    );
    for segment in &compressed_macho.segments {
        println!("  {:<16} {} bytes", segment.name, segment.size);
    }

    let output_dir = PathBuf::from(&args.output_directory);
    fs::create_dir_all(&output_dir).with_context(|| {
        format!(
            "Failed to create output directory: {}",
            output_dir.display()
        )
    })?;

    let out_path = output_dir.join("compressed.bin");
    fs::write(&out_path, &compressed_macho.compressed)
        .with_context(|| format!("Failed to write {}", out_path.display()))?;

    let decompressor_path = build::build_decompressor(
        &output_dir,
        &out_path,
        &compressed_macho,
        args.diagnostics,
        args.wrapper_script,
    )
    .context("Failed to build Mach-O decompressor")?;

    println!(
        "Compressed {} bytes -> {} bytes ({:.1}% of original)",
        total_uncompressed,
        compressed_macho.compressed.len(),
        100.0 * compressed_macho.compressed.len() as f64 / total_uncompressed as f64,
    );
    println!("Output written to {}", out_path.display());
    println!("Decompressor written to {}", decompressor_path.display());

    Ok(())
}
