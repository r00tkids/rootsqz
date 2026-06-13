use anyhow::{bail, Result};

use super::model::{
    DyldInfoCommand, DylibCommand, EntryPointCommand, FileType, Flags, Header, LinkEditDataCommand,
    LoadCommand, MachoFile, Section, SegmentCommand, SymtabCommand, MH_CIGAM_64,
};

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_LOAD_DYLIB: u32 = 0xc;
const LC_UUID: u32 = 0x1b;
const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const LC_DYLD_INFO: u32 = 0x22;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_REEXPORT_DYLIB: u32 = 0x8000_001f;
const LC_LAZY_LOAD_DYLIB: u32 = 0x20;
const LC_LOAD_UPWARD_DYLIB: u32 = 0x8000_0023;
const LC_MAIN: u32 = 0x8000_0028;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;

const CPU_TYPE_ARM64: u32 = 0x0100_000C;

// ── public entry point ────────────────────────────────────────────────────────

pub fn parse(data: &[u8]) -> Result<MachoFile> {
    if data.len() < 4 {
        bail!("data too short to be a Mach-O file");
    }
    let raw_magic = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    if raw_magic != MH_CIGAM_64 {
        bail!("not an ARM64 Mach-O binary (magic: {:#010x})", raw_magic);
    }

    let mut r = Reader::new(data);
    let (header, ncmds) = parse_header(&mut r)?;
    let load_commands = parse_load_commands(&mut r, ncmds)?;

    Ok(MachoFile {
        header,
        load_commands,
    })
}

// ── internal parsing ──────────────────────────────────────────────────────────

fn parse_header(r: &mut Reader) -> Result<(Header, u32)> {
    let _magic = r.read_u32()?;
    let cpu_type = r.read_u32()?;
    if cpu_type != CPU_TYPE_ARM64 {
        bail!("expected ARM64 cpu type, got {:#010x}", cpu_type);
    }
    let cpu_subtype = r.read_u32()?;
    let file_type = FileType::from_raw(r.read_u32()?);
    let ncmds = r.read_u32()?;
    let _sizeofcmds = r.read_u32()?;
    let flags = Flags::from_bits_truncate(r.read_u32()?);
    let _reserved = r.read_u32()?;
    Ok((
        Header {
            cpu_subtype,
            file_type,
            flags,
        },
        ncmds,
    ))
}

fn parse_load_commands(r: &mut Reader, ncmds: u32) -> Result<Vec<LoadCommand>> {
    let mut commands = Vec::with_capacity(ncmds as usize);
    for _ in 0..ncmds {
        let cmd = r.read_u32()?;
        let cmdsize = r.read_u32()?;
        if cmdsize < 8 {
            bail!("invalid load command size {cmdsize}");
        }
        let payload = r.read_slice((cmdsize - 8) as usize)?;
        let mut pr = Reader::new(payload);

        let lc = match cmd {
            LC_SEGMENT_64 => parse_segment(&mut pr)?,
            LC_SYMTAB => parse_symtab(&mut pr)?,
            LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LAZY_LOAD_DYLIB
            | LC_LOAD_UPWARD_DYLIB => parse_dylib(cmd, payload)?,
            LC_DYLD_INFO | LC_DYLD_INFO_ONLY => parse_dyld_info(&mut pr)?,
            LC_UUID => parse_uuid(&mut pr)?,
            LC_MAIN => parse_entry_point(&mut pr)?,
            LC_DYLD_CHAINED_FIXUPS => parse_chained_fixups(&mut pr)?,
            LC_CODE_SIGNATURE => parse_link_edit_data(&mut pr)?,
            _ => LoadCommand::Raw {
                cmd,
                data: payload.to_vec(),
            },
        };
        commands.push(lc);
    }
    Ok(commands)
}

fn parse_segment(r: &mut Reader) -> Result<LoadCommand> {
    let name = read_c_string(&r.read_fixed::<16>()?);
    let vm_addr = r.read_u64()?;
    let vm_size = r.read_u64()?;
    let file_offset = r.read_u64()?;
    let file_size = r.read_u64()?;
    let max_prot = r.read_u32()?;
    let init_prot = r.read_u32()?;
    let nsects = r.read_u32()?;
    let _seg_flags = r.read_u32()?;

    let mut sections = Vec::with_capacity(nsects as usize);
    for _ in 0..nsects {
        sections.push(parse_section(r)?);
    }

    Ok(LoadCommand::Segment(SegmentCommand {
        name,
        vm_addr,
        vm_size,
        file_offset,
        file_size,
        max_prot,
        init_prot,
        sections,
    }))
}

fn parse_section(r: &mut Reader) -> Result<Section> {
    let name = read_c_string(&r.read_fixed::<16>()?);
    let segment_name = read_c_string(&r.read_fixed::<16>()?);
    let addr = r.read_u64()?;
    let size = r.read_u64()?;
    let offset = r.read_u32()?;
    let align = r.read_u32()?;
    let _reloff = r.read_u32()?;
    let _nreloc = r.read_u32()?;
    let flags = r.read_u32()?;
    r.skip(12)?; // reserved1 + reserved2 + reserved3
    Ok(Section {
        name,
        segment_name,
        addr,
        size,
        offset,
        align,
        flags,
    })
}

fn parse_symtab(r: &mut Reader) -> Result<LoadCommand> {
    Ok(LoadCommand::SymbolTable(SymtabCommand {
        sym_offset: r.read_u32()?,
        nsyms: r.read_u32()?,
        str_offset: r.read_u32()?,
        str_size: r.read_u32()?,
    }))
}

