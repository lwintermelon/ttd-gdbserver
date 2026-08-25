//! x86-64 `Arch` implementation for the GDB RSP frontend.
//!
//! The register file reuses `gdbstub_arch::x86::reg::X86_64CoreRegs` (GPRs +
//! x87 + SSE, serialized in GDB's standard document order) and extends it with
//! `fs_base`/`gs_base` (the `64bit-seg.xml` feature). The target description is
//! embedded at compile time from the XML files in this directory (GDB's
//! `gdb/features/i386/` files, the same ones rr ships): `target.xml`
//! references `64bit-core.xml`, `64bit-sse.xml` and `64bit-seg.xml` via
//! `<xi:include>`.
//!
//! `gs_base` is filled from the TTD TEB address — on Windows x64 the GS base is
//! the TEB, which is what Delve needs to find the goroutine `g` without writing
//! into tracee memory. x87/SSE state comes from `AMD64_CONTEXT.FltSave` via the
//! `TtdX64Regs` projection.

use std::num::NonZeroUsize;

use gdbstub::arch::{Arch, RegId, Registers};
use gdbstub_arch::x86::reg::id::{X86_64CoreRegId, X86SegmentRegId, X87FpuInternalRegId};
use gdbstub_arch::x86::reg::{X86_64CoreRegs, X86SegmentRegs, X87FpuInternalRegs};

use crate::ttd::types::TtdX64Regs;

/// Total size of the `g` buffer: `X86_64CoreRegs` (536) + fs_base + gs_base.
pub const G_BUFFER_SIZE: usize = 536 + 8 + 8;

// ─── Target description XML ──────────────────────────────────────

// The GDB target description files (copied from `gdb/features/i386/`, the
// same ones rr ships) are embedded at compile time — no runtime files needed.
const TARGET_XML: &str = include_str!("target.xml");
const CORE_XML: &str = include_str!("64bit-core.xml");
const SSE_XML: &str = include_str!("64bit-sse.xml");
const SEG_XML: &str = include_str!("64bit-seg.xml");

/// Target description XML by annex name (for `TargetDescriptionXmlOverride`).
/// The empty annex means `target.xml`.
pub(crate) fn target_xml(annex: &str) -> Result<&'static [u8], String> {
    let xml: &'static str = match annex {
        "" | "target.xml" => TARGET_XML,
        "64bit-core.xml" => CORE_XML,
        "64bit-sse.xml" => SSE_XML,
        "64bit-seg.xml" => SEG_XML,
        _ => return Err(annex.to_string()),
    };
    Ok(xml.as_bytes())
}

// ─── Arch ────────────────────────────────────────────────────────

/// Zero-variant enum used only at the type level.
pub enum TtdArch {}

impl Arch for TtdArch {
    type Usize = u64;
    type Registers = TtdRegisters;
    type BreakpointKind = usize;
    type RegId = TtdRegId;

    fn target_description_xml() -> Option<&'static str> {
        // Served via `TargetDescriptionXmlOverride` instead: the description
        // is split across multiple files (`<xi:include>`), which the
        // single-string `Arch` hook cannot express.
        None
    }
}

/// The register file: GDB's x86-64 core+SSE layout (via gdbstub_arch) plus
/// `fs_base`/`gs_base` (the `64bit-seg.xml` feature).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TtdRegisters {
    pub core: X86_64CoreRegs,
    pub fs_base: u64,
    pub gs_base: u64,
}

