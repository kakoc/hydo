//! The guest's Linux kernel as hydo reads it from guest memory: its symbols (the `System.map` kept
//! beside the pinned kernel), its structures' layouts (its own BTF, in its image), its task list,
//! each task's saved registers, and its kernel stack through the kernel's ORC tables (x86_64).

use super::btf::Btf;
use super::mem::{translate, Arch, Phys, Space, PAGE};

/// The start of the kernel image's mapping on x86_64 (`__START_KERNEL_map`).
const KERNEL_MAP: u64 = 0xffff_ffff_8000_0000;
/// The kernel stack's size (x86_64 and arm64 with 4 KiB pages, no KASAN).
pub const THREAD_SIZE: u64 = 16384;
/// `PF_KTHREAD`.
const PF_KTHREAD: u32 = 0x0020_0000;

/// The kernel's symbols, from a `System.map`.
pub struct KSyms {
    all: Vec<(u64, String)>,
    text: Vec<(u64, String)>,
}

impl KSyms {
    pub fn parse(map: &str) -> KSyms {
        let mut all = Vec::new();
        let mut text = Vec::new();
        for l in map.lines() {
            let mut w = l.split_whitespace();
            let (Some(a), Some(t), Some(n)) = (w.next(), w.next(), w.next()) else {
                continue;
            };
            let Ok(a) = u64::from_str_radix(a, 16) else {
                continue;
            };
            if matches!(t, "T" | "t" | "W" | "w") {
                text.push((a, n.to_string()));
            }
            all.push((a, n.to_string()));
        }
        text.sort();
        all.sort_by(|x, y| x.1.cmp(&y.1));
        KSyms { all, text }
    }

    pub fn get(&self, name: &str) -> Option<u64> {
        let i = self.all.binary_search_by(|x| x.1.as_str().cmp(name)).ok()?;
        Some(self.all[i].0)
    }

    /// `function+0xoff` of a kernel text address.
    pub fn name(&self, pc: u64) -> Option<String> {
        let i = self.text.partition_point(|x| x.0 <= pc).checked_sub(1)?;
        let (a, n) = &self.text[i];
        (pc - a < 0x10_0000).then(|| format!("{n}+{:#x}", pc - a))
    }

    pub fn is_text(&self, pc: u64) -> bool {
        match (self.get("_stext"), self.get("_etext")) {
            (Some(a), Some(b)) => a <= pc && pc < b,
            _ => false,
        }
    }
}

/// The kernel's address space: its image, its direct map of all RAM, and the rest (the
/// stacks of `CONFIG_VMAP_STACK`) through its own page tables.
pub struct KSpace<'m> {
    pub m: &'m dyn Phys,
    pub arch: Arch,
    pub phys_base: u64,
    pub page_offset: u64,
    /// The guest-physical address of the kernel's top page table (`init_top_pgt`).
    pub pgd: u64,
}

impl Space for KSpace<'_> {
    fn phys(&self) -> &dyn Phys {
        self.m
    }
    fn gpa(&self, va: u64) -> Option<u64> {
        if self.arch == Arch::X86_64 {
            if va >= KERNEL_MAP {
                return Some(va - KERNEL_MAP + self.phys_base);
            }
            if va >= self.page_offset && va - self.page_offset < (64 << 40) {
                return Some(va - self.page_offset);
            }
        }
        translate(self.m, self.arch, self.pgd, va)
    }
}

/// The layouts hydo reads, from the kernel's BTF.
pub struct Layout {
    pub tasks: u64,
    pub pid: u64,
    pub tgid: u64,
    pub comm: u64,
    pub state: u64,
    pub flags: u64,
    pub stack: u64,
    pub mm: u64,
    pub signal: u64,
    pub thread_node: u64,
    pub thread_head: u64,
    pub thread_sp: u64,
    pub mm_pgd: u64,
    pub mm_exe: u64,
    pub file_dentry: u64,
    pub d_name: u64,
    pub d_parent: u64,
    pub pcpu_current: Option<u64>,
    pub frame_bp: u64,
    pub frame_ret: u64,
    pub frame_size: u64,
    pub regs: PtRegs,
}

