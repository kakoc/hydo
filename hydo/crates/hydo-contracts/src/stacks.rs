//! Every guest thread's stack at a stop, read without a change of the run (Milestone 383,
//! docs/design/inspect-stacks.md).
//!
//! An engine writes the stop's state to a file (`DUMP`: every vCPU's registers and the guest's
//! RAM). This module reads it on the host:
//!
//! - [`linux`]: the guest kernel's task list, through the symbols of the pinned kernel (its
//!   `System.map`) and the layouts of its own BTF, which the kernel keeps in its image;
//! - [`mem`]: guest memory through each process's page tables (x86_64 and arm64);
//! - [`user`]: each thread's user stack (Go's table of frame sizes, the call frame information,
//!   frame pointers), named with [`program`]'s symbols, lines and Go table;
//! - [`golang`]: a Go program's goroutines, from its runtime's structures;
//! - the locks: a thread in the futex system call waits on the address it passed, a goroutine
//!   parked on a semaphore on its `sudog`'s; each lock is named by the program's symbols, and
//!   another thread or goroutine whose stack keeps its address is its likely holder, so a cycle
//!   of waits shows.

pub mod btf;
pub mod golang;
pub mod linux;
pub mod mem;
pub mod program;
pub mod user;

use hydo_results::{
    InspectStacks, StackCycle, StackFrame, StackGoroutine, StackHolder, StackLock, StackSyscall,
    StackThread,
};
use mem::{Arch, Dump, Paged, Space};
use program::Program;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// What a read needs besides the dump.
pub struct Inputs<'a> {
    /// The guest kernel's `System.map`.
    pub kernel_map: &'a str,
    /// The host's copy of a guest program, by its path in the guest.
    pub find_program: &'a dyn Fn(&str) -> Option<String>,
    /// Only this process.
    pub pid: Option<u64>,
    /// Kernel threads too.
    pub kernel_threads: bool,
    /// The most frames of one stack.
    pub max_frames: usize,
}

/// x86_64's system call numbers hydo names.
fn syscall_name(nr: u64) -> &'static str {
    match nr {
        0 => "read",
        1 => "write",
        7 => "poll",
        23 => "select",
        35 => "nanosleep",
        43 => "accept",
        45 => "recvfrom",
        61 => "wait4",
        202 => "futex",
        232 => "epoll_wait",
        228 => "clock_gettime",
        230 => "clock_nanosleep",
        270 => "pselect6",
        271 => "ppoll",
        281 => "epoll_pwait",
        288 => "accept4",
        441 => "epoll_pwait2",
        449 => "futex_waitv",
        _ => "",
    }
}

fn hex(v: u64) -> String {
    format!("{v:#x}")
}

/// One waiter or holder candidate: a thread or a goroutine of a process, with its stack.
struct Actor {
    who: String,
    pid: u64,
    /// (pid, lock address) it waits on.
    waits: Option<u64>,
    /// The stack's words are read from `sp` up to `top`.
    sp: u64,
    top: u64,
    frames: Vec<(u64, String)>,
    /// Each frame's function, as link addresses `[lo, hi)`, and its words.
    funcs: Vec<(u64, u64, String)>,
    root: u64,
}

/// The functions of a stack's frames, innermost first.
fn funcs_of(prog: Option<&Program>, bias: u64, fr: &[user::UFrame]) -> Vec<(u64, u64, String)> {
    let Some(p) = prog else { return Vec::new() };
    fr.iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let pc = f.pc.wrapping_sub(bias).wrapping_sub(u64::from(i > 0));
            let (lo, hi) = p.range(pc)?;
            Some((lo, hi, describe(prog, bias, f.pc)))
        })
        .collect()
}

