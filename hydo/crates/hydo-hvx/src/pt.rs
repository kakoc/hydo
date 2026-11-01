//! Coverage tracing with Intel Processor Trace (M41, M44; ported from hvx_explore.c in M137).
//!
//! `HVX_EXPLORE_PT_ELF=<program>` traces guest user mode inside one static non-PIE program's
//! text (an image's symbol-enabled order service linked at a unique address) in each schedule's
//! workload VM, from S1 until that VM is torn down, and decodes the trace into the edge file
//! `<outdir>/../cov/<tag>.edges` plus a one-line `<tag>.stats`. `HVX_EXPLORE_PT_RANGES=
//! "<lo>-<hi>[,<lo>-<hi>]"` (hex) programs up to two IP ranges instead; with
//! `HVX_EXPLORE_PTMULTI="<decoder> <name>=<elf>[@<bias>] ..."` each trace is saved and handed to
//! that external decoder (the M44 whole-system decoder, deleted in M108). `HVX_EXPLORE_PT_MIB`
//! sets the trace buffer (default 256 MiB), `HVX_EXPLORE_PT_SAVE` keeps the raw `<tag>.pt`. The
//! recovery VM after a power cut is not traced. Tracing does not change the guest (M40): every
//! fingerprint is the same with it on or off.
//!
//! The decoder is libipt's instruction decoder, called from `pt/ptedges.c` (the decoding half of
//! the C engine's `ptedges.h`, the same text): built only with the cargo feature `pt`, so the
//! default build needs no libipt. Without the feature the trace is still taken and its stats
//! written, with `edges=-1`, as the C engine built without libipt did.

/// One executable segment of the traced program: its link address, size and bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub va: u64,
    pub bytes: Vec<u8>,
}

/// The traced program's executable segments (at most 8) and its text range [lo, hi).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elf {
    pub regions: Vec<Region>,
    pub lo: u64,
    pub hi: u64,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// `pte_load_elf`: the PT_LOAD segments with PF_X and file bytes, in program-header order.
pub fn load_elf(b: &[u8]) -> Option<Elf> {
    if b.get(0..4)? != b"\x7fELF" {
        return None;
    }
    let phoff = u64_at(b, 0x20)? as usize;
    let phentsize = u16_at(b, 0x36)? as usize;
    let phnum = u16_at(b, 0x38)? as usize;
    let mut e = Elf {
        regions: Vec::new(),
        lo: u64::MAX,
        hi: 0,
    };
    for i in 0..phnum {
        if e.regions.len() >= 8 {
            break;
        }
        let ph = phoff + i * phentsize;
        let (ty, flags) = (u32_at(b, ph)?, u32_at(b, ph + 4)?);
        let (off, va, filesz) = (
            u64_at(b, ph + 8)? as usize,
            u64_at(b, ph + 16)?,
            u64_at(b, ph + 32)?,
        );
        if ty != 1 || flags & 1 == 0 || filesz == 0 {
            continue; // PT_LOAD, PF_X
        }
        let bytes = b.get(off..off + filesz as usize)?.to_vec();
        e.lo = e.lo.min(va);
        e.hi = e.hi.max(va + filesz);
        e.regions.push(Region { va, bytes });
    }
    (!e.regions.is_empty()).then_some(e)
}

/// `ptm_parse_ranges`: one or two `<lo>-<hi>` hex ranges separated by `,`, each with hi > lo.
pub fn parse_ranges(s: &str) -> Option<Vec<(u64, u64)>> {
    let mut v = Vec::new();
    let mut rest = s.as_bytes();
    while !rest.is_empty() && v.len() < 2 {
        let (lo, n) = crate::cstr::strtoull(rest, 16);
        rest = &rest[n..];
        if rest.first() != Some(&b'-') {
            return None;
        }
        let (hi, n) = crate::cstr::strtoull(&rest[1..], 16);
        rest = &rest[1 + n..];
        if hi <= lo {
            return None;
        }
        v.push((lo, hi));
        if rest.first() == Some(&b',') {
            rest = &rest[1..];
        }
    }
    (rest.is_empty() && !v.is_empty()).then_some(v)
}