/// `struct pt_regs` (x86_64) by its members.
#[derive(Clone, Debug, Default)]
pub struct PtRegs {
    pub size: u64,
    pub off: Vec<(&'static str, u64)>,
}

const PT_REGS: [&str; 21] = [
    "r15", "r14", "r13", "r12", "bp", "bx", "r11", "r10", "r9", "r8", "ax", "cx", "dx", "si", "di",
    "orig_ax", "ip", "cs", "flags", "sp", "ss",
];

impl Layout {
    pub fn read(b: &Btf) -> Result<Layout, String> {
        let need = |s: &str, p: &str| {
            b.off(s, p)
                .ok_or_else(|| format!("the kernel's BTF has no {s}.{p}"))
        };
        let mut regs = PtRegs {
            size: b.struct_size("pt_regs").unwrap_or(168),
            off: Vec::new(),
        };
        for (i, n) in PT_REGS.iter().enumerate() {
            regs.off
                .push((n, b.off("pt_regs", n).unwrap_or(i as u64 * 8)));
        }
        Ok(Layout {
            tasks: need("task_struct", "tasks")?,
            pid: need("task_struct", "pid")?,
            tgid: need("task_struct", "tgid")?,
            comm: need("task_struct", "comm")?,
            state: need("task_struct", "__state")?,
            flags: need("task_struct", "flags")?,
            stack: need("task_struct", "stack")?,
            mm: need("task_struct", "mm")?,
            signal: need("task_struct", "signal")?,
            thread_node: need("task_struct", "thread_node")?,
            thread_head: need("signal_struct", "thread_head")?,
            thread_sp: need("task_struct", "thread.sp")?,
            mm_pgd: need("mm_struct", "pgd")?,
            mm_exe: need("mm_struct", "exe_file")?,
            file_dentry: need("file", "f_path.dentry")?,
            d_name: need("dentry", "d_name.name")?,
            d_parent: need("dentry", "d_parent")?,
            pcpu_current: b.off("pcpu_hot", "current_task"),
            frame_bp: b.off("inactive_task_frame", "bp").unwrap_or(40),
            frame_ret: b.off("inactive_task_frame", "ret_addr").unwrap_or(48),
            frame_size: b.struct_size("inactive_task_frame").unwrap_or(56),
            regs,
        })
    }
}

impl PtRegs {
    pub fn get(&self, s: &dyn Space, base: u64, name: &str) -> Option<u64> {
        let o = self.off.iter().find(|x| x.0 == name)?.1;
        s.u64(base + o)
    }
}

/// One task as read from the task list.
#[derive(Clone, Debug, Default)]
pub struct Task {
    pub addr: u64,
    pub pid: u64,
    pub tid: u64,
    pub comm: String,
    pub state: u32,
    pub kthread: bool,
    pub stack: u64,
    pub mm: u64,
    pub thread_sp: u64,
}

/// The task's state in words.
pub fn state_name(s: u32) -> &'static str {
    if s == 0 {
        "running"
    } else if s & 0x8000 != 0 {
        "frozen"
    } else if s & 4 != 0 {
        "stopped"
    } else if s & 8 != 0 {
        "traced"
    } else if s & 0x20 != 0 {
        "zombie"
    } else if s & 0x10 != 0 || s & 0x80 != 0 {
        "dead"
    } else if s & 0x402 == 0x402 {
        "idle"
    } else if s & 2 != 0 {
        "disk-sleep"
    } else if s & 1 != 0 {
        "sleeping"
    } else {
        "other"
    }
}

/// Every task: the thread groups of `init_task`'s list, and each group's threads.
pub fn tasks(k: &dyn Space, l: &Layout, init_task: u64) -> Vec<Task> {
    let mut out = Vec::new();
    let read = |t: u64| -> Option<Task> {
        let comm = k.bytes(t + l.comm, 16)?;
        let e = comm.iter().position(|&c| c == 0).unwrap_or(16);
        Some(Task {
            addr: t,
            pid: k.u32(t + l.tgid)? as u64,
            tid: k.u32(t + l.pid)? as u64,
            comm: String::from_utf8_lossy(&comm[..e]).into_owned(),
            state: k.u32(t + l.state)?,
            kthread: k.u32(t + l.flags)? & PF_KTHREAD != 0,
            stack: k.u64(t + l.stack)?,
            mm: k.u64(t + l.mm)?,
            thread_sp: k.u64(t + l.thread_sp)?,
        })
    };
    let mut p = init_task;
    for _ in 0..65536 {
        let Some(next) = k.u64(p + l.tasks) else {
            break;
        };
        let next = next.wrapping_sub(l.tasks);
        if p != init_task {
            // the group's threads
            let head = k.u64(p + l.signal).map(|s| s + l.thread_head);
            let mut seen = 0;
            if let Some(head) = head {
                let mut n = k.u64(head);
                while let Some(node) = n {
                    if node == head || seen > 4096 {
                        break;
                    }
                    let t = node.wrapping_sub(l.thread_node);
                    if let Some(x) = read(t) {
                        out.push(x);
                    }
                    seen += 1;
                    n = k.u64(node);
                }
            }
            if seen == 0 {
                if let Some(x) = read(p) {
                    out.push(x);
                }
            }
        }
        if next == init_task {
            break;
        }
        p = next;
    }
    out
}

