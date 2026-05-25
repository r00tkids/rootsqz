use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    rc::Rc,
};

use crate::compressor::model::{HashTable, LnMixerPred, Model, NOrderByte, NOrderByteData};

use super::{
    assembly::NORDER_RECORD_BYTES,
    fixups::{read_u32_at, FIXUP_KIND_BIND},
    model::LoadCommand,
    pack::compress_binary_with_model,
    parser, run, Args,
};

struct HalfModel;

impl Model for HalfModel {
    fn pred(&mut self) -> f64 {
        0. // It's squashed (1.0 / (1.0 + exp(-0.0))) = 0.5, so this model always predicts 50% probability
    }

    fn learn(&mut self, _bit: u8) {}
}

#[test]
fn bootstrap_round_trips_compressed_macho_segments() {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) || !command_available("clang") {
        return;
    }

    let binary = fs::read("tests/macho/helloworld").expect("Failed to read Mach-O fixture");
    let compressed_macho = compress_binary_with_model(&binary, Box::new(HalfModel))
        .expect("Failed to compress Mach-O fixture");

    let test_dir = PathBuf::from("./testout/macho_roundtrip");
    fs::create_dir_all(&test_dir).expect("Failed to create temp test directory");

    let compressed_path = test_dir.join("compressed.bin");
    let expected_path = test_dir.join("expected.bin");
    let payload_path = test_dir.join("payload.s");
    let harness_path = test_dir.join("harness.c");
    let output_path = test_dir.join("roundtrip");

    fs::write(&compressed_path, &compressed_macho.compressed)
        .expect("Failed to write compressed fixture");
    fs::write(&expected_path, &compressed_macho.uncompressed)
        .expect("Failed to write expected fixture");
    fs::write(
        &payload_path,
        render_payload_assembly(
            &compressed_path,
            &expected_path,
            compressed_macho.uncompressed.len(),
        ),
    )
    .expect("Failed to write payload assembly");
    fs::write(&harness_path, HARNESS_C).expect("Failed to write harness C");

    let link_output = Command::new("clang")
        .arg("-arch")
        .arg("arm64")
        .arg("src/macho/stubs/bootstrap.s")
        .arg("src/macho/stubs/decoder.s")
        .arg(&payload_path)
        .arg(&harness_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run clang");
    assert_success("link round-trip Mach-O", &link_output);

    let run_output = Command::new(&output_path)
        .output()
        .expect("Failed to run round-trip Mach-O");
    assert_success("run round-trip Mach-O", &run_output);
}

#[test]
fn assembly_model_stubs_round_trip() {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) || !command_available("clang") {
        return;
    }

    let binary = fs::read("tests/macho/helloworld").expect("Failed to read Mach-O fixture");
    let table_pow2 = 10;

    run_assembly_model_round_trip(
        "norder_order0",
        &binary,
        Box::new(NOrderByte::new_norder_model(
            0,
            Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(table_pow2))),
            15,
        )),
        render_norder_model_assembly(0, table_pow2, false),
    );

    run_assembly_model_round_trip(
        "norder_mask01",
        &binary,
        Box::new(NOrderByte::new_norder_model(
            0b0000_0011,
            Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(table_pow2))),
            15,
        )),
        render_norder_model_assembly(0b0000_0011, table_pow2, false),
    );

    run_assembly_model_round_trip(
        "word",
        &binary,
        Box::new(NOrderByte::new_word_model(
            Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(table_pow2))),
            15,
        )),
        render_norder_model_assembly(0, table_pow2, true),
    );

    let shared_table = Rc::new(RefCell::new(HashTable::<NOrderByteData>::new(table_pow2)));
    run_assembly_model_round_trip(
        "ln_mixer",
        &binary,
        Box::new(LnMixerPred::new(vec![
            Box::new(NOrderByte::new_norder_model(0, shared_table.clone(), 15)),
            Box::new(NOrderByte::new_norder_model(0b0000_0011, shared_table, 15)),
        ])),
        render_ln_mixer_model_assembly(table_pow2),
    );
}

#[test]
fn full_decompressor_round_trip() {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) || !command_available("clang") {
        return;
    }

    let binary_path = PathBuf::from("tests/macho/helloworld");
    let output_dir = PathBuf::from("./testout/macho_full_decompressor");
    run(Args {
        input: binary_path.clone(),
        output_directory: output_dir.to_string_lossy().into_owned(),
        diagnostics: false,
        wrapper_script: false,
    })
    .expect("Failed to build full Mach-O decompressor");

    let run_output = Command::new(output_dir.join("decompressor"))
        .output()
        .expect("Failed to run generated decompressor");
    assert_success("run generated decompressor", &run_output);
    assert_eq!(run_output.stdout, b"Hello, World!\n");
}

