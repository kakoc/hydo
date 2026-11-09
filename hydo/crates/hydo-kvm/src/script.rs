//! A scripted vCPU for the tests: a tiny guest program of port I/O, MMIO, MSR, HLT and interrupt
//! operations, interpreted with KVM's exit contract ([`crate::cpu`]), so the engine's run loop,
//! devices, interrupt rule, snapshots, fingerprints and lock-step protocol run on any machine.
//! Its instruction pointer is `rip - ENTRY` (the boot recipe's entry), its accumulator is `rax`,
//! and an interrupt pushes the return point into `r15` (handlers do not nest: IF is cleared).

use crate::abi::kvm_regs;
use crate::cpu::{Cpu, CpuState, Exit, Hypervisor, Reply};
use std::collections::HashMap;
use std::sync::Arc;

/// The boot recipe's entry (`boot::regs().rip`): the program's instruction 0.
pub const ENTRY: u64 = 0x100200;

/// One operation of a scripted guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// OUT of an immediate (size 1, 2 or 4)
    Out(u16, u8, u32),
    /// OUT of the accumulator's low dword
    OutAcc(u16),
    /// IN into the accumulator
    In(u16, u8),
    /// `rep insl` of two dwords into the accumulator (the hydo clock)
    InPair(u16),
    /// a string of bytes written to the UART one OUT at a time
    Print(Vec<u8>),
    Hlt,
    Sti,
    Cli,
    /// return from an interrupt handler: back to the interrupted instruction, IF set
    Iret,
    Jmp(usize),
    /// jump when the accumulator is below the value
    JmpIfBelow(u64, usize),
    /// add to the counter register (rbx)
    Inc,
    /// jump when the counter is below the value
    JmpIfCountBelow(u64, usize),
    RdMsr(u32),
    WrMsr(u32, u64),
    MmioRead(u64, u8),
    MmioWrite(u64, u8, u64),
    /// a triple fault
    Shutdown,
}

/// A program and its interrupt handlers (vector -> instruction).
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub ops: Vec<Op>,
    pub handlers: HashMap<u8, usize>,
}

/// The scripted machine maker: every fresh VM runs the same program.
pub struct Script {
    pub program: Arc<Program>,
}