/// Read the stacks of the stop in `dump`.
pub fn read(dump: &Path, inp: &Inputs) -> Result<InspectStacks, String> {
    let d = Dump::open(dump)?;
    if d.arch != "x86_64" {
        return Err(format!(
            "the stacks of an {} guest are not built yet (docs/design/inspect-stacks.md)",
            d.arch
        ));
    }
    let arch = Arch::X86_64;
    let syms = linux::KSyms::parse(inp.kernel_map);
    let sym = |n: &str| {
        syms.get(n)
            .ok_or_else(|| format!("the kernel's System.map has no {n}"))
    };
    // the kernel image's place: with nokaslr phys_base is 0; read it to be sure
    let mut k = linux::KSpace {
        m: &d,
        arch,
        phys_base: 0,
        page_offset: 0xffff_8880_0000_0000,
        pgd: 0,
    };
    if let Some(v) = syms.get("phys_base").and_then(|a| k.u64(a)) {
        k.phys_base = v;
    }
    if let Some(v) = syms.get("page_offset_base").and_then(|a| k.u64(a)) {
        k.page_offset = v;
    }
    k.pgd = k
        .gpa(sym("init_top_pgt")?)
        .ok_or("the kernel's page table is not in guest RAM")?;
    let init_task = sym("init_task")?;
    let btf_a = sym("__start_BTF")?;
    let btf_b = sym("__stop_BTF")?;
    let btf_bytes = k
        .bytes(btf_a, (btf_b - btf_a) as usize)
        .ok_or("the kernel's BTF is not in guest RAM")?;
    let btf = btf::Btf::parse(&btf_bytes)?;
    let l = linux::Layout::read(&btf)?;
    let swapper = k.cstr(init_task + l.comm, 16).unwrap_or_default();
    if !swapper.starts_with("swapper") {
        return Err(format!(
            "the System.map does not fit the guest's kernel (init_task's name reads {swapper:?})"
        ));
    }
    let kernel = syms
        .get("linux_banner")
        .and_then(|a| k.cstr(a, 200))
        .and_then(|b| b.split_whitespace().nth(2).map(str::to_string));
    let orc = linux::Orc::read(&k, &syms);
    // the task each vCPU runs
    let mut running: HashMap<u64, usize> = HashMap::new();
    if let (Some(pco), Some(cur), Some(off)) = (
        syms.get("__per_cpu_offset"),
        syms.get("pcpu_hot").or_else(|| syms.get("current_task")),
        Some(l.pcpu_current.unwrap_or(0)),
    ) {
        for i in 0..d.vcpus.len() {
            if let Some(t) = k.u64(pco + i as u64 * 8).and_then(|b| k.u64(b + cur + off)) {
                running.insert(t, i);
            }
        }
    }
    let mut out = InspectStacks {
        engine: d.engine.clone(),
        arch: "x86_64".into(),
        kernel,
        ..InspectStacks::default()
    };
    if orc.is_none() {
        out.notes.push(
            "the kernel has no ORC tables: its stacks are kernel addresses found on the stack"
                .into(),
        );
    }
    let mut programs: HashMap<String, Option<Program>> = HashMap::new();
    let mut exe: HashMap<u64, Option<String>> = HashMap::new();
    let mut actors: Vec<Actor> = Vec::new();
    // (pid, addr) -> kind
    let mut lock_kind: BTreeMap<(u64, u64), &'static str> = BTreeMap::new();
    let tasks = linux::tasks(&k, &l, init_task);
    let mut go_done: Vec<u64> = Vec::new();
    let mut bias_of_pid: HashMap<u64, u64> = HashMap::new();
    let mut services: HashMap<u64, Option<String>> = HashMap::new();
    for t in tasks {
        if (t.kthread || t.mm == 0) && !inp.kernel_threads {
            continue;
        }
        if inp.pid.is_some_and(|p| p != t.pid) {
            continue;
        }
        let path = exe
            .entry(t.mm)
            .or_insert_with(|| (t.mm != 0).then(|| linux::exe_path(&k, &l, t.mm)).flatten())
            .clone();
        let mut th = StackThread {
            pid: t.pid,
            tid: t.tid,
            comm: t.comm.clone(),
            program: path.clone(),
            state: linux::state_name(t.state).into(),
            ..StackThread::default()
        };
        let vcpu = running.get(&t.addr).copied();
        th.vcpu = vcpu.map(|v| v as u64);
        let top = t.stack + linux::THREAD_SIZE;
        let regs_at = top - l.regs.size;
        let in_user = vcpu.is_some_and(|v| d.vcpus[v].get("cs") & 3 == 3);
        // the kernel stack
        let kf = match (vcpu, &orc) {
            (Some(_), _) if in_user => Vec::new(),
            (Some(v), Some(o)) => {
                let c = &d.vcpus[v];
                o.unwind(&k, &l.regs, c.get("rip"), c.get("rsp"), c.get("rbp"), true)
            }
            (None, Some(o)) => {
                let fr = t.thread_sp;
                match (k.u64(fr + l.frame_ret), k.u64(fr + l.frame_bp)) {
                    (Some(ip), Some(bp)) => o.unwind(&k, &l.regs, ip, fr + l.frame_size, bp, false),
                    _ => Vec::new(),
                }
            }
            (_, None) => linux::scan(&k, &syms, t.thread_sp, top),
        };
        th.kernel = kf
            .iter()
            .map(|f| StackFrame {
                pc: hex(f.pc),
                function: syms.name(f.pc),
                how: f.how.into(),
                ..StackFrame::default()
            })
            .collect();
        if t.mm == 0 {
            out.threads.push(th);
            continue;
        }
        // the user registers: the vCPU's when the thread runs in user mode, else those the
        // kernel saved at the top of its stack
        let ureg = |n: &str| l.regs.get(&k, regs_at, n).unwrap_or(0);
        let (pc, sp, fp) = if in_user {
            let c = &d.vcpus[vcpu.unwrap_or(0)];
            (c.get("rip"), c.get("rsp"), c.get("rbp"))
        } else {
            (ureg("ip"), ureg("sp"), ureg("bp"))
        };
        if !in_user {
            let nr = ureg("orig_ax");
            if (nr as i64) >= 0 && nr < 1024 && t.state != 0 {
                let args = ["di", "si", "dx", "r10", "r8", "r9"].map(ureg);
                th.syscall = Some(StackSyscall {
                    nr,
                    name: syscall_name(nr).into(),
                    args: args.iter().map(|&a| hex(a)).collect(),
                });
                // FUTEX_WAIT, FUTEX_LOCK_PI, FUTEX_WAIT_BITSET, FUTEX_WAIT_REQUEUE_PI
                if nr == 202 && matches!(args[1] & 0x7f, 0 | 6 | 9 | 11) {
                    th.waits_on = Some(hex(args[0]));
                }
            }
        }
        let Some(pgd) = k.u64(t.mm + l.mm_pgd).and_then(|v| k.gpa(v)) else {
            th.notes.push("its page tables could not be read".into());
            out.threads.push(th);
            continue;
        };
        let space = Paged {
            m: &d,
            arch,
            root: pgd,
        };
        // the hydo service the process belongs to: `HYDO_SVC` of its environment
        th.service = services
            .entry(t.mm)
            .or_insert_with(|| service_of(&k, &btf, &space, t.mm))
            .clone();
        let prog: Option<&Program> = path.as_ref().and_then(|p| {
            programs
                .entry(p.clone())
                .or_insert_with(|| (inp.find_program)(p).and_then(|h| Program::load(&h).ok()))
                .as_ref()
        });
        let bias = prog.map(|p| bias_of(&k, &btf, t.mm, p)).unwrap_or(0);
        bias_of_pid.insert(t.pid, bias);
        let go_prog = prog.is_some_and(|p| p.go.is_some());
        if prog.is_none() {
            th.notes.push(match &path {
                Some(p) => format!("hydo has no copy of {p}: its frames are not named"),
                None => "its program is not known".into(),
            });
        }
        let uf = user::unwind(
            &space,
            prog,
            bias,
            arch,
            user::URegs { pc, sp, fp, lr: 0 },
            inp.max_frames,
        );
        th.user = uf
            .iter()
            .map(|f| frame(prog, bias, f.pc, f.how, false))
            .collect();
        // a Go program's threads park on the runtime's own futexes (an idle thread's note); its
        // locks are the goroutines'
        let waits = th
            .waits_on
            .as_ref()
            .map(|_| ureg("di"))
            .filter(|_| !go_prog);
        if let Some(a) = waits {
            lock_kind.insert((t.pid, a), "futex");
        } else if go_prog && th.waits_on.take().is_some() {
            th.notes
                .push("the Go runtime's own wait of an idle thread: see the goroutines".into());
        }
        // the stack's words up to the outermost frame found (a stack above it can be another
        // thread's)
        let outer = uf.iter().map(|f| f.sp).max().unwrap_or(sp);
        actors.push(Actor {
            who: format!("tid {} ({})", t.tid, t.comm),
            pid: t.pid,
            waits,
            sp,
            top: if uf.len() > 1 {
                outer + 4096
            } else {
                sp + 65536
            },
            frames: uf
                .iter()
                .map(|f| (f.sp, describe(prog, bias, f.pc)))
                .collect(),
            funcs: funcs_of(prog, bias, &uf),
            root: pgd,
        });
        // the goroutines of a Go program, once per process
        if let Some(p) = prog.filter(|p| p.go.is_some()) {
            if !go_done.contains(&t.pid) {
                go_done.push(t.pid);
                match golang::Layout::read(p) {
                    None => out.notes.push(format!(
                        "pid {}: {} has no debug information, so its goroutines are not read",
                        t.pid,
                        path.clone().unwrap_or_default()
                    )),
                    Some(gl) => {
                        for g in golang::goroutines(&space, p, &gl, bias) {
                            let mut sg = StackGoroutine {
                                pid: t.pid,
                                goid: g.goid,
                                status: golang::status_name(g.status).into(),
                                wait: g.wait.clone(),
                                ..StackGoroutine::default()
                            };
                            let lock = match (&g.wait, g.sema) {
                                (Some(w), Some(e)) if golang::is_lock_wait(w) => {
                                    Some(e - sema_off(w))
                                }
                                _ => None,
                            };
                            if let Some(a) = lock {
                                sg.waits_on = Some(hex(a));
                                lock_kind.insert((t.pid, a), "go-mutex");
                            }
                            let gf = if g.status & !0x1000 == 2 {
                                Vec::new() // running: its thread's stack shows it
                            } else {
                                user::unwind(
                                    &space,
                                    Some(p),
                                    bias,
                                    arch,
                                    user::URegs {
                                        pc: g.pc,
                                        sp: g.sp,
                                        fp: g.bp,
                                        lr: g.lr,
                                    },
                                    inp.max_frames,
                                )
                            };
                            sg.frames = gf
                                .iter()
                                .map(|f| frame(Some(p), bias, f.pc, f.how, true))
                                .collect();
                            actors.push(Actor {
                                who: format!("goroutine {} (pid {})", g.goid, t.pid),
                                pid: t.pid,
                                waits: lock,
                                sp: g.sp,
                                top: g.stack_hi.max(g.sp),
                                frames: gf
                                    .iter()
                                    .map(|f| (f.sp, describe(Some(p), bias, f.pc)))
                                    .collect(),
                                funcs: funcs_of(Some(p), bias, &gf),
                                root: pgd,
                            });
                            out.goroutines.push(sg);
                        }
                    }
                }
            }
        }
        out.threads.push(th);
    }
    // the locks: their names, waiters and likely holders
    for (&(pid, addr), &kind) in &lock_kind {
        let prog = out
            .threads
            .iter()
            .find(|t| t.pid == pid)
            .and_then(|t| t.program.as_ref())
            .and_then(|p| programs.get(p))
            .and_then(Option::as_ref);
        let bias = bias_of_pid.get(&pid).copied().unwrap_or(0);
        let name = prog
            .and_then(|p| p.object(addr.wrapping_sub(bias)))
            .map(|(n, o)| if o == 0 { n } else { format!("{n}+{o:#x}") });
        let waiters: Vec<String> = actors
            .iter()
            .filter(|a| a.pid == pid && a.waits == Some(addr))
            .map(|a| a.who.clone())
            .collect();
        // the global variable the lock is in (link addresses)
        let obj = prog.and_then(|p| p.object_range(addr.wrapping_sub(bias)));
        let mut holders = Vec::new();
        for a in actors
            .iter()
            .filter(|a| a.pid == pid && a.waits != Some(addr))
        {
            // a frame whose function refers to the lock's variable: it likely took the lock and
            // has not returned (the innermost such frame)
            if let (Some(p), Some((lo, hi))) = (prog, obj) {
                if let Some((_, _, f)) = a
                    .funcs
                    .iter()
                    .find(|(fl, fh, _)| p.refers(*fl, *fh, lo, hi))
                {
                    holders.push(StackHolder {
                        who: a.who.clone(),
                        frame: Some(f.clone()),
                        why: format!(
                            "its frame {f} refers to {} and has not returned",
                            name.clone().unwrap_or_else(|| hex(addr))
                        ),
                    });
                    continue;
                }
            }
            let space = Paged {
                m: &d,
                arch,
                root: a.root,
            };
            let base = obj.map(|(lo, _)| lo + bias).filter(|&b| b != addr);
            let slot = find_word(&space, a.sp, a.top, addr)
                .or_else(|| base.and_then(|b| find_word(&space, a.sp, a.top, b)));
            if let Some(slot) = slot {
                // the frame whose part of the stack holds the slot
                let fr = a
                    .frames
                    .iter()
                    .rev()
                    .find(|(sp, _)| *sp <= slot)
                    .map(|(_, f)| f.clone());
                holders.push(StackHolder {
                    who: a.who.clone(),
                    frame: fr,
                    why: format!("its stack keeps the lock's address (at {slot:#x})"),
                });
            }
        }
        out.locks.push(StackLock {
            addr: hex(addr),
            pid,
            kind: kind.into(),
            name,
            waiters,
            holders,
        });
    }
    out.cycles = cycles(&out.locks);
    Ok(out)
}