#[test]
fn full_decompressor_with_diagnostics_builds() {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) || !command_available("clang") {
        return;
    }

    let binary_path = PathBuf::from("tests/macho/helloworld");
    let output_dir = PathBuf::from("./testout/macho_full_decompressor_diagnostics");
    run(Args {
        input: binary_path,
        output_directory: output_dir.to_string_lossy().into_owned(),
        diagnostics: true,
        wrapper_script: false,
    })
    .expect("Failed to build diagnostic Mach-O decompressor");

    let run_output = Command::new(output_dir.join("decompressor"))
        .output()
        .expect("Failed to run diagnostic generated decompressor");
    assert_success("run diagnostic generated decompressor", &run_output);
    assert_eq!(run_output.stdout, b"Hello, World!\n");
}

#[test]
fn full_decompressor_wrapper_script_round_trip() {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64"))
        || !command_available("clang")
        || !command_available("gzip")
    {
        return;
    }

    let binary_path = PathBuf::from("tests/macho/helloworld");
    let output_dir = PathBuf::from("./testout/macho_full_decompressor_wrapper");
    run(Args {
        input: binary_path,
        output_directory: output_dir.to_string_lossy().into_owned(),
        diagnostics: false,
        wrapper_script: true,
    })
    .expect("Failed to build wrapped Mach-O decompressor");

    let run_output = Command::new(output_dir.join("decompressor"))
        .output()
        .expect("Failed to run wrapped generated decompressor");
    assert_success("run wrapped generated decompressor", &run_output);
    assert_eq!(run_output.stdout, b"Hello, World!\n");
}

#[test]
fn packed_macho_metadata_for_helloworld() {
    let binary = fs::read("tests/macho/helloworld").expect("Failed to read Mach-O fixture");
    let packed = compress_binary_with_model(&binary, Box::new(HalfModel))
        .expect("Failed to pack Mach-O fixture");

    assert_eq!(packed.image_size, 0x8000);
    assert_eq!(packed.entry_offset, 0x410);
    assert_eq!(packed.uncompressed.len(), 0x8000);

    assert_eq!(packed.segments.len(), 2);
    assert_eq!(packed.segments[0].name, "__TEXT");
    assert_eq!(packed.segments[0].offset, 0);
    assert_eq!(packed.segments[0].vm_size, 0x4000);
    assert_eq!(packed.segments[0].init_prot, 5);
    assert_eq!(packed.segments[1].name, "__DATA_CONST");
    assert_eq!(packed.segments[1].offset, 0x4000);
    assert_eq!(packed.segments[1].vm_size, 0x4000);

    assert_eq!(packed.decode_chunks.len(), 2);
    assert_eq!(packed.decode_chunks[0].offset, 0);
    assert_eq!(packed.decode_chunks[0].size, 0x4000);
    assert_eq!(packed.decode_chunks[1].offset, 0x4000);
    assert_eq!(packed.decode_chunks[1].size, 0x4000);

    assert_eq!(packed.imports.len(), 1);
    assert_eq!(packed.imports[0].name, "_puts");
    assert!(!packed.imports[0].weak);

    assert_eq!(packed.fixups.len(), 1);
    assert_eq!(packed.fixups[0].kind, FIXUP_KIND_BIND);
    assert_eq!(packed.fixups[0].offset, 0x4000);
    assert_eq!(packed.fixups[0].import_index, 0);
}

#[test]
fn rejects_missing_lc_main() {
    let mut binary = fs::read("tests/macho/helloworld").expect("Failed to read Mach-O fixture");
    patch_load_command(&mut binary, 0x8000_0028, 0x1b);

    let err = compress_binary_with_model(&binary, Box::new(HalfModel))
        .expect_err("binary without LC_MAIN should be rejected");
    assert!(format!("{err:#}").contains("missing LC_MAIN"));
}

