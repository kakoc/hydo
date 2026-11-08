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
//! # Counter loops (the rewind mode, Milestone 161)
//!
//! A loop such as `while (flag) n++` never returns to the same register state: the state at the
//! kick is the entry state plus k passes, and k depends on when the host-timed kick came. The
//! observer proves such a loop infinite from one stepped pass ([`counter_loop`]): between two
//! arrivals at the same instruction the general registers that changed (the counters) are
//! tainted, and a data-flow walk over the pass's decoded instructions ([`Flow`]) shows that no
//! tainted value reaches a conditional branch or a memory address (counters may only feed
//! themselves and other values nothing depends on). Every untainted value is then the same in
//! every pass (the stepped instructions write no memory, and on one vCPU without exits nothing
//! else does), so every later pass takes the same path and loads from the same addresses: the
//! loop never ends without an interrupt. Delivering it at a point fixed by the guest alone needs
//! the rewind mode of [`crate::vmm`] (the Mac engine's, ported): back to a checkpoint at or
//! before the last exit, the same exits again, then a hardware breakpoint at the loop's
//! canonical instruction (the smallest address of the pass where interrupts are enabled), and
//! the interrupt at the first arrival there. Without the rewind mode a proven counter loop ends
//! the run at once as a SPIN, and the host runs the schedule again with `spin=rewind`.

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
    /// What it reads and writes ([`counter_loop`]'s data flow).
    pub flow: Flow,
}

/// How the arithmetic flags are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlagsOut {
    #[default]
    None,
    /// every arithmetic flag the instruction defines is written from its inputs
    All,
    /// some (INC and DEC keep CF, BT writes CF only): the old flags stay in the result
    Partial,
}

/// The data flow of one instruction of the stepped set, over the 16 general registers (bit r is
/// register r in the x86 encoding: rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8..r15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flow {
    /// registers whose values the result depends on (a destination written in part, 8 or 16
    /// bits, counts as read)
    pub reads: u16,
    /// registers written
    pub writes: u16,
    /// registers that form a memory address (the load's address must be the same in every pass)
    pub addr: u16,
    /// the arithmetic flags are an input
    pub flags_in: bool,
    pub flags_out: FlagsOut,
    /// the next instruction depends on the inputs (a conditional branch)
    pub control: bool,
}

/// The general registers in the x86 encoding's order ([`Flow`]'s bits).
pub fn gprs(r: &kvm_regs) -> [u64; 16] {
    [
        r.rax, r.rcx, r.rdx, r.rbx, r.rsp, r.rbp, r.rsi, r.rdi, r.r8, r.r9, r.r10, r.r11, r.r12,
        r.r13, r.r14, r.r15,
    ]
}

/// The register of an 8-bit operand's number: without a REX prefix 4..7 are AH, CH, DH, BH (the
/// second bytes of rax..rbx).
fn reg8(code: u8, has_rex: bool) -> u16 {
    if !has_rex && (4..8).contains(&code) {
        1 << (code - 4)
    } else {
        1 << code
    }
}

