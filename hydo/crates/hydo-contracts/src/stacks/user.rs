//! A user stack, unwound from a thread's registers: by Go's table of frame sizes in a Go
//! program, by the call frame information (`.eh_frame`) where the program has it, and by frame
//! pointers otherwise.

use super::mem::{Arch, Space};
use super::program::Program;

/// The registers an unwind starts from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct URegs {
    pub pc: u64,
    pub sp: u64,
    /// rbp, or arm64's x29.
    pub fp: u64,
    /// arm64's x30 (0 on x86_64).
    pub lr: u64,
}

/// One frame: its program counter, its stack pointer, and how it was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UFrame {
    pub pc: u64,
    pub sp: u64,
    pub how: &'static str,
}

/// Go functions where a goroutine's or a thread's stack starts.
const GO_TOPS: [&str; 5] = [
    "runtime.goexit",
    "runtime.mstart",
    "runtime.rt0_go",
    "runtime.mstart0",
    "runtime.systemstack",
];

/// Unwind up to `max` frames. `bias` is where the program is loaded less its link address (0
/// for a program at a fixed address).
pub fn unwind(
    s: &dyn Space,
    prog: Option<&Program>,
    bias: u64,
    arch: Arch,
    r: URegs,
    max: usize,
) -> Vec<UFrame> {
    let mut out = Vec::new();
    let (mut pc, mut sp, mut fp) = (r.pc, r.sp, r.fp);
    let x86 = arch == Arch::X86_64;
    let mut how = "regs";
    for i in 0..max {
        if pc == 0 {
            break;
        }
        out.push(UFrame { pc, sp, how });
        let look = if i == 0 { pc } else { pc - 1 };
        let lpc = look.wrapping_sub(bias);
        let mut next: Option<(u64, u64, u64, &'static str)> = None;
        if let Some(p) = prog {
            if let Some(g) = &p.go {
                if let Some(f) = g.func(lpc) {
                    if GO_TOPS.contains(&g.name(&f).as_str()) {
                        break;
                    }
                    let d = g.sp_delta(&f, lpc).unwrap_or(0).max(0) as u64;
                    next = if x86 {
                        let nfp = if d >= 8 {
                            s.u64(sp + d - 8).unwrap_or(fp)
                        } else {
                            fp
                        };
                        s.u64(sp + d).map(|ra| (ra, sp + d + 8, nfp, "go"))
                    } else if d == 0 {
                        (i == 0).then_some((r.lr, sp, fp, "go"))
                    } else {
                        s.u64(sp)
                            .map(|ra| (ra, sp + d, s.u64(sp + d - 8).unwrap_or(fp), "go"))
                    };
                }
            } else if let Some(c) = p.cfi(lpc) {
                let base = match (x86, c.cfa_reg) {
                    (true, 7) | (false, 31) => Some(sp),
                    (true, 6) | (false, 29) => Some(fp),
                    _ => None,
                };
                if let Some(b) = base {
                    let cfa = b.wrapping_add(c.cfa_off as u64);
                    let ra = match c.ra {
                        Some(o) => s.u64(cfa.wrapping_add(o as u64)),
                        None if !x86 && i == 0 => Some(r.lr),
                        None => None,
                    };
                    let nfp = match c.fp {
                        Some(o) => s.u64(cfa.wrapping_add(o as u64)).unwrap_or(fp),
                        None => fp,
                    };
                    next = ra.map(|ra| (ra, cfa, nfp, "cfi"));
                }
            }
        }
        if next.is_none() && fp != 0 && fp >= sp {
            // frame pointers: [fp] is the caller's, [fp + 8] the return address
            next = match (s.u64(fp), s.u64(fp + 8)) {
                (Some(nfp), Some(ra)) => Some((ra, fp + 16, nfp, "fp")),
                _ => None,
            };
        }
        let Some((ra, nsp, nfp, h)) = next else { break };
        if nsp < sp || (i > 0 && nsp == sp) {
            break;
        }
        pc = ra;
        sp = nsp;
        fp = nfp;
        how = h;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacks::mem::Phys;

    struct Flat(Vec<u8>);
    impl Phys for Flat {
        fn read(&self, gpa: u64, b: &mut [u8]) -> bool {
            self.0.read(gpa, b)
        }
    }
    impl Space for Flat {
        fn phys(&self) -> &dyn Phys {
            self
        }
        fn gpa(&self, va: u64) -> Option<u64> {
            Some(va)
        }
    }

    #[test]
    fn frame_pointers_without_a_program() {
        let mut m = vec![0u8; 0x3000];
        let put = |m: &mut Vec<u8>, a: u64, v: u64| {
            m[a as usize..a as usize + 8].copy_from_slice(&v.to_le_bytes())
        };
        // frame at fp=0x2000: caller fp 0x2100, return 0x401234; then 0x2100: 0, 0x405678
        put(&mut m, 0x2000, 0x2100);
        put(&mut m, 0x2008, 0x401234);
        put(&mut m, 0x2100, 0);
        put(&mut m, 0x2108, 0x405678);
        let f = Flat(m);
        let fr = unwind(
            &f,
            None,
            0,
            Arch::X86_64,
            URegs {
                pc: 0x400100,
                sp: 0x1ff0,
                fp: 0x2000,
                lr: 0,
            },
            16,
        );
        assert_eq!(
            fr.iter().map(|x| x.pc).collect::<Vec<_>>(),
            vec![0x400100, 0x401234, 0x405678]
        );
        assert_eq!(
            fr.iter().map(|x| x.how).collect::<Vec<_>>(),
            vec!["regs", "fp", "fp"]
        );
    }
}