impl Hypervisor for Script {
    unsafe fn fresh(&mut self, _ram: *mut u8, _len: usize) -> Result<Box<dyn Cpu>, String> {
        Ok(Box::new(ScriptCpu {
            p: self.program.clone(),
            s: CpuState::default(),
            pending: None,
            window: false,
            print_at: 0,
            wait: Wait::None,
            stepping: None,
            ready: false,
            ifl: false,
        }))
    }
    fn describe(&self) -> String {
        "hydo-kvm: a scripted vCPU".into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Wait {
    None,
    /// an IN waiting for its answer (bytes, then completion)
    In(Option<Vec<u8>>),
    /// an exit that only needs completion (OUT, MMIO write, WRMSR)
    Done,
    /// an RDMSR or MMIO read waiting for its value
    Value(Option<u64>),
}

pub struct ScriptCpu {
    p: Arc<Program>,
    s: CpuState,
    pending: Option<u8>,
    window: bool,
    /// the next byte of a Print operation
    print_at: usize,
    wait: Wait,
    /// the addresses a step runs to ([`Cpu::step_to`]): at least one operation, then the first
    /// arrival at one of them
    stepping: Option<Vec<u64>>,
    /// what `kvm_run` says since the last exit (`ready_for_interrupt_injection`, `if_flag`): set
    /// only when a run or a completion returns, as KVM does, so a fresh vCPU says 0 until its
    /// first run, whatever state was loaded into it
    ready: bool,
    ifl: bool,
}

impl ScriptCpu {
    /// Whether an interrupt injected now is taken before the next instruction.
    fn can_take(&self) -> bool {
        self.if_on() && self.pending.is_none() && self.wait == Wait::None
    }

    /// KVM's `post_kvm_run_save`: the flags of the state the vCPU returned in.
    fn post_run(&mut self) {
        self.ready = self.can_take();
        self.ifl = self.if_on();
    }

    /// Run until an exit (the body of [`Cpu::run`]).
    fn exec(&mut self) -> Exit {
        self.finish();
        if let Some(v) = self.pending.take() {
            // the injected interrupt is taken before the next instruction; one taken between
            // two bytes of a Print keeps that Print's place in r13 (a forced preemption point
            // can make the timer due there)
            self.s.regs.r15 = self.s.regs.rip;
            self.s.regs.r13 = self.print_at as u64;
            self.print_at = 0;
            self.set_if(false);
            match self.p.handlers.get(&v) {
                Some(h) => self.set_pc(*h),
                None => return Exit::Fail(format!("no handler for vector {v:#x}")),
            }
        }
        for done in 0..100_000 {
            if let Some(at) = &self.stepping {
                if done > 0 && at.contains(&self.s.regs.rip) {
                    return Exit::Debug;
                }
            }
            if self.window && self.if_on() {
                self.window = false;
                return Exit::IrqWindow;
            }
            let pc = self.pc();
            let Some(op) = self.p.ops.get(pc).cloned() else {
                return Exit::Fail(format!("the program ran off its end at {pc}"));
            };
            match op {
                Op::Out(port, size, v) => {
                    self.wait = Wait::Done;
                    return Exit::Io {
                        port,
                        size,
                        out: true,
                        count: 1,
                        data: v.to_le_bytes()[..size as usize].to_vec(),
                    };
                }
                Op::OutAcc(port) => {
                    self.wait = Wait::Done;
                    return Exit::Io {
                        port,
                        size: 4,
                        out: true,
                        count: 1,
                        data: (self.s.regs.rax as u32).to_le_bytes().to_vec(),
                    };
                }
                Op::In(port, size) => {
                    self.wait = Wait::In(None);
                    return Exit::Io {
                        port,
                        size,
                        out: false,
                        count: 1,
                        data: Vec::new(),
                    };
                }
                Op::InPair(port) => {
                    self.wait = Wait::In(None);
                    return Exit::Io {
                        port,
                        size: 4,
                        out: false,
                        count: 2,
                        data: Vec::new(),
                    };
                }
                Op::Print(s) => {
                    self.wait = Wait::Done;
                    return Exit::Io {
                        port: crate::uart::BASE,
                        size: 1,
                        out: true,
                        count: 1,
                        data: vec![s[self.print_at]],
                    };
                }
                Op::Hlt => {
                    self.set_pc(pc + 1);
                    return Exit::Hlt;
                }
                Op::Sti => {
                    self.set_if(true);
                    self.set_pc(pc + 1);
                }
                Op::Cli => {
                    self.set_if(false);
                    self.set_pc(pc + 1);
                }
                Op::Iret => {
                    self.s.regs.rip = self.s.regs.r15;
                    self.print_at = self.s.regs.r13 as usize;
                    self.s.regs.r13 = 0;
                    self.set_if(true);
                }
                Op::Jmp(t) => self.set_pc(t),
                Op::JmpIfBelow(v, t) => {
                    if self.s.regs.rax < v {
                        self.set_pc(t)
                    } else {
                        self.set_pc(pc + 1)
                    }
                }
                Op::Inc => {
                    self.s.regs.rbx += 1;
                    self.set_pc(pc + 1);
                }
                Op::JmpIfCountBelow(v, t) => {
                    if self.s.regs.rbx < v {
                        self.set_pc(t)
                    } else {
                        self.set_pc(pc + 1)
                    }
                }
                Op::RdMsr(index) => {
                    self.wait = Wait::Value(None);
                    return Exit::RdMsr { index };
                }
                Op::WrMsr(index, value) => {
                    self.wait = Wait::Done;
                    return Exit::WrMsr { index, value };
                }
                Op::MmioRead(addr, len) => {
                    self.wait = Wait::Value(None);
                    return Exit::Mmio {
                        addr,
                        len,
                        write: false,
                        data: [0; 8],
                    };
                }
                Op::MmioWrite(addr, len, v) => {
                    self.wait = Wait::Done;
                    return Exit::Mmio {
                        addr,
                        len,
                        write: true,
                        data: v.to_le_bytes(),
                    };
                }
                Op::Shutdown => return Exit::Shutdown,
            }
        }
        // the watchdog's kick of a program that loops without an exit
        Exit::Kick
    }

    fn pc(&self) -> usize {
        self.s.regs.rip.wrapping_sub(ENTRY) as usize
    }
    fn set_pc(&mut self, pc: usize) {
        self.s.regs.rip = ENTRY + pc as u64;
    }
    fn if_on(&self) -> bool {
        self.s.regs.rflags & (1 << 9) != 0
    }
    fn set_if(&mut self, on: bool) {
        if on {
            self.s.regs.rflags |= 1 << 9;
        } else {
            self.s.regs.rflags &= !(1 << 9);
        }
    }
    /// Finish the instruction an exit stopped at.
    fn finish(&mut self) {
        let pc = self.pc();
        match std::mem::replace(&mut self.wait, Wait::None) {
            Wait::None => return,
            Wait::In(b) => {
                let b = b.unwrap_or_default();
                let mut v = [0u8; 8];
                let n = b.len().min(8);
                v[..n].copy_from_slice(&b[..n]);
                self.s.regs.rax = u64::from_le_bytes(v);
            }
            Wait::Value(v) => self.s.regs.rax = v.unwrap_or(0),
            Wait::Done => {}
        }
        if let Some(Op::Print(s)) = self.p.ops.get(pc) {
            self.print_at += 1;
            if self.print_at < s.len() {
                return;
            }
        }
        self.print_at = 0;
        self.set_pc(pc + 1);
    }
}

impl Cpu for ScriptCpu {
    fn run(&mut self) -> Exit {
        let e = self.exec();
        self.post_run();
        e
    }

    fn reply(&mut self, r: Reply) {
        self.wait = match (std::mem::replace(&mut self.wait, Wait::None), r) {
            (Wait::In(_), Reply::Io(b)) => Wait::In(Some(b)),
            (Wait::Value(_), Reply::Mmio(d)) => Wait::Value(Some(u64::from_le_bytes(d))),
            (Wait::Value(_), Reply::Msr { value, fault }) => {
                Wait::Value(Some(if fault { u64::MAX } else { value }))
            }
            (w, r) => panic!("a reply {r:?} while waiting for {w:?}"),
        };
    }

    fn complete(&mut self) -> Option<Exit> {
        self.finish();
        self.post_run();
        None
    }

    fn interruptible(&self) -> bool {
        self.ready
    }

    fn if_flag(&self) -> bool {
        self.ifl
    }

    fn inject(&mut self, vector: u8) {
        assert!(self.can_take(), "an injection the vCPU cannot take");
        self.pending = Some(vector);
    }

    fn request_window(&mut self, on: bool) {
        self.window = on;
    }

    fn run_flags(&self) -> [u8; 3] {
        [self.ready as u8, self.ifl as u8, self.window as u8]
    }

    fn set_run_flags(&mut self, f: [u8; 3]) {
        self.ready = f[0] != 0;
        self.ifl = f[1] != 0;
        self.window = f[2] != 0;
    }
    fn save(&self) -> CpuState {
        let mut s = self.s.clone();
        if let Some(v) = self.pending {
            s.events.interrupt.injected = 1;
            s.events.interrupt.nr = v;
        }
        s.regs.r14 = self.print_at as u64;
        s
    }

    fn load(&mut self, s: &CpuState) {
        self.s = s.clone();
        self.pending = (s.events.interrupt.injected != 0).then_some(s.events.interrupt.nr);
        self.s.events.interrupt.injected = 0;
        self.print_at = s.regs.r14 as usize;
        self.wait = Wait::None;
    }

    fn rip(&self) -> u64 {
        self.s.regs.rip
    }

    fn regs(&self) -> kvm_regs {
        self.s.regs
    }

    fn cr3(&self) -> u64 {
        0
    }

    fn step_to(&mut self, step: Option<&crate::spin::Step>) {
        self.stepping = step.map(|s| s.addrs().to_vec());
    }

    /// The scripted counterparts of [`crate::spin::decode`]: jumps, compares and register
    /// arithmetic, with their data flow (the counter `Inc` feeds only itself; a jump on the
    /// accumulator or on the counter is a conditional branch on that register).
    fn next_step(&self, _ram: &[u8], _cr3: u64) -> Option<(crate::spin::Step, crate::spin::Flow)> {
        use crate::spin::Flow;
        // rax is register 0 and rbx register 3 in the flow's numbering
        let flow = match self.p.ops.get(self.pc())? {
            Op::Jmp(_) => Flow::default(),
            Op::JmpIfBelow(..) => Flow {
                reads: 1,
                control: true,
                ..Flow::default()
            },
            Op::JmpIfCountBelow(..) => Flow {
                reads: 1 << 3,
                control: true,
                ..Flow::default()
            },
            Op::Inc => Flow {
                reads: 1 << 3,
                writes: 1 << 3,
                ..Flow::default()
            },
            _ => return None,
        };
        // where the operation can end: the next operation, and a jump's target
        let pc = self.pc();
        let at = |p: usize| ENTRY + p as u64;
        let step = match self.p.ops[pc] {
            Op::Jmp(t) => crate::spin::Step {
                at: [at(t), 0],
                n: 1,
            },
            Op::JmpIfBelow(_, t) | Op::JmpIfCountBelow(_, t) if t != pc + 1 => crate::spin::Step {
                at: [at(pc + 1), at(t)],
                n: 2,
            },
            _ => crate::spin::Step {
                at: [at(pc + 1), 0],
                n: 1,
            },
        };
        Some((step, flow))
    }

    fn kicker(&self) -> Box<dyn Fn() + Send + Sync> {
        Box::new(|| {})
    }
}

/// A synthetic bzImage the boot recipe accepts (the scripted vCPU never reads it).
pub fn bzimage() -> Vec<u8> {
    let mut b = vec![0u8; 0x4000];
    b[0x1f1] = 4;
    b[0x1fe] = 0x55;
    b[0x1ff] = 0xaa;
    b[0x202..0x206].copy_from_slice(b"HdrS");
    b[0x206..0x208].copy_from_slice(&0x020fu16.to_le_bytes());
    b[0x236..0x238].copy_from_slice(&1u16.to_le_bytes());
    b
}

/// The general registers a scripted program starts with (for tests that compare states).
pub fn start_regs() -> kvm_regs {
    crate::boot::regs()
}