/// The registers of a memory operand's address (ModRM at `code[at]`; RIP-relative and absolute
/// addresses use none).
fn mem_regs(code: &[u8], at: usize, rex: u8) -> u16 {
    let Some(&m) = code.get(at) else { return 0 };
    let (md, rm) = (m >> 6, m & 7);
    if rm == 4 {
        let Some(&sib) = code.get(at + 1) else {
            return 0;
        };
        let base = (sib & 7) | ((rex & 1) << 3);
        let index = ((sib >> 3) & 7) | ((rex & 2) << 2);
        let mut v = 0;
        if !(md == 0 && sib & 7 == 5) {
            v |= 1 << base;
        }
        if index != 4 {
            v |= 1 << index;
        }
        return v;
    }
    if md == 0 && rm == 5 {
        return 0; // RIP-relative
    }
    1 << (rm | ((rex & 1) << 3))
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
    let mut rex = 0u8;
    let mut has_rex = false;
    if i < code.len() && (0x40..=0x4f).contains(&code[i]) {
        rex = code[i] & 0xf;
        has_rex = true;
        i += 1;
    }
    let rex_w = rex & 8 != 0;
    let op = *code.get(i)?;
    let modrm = code.get(i + 1).copied();
    let md = modrm.map(|m| m >> 6);
    let reg = modrm.map(|m| (m >> 3) & 7);
    let imm = if opsize { 2 } else { 4 };
    // a 16-bit write (an operation that is not a byte one) merges into the register
    let partial = !rex_w && opsize;
    let i = i as u64;
    let plain = |len: u64, flow: Flow| {
        (code.len() as u64 >= len).then_some(Insn {
            len,
            rel: None,
            cond: false,
            flow,
        })
    };
    // ModRM at i+1 (one-byte opcodes), then `extra` bytes of immediate
    let with_modrm = |extra: u64, flow: Flow| {
        let n = modrm_len(code, i as usize + 1)?;
        plain(i + 1 + n + extra, flow)
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
            flow: Flow {
                flags_in: cond,
                control: cond,
                ..Flow::default()
            },
        })
    };
    // the ModRM operands at `at`: the reg field's register, the r/m register (0 for memory) and
    // the registers of the r/m operand's address, for a byte operation or not
    let ops = |at: usize, byte: bool| -> (u16, u16, u16) {
        let m = code.get(at).copied().unwrap_or(0);
        let r = ((m >> 3) & 7) | ((rex & 4) << 1);
        let rm = (m & 7) | ((rex & 1) << 3);
        let rg = |c: u8| if byte { reg8(c, has_rex) } else { 1 << c };
        if m >> 6 == 3 {
            (rg(r), rg(rm), 0)
        } else {
            (rg(r), 0, mem_regs(code, at, rex))
        }
    };
    let m1 = i as usize + 1;
    // dst <- src (the destination read too when written in part)
    let mov = |dst: u16, src: u16, addr: u16, merge: bool| Flow {
        reads: src | if merge { dst } else { 0 },
        writes: dst,
        addr,
        ..Flow::default()
    };
    // an ALU operation into dst: reads both, writes dst and every flag
    let alu = |dst: u16, src: u16, addr: u16, carry_in: bool| Flow {
        reads: dst | src,
        writes: dst,
        addr,
        flags_in: carry_in,
        flags_out: FlagsOut::All,
        control: false,
    };
    // a compare or test: reads both, writes the flags only
    let cmp = |a: u16, b: u16, addr: u16| Flow {
        reads: a | b,
        addr,
        flags_out: FlagsOut::All,
        ..Flow::default()
    };
    if rep {
        // PAUSE (F3 90) only
        return if op == 0x90 {
            plain(i + 1, Flow::default())
        } else {
            None
        };
    }
    match op {
        // NOP; with REX.B it is XCHG r8, rax
        0x90 => {
            let f = if rex & 1 != 0 {
                Flow {
                    reads: 1 | 1 << 8,
                    writes: 1 | 1 << 8,
                    ..Flow::default()
                }
            } else {
                Flow::default()
            };
            plain(i + 1, f)
        }
        // mov r, r/m (loads)
        0x8a | 0x8b => {
            let byte = op == 0x8a;
            let (r, rm, a) = ops(m1, byte);
            with_modrm(0, mov(r, rm, a, byte || partial))
        }
        // lea (a register form is #UD): the address registers are its data, no memory is read
        0x8d if md.is_some_and(|m| m != 3) => {
            let (r, _, a) = ops(m1, false);
            with_modrm(0, mov(r, a, 0, partial))
        }
        // mov r/m, r: register to register only
        0x88 | 0x89 if md == Some(3) => {
            let byte = op == 0x88;
            let (r, rm, _) = ops(m1, byte);
            with_modrm(0, mov(rm, r, 0, byte || partial))
        }
        // mov r, imm
        0xb0..=0xb7 => plain(
            i + 2,
            mov(reg8((op & 7) | ((rex & 1) << 3), has_rex), 0, 0, true),
        ),
        0xb8..=0xbf => plain(
            i + 1 + if rex_w { 8 } else { imm },
            mov(1 << ((op & 7) | ((rex & 1) << 3)), 0, 0, partial),
        ),
        // test, cmp (any operands: they write only flags)
        0x84 | 0x85 | 0x38..=0x3b => {
            let (r, rm, a) = ops(m1, op & 1 == 0);
            with_modrm(0, cmp(r, rm, a))
        }
        0x3c | 0xa8 => plain(i + 2, cmp(1, 0, 0)),
        0x3d | 0xa9 => plain(i + 1 + imm, cmp(1, 0, 0)),
        // ALU into a register: add, or, adc, sbb, and, sub, xor with a register destination
        0x02 | 0x03 | 0x0a | 0x0b | 0x12 | 0x13 | 0x1a | 0x1b | 0x22 | 0x23 | 0x2a | 0x2b
        | 0x32 | 0x33 => {
            let byte = op & 1 == 0;
            let (r, rm, a) = ops(m1, byte);
            let mut f = alu(r, rm, a, matches!(op >> 3, 2 | 3));
            // xor or sub of a register with itself, 32 or 64 bits: zero, whatever it held
            if matches!(op >> 3, 5 | 6) && !byte && !partial && md == Some(3) && r == rm {
                f.reads = 0;
            }
            with_modrm(0, f)
        }
        // ALU into r/m: register destination only
        0x00 | 0x01 | 0x08 | 0x09 | 0x10 | 0x11 | 0x18 | 0x19 | 0x20 | 0x21 | 0x28 | 0x29
        | 0x30 | 0x31
            if md == Some(3) =>
        {
            let byte = op & 1 == 0;
            let (r, rm, _) = ops(m1, byte);
            let mut f = alu(rm, r, 0, matches!(op >> 3, 2 | 3));
            if matches!(op >> 3, 5 | 6) && !byte && !partial && r == rm {
                f.reads = 0;
            }
            with_modrm(0, f)
        }
        // group 1 with an immediate: cmp anywhere, the rest on a register
        0x80 | 0x83 if reg == Some(7) || md == Some(3) => {
            let (_, rm, a) = ops(m1, op == 0x80);
            let f = if reg == Some(7) {
                cmp(rm, 0, a)
            } else {
                alu(rm, 0, 0, matches!(reg, Some(2) | Some(3)))
            };
            with_modrm(1, f)
        }
        0x81 if reg == Some(7) || md == Some(3) => {
            let (_, rm, a) = ops(m1, false);
            let f = if reg == Some(7) {
                cmp(rm, 0, a)
            } else {
                alu(rm, 0, 0, matches!(reg, Some(2) | Some(3)))
            };
            with_modrm(imm, f)
        }
        // test r/m, imm
        0xf6 if reg == Some(0) => {
            let (_, rm, a) = ops(m1, true);
            with_modrm(1, cmp(rm, 0, a))
        }
        0xf7 if reg == Some(0) => {
            let (_, rm, a) = ops(m1, false);
            with_modrm(imm, cmp(rm, 0, a))
        }
        // inc, dec of a register (CF is kept)
        0xfe | 0xff if md == Some(3) && matches!(reg, Some(0) | Some(1)) => {
            let (_, rm, _) = ops(m1, op == 0xfe);
            with_modrm(
                0,
                Flow {
                    reads: rm,
                    writes: rm,
                    flags_out: FlagsOut::Partial,
                    ..Flow::default()
                },
            )
        }
        // jumps: short Jcc, short JMP, near JMP
        0x70..=0x7f => branch(i + 1, 1, true),
        0xeb => branch(i + 1, 1, false),
        0xe9 => branch(i + 1, 4, false),
        0x0f => {
            let op2 = *code.get(i as usize + 1)?;
            let m2 = i as usize + 2;
            let modrm2 = |extra: u64, flow: Flow| {
                let n = modrm_len(code, m2)?;
                plain(i + 2 + n + extra, flow)
            };
            match op2 {
                // near Jcc
                0x80..=0x8f => branch(i + 2, 4, true),
                // movzx, movsx: the source is a byte (b6, be) or a word
                0xb6 | 0xb7 | 0xbe | 0xbf => {
                    let (r, _, _) = ops(m2, false);
                    let (_, rm, a) = ops(m2, op2 & 1 == 0);
                    modrm2(0, mov(r, rm, a, partial))
                }
                // cmovcc: the destination is kept when the condition is false
                0x40..=0x4f => {
                    let (r, rm, a) = ops(m2, false);
                    let mut f = mov(r, rm | r, a, false);
                    f.flags_in = true;
                    modrm2(0, f)
                }
                // nop r/m: reads nothing
                0x1f => modrm2(0, Flow::default()),
                // bt r/m, r: on memory the bit offset register is part of the address
                0xa3 => {
                    let (r, rm, a) = ops(m2, false);
                    let f = if code.get(m2).is_some_and(|m| m >> 6 == 3) {
                        Flow {
                            reads: r | rm,
                            flags_out: FlagsOut::Partial,
                            ..Flow::default()
                        }
                    } else {
                        Flow {
                            addr: a | r,
                            flags_out: FlagsOut::Partial,
                            ..Flow::default()
                        }
                    };
                    modrm2(0, f)
                }
                // bt r/m, imm
                0xba if code.get(i as usize + 2).is_some_and(|m| (m >> 3) & 7 == 4) => {
                    let (_, rm, a) = ops(m2, false);
                    modrm2(
                        1,
                        Flow {
                            reads: rm,
                            addr: a,
                            flags_out: FlagsOut::Partial,
                            ..Flow::default()
                        },
                    )
                }
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

/// Whether the instruction at the start of `code` writes or reloads RFLAGS as a whole, so that a
/// trap-flag single step of it would put the trap flag into the guest's state or lose the step:
/// PUSHF, POPF, IRET, INT3, INT n, INTO, INT1, SYSCALL, SYSRET, SYSENTER, SYSEXIT and UD2 (its
/// #UD frame). Unknown bytes (a prefix run with no opcode) count as such too. The full-state
/// cycle's walk ([`crate::vmm::Vmm::observe_state`]) does not step them.
pub fn saves_flags(code: &[u8]) -> bool {
    let mut i = 0;
    while i < code.len() {
        match code[i] {
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3 => i += 1,
            0x40..=0x4f => i += 1,
            _ => break,
        }
    }
    match code.get(i) {
        None => true,
        Some(0x9c | 0x9d | 0xcf | 0xcc | 0xcd | 0xce | 0xf1) => true,
        Some(0x0f) => matches!(
            code.get(i + 1),
            None | Some(0x05 | 0x07 | 0x0b | 0x34 | 0x35)
        ),
        Some(_) => false,
    }
}

/// The length of a SYSCALL instruction at the start of `code` (with its prefixes), None for any
/// other instruction. The full-state cycle's walk runs over it to a breakpoint at the next
/// instruction instead of single-stepping it (the kernel returns there, and the trap flag would
/// otherwise reach the guest through R11).
pub fn syscall_len(code: &[u8]) -> Option<u64> {
    let mut i = 0;
    while i < code.len()
        && matches!(
            code[i],
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf2 | 0xf3 | 0x40..=0x4f
        )
    {
        i += 1;
    }
    (code.get(i) == Some(&0x0f) && code.get(i + 1) == Some(&0x05)).then_some(i as u64 + 2)
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
    /// a proven counter loop ([`counter_loop`]); the canonical instruction's address (the
    /// smallest of the pass), None when interrupts are off in it (nothing can end it). The vCPU
    /// is where free running would have taken it.
    Counter(Option<u64>),
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

/// RFLAGS' arithmetic flags (CF, PF, AF, ZF, SF, OF): what a counter may change between passes.
pub const ARITH: u64 = 0x8d5;
/// RFLAGS.RF and IF.
const RF: u64 = 1 << 16;
const IF: u64 = 1 << 9;

/// The taint before each instruction of a pass, and after its last one.
type Walk = (Vec<(u16, bool)>, u16, bool);

/// The data-flow walk over one pass (see the module documentation): from the taint `t`
/// (registers) and `ft` (the arithmetic flags), the taint before each instruction and after the
/// last one; None (with `check`) when a tainted value reaches a conditional branch or a memory
/// address.
fn walk(flows: &[Flow], mut t: u16, mut ft: bool, check: bool) -> Option<Walk> {
    let mut at = Vec::with_capacity(flows.len());
    for f in flows {
        at.push((t, ft));
        let tin = f.reads & t != 0 || (f.flags_in && ft);
        if check && (f.addr & t != 0 || (f.control && tin)) {
            return None;
        }
        if tin {
            t |= f.writes;
        } else {
            t &= !f.writes;
        }
        match f.flags_out {
            FlagsOut::None => {}
            FlagsOut::All => ft = tin,
            FlagsOut::Partial => ft |= tin,
        }
    }
    Some((at, t, ft))
}

/// Whether two consecutive passes over the same instructions prove an infinite counter loop:
/// `states` are the 2p+1 states of the passes (`states[0]`, `states[p]` and `states[2p]` at the
/// same instruction), `flows[i]` the instruction executed at `states[i]`. The registers that
/// change between passes are tainted; the taint is closed over a pass (what a tainted value
/// writes is tainted, what only untainted values write is not); then no tainted value may reach a
/// conditional branch or an address, and the two passes must agree, state by state, on every
/// untainted register (and on the flags where they are untainted) and on every bit of RFLAGS
/// but the arithmetic ones. An exact cycle (nothing changes) is not a counter loop: the exact
/// rule resolves it without the rewind mode.
pub fn counter_loop(states: &[kvm_regs], flows: &[Flow]) -> bool {
    let n = states.len();
    if n < 3 || n.is_multiple_of(2) || flows.len() + 1 != n {
        return false;
    }
    let p = n / 2;
    for i in 0..p {
        if states[i].rip != states[p + i].rip || flows[i] != flows[p + i] {
            return false;
        }
    }
    if states[0].rip != states[2 * p].rip {
        return false;
    }
    for i in 0..=p {
        if (states[i].rflags ^ states[p + i].rflags) & !(ARITH | RF) != 0 {
            return false;
        }
    }
    let differ = |x: &kvm_regs, y: &kvm_regs| {
        let (gx, gy) = (gprs(x), gprs(y));
        (0..16)
            .filter(|&r| gx[r] != gy[r])
            .fold(0u16, |a, r| a | 1 << r)
    };
    let m = differ(&states[0], &states[p]) | differ(&states[p], &states[2 * p]);
    let fd = (states[0].rflags ^ states[p].rflags) & ARITH != 0
        || (states[p].rflags ^ states[2 * p].rflags) & ARITH != 0;
    if m == 0 && !fd {
        return false;
    }
    let pass = &flows[p..];
    // the closure of the taint over a pass (joined, so it only grows: at most 17 rounds)
    let (mut t, mut ft) = (m, fd);
    loop {
        let Some((_, t2, ft2)) = walk(pass, t, ft, false) else {
            return false;
        };
        let (u, uf) = (t | t2, ft || ft2);
        if (u, uf) == (t, ft) {
            break;
        }
        (t, ft) = (u, uf);
    }
    let Some((taint, _, _)) = walk(pass, t, ft, true) else {
        return false;
    };
    if walk(&flows[..p], t, ft, true).is_none() {
        return false;
    }
    for (i, &(ti, fti)) in taint.iter().enumerate() {
        let (ga, gb) = (gprs(&states[i]), gprs(&states[p + i]));
        if (0..16).any(|r| ti & (1 << r) == 0 && ga[r] != gb[r]) {
            return false;
        }
        if !fti && (states[i].rflags ^ states[p + i].rflags) & ARITH != 0 {
            return false;
        }
    }
    true
}

/// The canonical instruction of a counter loop's pass (`states` of one pass): the smallest
/// address among its states with interrupts enabled, None when interrupts are off.
pub fn canonical_pc(states: &[kvm_regs]) -> Option<u64> {
    states
        .iter()
        .filter(|r| r.rflags & IF != 0)
        .map(|r| r.rip)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_that_save_rflags_are_not_single_stepped() {
        for c in [
            &[0x9c][..],         // pushf
            &[0x66, 0x9c],       // pushf with an operand-size prefix
            &[0x9d],             // popf
            &[0x48, 0xcf],       // iretq
            &[0xcc],             // int3
            &[0xcd, 0x80],       // int 0x80
            &[0x0f, 0x05],       // syscall
            &[0x48, 0x0f, 0x07], // sysretq
            &[0x0f, 0x0b],       // ud2
            &[0x66, 0x66],       // prefixes only
            &[0x0f],             // cut short
        ] {
            assert!(saves_flags(c), "{c:02x?}");
        }
        // TigerBeetle's poll loop (Milestone 189's SPIN): AVX stores, sub, cmp, jne, call, ret
        for c in [
            &[0xc5, 0xfe, 0x7f, 0x04, 0x38][..], // vmovdqu [rax+rdi], ymm0
            &[0x48, 0x83, 0xef, 0x80],           // sub rdi, -128
            &[0x48, 0x39, 0xf9],                 // cmp rcx, rdi
            &[0x75, 0xe0],                       // jne
            &[0xe8, 0, 0, 0, 0],                 // call
            &[0xc3],                             // ret
            &[0x0f, 0x1f, 0x40, 0x00],           // nop
        ] {
            assert!(!saves_flags(c), "{c:02x?}");
        }
    }

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

    #[test]
    fn flows() {
        let f = |c: &[u8]| decode(c).unwrap().flow;
        // mov edx,[rip+disp32]: a load into rdx (bit 2), no address register
        let x = f(&[0x8b, 0x15, 0, 1, 0, 0]);
        assert_eq!((x.reads, x.writes, x.addr), (0, 1 << 2, 0));
        // test edx,edx: reads rdx, writes the flags
        let x = f(&[0x85, 0xd2]);
        assert_eq!((x.reads, x.writes, x.flags_out), (1 << 2, 0, FlagsOut::All));
        // add rax,1: rax into rax and the flags
        let x = f(&[0x48, 0x83, 0xc0, 0x01]);
        assert_eq!((x.reads, x.writes, x.flags_out), (1, 1, FlagsOut::All));
        // inc ecx: CF is kept
        let x = f(&[0xff, 0xc1]);
        assert_eq!((x.reads, x.writes, x.flags_out), (2, 2, FlagsOut::Partial));
        // mov al,[rdi]: a byte merges into rax; rdi is the address
        let x = f(&[0x8a, 0x07]);
        assert_eq!((x.reads, x.writes, x.addr), (1, 1, 1 << 7));
        // mov ah,cl (88 cc: AH without a REX prefix is rax's second byte)
        let x = f(&[0x88, 0xcc]);
        assert_eq!((x.reads, x.writes), (1 | 2, 1));
        // with a REX prefix the same number is SPL: mov spl,cl (40 88 cc)
        let x = f(&[0x40, 0x88, 0xcc]);
        assert_eq!((x.reads, x.writes), (2 | 1 << 4, 1 << 4));
        // xor eax,eax: zero whatever rax held
        let x = f(&[0x31, 0xc0]);
        assert_eq!((x.reads, x.writes), (0, 1));
        // lea rax,[rax+1]: the address registers are data, no memory
        let x = f(&[0x48, 0x8d, 0x40, 0x01]);
        assert_eq!((x.reads, x.writes, x.addr), (1, 1, 0));
        // mov r9,[r12+r13*8]: REX.B/X/R numbers
        let x = f(&[0x4f, 0x8b, 0x0c, 0xec]);
        assert_eq!((x.writes, x.addr), (1 << 9, 1 << 12 | 1 << 13));
        // bt [rdi],eax: the bit offset is part of the address
        let x = f(&[0x0f, 0xa3, 0x07]);
        assert_eq!((x.addr, x.flags_out), (1 << 7 | 1, FlagsOut::Partial));
        // jne: reads the flags, decides the next instruction
        let x = f(&[0x75, 0xee]);
        assert!(x.flags_in && x.control);
        // cmovne eax,ecx: the flags, both registers
        let x = f(&[0x0f, 0x45, 0xc1]);
        assert_eq!((x.reads, x.writes, x.flags_in), (1 | 2, 1, true));
        // nop with REX.B is xchg r8,rax
        let x = f(&[0x41, 0x90]);
        assert_eq!((x.reads, x.writes), (1 | 1 << 8, 1 | 1 << 8));
    }

    /// The states of `passes` passes of `body` (instruction bytes at consecutive addresses from
    /// `at`, the last a jump back), each pass applying `step(pass, index, regs)`; the flows.
    fn trace(
        at: u64,
        body: &[&[u8]],
        passes: usize,
        step: impl Fn(usize, usize, &mut kvm_regs),
    ) -> (Vec<kvm_regs>, Vec<Flow>) {
        let mut r = kvm_regs {
            rip: at,
            rflags: 0x202,
            rax: 5,
            rdx: 1,
            ..kvm_regs::default()
        };
        let (mut st, mut fl) = (Vec::new(), Vec::new());
        for k in 0..passes {
            let mut rip = at;
            for (i, c) in body.iter().enumerate() {
                r.rip = rip;
                st.push(r);
                fl.push(decode(c).unwrap().flow);
                step(k, i, &mut r);
                rip += c.len() as u64;
            }
        }
        r.rip = at;
        st.push(r);
        (st, fl)
    }

    #[test]
    fn counter_loops() {
        // gcc -O2's `while (flag) n++`: add rax,1; mov edx,[rip+x]; test edx,edx; jne back
        let body: [&[u8]; 4] = [
            &[0x48, 0x83, 0xc0, 0x01],
            &[0x8b, 0x15, 0, 1, 0, 0],
            &[0x85, 0xd2],
            &[0x75, 0xf2],
        ];
        // the counter's flags: PF and SF follow its value
        let count = |_: usize, i: usize, r: &mut kvm_regs| {
            if i == 0 {
                r.rax += 1;
                r.rflags = 0x202
                    | if r.rax.count_ones().is_multiple_of(2) {
                        4
                    } else {
                        0
                    };
            }
            if i == 2 {
                r.rflags = 0x202; // test of 1: ZF, SF, PF clear
            }
        };
        let (st, fl) = trace(0x1000, &body, 2, count);
        assert!(counter_loop(&st, &fl));
        assert_eq!(canonical_pc(&st[4..8]), Some(0x1000));
        // with interrupts off it is still a counter loop, but has no canonical instruction
        let (mut st2, fl2) = trace(0x1000, &body, 2, count);
        for s in &mut st2 {
            s.rflags &= !IF;
        }
        assert!(counter_loop(&st2, &fl2));
        assert_eq!(canonical_pc(&st2[4..8]), None);

        // the counter decides the branch: cmp rax,rbx; jne -- it ends when rax reaches rbx
        let cmp: [&[u8]; 3] = [
            &[0x48, 0x83, 0xc0, 0x01],
            &[0x48, 0x39, 0xd8],
            &[0x75, 0xf7],
        ];
        let (st, fl) = trace(0x1000, &cmp, 2, |_, i, r| {
            if i == 0 {
                r.rax += 1
            }
        });
        assert!(!counter_loop(&st, &fl));

        // the counter is in an address: mov edx,[rdi+rax*4] -- other bytes each pass
        let idx: [&[u8]; 4] = [
            &[0x48, 0x83, 0xc0, 0x01],
            &[0x8b, 0x14, 0x87],
            &[0x85, 0xd2],
            &[0x75, 0xf5],
        ];
        let (st, fl) = trace(0x1000, &idx, 2, |_, i, r| {
            if i == 0 {
                r.rax += 1
            }
        });
        assert!(!counter_loop(&st, &fl));

        // an untainted register that differs between the passes: not the same pass twice
        let (st, fl) = trace(0x1000, &body, 2, |k, i, r| {
            count(k, i, r);
            if k == 1 && i == 1 {
                r.rdx = 7; // the loaded value changed: memory is not what the proof assumes
            }
        });
        assert!(!counter_loop(&st, &fl));

        // nothing changes: an exact cycle, the exact rule's
        let (st, fl) = trace(0x1000, &body[1..], 2, |_, _, _| {});
        assert!(!counter_loop(&st, &fl));
        // and an odd number of states, or a pass back to another instruction, is no proof
        assert!(!counter_loop(&st[..st.len() - 1], &fl[..fl.len() - 1]));
    }
}
