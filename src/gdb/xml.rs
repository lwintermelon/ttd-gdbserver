//! The XML blobs the stub serves through `qXfer` (library list, memory map),
//! plus the chunking every qXfer-style read shares.
//!
//! Kept out of `target.rs` because they are pure data → string functions:
//! self-contained, and unit-testable without a stub or a socket.

use gdbstub::target::TargetError;
use quick_xml::Writer;

use crate::target::ModuleInfo;

/// Copy the substring `[offset, offset+length)` of `data` into `buf`.
/// Returns `Ok(0)` when `offset` is past the end of `data` (gdb expects
/// the `l` terminator in that case).
///
/// Shared by every qXfer-style read (XML annexes, exec-file, auxv) so the
/// chunking semantics — clamp offset, saturate end, cap at the caller's
/// buffer — cannot drift between them.
pub fn write_xml_chunk<E>(
    data: &[u8],
    offset: u64,
    length: usize,
    buf: &mut [u8],
) -> Result<usize, TargetError<E>> {
    let start = (offset as usize).min(data.len());
    let end = start.saturating_add(length).min(data.len());
    if start == data.len() {
        return Ok(0);
    }
    let n = (end - start).min(buf.len());
    buf[..n].copy_from_slice(&data[start..start + n]);
    Ok(n)
}

/// Build a GDB library-list XML for the given modules.
///
/// Schema (from the gdbstub `Libraries` trait doc):
///   `<library-list version="1.0"><library name="…"><segment address="0x…"/></library>…</library-list>`
///
/// The `<segment address>` is the address the *first section* was loaded at
/// per the GDB manual. For TTD the only address we have is the module's
/// image base (`ModuleInstance.Address`); we report that. gdb's PE loader
/// reads the PE headers from this address, so symbols are found correctly
/// even though the address is technically the image base rather than the
/// first section. For full PE-fidelity we would need a per-section table
/// from TTD (not currently exposed), which can be added in a follow-up.
pub fn build_libraries_xml(modules: &[ModuleInfo]) -> String {
    let mut buf = Vec::with_capacity(64 + modules.len() * 96);
    let mut w = Writer::new(&mut buf);
    // The image base is what TTD gives us; the name is a fully-qualified
    // path on Windows. quick-xml handles the escaping.
    w.create_element("library-list")
        .with_attribute(("version", "1.0"))
        .write_inner_content(|w| {
            for m in modules {
                let addr = format!("0x{:x}", m.base_addr);
                w.create_element("library")
                    .with_attribute(("name", m.name.as_str()))
                    .write_inner_content(|w| {
                        w.create_element("segment")
                            .with_attribute(("address", addr.as_str()))
                            .write_empty()?;
                        Ok(())
                    })?;
            }
            Ok(())
        })
        .expect("writing to a Vec cannot fail");
    String::from_utf8(buf).expect("XML is valid UTF-8")
}

