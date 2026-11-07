//! Exit-free spin loops: "a provable cycle is idle" (the Mac engine's spin observer, hydo-hvm's
//! `spin`, for x86 and KVM).
//!
//! A guest that loops without a VM exit gets no interrupt: interrupts are delivered at exits only.
//! On one vCPU with no exits nothing outside the guest changes its memory, so a loop that returns
//! to exactly the same register state without writing memory waits forever for an interrupt (a
//! thread spinning on a flag another thread must set, on a uniprocessor). Every iteration is then
//! the same state, so delivering the interrupt at a canonical point of the cycle gives the same
//! guest state whenever the loop was noticed: the host-timed kick is only a hint.
//!
//! On a kick (no exit for `HYDO_KVM_SPIN_MS` of host time) the engine steps the vCPU one
//! instruction at a time, but only over instructions [`decode`] accepts: loads, compares, tests,
//! register arithmetic, jumps and PAUSE, which write only general registers and flags (no store,
//! no push, no I/O, no LOCK). A step is a run to a hardware instruction breakpoint
//! (`KVM_GUESTDBG_USE_HW_BP`) at each address where the instruction can end (the next
//! instruction, and a branch's target), not the trap flag: an instruction that faults (a load from
//! a page the guest has not mapped in yet) then has its exception delivered exactly as in free
//! running, with no trap flag in the frame the CPU pushes, and the guest's handler runs as it
//! would have (Milestone 159; the trap flag of the first version could end up in that frame, and
//! so in guest RAM, depending on when the kick came). After each step the engine hashes the
//! general registers, RIP and RFLAGS. When a state repeats and one more period repeats it
//! exactly, the cycle is proven; the vCPU is stepped to the cycle's canonical state (the smallest
//! hash among the states with interrupts enabled), the clock jumps to the next event as at an
//! idle HLT, and the interrupt is delivered at the next entry. Anything else (an instruction
//! outside the set, a real exit while stepping, no repeat within the budget) leaves the guest as
//! free running would have (running to the end of an instruction is executing it) and it runs
//! on; a guest that stays without exits for `HYDO_KVM_SPIN_SECS` ends the run as a SPIN.
//!
//! Not resolved, as on the Mac without its rewind mode: counter loops (`while (!flag) n++`),
//! whose register state never repeats.

use crate::abi::kvm_regs;
use crate::fnv::{fnv_u64, OFFSET};

/// One decoded instruction of the set the observer may step (64-bit mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    /// Its length in bytes.
    pub len: u64,
    /// A relative branch: the displacement from the next instruction.
    pub rel: Option<i64>,
    /// Whether the branch may fall through (a conditional jump).
    pub cond: bool,
}

/// Where a stepped instruction can end: up to two addresses (the next instruction, a branch's
/// target), the observer's breakpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub at: [u64; 2],
    pub n: usize,
}

impl Step {
    pub fn addrs(&self) -> &[u64] {
        &self.at[..self.n]
    }
}

/// A canonical 48-bit address.
pub fn canonical(a: u64) -> bool {
    let top = a >> 47;
    top == 0 || top == 0x1_ffff
}

/// The step of the instruction `insn` at `rip`: None when an end is not canonical (the jump would
/// fault; free running handles it).
pub fn step_of(rip: u64, insn: &Insn) -> Option<Step> {
    let next = rip.wrapping_add(insn.len);
    let mut s = Step { at: [0; 2], n: 0 };
    let mut add = |a: u64| {
        if !s.at[..s.n].contains(&a) {
            s.at[s.n] = a;
            s.n += 1;
        }
    };
    match insn.rel {
        None => add(next),
        Some(r) => {
            if insn.cond {
                add(next);
            }
            add(next.wrapping_add(r as u64));
        }
    }
    s.addrs().iter().all(|&a| canonical(a)).then_some(s)
}

