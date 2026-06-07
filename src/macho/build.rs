use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{bail, Context, Result};

use crate::compressor::compress_config::ModelConfig;

use super::{
    assembly::render_model_assembly, pack::CompressedMacho, payload::render_payload_assembly,
    DEFAULT_NORDER_TABLE_POW2,
};

const STUBS_S: &str = include_str!("stubs/stubs.s");
const TINY_RUNTIME_S: &str = include_str!("stubs/tiny_runtime.s");
const DIAGNOSTIC_RUNTIME_C: &str = include_str!("stubs/diagnostic_runtime.c");

pub fn build_decompressor(
    output_dir: &Path,
    model_config: &ModelConfig,
    compressed_path: &Path,
    packed: &CompressedMacho,
    diagnostics: bool,
    wrapper_script: bool,
) -> Result<PathBuf> {
    if !command_available("clang") {
        bail!("clang is required to build the Mach-O decompressor");
    }
    if wrapper_script && !command_available("gzip") {
        bail!("gzip is required to build the Mach-O wrapper script");
    }
    if wrapper_script && !command_available("chmod") {
        bail!("chmod is required to build the Mach-O wrapper script");
    }

    let build_dir = output_dir.join("build");
    fs::create_dir_all(&build_dir)
        .with_context(|| format!("Failed to create {}", build_dir.display()))?;

    let mut sources = vec![
        ("stubs.s", STUBS_S.to_owned()),
        (
            "payload.s",
            render_payload_assembly(compressed_path, packed),
        ),
        (
            "model.s",
            render_model_assembly(model_config, DEFAULT_NORDER_TABLE_POW2)?,
        ),
    ];
    if diagnostics {
        sources.push(("runtime.c", DIAGNOSTIC_RUNTIME_C.to_owned()));
    } else {
        sources.push(("runtime.s", TINY_RUNTIME_S.to_owned()));
    }

    let mut source_paths = Vec::with_capacity(sources.len());
    for (name, src) in sources {
        let path = build_dir.join(name);
        fs::write(&path, src).with_context(|| format!("Failed to write {}", path.display()))?;
        source_paths.push(path);
    }

    let decompressor_path = output_dir.join("decompressor");
    let mut command = Command::new("clang");
    command.arg("-arch").arg("arm64");
    command.arg("-Oz");
    command.arg("-fno-unwind-tables");
    command.arg("-fno-asynchronous-unwind-tables");
    command.arg("-Wl,-dead_strip");
    command.arg("-Wl,-x");
    command.arg("-flto");
    command.arg("-Wl,-no_data_const");
    command.arg("-Wl,-no_function_starts");
    command.arg("-Wl,-no_source_version");
    command.arg("-Wl,-no_data_in_code_info");
    command.arg("-Wl,-no_compact_unwind");
    append_import_dylibs(&mut command, &packed.dylibs)?;
    for path in &source_paths {
        command.arg(path);
    }
    command.arg("-o").arg(&decompressor_path);

    let output = command.output().context("Failed to run clang")?;
    assert_command_success("build Mach-O decompressor", &output)?;
    strip_decompressor(&decompressor_path);
    if wrapper_script {
        let wrapper_script_path = output_dir.join("decompressor.sh");
        write_wrapper_script(&decompressor_path, &wrapper_script_path)?;
    }

    Ok(decompressor_path)
}

fn strip_decompressor(path: &Path) {
    let _ = Command::new("strip").arg(path).output();
}

fn append_import_dylibs(command: &mut Command, dylibs: &[String]) -> Result<()> {
    let mut linked = Vec::new();
    for dylib in dylibs {
        if linked.iter().any(|linked| linked == dylib) {
            continue;
        }
        linked.push(dylib.clone());

        if dylib == "/usr/lib/libSystem.B.dylib" {
            continue;
        }

        if let Some(framework) = framework_name(dylib) {
            command.arg("-framework").arg(framework);
            continue;
        }

        if let Some(library) = usr_lib_name(dylib) {
            command.arg(format!("-l{library}"));
            continue;
        }

        if dylib.starts_with('/') && Path::new(dylib).exists() {
            command.arg(dylib);
            continue;
        }

        bail!("Unsupported imported dylib path for Mach-O decompressor: {dylib}");
    }
    Ok(())
}

fn framework_name(dylib: &str) -> Option<String> {
    dylib
        .split('/')
        .find_map(|component| component.strip_suffix(".framework"))
        .map(ToOwned::to_owned)
}

fn usr_lib_name(dylib: &str) -> Option<String> {
    let filename = dylib.strip_prefix("/usr/lib/")?;
    let stem = filename.strip_suffix(".dylib")?;
    let name = stem.strip_prefix("lib")?;
    Some(name.split('.').next().unwrap_or(name).to_owned())
}

fn write_wrapper_script(native_path: &Path, wrapper_path: &Path) -> Result<()> {
    let output = Command::new("gzip")
        .arg("-9")
        .arg("-n")
        .arg("-c")
        .arg(native_path)
        .output()
        .context("Failed to run gzip")?;
    assert_command_success("compress Mach-O decompressor for wrapper", &output)?;

    let compressed = output.stdout;
    let stub = format!(
        "#!/bin/sh\n\
         t=${{TMPDIR:-/tmp}}/w$$\n\
         tail -c {} \"$0\"|gzip -dc>$t\n\
         chmod +x $t\n\
         $t \"$@\";r=$?\n\
         rm $t\n\
         exit $r\n",
        compressed.len()
    );

    let mut file = fs::File::create(wrapper_path)
        .with_context(|| format!("Failed to create {}", wrapper_path.display()))?;
    file.write_all(stub.as_bytes())
        .with_context(|| format!("Failed to write {}", wrapper_path.display()))?;
    file.write_all(&compressed)
        .with_context(|| format!("Failed to write {}", wrapper_path.display()))?;

    let output = Command::new("chmod")
        .arg("+x")
        .arg(wrapper_path)
        .output()
        .context("Failed to run chmod")?;
    assert_command_success("chmod Mach-O wrapper script", &output)?;

    Ok(())
}

fn command_available(command: &str) -> bool {
    Command::new(command).arg("--version").output().is_ok()
}

fn assert_command_success(action: &str, output: &Output) -> Result<()> {
    if !output.status.success() {
        bail!(
            "{action} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(())
}
