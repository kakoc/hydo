//! hydo-clock: one service's own wall clock in the guest (Milestone 372,
//! `docs/design/service-clocks.md`).
//!
//! `hydo-clock <offset file> <program> [args...]` runs the program. The program and every
//! process it starts read the guest's realtime clock plus an offset. The other processes of the
//! guest keep the guest's clock. The host controller writes the offset file before a workload
//! step: one line `<offset ns> <drift ppm>`. hydo-clock reads it at every read of the wall clock,
//! so a step takes effect at the next read. A missing file means offset 0 and no drift.
//!
//! How it works, in four parts:
//!
//! 1. A seccomp filter, installed before the program starts and inherited by every process it
//!    starts, sends the system calls that read the wall clock to hydo-clock: `clock_gettime` of
//!    `CLOCK_REALTIME`, `CLOCK_REALTIME_COARSE` and `CLOCK_TAI`, `gettimeofday` and `time`. It
//!    also sends the calls that wait until a wall-clock time: `clock_nanosleep` with
//!    `TIMER_ABSTIME` on those clocks, and the futex operations with an absolute realtime
//!    timeout (`pthread_cond_timedwait` by default). Every other call, the monotonic clock's
//!    reads among them, runs as before.
//! 2. hydo-clock is the tracer (ptrace) of every process of the tree. At a read it writes the
//!    guest's time plus the offset into the process's memory and skips the call. At a wait it
//!    gives the kernel the deadline minus the offset, in a copy below the stack pointer.
//! 3. Programs read the clock without a system call, through the kernel's vDSO. At every
//!    `execve` hydo-clock hides the vDSO from the new program: it turns the `AT_SYSINFO_EHDR`
//!    entry of the auxiliary vector into `AT_IGNORE`. glibc, musl and Go then make the system
//!    call. A program that reads the TSC itself, or finds the vDSO in /proc/self/maps, is not
//!    covered.
//! 4. Signals pass through. hydo-clock ignores SIGTERM, SIGINT, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2
//!    and SIGPIPE itself, because hydo signals the service's whole process tree. A stop of the
//!    tree (`P=`) stops hydo-clock too; a traced process then waits in its stop until hydo-clock
//!    runs again, and a stop that a SIGCONT already ended is not delivered again. When the
//!    program ends, hydo-clock ends with its status, and every process left in the tree is
//!    killed (`PTRACE_O_EXITKILL`).
//!
//! x86_64 only: the registers and the system call numbers are x86_64's. The guest kernel needs
//! `CONFIG_SECCOMP_FILTER` (hvx's kernel has it; on the Mac and KVM engines the `containers`
//! kernels have it).

use std::collections::BTreeSet;
use std::ffi::CString;

type Pid = libc::pid_t;

// ptrace requests and options (linux/ptrace.h)
const PTRACE_CONT: libc::c_long = 7;
const PTRACE_GETREGS: libc::c_long = 12;
const PTRACE_SETREGS: libc::c_long = 13;
const PTRACE_SEIZE: libc::c_long = 0x4206;
const PTRACE_LISTEN: libc::c_long = 0x4208;
const OPT_TRACEFORK: libc::c_long = 0x02;
const OPT_TRACEVFORK: libc::c_long = 0x04;
const OPT_TRACECLONE: libc::c_long = 0x08;
const OPT_TRACEEXEC: libc::c_long = 0x10;
const OPT_TRACESECCOMP: libc::c_long = 0x80;
const OPT_EXITKILL: libc::c_long = 0x10_0000;
const EVENT_FORK: i32 = 1;
const EVENT_VFORK: i32 = 2;
const EVENT_CLONE: i32 = 3;
const EVENT_EXEC: i32 = 4;
const EVENT_SECCOMP: i32 = 7;
const EVENT_STOP: i32 = 128;
const WALL: libc::c_int = 0x4000_0000;