impl TtdRegisters {
    /// Build from a TTD register snapshot + TEB address.
    pub fn from_ttd(regs: &TtdX64Regs, teb: u64) -> Self {
        let mut st = [[0u8; 10]; 8];
        for (i, slot) in st.iter_mut().enumerate() {
            slot.copy_from_slice(&regs.st[i * 10..i * 10 + 10]);
        }
        let mut xmm = [0u128; 16];
        for (i, slot) in xmm.iter_mut().enumerate() {
            *slot = u128::from_le_bytes(regs.xmm[i * 16..i * 16 + 16].try_into().unwrap());
        }
        TtdRegisters {
            core: X86_64CoreRegs {
                regs: [
                    regs.rax, regs.rbx, regs.rcx, regs.rdx, regs.rsi, regs.rdi, regs.rbp, regs.rsp,
                    regs.r8, regs.r9, regs.r10, regs.r11, regs.r12, regs.r13, regs.r14, regs.r15,
                ],
                eflags: regs.eflags,
                rip: regs.rip,
                segments: X86SegmentRegs {
                    cs: regs.cs as u32,
                    ss: regs.ss as u32,
                    ds: regs.ds as u32,
                    es: regs.es as u32,
                    fs: regs.fs as u32,
                    gs: regs.gs as u32,
                },
                st,
                fpu: X87FpuInternalRegs {
                    fctrl: regs.fctrl as u32,
                    fstat: regs.fstat as u32,
                    ftag: regs.ftag as u32,
                    fiseg: regs.fiseg as u32,
                    fioff: regs.fioff,
                    foseg: regs.foseg as u32,
                    fooff: regs.fooff,
                    fop: regs.fop as u32,
                },
                xmm,
                mxcsr: regs.mxcsr,
            },
            fs_base: 0,
            gs_base: teb,
        }
    }
}

impl Registers for TtdRegisters {
    type ProgramCounter = u64;

    fn pc(&self) -> Self::ProgramCounter {
        self.core.rip
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        self.core.gdb_serialize(&mut write_byte);
        for b in self.fs_base.to_le_bytes() {
            write_byte(Some(b));
        }
        for b in self.gs_base.to_le_bytes() {
            write_byte(Some(b));
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() < G_BUFFER_SIZE {
            return Err(());
        }
        self.core.gdb_deserialize(&bytes[..536])?;
        self.fs_base = u64::from_le_bytes(bytes[536..544].try_into().unwrap());
        self.gs_base = u64::from_le_bytes(bytes[544..552].try_into().unwrap());
        Ok(())
    }
}

// ─── RegId ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum TtdRegId {
    Core(X86_64CoreRegId),
    FsBase,
    GsBase,
}

impl RegId for TtdRegId {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        if id < 57 {
            let (reg, size) = X86_64CoreRegId::from_raw_id(id)?;
            Some((TtdRegId::Core(reg), size))
        } else {
            match id {
                57 => Some((TtdRegId::FsBase, NonZeroUsize::new(8))),
                58 => Some((TtdRegId::GsBase, NonZeroUsize::new(8))),
                _ => None,
            }
        }
    }

    fn to_raw_id(&self) -> Option<usize> {
        match self {
            TtdRegId::Core(reg) => match reg {
                X86_64CoreRegId::Gpr(i) => Some(*i as usize),
                X86_64CoreRegId::Rip => Some(16),
                X86_64CoreRegId::Eflags => Some(17),
                X86_64CoreRegId::Segment(s) => Some(18 + seg_regnum(*s)),
                X86_64CoreRegId::St(i) => Some(24 + *i as usize),
                X86_64CoreRegId::Fpu(f) => Some(32 + fpu_regnum(*f)),
                X86_64CoreRegId::Xmm(i) => Some(40 + *i as usize),
                X86_64CoreRegId::Mxcsr => Some(56),
                _ => None,
            },
            TtdRegId::FsBase => Some(57),
            TtdRegId::GsBase => Some(58),
        }
    }
}

fn seg_regnum(s: X86SegmentRegId) -> usize {
    use gdbstub_arch::x86::reg::id::X86SegmentRegId::*;
    match s {
        CS => 0,
        SS => 1,
        DS => 2,
        ES => 3,
        FS => 4,
        GS => 5,
    }
}

fn fpu_regnum(f: X87FpuInternalRegId) -> usize {
    use gdbstub_arch::x86::reg::id::X87FpuInternalRegId::*;
    match f {
        Fctrl => 0,
        Fstat => 1,
        Ftag => 2,
        Fiseg => 3,
        Fioff => 4,
        Foseg => 5,
        Fooff => 6,
        Fop => 7,
    }
}