#[test]
fn rejects_unsupported_chained_pointer_format() {
    let mut binary = fs::read("tests/macho/helloworld").expect("Failed to read Mach-O fixture");
    let macho = parser::parse(&binary).expect("Failed to parse Mach-O fixture");
    let fixups = macho
        .load_commands
        .iter()
        .find_map(|lc| {
            if let LoadCommand::ChainedFixups(fixups) = lc {
                Some(fixups)
            } else {
                None
            }
        })
        .expect("LC_DYLD_CHAINED_FIXUPS not found");
    let blob_start = fixups.data_offset as usize;
    let starts_offset = read_u32_at(&binary, blob_start + 4).unwrap() as usize;
    let seg_info_offset =
        read_u32_at(&binary, blob_start + starts_offset + 4 + 2 * 4).unwrap() as usize;
    let pointer_format_offset = blob_start + starts_offset + seg_info_offset + 6;
    binary[pointer_format_offset..pointer_format_offset + 2].copy_from_slice(&2u16.to_le_bytes());

    let err = compress_binary_with_model(&binary, Box::new(HalfModel))
        .expect_err("unsupported chained pointer format should be rejected");
    assert!(format!("{err:#}").contains("Unsupported chained pointer format"));
}

fn run_assembly_model_round_trip(
    name: &str,
    binary: &[u8],
    model: Box<dyn Model>,
    model_assembly: String,
) {
    let compressed_macho =
        compress_binary_with_model(binary, model).expect("Failed to compress Mach-O fixture");

    let test_dir = PathBuf::from(format!("./testout/macho_assembly_models/{name}"));
    fs::create_dir_all(&test_dir).expect("Failed to create temp test directory");

    let compressed_path = test_dir.join("compressed.bin");
    let expected_path = test_dir.join("expected.bin");
    let payload_path = test_dir.join("payload.s");
    let model_path = test_dir.join("model.s");
    let harness_path = test_dir.join("harness.c");
    let output_path = test_dir.join("roundtrip");

    fs::write(&compressed_path, &compressed_macho.compressed)
        .expect("Failed to write compressed fixture");
    fs::write(&expected_path, &compressed_macho.uncompressed)
        .expect("Failed to write expected fixture");
    fs::write(
        &payload_path,
        render_payload_assembly(
            &compressed_path,
            &expected_path,
            compressed_macho.uncompressed.len(),
        ),
    )
    .expect("Failed to write payload assembly");
    fs::write(&model_path, model_assembly).expect("Failed to write model assembly");
    fs::write(&harness_path, HARNESS_AFTER_DECODE_C).expect("Failed to write harness C");

    let link_output = Command::new("clang")
        .arg("-arch")
        .arg("arm64")
        .arg("src/macho/stubs/bootstrap.s")
        .arg("src/macho/stubs/decoder.s")
        .arg("src/macho/stubs/model_support.s")
        .arg("src/macho/stubs/norder_byte.s")
        .arg("src/macho/stubs/word.s")
        .arg("src/macho/stubs/ln_mixer.s")
        .arg(&payload_path)
        .arg(&model_path)
        .arg(&harness_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run clang");
    assert_success(&format!("link {name} assembly model"), &link_output);

    let run_output = Command::new(&output_path)
        .output()
        .expect("Failed to run round-trip Mach-O");
    assert_success(&format!("run {name} assembly model"), &run_output);
}

fn command_available(command: &str) -> bool {
    Command::new(command).arg("--version").output().is_ok()
}

fn patch_load_command(binary: &mut [u8], old_cmd: u32, new_cmd: u32) {
    let ncmds = u32::from_le_bytes(binary[16..20].try_into().unwrap()) as usize;
    let mut offset = 32usize;
    for _ in 0..ncmds {
        let cmd = u32::from_le_bytes(binary[offset..offset + 4].try_into().unwrap());
        let cmdsize =
            u32::from_le_bytes(binary[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if cmd == old_cmd {
            binary[offset..offset + 4].copy_from_slice(&new_cmd.to_le_bytes());
            return;
        }
        offset += cmdsize;
    }
    panic!("load command {old_cmd:#x} not found");
}

fn render_payload_assembly(
    compressed_path: &Path,
    expected_path: &Path,
    output_len: usize,
) -> String {
    format!(
        r#".section __DATA,__const
.p2align 3
.globl _rootsqz_compressed_start
_rootsqz_compressed_start:
.incbin "{compressed_path}"
.globl _rootsqz_compressed_end
_rootsqz_compressed_end:

.p2align 3
.globl _rootsqz_expected_start
_rootsqz_expected_start:
.incbin "{expected_path}"
.globl _rootsqz_expected_end
_rootsqz_expected_end:

.p2align 3
.globl _rootsqz_image_size
_rootsqz_image_size:
    .quad {output_len}
.globl _rootsqz_entry_offset
_rootsqz_entry_offset:
    .quad 0

.p2align 3
.globl _rootsqz_decode_chunks_start
_rootsqz_decode_chunks_start:
    .quad 0
    .quad {output_len}
.globl _rootsqz_decode_chunks_end
_rootsqz_decode_chunks_end:

.p2align 3
.globl _rootsqz_segments_start
_rootsqz_segments_start:
.globl _rootsqz_segments_end
_rootsqz_segments_end:

.p2align 3
.globl _rootsqz_imports_start
_rootsqz_imports_start:
.globl _rootsqz_imports_end
_rootsqz_imports_end:

.p2align 3
.globl _rootsqz_fixups_start
_rootsqz_fixups_start:
.globl _rootsqz_fixups_end
_rootsqz_fixups_end:
"#,
        compressed_path = escape_assembly_path(compressed_path),
        expected_path = escape_assembly_path(expected_path),
    )
}

fn escape_assembly_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn render_norder_model_assembly(byte_mask: u8, table_pow2: u32, is_word: bool) -> String {
    let table_len = 1usize << table_pow2;
    let (magic_num, prev_bytes, mask, predict_fn, learn_fn, is_word_flag) = if is_word {
        (
            model_hash(1337, 2),
            2166136261u64,
            u64::MAX,
            "_rootsqz_word_predict",
            "_rootsqz_word_learn",
            1u32,
        )
    } else {
        (
            model_hash(byte_mask as u32, 2),
            0u64,
            byte_mask_to_context_mask(byte_mask),
            "_rootsqz_norder_byte_predict",
            "_rootsqz_norder_byte_learn",
            0u32,
        )
    };

    format!(
        r#".section __DATA,__data
.p2align 3
.globl _rootsqz_model_ctx
_rootsqz_model_ctx:
    .long 0
    .long 1
    .long 0x{magic_num:08x}
    .long 15
    .quad 0x{prev_bytes:016x}
    .quad 0x{mask:016x}
    .long {is_word_flag}
    .long 0
    .quad _rootsqz_norder_table
    .quad {hash_mask}

.p2align 2
_rootsqz_norder_table:
.space {table_bytes}

.text
.align 2
.globl _rootsqz_model_predict
_rootsqz_model_predict:
    stp     x29, x30, [sp, #-16]!
    mov     x29, sp
    bl      {predict_fn}
    bl      _rootsqz_prob_squash
    ldp     x29, x30, [sp], #16
    ret

.globl _rootsqz_model_learn
_rootsqz_model_learn:
    b       {learn_fn}
"#,
        hash_mask = table_len - 1,
        table_bytes = table_len * NORDER_RECORD_BYTES,
    )
}

fn render_ln_mixer_model_assembly(table_pow2: u32) -> String {
    let table_len = 1usize << table_pow2;
    let child0_magic = model_hash(0, 2);
    let child1_mask = 0b0000_0011;
    let child1_magic = model_hash(child1_mask, 2);
    let child1_context_mask = byte_mask_to_context_mask(child1_mask as u8);
    let ctx_rows = 256usize * 255usize;
    let num_models = 2usize;
    let ctx_weight_bytes = ctx_rows * num_models * size_of::<f64>();

    format!(
        r#".section __DATA,__data
.p2align 3
.globl _rootsqz_model_ctx
_rootsqz_model_ctx:
    .long {num_models}
    .long 1
    .long 0
    .long 0
    .double 0.0
    .quad _rootsqz_mixer_child_contexts
    .quad _rootsqz_mixer_predict_fns
    .quad _rootsqz_mixer_learn_fns
    .quad _rootsqz_mixer_base_weights
    .quad _rootsqz_mixer_ctx_weights
    .quad _rootsqz_mixer_ctx_init
    .quad _rootsqz_mixer_last_p

.p2align 3
_rootsqz_mixer_child0:
    .long 0
    .long 1
    .long 0x{child0_magic:08x}
    .long 15
    .quad 0
    .quad 0
    .long 0
    .long 0
    .quad _rootsqz_mixer_table
    .quad {hash_mask}

.p2align 3
_rootsqz_mixer_child1:
    .long 0
    .long 1
    .long 0x{child1_magic:08x}
    .long 15
    .quad 0
    .quad 0x{child1_context_mask:016x}
    .long 0
    .long 0
    .quad _rootsqz_mixer_table
    .quad {hash_mask}

.p2align 3
_rootsqz_mixer_child_contexts:
    .quad _rootsqz_mixer_child0
    .quad _rootsqz_mixer_child1
_rootsqz_mixer_predict_fns:
    .quad _rootsqz_norder_byte_predict
    .quad _rootsqz_norder_byte_predict
_rootsqz_mixer_learn_fns:
    .quad _rootsqz_norder_byte_learn
    .quad _rootsqz_norder_byte_learn
_rootsqz_mixer_base_weights:
    .double 0.5
    .double 0.5
_rootsqz_mixer_last_p:
    .space 16

.p2align 2
_rootsqz_mixer_table:
.space {table_bytes}

.p2align 3
_rootsqz_mixer_ctx_init:
    .space {ctx_rows}
.p2align 3
_rootsqz_mixer_ctx_weights:
    .space {ctx_weight_bytes}

.text
.align 2
.globl _rootsqz_model_predict
_rootsqz_model_predict:
    stp     x29, x30, [sp, #-16]!
    mov     x29, sp
    bl      _rootsqz_ln_mixer_predict_stretched
    bl      _rootsqz_prob_squash
    ldp     x29, x30, [sp], #16
    ret

.globl _rootsqz_model_learn
_rootsqz_model_learn:
    b       _rootsqz_ln_mixer_learn
"#,
        hash_mask = table_len - 1,
        table_bytes = table_len * NORDER_RECORD_BYTES,
    )
}

fn model_hash(mut value: u32, shift: u32) -> u32 {
    value ^= value >> shift;
    0x9E35_A7BDu32.wrapping_mul(value) >> shift
}

fn byte_mask_to_context_mask(byte_mask: u8) -> u64 {
    let mut bit_mask = 0u64;
    for i in 0..8 {
        bit_mask |= ((byte_mask >> i) & 1) as u64 * (0xffu64 << (i * 8));
    }
    bit_mask
}

fn assert_success(action: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{action} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

const HARNESS_C: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern const uint8_t rootsqz_expected_start[];
extern const uint8_t rootsqz_expected_end[];
extern const uint64_t rootsqz_image_size;

uintptr_t rootsqz_model_ctx;

double rootsqz_model_predict(void *ctx) {
    (void)ctx;
    return 0.5;
}

void rootsqz_model_learn(void *ctx, uint32_t bit) {
    (void)ctx;
    (void)bit;
}

void *rootsqz_prepare_image(void) {
    uint8_t *output = calloc(1, (size_t)rootsqz_image_size);
    if (!output) {
        fprintf(stderr, "calloc failed\n");
        exit(1);
    }
    return output;
}

int rootsqz_launch_image(uint8_t *output, int argc, char **argv, char **envp) {
    (void)argc;
    (void)argv;
    (void)envp;
    const uint8_t *expected = rootsqz_expected_start;
    size_t expected_len = (size_t)(rootsqz_expected_end - rootsqz_expected_start);

    if ((size_t)rootsqz_image_size != expected_len) {
        fprintf(stderr, "decoded length mismatch: got %llu, expected %zu\n",
                (unsigned long long)rootsqz_image_size, expected_len);
        exit(1);
    }

    if (memcmp(output, expected, expected_len) != 0) {
        for (size_t i = 0; i < expected_len; i++) {
            if (output[i] != expected[i]) {
                fprintf(stderr,
                        "decoded byte mismatch at %zu: got 0x%02x, expected 0x%02x\n",
                        i, output[i], expected[i]);
                break;
            }
        }
        exit(1);
    }
    free(output);
    return 0;
}
"#;

const HARNESS_AFTER_DECODE_C: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern const uint8_t rootsqz_expected_start[];
extern const uint8_t rootsqz_expected_end[];
extern const uint64_t rootsqz_image_size;

void *rootsqz_prepare_image(void) {
    uint8_t *output = calloc(1, (size_t)rootsqz_image_size);
    if (!output) {
        fprintf(stderr, "calloc failed\n");
        exit(1);
    }
    return output;
}

int rootsqz_launch_image(uint8_t *output, int argc, char **argv, char **envp) {
    (void)argc;
    (void)argv;
    (void)envp;
    const uint8_t *expected = rootsqz_expected_start;
    size_t expected_len = (size_t)(rootsqz_expected_end - rootsqz_expected_start);

    if ((size_t)rootsqz_image_size != expected_len) {
        fprintf(stderr, "decoded length mismatch: got %llu, expected %zu\n",
                (unsigned long long)rootsqz_image_size, expected_len);
        exit(1);
    }

    if (memcmp(output, expected, expected_len) != 0) {
        for (size_t i = 0; i < expected_len; i++) {
            if (output[i] != expected[i]) {
                fprintf(stderr,
                        "decoded byte mismatch at %zu: got 0x%02x, expected 0x%02x\n",
                        i, output[i], expected[i]);
                break;
            }
        }
        exit(1);
    }
    free(output);
    return 0;
}
"#;
