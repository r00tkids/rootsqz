use std::path::Path;

use super::pack::CompressedMacho;

pub fn render_payload_assembly(compressed_path: &Path, packed: &CompressedMacho) -> String {
    let mut src = format!(
        r#".section __DATA,__const
.p2align 3
.globl _rootsqz_compressed_start
_rootsqz_compressed_start:
.incbin "{compressed_path}"
.globl _rootsqz_compressed_end
_rootsqz_compressed_end:

.p2align 3
.globl _rootsqz_image_size
_rootsqz_image_size:
    .quad {image_size}
.globl _rootsqz_entry_offset
_rootsqz_entry_offset:
    .quad {entry_offset}

.p2align 3
.globl _rootsqz_decode_chunks_start
_rootsqz_decode_chunks_start:
"#,
        compressed_path = escape_assembly_path(compressed_path),
        image_size = packed.image_size,
        entry_offset = packed.entry_offset,
    );

    for chunk in &packed.decode_chunks {
        src.push_str(&format!(
            "    .quad {offset}\n    .quad {size}\n",
            offset = chunk.offset,
            size = chunk.size,
        ));
    }
    src.push_str(
        r#".globl _rootsqz_decode_chunks_end
_rootsqz_decode_chunks_end:

.p2align 3
.globl _rootsqz_segments_start
_rootsqz_segments_start:
"#,
    );
    for segment in &packed.segments {
        src.push_str(&format!(
            "    .quad {offset}\n    .quad {size}\n    .long {init_prot}\n    .long 0\n",
            offset = segment.offset,
            size = segment.vm_size,
            init_prot = segment.init_prot,
        ));
    }
    src.push_str(
        r#".globl _rootsqz_segments_end
_rootsqz_segments_end:

.p2align 3
.globl _rootsqz_imports_start
_rootsqz_imports_start:
"#,
    );
    for import in &packed.imports {
        if import.weak {
            src.push_str(&format!(
                ".weak_reference {}\n",
                import.name,
            ));
        }
    }
    for import in &packed.imports {
        src.push_str(&format!(
            "    .quad {}\n    .long {weak}\n    .long 0\n",
            import.name,
            weak = if import.weak { 1 } else { 0 },
        ));
    }
    src.push_str(
        r#".globl _rootsqz_imports_end
_rootsqz_imports_end:
"#,
    );

    src.push_str(
        r#"
.p2align 3
.globl _rootsqz_fixups_start
_rootsqz_fixups_start:
"#,
    );
    for fixup in &packed.fixups {
        src.push_str(&format!(
            "    .quad {offset}\n    .quad {target}\n    .quad {addend}\n    .long {import_index}\n    .long {high8}\n    .long {kind}\n    .long 0\n",
            offset = fixup.offset,
            target = fixup.target,
            addend = fixup.addend,
            import_index = fixup.import_index,
            high8 = fixup.high8,
            kind = fixup.kind,
        ));
    }
    src.push_str(
        r#".globl _rootsqz_fixups_end
_rootsqz_fixups_end:
"#,
    );
    src
}

fn escape_assembly_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