/// How far a wait's semaphore is from the lock's start (`sudog.elem` is the semaphore).
fn sema_off(reason: &str) -> u64 {
    match reason {
        "sync.RWMutex.Lock" => 8,
        "sync.RWMutex.RLock" => 12,
        _ => 4,
    }
}

/// A program counter as a frame.
fn frame(prog: Option<&Program>, bias: u64, pc: u64, how: &str, _go: bool) -> StackFrame {
    let p = prog
        .map(|p| p.place(pc.wrapping_sub(bias).wrapping_sub(u64::from(how != "regs"))))
        .unwrap_or_default();
    StackFrame {
        pc: hex(pc),
        function: p.function,
        file: p.file,
        line: p.line,
        how: how.into(),
    }
}

/// `function (file:line)` of a program counter.
fn describe(prog: Option<&Program>, bias: u64, pc: u64) -> String {
    let f = frame(prog, bias, pc, "", false);
    let name = f.function.unwrap_or_else(|| hex(pc));
    match (f.file, f.line) {
        (Some(file), Some(line)) => {
            let short = file.rsplit('/').next().unwrap_or(&file).to_string();
            format!("{name} ({short}:{line})")
        }
        _ => name,
    }
}

/// The load bias of a position-independent program: `mm->start_code`, where the kernel put its
/// first executable segment, less that segment's link address; 0 for a program at a fixed
/// address.
fn bias_of(k: &dyn Space, btf: &btf::Btf, mm: u64, p: &Program) -> u64 {
    if !p.pie {
        return 0;
    }
    let Some(o) = btf.off("mm_struct", "start_code") else {
        return 0;
    };
    match k.u64(mm + o) {
        Some(start) => start.wrapping_sub(p.first_vaddr),
        None => 0,
    }
}