/// Whether this build can decode traces (the cargo feature `pt`, which links libipt).
pub const DECODER: bool = cfg!(all(feature = "pt", target_os = "linux"));

#[cfg(all(feature = "pt", target_os = "linux"))]
mod ffi {
    #[repr(C)]
    pub struct Region {
        pub va: u64,
        pub size: u64,
        pub bytes: *const u8,
    }
    extern "C" {
        pub fn hydo_pte_decode(
            trace: *const u8,
            len: usize,
            regs: *const Region,
            nregs: i32,
            outpath: *const std::ffi::c_char,
            errs: *mut u64,
        ) -> std::ffi::c_long;
    }
}

/// `pte_decode`: decode `trace` into the edge file `out`; the number of edges (or -1) and the
/// number of decode errors. Without the decoder: (-1, 0) and a note on `diag`, as the C engine
/// built without libipt.
pub fn decode(
    trace: &[u8],
    elf: &Elf,
    out: &std::path::Path,
    diag: &mut dyn std::io::Write,
) -> (i64, u64) {
    #[cfg(all(feature = "pt", target_os = "linux"))]
    {
        let _ = diag;
        use std::os::unix::ffi::OsStrExt;
        let regs: Vec<ffi::Region> = elf
            .regions
            .iter()
            .map(|r| ffi::Region {
                va: r.va,
                size: r.bytes.len() as u64,
                bytes: r.bytes.as_ptr(),
            })
            .collect();
        let Ok(path) = std::ffi::CString::new(out.as_os_str().as_bytes()) else {
            return (-1, 0);
        };
        let mut errs = 0u64;
        // SAFETY: the trace, the regions' bytes and the path outlive the call; the decoder only
        // reads them and writes the file and *errs
        let n = unsafe {
            ffi::hydo_pte_decode(
                trace.as_ptr(),
                trace.len(),
                regs.as_ptr(),
                regs.len() as i32,
                path.as_ptr(),
                &mut errs,
            )
        };
        (n as i64, errs)
    }
    #[cfg(not(all(feature = "pt", target_os = "linux")))]
    {
        let _ = (trace, elf, out);
        let _ = writeln!(
            diag,
            "hydo-hvx: built without the cargo feature pt (libipt): no coverage decoding"
        );
        (-1, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(
            parse_ranges("400000-500000"),
            Some(vec![(0x400000, 0x500000)])
        );
        assert_eq!(
            parse_ranges("10-20,30-40"),
            Some(vec![(0x10, 0x20), (0x30, 0x40)])
        );
        assert_eq!(parse_ranges("20-10"), None);
        assert_eq!(parse_ranges("10"), None);
        assert_eq!(parse_ranges(""), None);
        assert_eq!(parse_ranges("10-20,30-40,50-60"), None); // a third range is left over
    }

    #[test]
    fn elf_segments() {
        // a 64-bit ELF header with two program headers: an executable PT_LOAD and a data one
        let mut b = vec![0u8; 0x200];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes());
        b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());
        let ph = |b: &mut Vec<u8>, at: usize, flags: u32, off: u64, va: u64, sz: u64| {
            b[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
            b[at + 4..at + 8].copy_from_slice(&flags.to_le_bytes());
            b[at + 8..at + 16].copy_from_slice(&off.to_le_bytes());
            b[at + 16..at + 24].copy_from_slice(&va.to_le_bytes());
            b[at + 32..at + 40].copy_from_slice(&sz.to_le_bytes());
        };
        ph(&mut b, 0x40, 5, 0x100, 0x401000, 0x20);
        ph(&mut b, 0x40 + 56, 6, 0x120, 0x402000, 0x10);
        b[0x100] = 0xc3;
        let e = load_elf(&b).unwrap();
        assert_eq!((e.lo, e.hi), (0x401000, 0x401020));
        assert_eq!(e.regions.len(), 1);
        assert_eq!(e.regions[0].bytes[0], 0xc3);
        assert!(load_elf(b"not an elf").is_none());
    }
}
