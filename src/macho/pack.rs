use anyhow::{bail, Context, Result};

use crate::compressor::{model::Model, Encoder};

use super::{
    fixups::{parse_chained_fixups, PackedFixup, PackedImport},
    model::{self, FileType, Flags, LoadCommand, SegmentCommand},
    parser,
};

const PAGE_SIZE: u64 = 0x4000;

#[derive(Debug)]
pub struct CompressedMacho {
    pub compressed: Vec<u8>,
    pub uncompressed: Vec<u8>,
    pub image_size: u64,
    pub entry_offset: u64,
    pub decode_chunks: Vec<DecodeChunk>,
    pub segments: Vec<PackedSegment>,
    pub imports: Vec<PackedImport>,
    pub dylibs: Vec<String>,
    pub fixups: Vec<PackedFixup>,
}

#[derive(Debug)]
pub struct PackedSegment {
    pub name: String,
    pub size: usize,
    pub offset: u64,
    pub vm_size: u64,
    pub init_prot: u32,
}

#[derive(Debug)]
pub struct DecodeChunk {
    pub offset: u64,
    pub size: usize,
}

pub fn compress_binary_with_model(binary: &[u8], model: Box<dyn Model>) -> Result<CompressedMacho> {
    let macho = parser::parse(&binary)?;
    validate_supported_macho(&macho)?;

    let macho_segments: Vec<&SegmentCommand> = macho
        .load_commands
        .iter()
        .filter_map(|lc| {
            if let LoadCommand::Segment(seg) = lc {
                Some(seg)
            } else {
                None
            }
        })
        .collect();

    let image_segments: Vec<&SegmentCommand> = macho_segments
        .iter()
        .copied()
        .filter(|seg| seg.vm_size > 0 && seg.name != "__PAGEZERO" && seg.name != "__LINKEDIT")
        .collect();
    if image_segments.is_empty() {
        bail!("No loadable app segments found in the binary");
    }
    if !image_segments.iter().any(|seg| seg.name == "__TEXT") {
        bail!("Unsupported Mach-O: missing __TEXT segment");
    }

    let min_vm_addr = image_segments
        .iter()
        .map(|seg| seg.vm_addr)
        .min()
        .expect("image segments checked above");
    let max_vm_addr = image_segments
        .iter()
        .map(|seg| seg.vm_addr.saturating_add(seg.vm_size))
        .max()
        .expect("image segments checked above");
    let image_size = align_up(max_vm_addr - min_vm_addr, PAGE_SIZE);
    let entry_offset = entry_offset(&macho, &macho_segments, min_vm_addr)?;

    let mut decode_sources: Vec<(u64, u64, String, &[u8])> = Vec::new();
    let mut packed_segments = Vec::new();
    for seg in &image_segments {
        let offset = seg.vm_addr - min_vm_addr;
        packed_segments.push(PackedSegment {
            name: seg.name.clone(),
            size: seg.file_size as usize,
            offset,
            vm_size: seg.vm_size,
            init_prot: seg.init_prot,
        });

        if seg.file_size == 0 {
            continue;
        }
        let start = seg.file_offset as usize;
        let end = start
            .checked_add(seg.file_size as usize)
            .context("Mach-O segment file range overflowed usize")?;
        let data = binary
            .get(start..end)
            .with_context(|| format!("Segment {} extends past end of file", seg.name))?;
        decode_sources.push((seg.file_offset, offset, seg.name.clone(), data));
    }

    decode_sources.sort_by_key(|(file_offset, _, _, _)| *file_offset);
    let decode_chunks = decode_sources
        .iter()
        .map(|(_, offset, _, data)| DecodeChunk {
            offset: *offset,
            size: data.len(),
        })
        .collect();

    if decode_sources.is_empty() {
        bail!("No compressible segments found in the binary");
    }

    let (mut imports, fixups) = parse_chained_fixups(binary, &macho, &macho_segments, min_vm_addr)?;
    for import in &mut imports {
        import.name = import.name.clone();
    }
    let dylibs = macho
        .load_commands
        .iter()
        .filter_map(|lc| {
            if let LoadCommand::Dylib(dylib) = lc {
                Some(dylib.name.clone())
            } else {
                None
            }
        })
        .collect();

    let mut compressed: Vec<u8> = Vec::new();
    let mut uncompressed = Vec::with_capacity(
        decode_sources
            .iter()
            .map(|(_, _, _, data)| data.len())
            .sum(),
    );
    let mut encoder = Encoder::new(model, &mut compressed)?;

    for (_, _, _, data) in &decode_sources {
        encoder.encode_section(*data)?;
        uncompressed.extend_from_slice(data);
    }
    encoder.finish()?;

    Ok(CompressedMacho {
        compressed,
        uncompressed,
        image_size,
        entry_offset,
        decode_chunks,
        segments: packed_segments,
        imports,
        dylibs,
        fixups,
    })
}

fn validate_supported_macho(macho: &model::MachoFile) -> Result<()> {
    if macho.header.file_type != FileType::Execute {
        bail!("Unsupported Mach-O: expected MH_EXECUTE");
    }
    if !macho.header.flags.contains(Flags::PIE) {
        bail!("Unsupported Mach-O: only PIE executables are supported");
    }
    if !macho
        .load_commands
        .iter()
        .any(|lc| matches!(lc, LoadCommand::EntryPoint(_)))
    {
        bail!("Unsupported Mach-O: missing LC_MAIN entry point");
    }
    for lc in &macho.load_commands {
        if let LoadCommand::DyldInfo(info) = lc {
            if info.rebase_size != 0
                || info.bind_size != 0
                || info.weak_bind_size != 0
                || info.lazy_bind_size != 0
            {
                bail!("Unsupported Mach-O: classic LC_DYLD_INFO fixups are not supported");
            }
        }
    }
    Ok(())
}

fn entry_offset(
    macho: &model::MachoFile,
    segments: &[&SegmentCommand],
    min_vm_addr: u64,
) -> Result<u64> {
    let entry_file_offset = macho
        .load_commands
        .iter()
        .find_map(|lc| {
            if let LoadCommand::EntryPoint(entry) = lc {
                Some(entry.entry_offset)
            } else {
                None
            }
        })
        .context("Unsupported Mach-O: missing LC_MAIN entry point")?;

    let entry_segment = segments
        .iter()
        .find(|seg| {
            entry_file_offset >= seg.file_offset
                && entry_file_offset < seg.file_offset.saturating_add(seg.file_size)
        })
        .context("Unsupported Mach-O: LC_MAIN entry point is outside file-backed segments")?;

    Ok(entry_segment.vm_addr + (entry_file_offset - entry_segment.file_offset) - min_vm_addr)
}

fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