/// The bytes of a ModRM operand (ModRM, SIB, displacement) at `code[at..]`, or None.
fn modrm_len(code: &[u8], at: usize) -> Option<u64> {
    let m = *code.get(at)?;
    let (md, rm) = (m >> 6, m & 7);
    let mut n = 1u64;
    if md != 3 && rm == 4 {
        let sib = *code.get(at + 1)?;
        n += 1;
        if md == 0 && sib & 7 == 5 {
            n += 4;
        }
    }
    if md == 0 && rm == 5 {
        n += 4; // RIP-relative
    }
    n += match md {
        1 => 1,
        2 => 4,
        _ => 0,
    };
    // every byte of the instruction must be there (read through the guest's page tables)
    (code.len() as u64 >= at as u64 + n).then_some(n)
}

/// Decode the instruction at the start of `code` (64-bit mode) when it is one the observer may
/// step: it reads registers and memory and writes only general registers and RFLAGS (no store,
/// no push, no I/O, no LOCK, nothing that reads the trap flag). None for anything else, and for
/// an instruction whose bytes are not all in `code`.
pub fn decode(code: &[u8]) -> Option<Insn> {
    let mut i = 0usize;
    let mut rep = false;
    let mut opsize = false;
    // legacy prefixes (operand and address size, segments)
    while i < code.len() && i < 4 {
        match code[i] {
            0x66 => {
                opsize = true;
                i += 1
            }
            0x67 | 0x2e | 0x3e | 0x26 | 0x36 | 0x64 | 0x65 => i += 1,
            0xf3 => {
                rep = true;
                i += 1
            }
            _ => break,
        }
    }
    // one REX
    let mut rex_w = false;
    if i < code.len() && (0x40..=0x4f).contains(&code[i]) {
        rex_w = code[i] & 8 != 0;
        i += 1;
    }
    let op = *code.get(i)?;
    let modrm = code.get(i + 1).copied();
    let md = modrm.map(|m| m >> 6);
    let reg = modrm.map(|m| (m >> 3) & 7);
    let imm = if opsize { 2 } else { 4 };
    let i = i as u64;
    let plain = |len: u64| {
        (code.len() as u64 >= len).then_some(Insn {
            len,
            rel: None,
            cond: false,
        })
    };
    // ModRM at i+1 (one-byte opcodes), then `extra` bytes of immediate
    let with_modrm = |extra: u64| {
        let n = modrm_len(code, i as usize + 1)?;
        plain(i + 1 + n + extra)
    };
    // a relative branch: the displacement's size; the operand-size prefix is refused (Intel
    // ignores it on a near branch in 64-bit mode, AMD truncates the target to 16 bits)
    let branch = |at: u64, size: u64, cond: bool| {
        if opsize {
            return None;
        }
        let b = code.get(at as usize..(at + size) as usize)?;
        let rel = if size == 1 {
            b[0] as i8 as i64
        } else {
            i32::from_le_bytes(b.try_into().ok()?) as i64
        };
        Some(Insn {
            len: at + size,
            rel: Some(rel),
            cond,
        })
    };
    if rep {
        // PAUSE (F3 90) only
        return if op == 0x90 { plain(i + 1) } else { None };
    }
    match op {
        0x90 => plain(i + 1),
        // mov r, r/m (loads)
        0x8a | 0x8b => with_modrm(0),
        // lea (a register form is #UD)
        0x8d if md.is_some_and(|m| m != 3) => with_modrm(0),
        // mov r/m, r: register to register only
        0x88 | 0x89 if md == Some(3) => with_modrm(0),
        // mov r, imm
        0xb0..=0xb7 => plain(i + 2),
        0xb8..=0xbf => plain(i + 1 + if rex_w { 8 } else { imm }),
        // test, cmp (any operands: they write only flags)
        0x84 | 0x85 | 0x38..=0x3b => with_modrm(0),
        0x3c | 0xa8 => plain(i + 2),
        0x3d | 0xa9 => plain(i + 1 + imm),
        // ALU into a register: add, or, adc, sbb, and, sub, xor with a register destination
        0x02 | 0x03 | 0x0a | 0x0b | 0x12 | 0x13 | 0x1a | 0x1b | 0x22 | 0x23 | 0x2a | 0x2b
        | 0x32 | 0x33 => with_modrm(0),
        // ALU into r/m: register destination only
        0x00 | 0x01 | 0x08 | 0x09 | 0x10 | 0x11 | 0x18 | 0x19 | 0x20 | 0x21 | 0x28 | 0x29
        | 0x30 | 0x31
            if md == Some(3) =>
        {
            with_modrm(0)
        }
        // group 1 with an immediate: cmp anywhere, the rest on a register
        0x80 | 0x83 if reg == Some(7) || md == Some(3) => with_modrm(1),
        0x81 if reg == Some(7) || md == Some(3) => with_modrm(imm),
        // test r/m, imm
        0xf6 if reg == Some(0) => with_modrm(1),
        0xf7 if reg == Some(0) => with_modrm(imm),
        // inc, dec of a register
        0xfe | 0xff if md == Some(3) && matches!(reg, Some(0) | Some(1)) => with_modrm(0),
        // jumps: short Jcc, short JMP, near JMP
        0x70..=0x7f => branch(i + 1, 1, true),
        0xeb => branch(i + 1, 1, false),
        0xe9 => branch(i + 1, 4, false),
        0x0f => {
            let op2 = *code.get(i as usize + 1)?;
            let modrm2 = |extra: u64| {
                let n = modrm_len(code, i as usize + 2)?;
                plain(i + 2 + n + extra)
            };
            match op2 {
                // near Jcc
                0x80..=0x8f => branch(i + 2, 4, true),
                // movzx, movsx, cmovcc, nop r/m, bt r/m, r
                0xb6 | 0xb7 | 0xbe | 0xbf | 0x1f | 0xa3 | 0x40..=0x4f => modrm2(0),
                // bt r/m, imm
                0xba if code.get(i as usize + 2).is_some_and(|m| (m >> 3) & 7 == 4) => modrm2(1),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether the instruction at the start of `code` is one the observer may step ([`decode`]).
pub fn pure(code: &[u8]) -> bool {
    decode(code).is_some()
}

/// The state the cycle detection compares: every general register, RIP and RFLAGS.
pub fn state_hash(r: &kvm_regs) -> u64 {
    let mut h = OFFSET;
    for v in [
        r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11, r.r12,
        r.r13, r.r14, r.r15, r.rip, r.rflags,
    ] {
        h = fnv_u64(v, h);
    }
    h
}

/// What one observation found.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// a proven cycle; the vCPU is at its canonical state, interruptible
    Resolved,
    /// a proven cycle in which interrupts are never enabled: nothing can end it
    Deadlock,
    /// not proven (the guest is where free running would have taken it)
    GaveUp,
    /// a real exit happened while stepping: the engine handles it as any exit
    Exit(crate::cpu::Exit),
}

/// The step budget of one observation.
pub const STEPS: usize = 4096;

/// Detect an exact cycle in a sequence of state hashes: Some((start, period)) when the last
/// `period` states repeat the `period` before them (two full periods seen: one repeated state
/// already proves the cycle, since the next instruction is a function of the registers and of
/// memory that nothing changes; the second period guards against a hash collision).
pub fn cycle(h: &[u64]) -> Option<(usize, usize)> {
    let n = h.len();
    for p in 1..=n / 2 {
        if h[n - p..] == h[n - 2 * p..n - p] {
            return Some((n - 2 * p, p));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spin_loop_of_mysqld_is_pure() {
        // the loop found in phase 2 (mysqld --initialize on one vCPU):
        // mov rax,[rdi+0x10]; test rax,rax; jz; mov eax,[rax]; cmp eax,2; jne; pause; jnz back
        for c in [
            &[0x48, 0x8b, 0x47, 0x10][..],
            &[0x48, 0x85, 0xc0],
            &[0x74, 0x12],
            &[0x8b, 0x00],
            &[0x83, 0xf8, 0x02],
            &[0x75, 0x41],
            &[0xf3, 0x90],
            &[0x75, 0xee],
        ] {
            assert!(pure(c), "{c:x?}");
        }
    }

    #[test]
    fn stores_pushes_io_and_locks_are_not() {
        for c in [
            &[0x89, 0x07][..],         // mov [rdi],eax
            &[0x87, 0x02],             // xchg [rdx],eax
            &[0xf0, 0x0f, 0xb1, 0x0a], // lock cmpxchg
            &[0x9c],                   // pushf
            &[0x50],                   // push rax
            &[0xe8, 0, 0, 0, 0],       // call
            &[0xc3],                   // ret
            &[0x0f, 0x05],             // syscall
            &[0xec],                   // in al,dx
            &[0xee],                   // out dx,al
            &[0xf4],                   // hlt
            &[0xfb],                   // sti
            &[0x0f, 0x31],             // rdtsc
            &[0xff, 0x07],             // inc dword [rdi]
            &[0x83, 0x07, 0x01],       // add dword [rdi],1
            &[0xf3, 0xa4],             // rep movsb
        ] {
            assert!(!pure(c), "{c:x?}");
        }
    }

    #[test]
    fn lengths_and_ends() {
        let len = |c: &[u8]| decode(c).map(|x| x.len);
        assert_eq!(len(&[0x48, 0x8b, 0x47, 0x10]), Some(4)); // mov rax,[rdi+0x10]
        assert_eq!(len(&[0x8b, 0x05, 1, 2, 3, 4]), Some(6)); // mov eax,[rip+disp32]
        assert_eq!(len(&[0x8b, 0x04, 0x24]), Some(3)); // mov eax,[rsp]
        assert_eq!(len(&[0x8b, 0x04, 0x25, 0, 0, 0, 0]), Some(7)); // mov eax,[disp32]
        assert_eq!(len(&[0x8b, 0x84, 0x24, 0, 1, 0, 0]), Some(7)); // mov eax,[rsp+disp32]
        assert_eq!(len(&[0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0, 0, 0]), Some(9)); // mov rax,fs:0x28
        assert_eq!(len(&[0x48, 0xb8, 1, 2, 3, 4, 5, 6, 7, 8]), Some(10)); // movabs rax
        assert_eq!(len(&[0x66, 0xb8, 1, 2]), Some(4)); // mov ax, imm16
        assert_eq!(len(&[0x81, 0x7f, 0x08, 1, 2, 3, 4]), Some(7)); // cmp dword [rdi+8], imm32
        assert_eq!(len(&[0x66, 0x81, 0xff, 1, 2]), Some(5)); // cmp di, imm16
        assert_eq!(len(&[0x0f, 0xba, 0x27, 3]), Some(4)); // bt dword [rdi], 3
        assert_eq!(len(&[0x0f, 0xb6, 0x07]), Some(3)); // movzx eax, byte [rdi]
        assert_eq!(len(&[0xf3, 0x90]), Some(2)); // pause
                                                 // incomplete bytes, a register lea (#UD), an operand-size prefix on a branch
        assert_eq!(len(&[0x8b, 0x05, 1, 2]), None);
        assert_eq!(len(&[0x8d, 0xc0]), None);
        assert_eq!(len(&[0x66, 0x75, 0x02]), None);
        // the ends: a fall-through and a target, a target alone, the next instruction
        let jnz = decode(&[0x75, 0xee]).unwrap();
        assert_eq!(
            step_of(0x1000, &jnz).unwrap().addrs(),
            &[0x1002, 0x1002 - 0x12]
        );
        let jmp = decode(&[0xe9, 0xfb, 0xff, 0xff, 0xff]).unwrap(); // jmp to itself
        assert_eq!(step_of(0x1000, &jmp).unwrap().addrs(), &[0x1000]);
        let jz0 = decode(&[0x74, 0x00]).unwrap(); // both ends the same address
        assert_eq!(step_of(0x1000, &jz0).unwrap().addrs(), &[0x1002]);
        let mov = decode(&[0x8b, 0x00]).unwrap();
        assert_eq!(step_of(0x1000, &mov).unwrap().addrs(), &[0x1002]);
        // an instruction that ends past the canonical range is not stepped
        assert_eq!(step_of(0x7fff_ffff_fffe, &mov), None);
    }

    #[test]
    fn cycles() {
        assert_eq!(cycle(&[1, 2, 3, 2]), None);
        assert_eq!(cycle(&[1, 2, 3, 2, 3]), Some((1, 2)));
        assert_eq!(cycle(&[1, 2, 3, 2, 3, 2, 3]), Some((3, 2)));
        assert_eq!(cycle(&[1, 2, 1]), None);
        assert_eq!(cycle(&[7, 7]), Some((0, 1)));
        assert_eq!(cycle(&[1, 2, 3, 4]), None);
    }
}