/// The value of `HYDO_SVC` in a process's environment (`mm->env_start` to `env_end`, read
/// through its page tables): the service hydo started it for.
fn service_of(k: &dyn Space, btf: &btf::Btf, s: &dyn Space, mm: u64) -> Option<String> {
    let a = k.u64(mm + btf.off("mm_struct", "env_start")?)?;
    let b = k.u64(mm + btf.off("mm_struct", "env_end")?)?;
    if b <= a || b - a > 256 * 1024 {
        return None;
    }
    let env = s.bytes(a, (b - a) as usize)?;
    env.split(|&c| c == 0)
        .find_map(|v| v.strip_prefix(b"HYDO_SVC="))
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

/// The first slot in `[from, to)` that holds `word`.
fn find_word(s: &dyn Space, from: u64, to: u64, word: u64) -> Option<u64> {
    let mut a = from & !7;
    while a < to {
        let page_end = (a | 0xfff) + 1;
        let n = ((page_end.min(to) - a) / 8) as usize;
        let b = s.bytes(a, n * 8)?;
        for (i, c) in b.chunks_exact(8).enumerate() {
            if u64::from_le_bytes(c.try_into().ok()?) == word {
                return Some(a + i as u64 * 8);
            }
        }
        a = page_end;
    }
    None
}

/// The cycles of waits: a waiter's lock is held by another waiter, and so on, back to it.
pub fn cycles(locks: &[StackLock]) -> Vec<StackCycle> {
    // who -> (its lock, that lock's holders)
    let mut edges: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for l in locks {
        let label = match &l.name {
            Some(n) => format!("{n} ({})", l.addr),
            None => l.addr.clone(),
        };
        for w in &l.waiters {
            edges.insert(
                w.clone(),
                (
                    label.clone(),
                    l.holders.iter().map(|h| h.who.clone()).collect(),
                ),
            );
        }
    }
    let mut out: Vec<StackCycle> = Vec::new();
    let mut seen_sets: Vec<Vec<String>> = Vec::new();
    for start in edges.keys() {
        let mut path = vec![start.clone()];
        let mut cur = start.clone();
        for _ in 0..16 {
            let Some((_, hs)) = edges.get(&cur) else {
                break;
            };
            let Some(next) = hs.iter().find(|h| edges.contains_key(*h)) else {
                break;
            };
            if next == start {
                let mut set = path.clone();
                set.sort();
                if !seen_sets.contains(&set) {
                    seen_sets.push(set);
                    let n = path.len();
                    let waits = (0..n)
                        .map(|i| {
                            format!(
                                "{} waits on {}, held by {}",
                                path[i],
                                edges[&path[i]].0,
                                path[(i + 1) % n]
                            )
                        })
                        .collect();
                    out.push(StackCycle { waits });
                }
                break;
            }
            if path.contains(next) {
                break;
            }
            path.push(next.clone());
            cur = next.clone();
        }
    }
    out
}

/// The stacks as lines for people (`--text`).
pub fn text(s: &InspectStacks) -> String {
    let mut o = String::new();
    let fr = |f: &StackFrame| {
        let name = f.function.clone().unwrap_or_else(|| "?".into());
        match (&f.file, f.line) {
            (Some(file), Some(line)) => format!("      {name}  {file}:{line}  [{}]\n", f.how),
            _ => format!("      {name}  {}  [{}]\n", f.pc, f.how),
        }
    };
    o.push_str(&format!(
        "{} threads, {} goroutines, {} locks waited on ({} {} guest{})\n",
        s.threads.len(),
        s.goroutines.len(),
        s.locks.len(),
        s.engine,
        s.arch,
        s.kernel
            .as_ref()
            .map(|k| format!(", Linux {k}"))
            .unwrap_or_default()
    ));
    for c in &s.cycles {
        o.push_str("\nA cycle of waits (a deadlock):\n");
        for w in &c.waits {
            o.push_str(&format!("  {w}\n"));
        }
    }
    for l in &s.locks {
        o.push_str(&format!(
            "\nlock {} ({}, pid {}): waited on by {}\n",
            l.name.clone().unwrap_or_else(|| l.addr.clone()),
            l.kind,
            l.pid,
            l.waiters.join(", ")
        ));
        for h in &l.holders {
            o.push_str(&format!(
                "  likely held by {}{}: {}\n",
                h.who,
                h.frame
                    .as_ref()
                    .map(|f| format!(" in {f}"))
                    .unwrap_or_default(),
                h.why
            ));
        }
    }
    for t in &s.threads {
        o.push_str(&format!(
            "\npid {} tid {} ({}){} {}{}{}{}\n",
            t.pid,
            t.tid,
            t.comm,
            t.service
                .as_ref()
                .map(|s| format!(" of {s}"))
                .unwrap_or_default(),
            t.state,
            t.vcpu.map(|v| format!(" on vCPU {v}")).unwrap_or_default(),
            t.syscall
                .as_ref()
                .map(|c| format!(
                    " in {}({})",
                    if c.name.is_empty() {
                        c.nr.to_string()
                    } else {
                        c.name.clone()
                    },
                    c.args
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .unwrap_or_default(),
            t.waits_on
                .as_ref()
                .map(|w| format!(", waits on {w}"))
                .unwrap_or_default()
        ));
        if let Some(p) = &t.program {
            o.push_str(&format!("    program {p}\n"));
        }
        if !t.kernel.is_empty() {
            o.push_str("    kernel:\n");
            for f in &t.kernel {
                o.push_str(&fr(f));
            }
        }
        if !t.user.is_empty() {
            o.push_str("    user:\n");
            for f in &t.user {
                o.push_str(&fr(f));
            }
        }
        for n in &t.notes {
            o.push_str(&format!("    ({n})\n"));
        }
    }
    for g in &s.goroutines {
        o.push_str(&format!(
            "\ngoroutine {} (pid {}) {}{}{}\n",
            g.goid,
            g.pid,
            g.status,
            g.wait
                .as_ref()
                .map(|w| format!(" [{w}]"))
                .unwrap_or_default(),
            g.waits_on
                .as_ref()
                .map(|w| format!(", waits on {w}"))
                .unwrap_or_default()
        ));
        for f in &g.frames {
            o.push_str(&fr(f));
        }
    }
    for n in &s.notes {
        o.push_str(&format!("\n({n})\n"));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(addr: &str, name: &str, w: &str, h: &str) -> StackLock {
        StackLock {
            addr: addr.into(),
            pid: 1,
            kind: "futex".into(),
            name: Some(name.into()),
            waiters: vec![w.into()],
            holders: vec![StackHolder {
                who: h.into(),
                frame: None,
                why: String::new(),
            }],
        }
    }

    #[test]
    fn a_cycle_of_two_waits_is_found_once() {
        let ls = vec![
            lock("0x10", "A", "tid 2 (watchdog)", "tid 1 (worker)"),
            lock("0x20", "B", "tid 1 (worker)", "tid 2 (watchdog)"),
        ];
        let c = cycles(&ls);
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].waits,
            vec![
                "tid 1 (worker) waits on B (0x20), held by tid 2 (watchdog)",
                "tid 2 (watchdog) waits on A (0x10), held by tid 1 (worker)"
            ]
        );
        // a wait without a cycle
        assert!(cycles(&ls[..1]).is_empty());
    }
}