/// Build a GDB memory-map XML for the given modules.
///
/// Schema:
///   `<memory-map><memory type="ram" start="0x…" length="0x…"/>…</memory-map>`
///
/// We emit one `ram` region per loaded module covering `[base, base+size)`.
/// Querying the engine for every observed address range would be too
/// expensive (and meaningless for replay: the cursor's "memory at this
/// position" is the memory the trace recorded touching, which is the
/// module's image anyway). gdb's PE loader is happy with module-level
/// regions; finer granularity is not served anywhere.
pub fn build_memory_map_xml(modules: &[ModuleInfo]) -> String {
    let mut buf = Vec::with_capacity(64 + modules.len() * 80);
    let mut w = Writer::new(&mut buf);
    w.create_element("memory-map")
        .write_inner_content(|w| {
            for m in modules {
                let start = format!("0x{:x}", m.base_addr);
                let len = format!("0x{:x}", m.size);
                w.create_element("memory")
                    .with_attribute(("type", "ram"))
                    .with_attribute(("start", start.as_str()))
                    .with_attribute(("length", len.as_str()))
                    .write_empty()?;
            }
            Ok(())
        })
        .expect("writing to a Vec cannot fail");
    String::from_utf8(buf).expect("XML is valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::DebugError;

    /// `write_xml_chunk` is generic over the target error type; any
    /// `Debug`-able one will do for assertions.
    fn chunk(data: &[u8], offset: u64, length: usize, buf: &mut [u8]) -> usize {
        // `.ok()` first: `TargetError<E>` is not `Debug` for every E.
        write_xml_chunk::<DebugError>(data, offset, length, buf)
            .ok()
            .expect("chunk read cannot fail")
    }

    fn module(name: &str, base: u64, size: u64) -> ModuleInfo {
        ModuleInfo {
            base_addr: base,
            size,
            name: name.to_string(),
        }
    }

    /// A qXfer read is a slice of a byte string; the four cases below are the
    /// ones that silently corrupt payloads when wrong: exact fit, offset past
    /// the end (`l` terminator), a caller buffer smaller than the chunk, and
    /// a zero-length request.
    #[test]
    fn chunk_reads() {
        let data = b"0123456789";

        // Exact fit.
        let mut buf = [0u8; 10];
        assert_eq!(chunk(data, 0, 10, &mut buf), 10);
        assert_eq!(&buf, data);

        // Whole buffer requested but the caller's is smaller.
        let mut small = [0u8; 4];
        assert_eq!(chunk(data, 0, 10, &mut small), 4);
        assert_eq!(&small, b"0123");

        // Middle slice.
        let mut mid = [0u8; 3];
        assert_eq!(chunk(data, 4, 3, &mut mid), 3);
        assert_eq!(&mid, b"456");

        // Saturating end: 2 bytes left from offset 8.
        let mut tail = [0u8; 8];
        assert_eq!(chunk(data, 8, 8, &mut tail), 2);
        assert_eq!(&tail[..2], b"89");

        // Offset past the end -> 0 (gdb reads that as "last chunk").
        let mut none = [0u8; 4];
        assert_eq!(chunk(data, 10, 4, &mut none), 0);
        assert_eq!(chunk(data, 999, 4, &mut none), 0);

        // Zero bytes of payload: no data, no error.
        assert_eq!(chunk(b"", 0, 4, &mut none), 0);
    }

    #[test]
    fn libraries_xml_shape() {
        let xml = build_libraries_xml(&[
            module("C:\\Windows\\System32\\ntdll.dll", 0x140_000_000, 0x1000),
            module("C:\\path with space\\test.exe", 0x7ff6_0000_0000, 0x2000),
        ]);
        assert!(xml.starts_with("<library-list version=\"1.0\">"), "{xml}");
        assert!(xml.ends_with("</library-list>"), "{xml}");
        assert!(
            xml.contains("<library name=\"C:\\Windows\\System32\\ntdll.dll\">"),
            "{xml}"
        );
        assert!(xml.contains("<segment address=\"0x140000000\"/>"), "{xml}");
        assert!(
            xml.contains("<segment address=\"0x7ff600000000\"/>"),
            "{xml}"
        );
    }

    /// Module paths are attacker-controlled data on the wire only in the
    /// sense that they come from the trace; a name with XML metacharacters
    /// must not be able to break out of the attribute.
    #[test]
    fn libraries_xml_escapes_module_names() {
        let xml = build_libraries_xml(&[module("a&b<c>\"d\".dll", 0x1000, 0x10)]);
        assert!(
            xml.contains("a&amp;b&lt;c&gt;&quot;d&quot;.dll"),
            "module name must be XML-escaped: {xml}"
        );
        assert!(!xml.contains("a&b"), "raw ampersand leaked: {xml}");
    }

    #[test]
    fn memory_map_xml_shape() {
        let xml = build_memory_map_xml(&[module("a.dll", 0x1000, 0x5000)]);
        assert_eq!(
            xml,
            "<memory-map><memory type=\"ram\" start=\"0x1000\" length=\"0x5000\"/></memory-map>"
        );
    }

    /// An empty module list must still be a well-formed document — gdb parses
    /// it and simply shows nothing (a trace always has modules, but the stub
    /// must not emit a half-open tag if it ever sees none).
    #[test]
    fn empty_module_lists_are_valid_documents() {
        assert_eq!(
            build_libraries_xml(&[]),
            "<library-list version=\"1.0\"></library-list>"
        );
        assert_eq!(build_memory_map_xml(&[]), "<memory-map></memory-map>");
    }
}