/// The program of a process: the path of `mm->exe_file`, from its dentries.
pub fn exe_path(k: &dyn Space, l: &Layout, mm: u64) -> Option<String> {
    let file = k.u64(mm + l.mm_exe)?;
    if file == 0 {
        return None;
    }
    let mut d = k.u64(file + l.file_dentry)?;
    let mut parts = Vec::new();
    for _ in 0..32 {
        let name = k.cstr(k.u64(d + l.d_name)?, 256)?;
        let parent = k.u64(d + l.d_parent)?;
        if parent == d || name == "/" {
            break;
        }
        parts.push(name);
        d = parent;
    }
    parts.reverse();
    Some(format!("/{}", parts.join("/")))
}

// ---------------------------------------------------------------------------- ORC (x86_64)

/// The kernel's ORC unwind tables (`CONFIG_UNWINDER_ORC`), read once from guest memory.
pub struct Orc {
    ips: Vec<u64>,
    entries: Vec<[u8; 6]>,
}

/// One frame of a kernel stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KFrame {
    pub pc: u64,
    pub how: &'static str,
}

impl Orc {
    pub fn read(k: &dyn Space, syms: &KSyms) -> Option<Orc> {
        let a = syms.get("__start_orc_unwind_ip")?;
        let b = syms.get("__stop_orc_unwind_ip")?;
        let e = syms.get("__start_orc_unwind")?;
        let n = ((b - a) / 4) as usize;
        let raw = k.bytes(a, n * 4)?;
        let ent = k.bytes(e, n * 6)?;
        let mut ips = Vec::with_capacity(n);
        for i in 0..n {
            let v = i32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().ok()?);
            ips.push((a + i as u64 * 4).wrapping_add(v as i64 as u64));
        }
        let entries = ent
            .chunks_exact(6)
            .map(|c| c.try_into().unwrap_or([0; 6]))
            .collect();
        Some(Orc { ips, entries })
    }

    fn find(&self, ip: u64) -> Option<[u8; 6]> {
        let i = self.ips.partition_point(|&x| x <= ip).checked_sub(1)?;
        self.entries.get(i).copied()
    }

    /// Unwind from `(ip, sp, bp)`; `first`: the ip is where the task is (a running vCPU), not a
    /// return address. Ends at the user's registers (the frame of type REGS from user mode).
    pub fn unwind(
        &self,
        k: &dyn Space,
        regs: &PtRegs,
        ip: u64,
        sp: u64,
        bp: u64,
        first: bool,
    ) -> Vec<KFrame> {
        let (mut ip, mut sp, mut bp, mut signal) = (ip, sp, bp, first);
        let mut out = Vec::new();
        for _ in 0..64 {
            out.push(KFrame { pc: ip, how: "orc" });
            let Some(e) = self.find(if signal { ip } else { ip.wrapping_sub(1) }) else {
                break;
            };
            let sp_off = i16::from_le_bytes([e[0], e[1]]) as i64;
            let bp_off = i16::from_le_bytes([e[2], e[3]]) as i64;
            let (sp_reg, bp_reg) = (e[4] & 0xf, e[4] >> 4);
            let ty = e[5] & 7;
            if ty == 1 || ty == 0 {
                break; // the end of the stack, or no information
            }
            let mut indirect = false;
            let mut nsp = match sp_reg {
                5 => sp.wrapping_add(sp_off as u64),
                4 => bp.wrapping_add(sp_off as u64),
                9 => {
                    indirect = true;
                    sp
                }
                8 => {
                    indirect = true;
                    bp.wrapping_add(sp_off as u64)
                }
                _ => break,
            };
            if indirect {
                let Some(v) = k.u64(nsp) else { break };
                nsp = v;
                if sp_reg == 9 {
                    nsp = nsp.wrapping_add(sp_off as u64);
                }
            }
            match ty {
                2 => {
                    let Some(r) = k.u64(nsp.wrapping_sub(8)) else {
                        break;
                    };
                    match bp_reg {
                        1 => match k.u64(nsp.wrapping_add(bp_off as u64)) {
                            Some(v) => bp = v,
                            None => break,
                        },
                        4 => match k.u64(bp.wrapping_add(bp_off as u64)) {
                            Some(v) => bp = v,
                            None => break,
                        },
                        _ => {}
                    }
                    if nsp <= sp {
                        break;
                    }
                    ip = r;
                    sp = nsp;
                    signal = false;
                }
                3 | 4 => {
                    // the registers an interrupt or a system call saved
                    let base = if ty == 3 {
                        nsp
                    } else {
                        nsp.wrapping_sub(regs.off.iter().find(|x| x.0 == "ip").map_or(128, |x| x.1))
                    };
                    let (Some(rip), Some(rsp), Some(cs)) = (
                        regs.get(k, base, "ip"),
                        regs.get(k, base, "sp"),
                        regs.get(k, base, "cs"),
                    ) else {
                        break;
                    };
                    if cs & 3 == 3 {
                        break; // the user's registers: the kernel stack ends here
                    }
                    if ty == 3 {
                        bp = regs.get(k, base, "bp").unwrap_or(bp);
                    }
                    ip = rip;
                    sp = rsp;
                    signal = true;
                }
                _ => break,
            }
            if ip == 0 {
                break;
            }
        }
        out
    }
}

