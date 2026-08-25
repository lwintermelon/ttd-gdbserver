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
use gdbstub_arch::x86::reg::id::{X86SegmentRegId, X86_64CoreRegId, X87FpuInternalRegId};
use gdbstub_arch::x86::reg::{X86SegmentRegs, X86_64CoreRegs, X87FpuInternalRegs};

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