// x86_64 system calls
const NR_GETTIMEOFDAY: u64 = 96;
const NR_TIME: u64 = 201;
const NR_FUTEX: u64 = 202;
const NR_CLOCK_GETTIME: u64 = 228;
const NR_CLOCK_NANOSLEEP: u64 = 230;
const CLOCK_REALTIME: i32 = 0;
const CLOCK_REALTIME_COARSE: i32 = 5;
const CLOCK_TAI: i32 = 11;
const TIMER_ABSTIME: u64 = 1;
const FUTEX_CMD_MASK: u32 = 127;
const FUTEX_CLOCK_REALTIME: u32 = 256;
const FUTEX_LOCK_PI: u32 = 6;
const FUTEX_WAIT_BITSET: u32 = 9;
const FUTEX_WAIT_REQUEUE_PI: u32 = 11;
const FUTEX_LOCK_PI2: u32 = 13;
const AT_NULL: u64 = 0;
const AT_IGNORE: u64 = 1;
const AT_SYSINFO_EHDR: u64 = 33;

// seccomp and classic BPF
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_TRACE: u32 = 0x7ff0_0000;
const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;
const PR_SET_SECCOMP: libc::c_int = 22;
const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JSET_K: u16 = 0x45;
const BPF_AND_K: u16 = 0x54;
const BPF_RET_K: u16 = 0x06;
const SD_NR: u32 = 0;
const SD_ARCH: u32 = 4;
const SD_ARG0: u32 = 16;
const SD_ARG1: u32 = 24;

/// struct user_regs_struct of x86_64 (sys/user.h), in its order.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Regs {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbp: u64,
    rbx: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rax: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    orig_rax: u64,
    rip: u64,
    cs: u64,
    eflags: u64,
    rsp: u64,
    ss: u64,
    fs_base: u64,
    gs_base: u64,
    ds: u64,
    es: u64,
    fs: u64,
    gs: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn ptrace(req: libc::c_long, pid: Pid, addr: u64, data: u64) -> libc::c_long {
    // SAFETY: the raw system call; the requests used here take integers or a pointer to Regs.
    unsafe { libc::syscall(libc::SYS_ptrace, req, pid as libc::c_long, addr, data) }
}

fn cont(pid: Pid, sig: i32) {
    ptrace(PTRACE_CONT, pid, 0, sig as u64);
}

fn get_regs(pid: Pid) -> Option<Regs> {
    let mut r = Regs::default();
    (ptrace(PTRACE_GETREGS, pid, 0, &mut r as *mut Regs as u64) == 0).then_some(r)
}

fn set_regs(pid: Pid, r: &Regs) -> bool {
    ptrace(PTRACE_SETREGS, pid, 0, r as *const Regs as u64) == 0
}

/// Reads `buf.len()` bytes of the traced process at `addr`.
fn peek(pid: Pid, addr: u64, buf: &mut [u8]) -> bool {
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let remote = libc::iovec {
        iov_base: addr as *mut libc::c_void,
        iov_len: buf.len(),
    };
    // SAFETY: both vectors describe memory of the right length; the kernel checks the remote one.
    let n = unsafe {
        libc::syscall(
            libc::SYS_process_vm_readv,
            pid as libc::c_long,
            &local as *const libc::iovec,
            1u64,
            &remote as *const libc::iovec,
            1u64,
            0u64,
        )
    };
    n == buf.len() as libc::c_long || words::peek(pid, addr, buf, n)
}

/// Writes `buf` into the traced process at `addr`.
fn poke(pid: Pid, addr: u64, buf: &[u8]) -> bool {
    let local = libc::iovec {
        iov_base: buf.as_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let remote = libc::iovec {
        iov_base: addr as *mut libc::c_void,
        iov_len: buf.len(),
    };
    // SAFETY: as in peek; the local buffer is only read.
    let n = unsafe {
        libc::syscall(
            libc::SYS_process_vm_writev,
            pid as libc::c_long,
            &local as *const libc::iovec,
            1u64,
            &remote as *const libc::iovec,
            1u64,
            0u64,
        )
    };
    n == buf.len() as libc::c_long || words::poke(pid, addr, buf, n)
}

fn peek_u64(pid: Pid, addr: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    peek(pid, addr, &mut b).then(|| u64::from_ne_bytes(b))
}

/// A clock of this process (hydo-clock itself is not traced), in nanoseconds.
fn clock_ns(clk: i32) -> i128 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid timespec.
    unsafe { libc::clock_gettime(clk as libc::clockid_t, &mut ts) };
    ts.tv_sec as i128 * 1_000_000_000 + ts.tv_nsec as i128
}