fn parse_dylib(cmd: u32, payload: &[u8]) -> Result<LoadCommand> {
    let mut r = Reader::new(payload);
    let name_offset = r.read_u32()? as usize;
    r.skip(12)?; // timestamp + current_version + compatibility_version

    if name_offset < 8 {
        bail!("invalid dylib name offset {name_offset}");
    }
    let payload_name_offset = name_offset - 8;
    if payload_name_offset >= payload.len() {
        bail!("dylib name offset {name_offset} is outside load command");
    }

    Ok(LoadCommand::Dylib(DylibCommand {
        name: read_c_string(&payload[payload_name_offset..]),
        weak: cmd == LC_LOAD_WEAK_DYLIB,
    }))
}

fn parse_dyld_info(r: &mut Reader) -> Result<LoadCommand> {
    let rebase_offset = r.read_u32()?;
    let rebase_size = r.read_u32()?;
    let bind_offset = r.read_u32()?;
    let bind_size = r.read_u32()?;
    let weak_bind_offset = r.read_u32()?;
    let weak_bind_size = r.read_u32()?;
    let lazy_bind_offset = r.read_u32()?;
    let lazy_bind_size = r.read_u32()?;
    let export_offset = r.read_u32()?;
    let export_size = r.read_u32()?;
    Ok(LoadCommand::DyldInfo(DyldInfoCommand {
        rebase_offset,
        rebase_size,
        bind_offset,
        bind_size,
        weak_bind_offset,
        weak_bind_size,
        lazy_bind_offset,
        lazy_bind_size,
        export_offset,
        export_size,
    }))
}

fn parse_uuid(r: &mut Reader) -> Result<LoadCommand> {
    Ok(LoadCommand::Uuid(r.read_fixed::<16>()?))
}

fn parse_entry_point(r: &mut Reader) -> Result<LoadCommand> {
    Ok(LoadCommand::EntryPoint(EntryPointCommand {
        entry_offset: r.read_u64()?,
        stack_size: r.read_u64()?,
    }))
}

fn parse_link_edit_data(r: &mut Reader) -> Result<LoadCommand> {
    Ok(LoadCommand::CodeSignature(LinkEditDataCommand {
        data_offset: r.read_u32()?,
        data_size: r.read_u32()?,
    }))
}

fn parse_chained_fixups(r: &mut Reader) -> Result<LoadCommand> {
    Ok(LoadCommand::ChainedFixups(LinkEditDataCommand {
        data_offset: r.read_u32()?,
        data_size: r.read_u32()?,
    }))
}

// ── byte reader ───────────────────────────────────────────────────────────────

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read_fixed::<4>()?))
    }

    fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.read_fixed::<8>()?))
    }

    fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.pos + N > self.data.len() {
            bail!("unexpected end of data at offset {}", self.pos);
        }
        let mut arr = [0u8; N];
        arr.copy_from_slice(&self.data[self.pos..self.pos + N]);
        self.pos += N;
        Ok(arr)
    }

    fn read_slice(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            bail!("unexpected end of data at offset {}", self.pos);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        if self.pos + n > self.data.len() {
            bail!("unexpected end of data at offset {}", self.pos);
        }
        self.pos += n;
        Ok(())
    }
}

fn read_c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::super::model::{FileType, LoadCommand};
    use super::*;

    #[test]
    fn parse_helloworld() {
        let data = std::fs::read("tests/macho/helloworld").unwrap();
        let macho = parse(&data).unwrap();

        assert_eq!(macho.header.file_type, FileType::Execute);

        let segments: Vec<&SegmentCommand> = macho
            .load_commands
            .iter()
            .filter_map(|lc| {
                if let LoadCommand::Segment(s) = lc {
                    Some(s)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(segments.len(), 4);
        assert_eq!(segments[0].name, "__PAGEZERO");
        assert_eq!(segments[1].name, "__TEXT");
        assert_eq!(segments[2].name, "__DATA");
        assert_eq!(segments[3].name, "__LINKEDIT");

        let text = &segments[1];
        assert_eq!(text.sections.len(), 3);
        assert_eq!(text.sections[0].name, "__text");
        assert_eq!(text.sections[1].name, "__stubs");
        assert_eq!(text.sections[2].name, "__cstring");

        let uuid = macho
            .load_commands
            .iter()
            .find_map(|lc| {
                if let LoadCommand::Uuid(u) = lc {
                    Some(u)
                } else {
                    None
                }
            })
            .expect("UUID load command not found");
        assert_eq!(
            *uuid,
            [
                0xE3, 0x40, 0xC2, 0x5E, 0x5E, 0x04, 0x31, 0xB4, 0xAD, 0x2E, 0x75, 0xCC, 0x93, 0x04,
                0x22, 0x00
            ],
        );

        let has_entry_point = macho
            .load_commands
            .iter()
            .any(|lc| matches!(lc, LoadCommand::EntryPoint(_)));
        assert!(has_entry_point);

        let has_code_sig = macho
            .load_commands
            .iter()
            .any(|lc| matches!(lc, LoadCommand::CodeSignature(_)));
        assert!(has_code_sig);

        let chained_fixups = macho
            .load_commands
            .iter()
            .find_map(|lc| {
                if let LoadCommand::ChainedFixups(fixups) = lc {
                    Some(fixups)
                } else {
                    None
                }
            })
            .expect("LC_DYLD_CHAINED_FIXUPS load command not found");
        assert_eq!(chained_fixups.data_offset, 32768);
        assert_eq!(chained_fixups.data_size, 96);
    }
}