/// Kernel text addresses found on a stack, innermost first (no unwind tables).
pub fn scan(k: &dyn Space, syms: &KSyms, sp: u64, top: u64) -> Vec<KFrame> {
    let mut out = Vec::new();
    let mut a = sp & !7;
    while a < top && out.len() < 64 {
        if let Some(v) = k.u64(a) {
            if syms.is_text(v) {
                out.push(KFrame { pc: v, how: "scan" });
            }
        }
        a += 8;
    }
    out
}

/// The guest-physical address of a page-aligned kernel table, for the tests.
pub fn page_of(x: u64) -> u64 {
    x & !(PAGE - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_and_states() {
        let s = KSyms::parse(
            "ffffffff81000000 T _stext\nffffffff811b40a0 T futex_wait\nffffffff811b4200 t futex_wait_queue\nffffffff81e00000 T _etext\nffffffff82a10900 D init_task\n",
        );
        assert_eq!(s.get("init_task"), Some(0xffffffff82a10900));
        assert_eq!(
            s.name(0xffffffff811b40b0).as_deref(),
            Some("futex_wait+0x10")
        );
        assert_eq!(
            s.name(0xffffffff811b4201).as_deref(),
            Some("futex_wait_queue+0x1")
        );
        assert!(s.is_text(0xffffffff811b40b0));
        assert!(!s.is_text(0x1000));
        assert_eq!(state_name(0), "running");
        assert_eq!(state_name(1), "sleeping");
        assert_eq!(state_name(0x402), "idle");
        assert_eq!(state_name(4), "stopped");
        assert_eq!(state_name(0x8001), "frozen");
    }

    #[test]
    fn orc_walks_call_frames_to_the_users_registers() {
        // memory: a direct-map space where va == gpa
        struct Flat(Vec<u8>);
        impl Phys for Flat {
            fn read(&self, gpa: u64, b: &mut [u8]) -> bool {
                self.0.read(gpa, b)
            }
        }
        struct S<'a>(&'a Flat);
        impl Space for S<'_> {
            fn phys(&self) -> &dyn Phys {
                self.0
            }
            fn gpa(&self, va: u64) -> Option<u64> {
                Some(va)
            }
        }
        let mut m = vec![0u8; 0x2000];
        let put = |m: &mut Vec<u8>, a: u64, v: u64| {
            m[a as usize..a as usize + 8].copy_from_slice(&v.to_le_bytes())
        };
        // f at [0x100,0x200): CALL, sp_reg SP + 16; g at [0x200,0x300): REGS at sp + 8
        // stack: sp=0x1000; [0x1008] = return address 0x250 (into g)
        put(&mut m, 0x1008, 0x250);
        // pt_regs at 0x1010 + 8 = 0x1018 for g's frame: ip, sp, cs at 16*8, 19*8, 17*8
        let regs = PtRegs {
            size: 168,
            off: PT_REGS
                .iter()
                .enumerate()
                .map(|(i, n)| (*n, i as u64 * 8))
                .collect(),
        };
        put(&mut m, 0x1018 + 16 * 8, 0x401000);
        put(&mut m, 0x1018 + 19 * 8, 0x7ffd0000);
        put(&mut m, 0x1018 + 17 * 8, 0x33);
        let orc = Orc {
            ips: vec![0x100, 0x200, 0x300],
            entries: vec![[16, 0, 0, 0, 5, 2], [8, 0, 0, 0, 5, 3], [0, 0, 0, 0, 0, 1]],
        };
        let f = Flat(m);
        let k = S(&f);
        let fr = orc.unwind(&k, &regs, 0x150, 0x1000, 0, true);
        assert_eq!(
            fr.iter().map(|x| x.pc).collect::<Vec<_>>(),
            vec![0x150, 0x250]
        );
    }
}