fn mono_ns() -> i128 {
    clock_ns(libc::CLOCK_MONOTONIC)
}

/// A time in nanoseconds as the 16 bytes of a struct timespec.
fn timespec_bytes(ns: i128) -> [u8; 16] {
    let sec = ns.div_euclid(1_000_000_000) as i64;
    let nsec = ns.rem_euclid(1_000_000_000) as i64;
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&sec.to_ne_bytes());
    b[8..].copy_from_slice(&nsec.to_ne_bytes());
    b
}

/// The offset file's state: the step and the drift last read, and the drift built up so far.
struct Offset {
    path: CString,
    last: Vec<u8>,
    step_ns: i128,
    ppm: i128,
    acc_ns: i128,
    base_ns: i128,
}

impl Offset {
    fn new(path: CString) -> Offset {
        Offset {
            path,
            last: Vec::new(),
            step_ns: 0,
            ppm: 0,
            acc_ns: 0,
            base_ns: mono_ns(),
        }
    }

    /// The file's text, when it can be read.
    fn read(&self) -> Option<Vec<u8>> {
        // SAFETY: a plain open, read and close of a file by its path.
        unsafe {
            let fd = libc::open(self.path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
            if fd < 0 {
                return None;
            }
            let mut buf = [0u8; 128];
            let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
            libc::close(fd);
            (n > 0).then(|| buf[..n as usize].to_vec())
        }
    }

    /// The offset now, in nanoseconds. A new line in the file takes effect here, at the first read
    /// of the clock after the controller wrote it: the drift so far is kept, and the new drift
    /// counts from now. A line without its newline (a write in
    /// progress) or one that does not parse is ignored until it is whole.
    fn now(&mut self) -> i128 {
        let now = mono_ns();
        if let Some(text) = self.read() {
            if text != self.last && text.ends_with(b"\n") {
                let s = String::from_utf8_lossy(&text);
                let mut w = s.split_whitespace();
                if let (Some(Ok(step)), Some(Ok(ppm)), None) = (
                    w.next().map(str::parse::<i128>),
                    w.next().map(str::parse::<i128>),
                    w.next(),
                ) {
                    self.acc_ns += self.ppm * (now - self.base_ns) / 1_000_000;
                    self.base_ns = now;
                    self.step_ns = step;
                    self.ppm = ppm;
                    self.last = text;
                }
            }
        }
        self.step_ns + self.acc_ns + self.ppm * (now - self.base_ns) / 1_000_000
    }
}

/// The filter of the clock's system calls: SECCOMP_RET_TRACE for the reads and waits named in
/// the module's comment, SECCOMP_RET_ALLOW for every other call.
fn filter() -> Vec<SockFilter> {
    // targets: an index of the program, or the two returns at its end
    #[derive(Clone, Copy)]
    enum T {
        Next,
        At(&'static str),
    }
    let mut code: Vec<(u16, T, T, u32)> = Vec::new();
    let mut labels: Vec<(&'static str, usize)> = Vec::new();
    let ld =
        |c: &mut Vec<(u16, T, T, u32)>, off: u32| c.push((BPF_LD_W_ABS, T::Next, T::Next, off));
    ld(&mut code, SD_ARCH);
    code.push((BPF_JEQ_K, T::Next, T::At("allow"), AUDIT_ARCH_X86_64));
    ld(&mut code, SD_NR);
    code.push((
        BPF_JEQ_K,
        T::At("gettime"),
        T::Next,
        NR_CLOCK_GETTIME as u32,
    ));
    code.push((BPF_JEQ_K, T::At("trace"), T::Next, NR_GETTIMEOFDAY as u32));
    code.push((BPF_JEQ_K, T::At("trace"), T::Next, NR_TIME as u32));
    code.push((
        BPF_JEQ_K,
        T::At("sleep"),
        T::Next,
        NR_CLOCK_NANOSLEEP as u32,
    ));
    code.push((BPF_JEQ_K, T::At("futex"), T::At("allow"), NR_FUTEX as u32));
    labels.push(("gettime", code.len()));
    ld(&mut code, SD_ARG0);
    code.push((BPF_JEQ_K, T::At("trace"), T::Next, CLOCK_REALTIME as u32));
    code.push((
        BPF_JEQ_K,
        T::At("trace"),
        T::Next,
        CLOCK_REALTIME_COARSE as u32,
    ));
    code.push((BPF_JEQ_K, T::At("trace"), T::At("allow"), CLOCK_TAI as u32));
    labels.push(("sleep", code.len()));
    ld(&mut code, SD_ARG1);
    code.push((BPF_JSET_K, T::Next, T::At("allow"), TIMER_ABSTIME as u32));
    ld(&mut code, SD_ARG0);
    code.push((BPF_JEQ_K, T::At("trace"), T::Next, CLOCK_REALTIME as u32));
    code.push((BPF_JEQ_K, T::At("trace"), T::At("allow"), CLOCK_TAI as u32));
    labels.push(("futex", code.len()));
    ld(&mut code, SD_ARG1);
    code.push((BPF_JSET_K, T::At("trace"), T::Next, FUTEX_CLOCK_REALTIME));
    code.push((BPF_AND_K, T::Next, T::Next, FUTEX_CMD_MASK));
    code.push((BPF_JEQ_K, T::At("trace"), T::At("allow"), FUTEX_LOCK_PI));
    labels.push(("allow", code.len()));
    code.push((BPF_RET_K, T::Next, T::Next, SECCOMP_RET_ALLOW));
    labels.push(("trace", code.len()));
    code.push((BPF_RET_K, T::Next, T::Next, SECCOMP_RET_TRACE));
    let at = |name: &str| labels.iter().find(|(n, _)| *n == name).map(|(_, i)| *i);
    code.iter()
        .enumerate()
        .map(|(i, &(c, jt, jf, k))| {
            let off = |t: T| match t {
                T::Next => 0u8,
                T::At(n) => (at(n).expect("a label of the filter") - i - 1) as u8,
            };
            SockFilter {
                code: c,
                jt: off(jt),
                jf: off(jf),
                k,
            }
        })
        .collect()
}

/// Installs the filter in this process (the child, before it runs the program).
fn install_filter() -> Result<(), i32> {
    let f = filter();
    let prog = SockFprog {
        len: f.len() as u16,
        filter: f.as_ptr(),
    };
    let set = || {
        // SAFETY: prog points at the filter, which lives until the call returns.
        unsafe {
            libc::prctl(
                PR_SET_SECCOMP,
                SECCOMP_MODE_FILTER,
                &prog as *const SockFprog as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        }
    };
    if set() == 0 {
        return Ok(());
    }
    // without CAP_SYS_ADMIN the kernel wants no_new_privs first
    if errno() == libc::EACCES {
        // SAFETY: a plain prctl.
        unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0) };
        if set() == 0 {
            return Ok(());
        }
    }
    Err(errno())
}

/// After an execve: hides the vDSO from the new program, so that its clock reads are system
/// calls. The auxiliary vector follows argc, argv and envp on the new stack.
fn hide_vdso(pid: Pid) {
    let Some(r) = get_regs(pid) else { return };
    if r.cs != 0x33 {
        // a 32-bit program: another layout, and its calls are not filtered
        return;
    }
    let Some(argc) = peek_u64(pid, r.rsp) else {
        return;
    };
    let mut p = r.rsp + 8 + 8 * (argc + 1);
    for _ in 0..65_536 {
        match peek_u64(pid, p) {
            Some(0) => break,
            Some(_) => p += 8,
            None => return,
        }
    }
    p += 8;
    for _ in 0..256 {
        match peek_u64(pid, p) {
            Some(AT_NULL) | None => return,
            Some(AT_SYSINFO_EHDR) => {
                poke(pid, p, &AT_IGNORE.to_ne_bytes());
                return;
            }
            Some(_) => p += 16,
        }
    }
}

/// The skipped call returns `val`.
fn skip(pid: Pid, r: &mut Regs, val: u64) {
    r.orig_rax = u64::MAX;
    r.rax = val;
    set_regs(pid, r);
}

/// An absolute wall-clock deadline (a struct timespec at `ptr`) moved from the service's clock
/// to the guest's: the kernel gets a copy below the stack pointer, so the program's own struct
/// stays as it wrote it. Returns the address to give the kernel (None: leave the call alone).
fn shifted(pid: Pid, r: &Regs, ptr: u64, off: i128) -> Option<u64> {
    let mut b = [0u8; 16];
    if !peek(pid, ptr, &mut b) {
        return None;
    }
    let sec = i64::from_ne_bytes(b[..8].try_into().ok()?);
    let nsec = i64::from_ne_bytes(b[8..].try_into().ok()?);
    if !(0..1_000_000_000).contains(&nsec) {
        return None; // the kernel refuses it as it is
    }
    let ns = (sec as i128 * 1_000_000_000 + nsec as i128 - off).max(0);
    let bytes = timespec_bytes(ns);
    let scratch = (r.rsp - 512) & !15;
    if poke(pid, scratch, &bytes) {
        return Some(scratch);
    }
    // no stack below: the program's own struct, in place
    poke(pid, ptr, &bytes).then_some(ptr)
}

/// One filtered system call, at its seccomp stop.
fn on_call(pid: Pid, off: &mut Offset) {
    let Some(mut r) = get_regs(pid) else { return };
    match r.orig_rax {
        NR_CLOCK_GETTIME => {
            if r.rsi == 0 {
                return; // the kernel answers EFAULT
            }
            let t = clock_ns(r.rdi as i32) + off.now();
            if poke(pid, r.rsi, &timespec_bytes(t)) {
                skip(pid, &mut r, 0);
            }
        }
        NR_GETTIMEOFDAY => {
            let t = clock_ns(CLOCK_REALTIME) + off.now();
            let mut ok = true;
            if r.rdi != 0 {
                let mut tv = [0u8; 16];
                tv[..8].copy_from_slice(&(t.div_euclid(1_000_000_000) as i64).to_ne_bytes());
                tv[8..]
                    .copy_from_slice(&((t.rem_euclid(1_000_000_000) / 1000) as i64).to_ne_bytes());
                ok &= poke(pid, r.rdi, &tv);
            }
            if r.rsi != 0 {
                ok &= poke(pid, r.rsi, &[0u8; 8]); // struct timezone: the kernel's is 0, 0
            }
            if ok {
                skip(pid, &mut r, 0);
            }
        }
        NR_TIME => {
            let sec = (clock_ns(CLOCK_REALTIME) + off.now()).div_euclid(1_000_000_000) as i64;
            if r.rdi != 0 && !poke(pid, r.rdi, &sec.to_ne_bytes()) {
                return;
            }
            skip(pid, &mut r, sec as u64);
        }
        NR_CLOCK_NANOSLEEP => {
            if r.rdx != 0 {
                let o = off.now();
                if let Some(a) = shifted(pid, &r, r.rdx, o) {
                    r.rdx = a;
                    set_regs(pid, &r);
                }
            }
        }
        NR_FUTEX => {
            let op = r.rsi as u32;
            let cmd = op & FUTEX_CMD_MASK;
            let rt = op & FUTEX_CLOCK_REALTIME != 0;
            let absolute = cmd == FUTEX_LOCK_PI
                || (rt
                    && matches!(
                        cmd,
                        FUTEX_WAIT_BITSET | FUTEX_WAIT_REQUEUE_PI | FUTEX_LOCK_PI2
                    ));
            if absolute && r.r10 != 0 {
                let o = off.now();
                if let Some(a) = shifted(pid, &r, r.r10, o) {
                    r.r10 = a;
                    set_regs(pid, &r);
                }
            }
        }
        _ => {}
    }
}

fn is_stop_signal(sig: i32) -> bool {
    matches!(
        sig,
        libc::SIGSTOP | libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU
    )
}

/// Whether a SIGCONT waits for the process: then a stop signal it is about to take came before
/// that SIGCONT, and stopping now would leave it stopped for good.
fn cont_pending(pid: Pid) -> bool {
    let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
        return false;
    };
    let bit = 1u64 << (libc::SIGCONT - 1);
    s.lines().any(|l| {
        (l.starts_with("SigPnd:") || l.starts_with("ShdPnd:"))
            && l.split_whitespace()
                .nth(1)
                .and_then(|h| u64::from_str_radix(h, 16).ok())
                .is_some_and(|m| m & bit != 0)
    })
}

/// Ends hydo-clock as the program ended.
fn finish(status: libc::c_int) -> ! {
    if libc::WIFEXITED(status) {
        std::process::exit(libc::WEXITSTATUS(status));
    }
    let sig = libc::WTERMSIG(status);
    // SAFETY: the default action of the program's signal, then the signal itself.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::kill(libc::getpid(), sig);
    }
    std::process::exit(128 + sig);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: hydo-clock <offset file> <program> [args...]");
        std::process::exit(2);
    }
    let cs = |s: &String| CString::new(s.as_bytes()).unwrap_or_default();
    let path = cs(&args[1]);
    let argv: Vec<CString> = args[2..].iter().map(cs).collect();
    let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: a pipe and a fork; the child only reads, installs the filter and execs.
    let child = unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            eprintln!("hydo-clock: pipe: {}", std::io::Error::last_os_error());
            std::process::exit(126);
        }
        libc::fork()
    };
    if child < 0 {
        eprintln!("hydo-clock: fork: {}", std::io::Error::last_os_error());
        std::process::exit(126);
    }
    if child == 0 {
        // SAFETY: the child waits for the tracer, then becomes the program.
        unsafe {
            libc::close(fds[1]);
            let mut b = [0u8; 1];
            libc::read(fds[0], b.as_mut_ptr().cast(), 1);
            libc::close(fds[0]);
            if let Err(e) = install_filter() {
                eprintln!(
                    "hydo-clock: cannot install the clock filter: {}",
                    std::io::Error::from_raw_os_error(e)
                );
                libc::_exit(126);
            }
            libc::execvp(ptrs[0], ptrs.as_ptr());
            eprintln!(
                "hydo-clock: cannot run {}: {}",
                args[2],
                std::io::Error::last_os_error()
            );
            libc::_exit(127);
        }
    }
    let opts = OPT_TRACEFORK
        | OPT_TRACEVFORK
        | OPT_TRACECLONE
        | OPT_TRACEEXEC
        | OPT_TRACESECCOMP
        | OPT_EXITKILL;
    if ptrace(PTRACE_SEIZE, child, 0, opts as u64) != 0 {
        eprintln!(
            "hydo-clock: cannot trace the program: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the child is ours.
        unsafe { libc::kill(child, libc::SIGKILL) };
        std::process::exit(126);
    }
    // SAFETY: release the child, and ignore the signals hydo sends the whole tree.
    unsafe {
        libc::close(fds[0]);
        libc::write(fds[1], b"x".as_ptr().cast(), 1);
        libc::close(fds[1]);
        for s in [
            libc::SIGTERM,
            libc::SIGINT,
            libc::SIGHUP,
            libc::SIGQUIT,
            libc::SIGUSR1,
            libc::SIGUSR2,
            libc::SIGPIPE,
        ] {
            libc::signal(s, libc::SIG_IGN);
        }
    }
    let mut off = Offset::new(path);
    let mut known: BTreeSet<Pid> = BTreeSet::new();
    known.insert(child);
    loop {
        let mut st: libc::c_int = 0;
        // SAFETY: waits for any traced task.
        let w = unsafe { libc::waitpid(-1, &mut st, WALL) };
        if w < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            std::process::exit(0);
        }
        if libc::WIFEXITED(st) || libc::WIFSIGNALED(st) {
            known.remove(&w);
            if w == child {
                finish(st);
            }
            continue;
        }
        if !libc::WIFSTOPPED(st) {
            continue;
        }
        let sig = libc::WSTOPSIG(st);
        let ev = (st >> 16) & 0xffff;
        if known.insert(w) && ev == EVENT_STOP {
            // a new task's first stop
            cont(w, 0);
            continue;
        }
        match ev {
            EVENT_EXEC => {
                hide_vdso(w);
                cont(w, 0);
            }
            EVENT_SECCOMP => {
                on_call(w, &mut off);
                cont(w, 0);
            }
            EVENT_FORK | EVENT_VFORK | EVENT_CLONE => cont(w, 0),
            EVENT_STOP => {
                if is_stop_signal(sig) {
                    // a group stop: the task stays stopped until a SIGCONT
                    ptrace(PTRACE_LISTEN, w, 0, 0);
                } else {
                    cont(w, 0);
                }
            }
            0 => {
                let pass = if is_stop_signal(sig) && cont_pending(w) {
                    0
                } else {
                    sig
                };
                cont(w, pass);
            }
            _ => cont(w, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_jumps_forward_inside_itself() {
        let f = filter();
        assert!(f.len() < 64);
        for (i, x) in f.iter().enumerate() {
            if x.code == BPF_JEQ_K || x.code == BPF_JSET_K {
                assert!(i + 1 + (x.jt as usize) < f.len());
                assert!(i + 1 + (x.jf as usize) < f.len());
            }
        }
        assert_eq!(f[f.len() - 1].k, SECCOMP_RET_TRACE);
        assert_eq!(f[f.len() - 2].k, SECCOMP_RET_ALLOW);
    }

    #[test]
    fn a_time_as_a_timespec() {
        let b = timespec_bytes(-1);
        assert_eq!(i64::from_ne_bytes(b[..8].try_into().unwrap()), -1);
        assert_eq!(i64::from_ne_bytes(b[8..].try_into().unwrap()), 999_999_999);
        let b = timespec_bytes(1_500_000_000);
        assert_eq!(i64::from_ne_bytes(b[..8].try_into().unwrap()), 1);
        assert_eq!(i64::from_ne_bytes(b[8..].try_into().unwrap()), 500_000_000);
    }

    #[test]
    fn the_offset_file() {
        let dir = std::env::temp_dir().join(format!("hydo-clock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("clock.svc");
        let mut o = Offset::new(CString::new(p.to_string_lossy().as_bytes()).unwrap());
        assert_eq!(o.now(), 0, "a missing file is offset 0");
        std::fs::write(&p, "1500000000 0\n").unwrap();
        assert_eq!(o.now(), 1_500_000_000);
        std::fs::write(&p, "-3000000000 0").unwrap();
        assert_eq!(o.now(), 1_500_000_000, "a line without its newline waits");
        std::fs::write(&p, "-3000000000 0\n").unwrap();
        assert_eq!(o.now(), -3_000_000_000);
        std::fs::write(&p, "0 500000\n").unwrap();
        assert_eq!(
            o.now(),
            0,
            "the drift counts from the first read of its line"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        let d = o.now();
        assert!((10_000_000..1_000_000_000).contains(&d), "drift {d}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

mod words;
