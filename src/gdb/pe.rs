//! Minimal PE section-table reader for `qOffsets`.
//!
//! GDB's `qOffsets` `Segments` form wants the **absolute address of the text
//! segment**, not the image base: it relocates that segment to the reported
//! address and keeps the rest of the image at fixed offsets. Reporting the
//! image base puts every symbol one section alignment (0x1000) low. The
//! section table gives us the real `.text` RVA; the module path in the trace
//! is normally the recorded executable, so it can be read from disk.
//!
//! Only the first few KiB are read (DOS header, PE/COFF header, optional
//! header, section table); a missing or foreign file falls back to the
//! standard 0x1000 text RVA at the call site.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const DOS_HEADER_LEN: usize = 64;
const PE_SIGNATURE: &[u8; 4] = b"PE\0\0";
const COFF_HEADER_LEN: usize = 20;
const SECTION_HEADER_LEN: usize = 40;
const MAX_SECTIONS: usize = 96;
const MAX_OPTIONAL_HEADER: usize = 0x1000;
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

/// Conventional PE `.text` RVA, used when the module file is unavailable.
pub(crate) const DEFAULT_TEXT_RVA: u32 = 0x1000;

/// The `.text` section RVA of `path`, if it is a readable PE file.
pub(crate) fn text_rva(path: &Path) -> Option<u32> {
    let mut file = File::open(path).ok()?;
    let mut dos = [0u8; DOS_HEADER_LEN];
    file.read_exact(&mut dos).ok()?;
    let pe_offset = usize::try_from(read_u32(&dos, 0x3c)?).ok()?;
    // The PE header, COFF header, optional header and section table must all
    // fit in a small bounded read; reject absurd/truncated files early.
    file.seek(SeekFrom::Start(u64::try_from(pe_offset).ok()?))
        .ok()?;
    let mut header = vec![0u8; PE_SIGNATURE.len() + COFF_HEADER_LEN];
    file.read_exact(&mut header).ok()?;
    if &header[..4] != PE_SIGNATURE {
        return None;
    }
    let coff = 4;
    let section_count = usize::from(read_u16(&header, coff + 2)?);
    let optional_size = usize::from(read_u16(&header, coff + 16)?);
    if section_count == 0 || section_count > MAX_SECTIONS || optional_size > MAX_OPTIONAL_HEADER {
        return None;
    }
    let table_offset =
        pe_offset.checked_add(PE_SIGNATURE.len() + COFF_HEADER_LEN + optional_size)?;
    file.seek(SeekFrom::Start(u64::try_from(table_offset).ok()?))
        .ok()?;
    let mut table = vec![0u8; section_count * SECTION_HEADER_LEN];
    file.read_exact(&mut table).ok()?;
    text_rva_from_section_table(&table, section_count)
}

/// Parse section headers from an already-read table.
fn text_rva_from_section_table(table: &[u8], section_count: usize) -> Option<u32> {
    let mut executable = None;
    for index in 0..section_count {
        let base = index * SECTION_HEADER_LEN;
        let header = table.get(base..base + SECTION_HEADER_LEN)?;
        let name = &header[..8];
        let virtual_size = read_u32(header, 8)?;
        let virtual_address = read_u32(header, 12)?;
        let characteristics = read_u32(header, 36)?;
        if virtual_size == 0 || virtual_address == 0 {
            continue;
        }
        if name.starts_with(b".text") {
            return Some(virtual_address);
        }
        if executable.is_none() && characteristics & IMAGE_SCN_MEM_EXECUTE != 0 {
            executable = Some(virtual_address);
        }
    }
    executable
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal PE image with one executable section.
    fn fake_pe(section_name: [u8; 8], virtual_address: u32) -> Vec<u8> {
        const PE_OFFSET: usize = 0x80;
        let mut bytes = vec![0u8; PE_OFFSET + 24 + SECTION_HEADER_LEN];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(
            &u32::try_from(PE_OFFSET)
                .expect("test PE offset fits u32")
                .to_le_bytes(),
        );
        bytes[PE_OFFSET..PE_OFFSET + 4].copy_from_slice(PE_SIGNATURE);
        let coff = PE_OFFSET + 4;
        bytes[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        bytes[coff + 16..coff + 18].copy_from_slice(&0u16.to_le_bytes());
        let section = PE_OFFSET + 24;
        bytes[section..section + 8].copy_from_slice(&section_name);
        bytes[section + 8..section + 12].copy_from_slice(&0x1000u32.to_le_bytes());
        bytes[section + 12..section + 16].copy_from_slice(&virtual_address.to_le_bytes());
        bytes[section + 36..section + 40].copy_from_slice(&IMAGE_SCN_MEM_EXECUTE.to_le_bytes());
        bytes
    }

    fn parse(bytes: &[u8]) -> Option<u32> {
        let pe_offset = usize::try_from(read_u32(bytes, 0x3c)?).ok()?;
        let coff = pe_offset + 4;
        let section_count = usize::from(read_u16(bytes, coff + 2)?);
        let optional_size = usize::from(read_u16(bytes, coff + 16)?);
        let table = pe_offset + 24 + optional_size;
        text_rva_from_section_table(&bytes[table..], section_count)
    }

    #[test]
    fn finds_the_text_section() {
        let image = fake_pe(*b".text\0\0\0", 0x1000);
        assert_eq!(parse(&image), Some(0x1000));
    }

    #[test]
    fn falls_back_to_an_executable_section_without_the_text_name() {
        let image = fake_pe(*b".code\0\0\0", 0x2000);
        assert_eq!(parse(&image), Some(0x2000));
    }

    #[test]
    fn rejects_non_pe_and_truncated_images() {
        assert_eq!(parse(b"not a pe"), None);
        let mut image = fake_pe(*b".text\0\0\0", 0x1000);
        image.truncate(0x90);
        assert_eq!(parse(&image), None);
    }
}