/// Serialize one register into `buf` for GDB's `p` packet.
///
/// Returns the register's width in bytes capped at `buf.len()` (GDB `p`
/// semantics: a short buffer truncates the register), or `None` for an
/// unknown register id. `st0..st7` (10 bytes) and `xmm0..xmm15` (16 bytes)
/// are byte arrays; every other register fits in a `u64`.
pub(crate) fn read_register_value(
    reg_id: TtdRegId,
    regs: &TtdRegisters,
    buf: &mut [u8],
) -> Option<usize> {
    let (value, size): (u64, usize) = match reg_id {
        TtdRegId::Core(X86_64CoreRegId::Gpr(i)) => (regs.core.regs[i as usize], 8),
        TtdRegId::Core(X86_64CoreRegId::Rip) => (regs.core.rip, 8),
        TtdRegId::Core(X86_64CoreRegId::Eflags) => (regs.core.eflags as u64, 4),
        TtdRegId::Core(X86_64CoreRegId::Segment(s)) => {
            let v = match s {
                X86SegmentRegId::CS => regs.core.segments.cs,
                X86SegmentRegId::SS => regs.core.segments.ss,
                X86SegmentRegId::DS => regs.core.segments.ds,
                X86SegmentRegId::ES => regs.core.segments.es,
                X86SegmentRegId::FS => regs.core.segments.fs,
                X86SegmentRegId::GS => regs.core.segments.gs,
            };
            (v as u64, 4)
        }
        TtdRegId::Core(X86_64CoreRegId::St(i)) => {
            let n = buf.len().min(10);
            buf[..n].copy_from_slice(&regs.core.st[i as usize][..n]);
            return Some(n);
        }
        TtdRegId::Core(X86_64CoreRegId::Fpu(f)) => {
            let v = match f {
                X87FpuInternalRegId::Fctrl => regs.core.fpu.fctrl,
                X87FpuInternalRegId::Fstat => regs.core.fpu.fstat,
                X87FpuInternalRegId::Ftag => regs.core.fpu.ftag,
                X87FpuInternalRegId::Fiseg => regs.core.fpu.fiseg,
                X87FpuInternalRegId::Fioff => regs.core.fpu.fioff,
                X87FpuInternalRegId::Foseg => regs.core.fpu.foseg,
                X87FpuInternalRegId::Fooff => regs.core.fpu.fooff,
                X87FpuInternalRegId::Fop => regs.core.fpu.fop,
            };
            (v as u64, 4)
        }
        TtdRegId::Core(X86_64CoreRegId::Xmm(i)) => {
            let n = buf.len().min(16);
            buf[..n].copy_from_slice(&regs.core.xmm[i as usize].to_le_bytes()[..n]);
            return Some(n);
        }
        TtdRegId::Core(X86_64CoreRegId::Mxcsr) => (regs.core.mxcsr as u64, 4),
        TtdRegId::FsBase => (regs.fs_base, 8),
        TtdRegId::GsBase => (regs.gs_base, 8),
        // X86_64CoreRegId is #[non_exhaustive]; unknown variants cannot be
        // produced by from_raw_id, so this is only a safety valve.
        TtdRegId::Core(_) => return None,
    };
    let size = buf.len().min(size);
    buf[..size].copy_from_slice(&value.to_le_bytes()[..size]);
    Some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> TtdX64Regs {
        let mut st = [0u8; 80];
        st[0] = 1; // st0 byte 0
        st[10 + 9] = 0xFF; // st1 byte 9
        let mut xmm = [0u8; 256];
        xmm[0] = 0xAA; // xmm0 byte 0
        xmm[16] = 0xBB; // xmm1 byte 0
        TtdX64Regs {
            rax: 0x1111,
            r15: 0xFFFF,
            rip: 0x1_0000,
            eflags: 0x202,
            cs: 0x33,
            gs: 0x2B,
            st,
            xmm,
            ..Default::default()
        }
    }

    #[test]
    fn from_ttd_copies_gprs_segments_x87_and_sse() {
        let source = snapshot();
        let regs = TtdRegisters::from_ttd(&source, 0x7FFE_0000);

        assert_eq!(regs.core.regs[0], 0x1111, "rax");
        assert_eq!(regs.core.regs[15], 0xFFFF, "r15");
        assert_eq!(regs.core.rip, 0x1_0000);
        assert_eq!(regs.core.eflags, 0x202);
        assert_eq!(regs.core.segments.cs, 0x33);
        assert_eq!(regs.core.segments.gs, 0x2B);
        assert_eq!(regs.core.st[0][0], 1, "st0 byte 0");
        assert_eq!(regs.core.st[1][9], 0xFF, "st1 byte 9");
        assert_eq!(regs.core.xmm[0] & 0xFF, 0xAA, "xmm0 byte 0");
        assert_eq!(regs.core.xmm[1] & 0xFF, 0xBB, "xmm1 byte 0");
        assert_eq!(regs.gs_base, 0x7FFE_0000, "gs_base is the TEB");
        assert_eq!(regs.fs_base, 0);
    }

    #[test]
    fn read_single_register_respects_width_and_short_buffers() {
        let regs = TtdRegisters::from_ttd(&snapshot(), 0x7FFE_0000);

        let mut buf = [0u8; 8];
        assert_eq!(
            read_register_value(TtdRegId::Core(X86_64CoreRegId::Rip), &regs, &mut buf),
            Some(8)
        );
        assert_eq!(u64::from_le_bytes(buf), 0x1_0000);

        // Eflags is a 32-bit register: exactly 4 bytes are written.
        let mut buf = [0u8; 8];
        assert_eq!(
            read_register_value(TtdRegId::Core(X86_64CoreRegId::Eflags), &regs, &mut buf),
            Some(4)
        );
        assert_eq!(u32::from_le_bytes(buf[..4].try_into().unwrap()), 0x202);

        // GDB `p` semantics: a short buffer truncates the register.
        let mut buf = [0u8; 2];
        assert_eq!(
            read_register_value(TtdRegId::Core(X86_64CoreRegId::Rip), &regs, &mut buf),
            Some(2)
        );

        // Byte-array registers keep their raw order (st1 byte 9 survives).
        let mut buf = [0u8; 10];
        assert_eq!(
            read_register_value(TtdRegId::Core(X86_64CoreRegId::St(1)), &regs, &mut buf),
            Some(10)
        );
        assert_eq!(buf[9], 0xFF);

        // XMM is serialized little-endian.
        let mut buf = [0u8; 16];
        assert_eq!(
            read_register_value(TtdRegId::Core(X86_64CoreRegId::Xmm(1)), &regs, &mut buf),
            Some(16)
        );
        assert_eq!(buf[0], 0xBB);
    }

    #[test]
    fn extended_registers_read_from_their_ttd_source() {
        let regs = TtdRegisters::from_ttd(&TtdX64Regs::default(), 0xDEAD_BEEF);

        let mut buf = [0u8; 8];
        assert_eq!(
            read_register_value(TtdRegId::FsBase, &regs, &mut buf),
            Some(8)
        );
        assert_eq!(u64::from_le_bytes(buf), 0, "fs_base has no TTD source");

        let mut buf = [0u8; 8];
        assert_eq!(
            read_register_value(TtdRegId::GsBase, &regs, &mut buf),
            Some(8)
        );
        assert_eq!(u64::from_le_bytes(buf), 0xDEAD_BEEF);
    }

    /// The 59-register layout is a wire contract: every raw id the target
    /// description advertises must survive `from_raw_id` -> `to_raw_id`.
    #[test]
    fn register_ids_roundtrip_through_raw_wire_ids() {
        for raw in 0..59 {
            let (id, _) = TtdRegId::from_raw_id(raw).unwrap_or_else(|| panic!("raw id {raw}"));
            assert_eq!(id.to_raw_id(), Some(raw), "raw id {raw}");
        }
        assert!(TtdRegId::from_raw_id(59).is_none());
        assert!(TtdRegId::from_raw_id(usize::MAX).is_none());
    }

    #[test]
    fn register_file_serializes_to_the_documented_g_buffer_size() {
        use gdbstub::arch::Registers;

        let regs = TtdRegisters::from_ttd(&TtdX64Regs::default(), 0);
        let mut written = 0usize;
        regs.gdb_serialize(|byte| {
            assert!(byte.is_some());
            written += 1;
        });
        assert_eq!(written, G_BUFFER_SIZE);

        let bytes = vec![0u8; G_BUFFER_SIZE];
        assert!(regs.clone().gdb_deserialize(&bytes).is_ok());
        assert!(
            regs.clone()
                .gdb_deserialize(&bytes[..G_BUFFER_SIZE - 1])
                .is_err()
        );
    }
}
