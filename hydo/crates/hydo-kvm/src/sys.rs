//! Linux KVM through its ioctls: `/dev/kvm`, a VM per fresh machine, its one vCPU and the shared
//! `kvm_run` page. With [`crate::mem`] this is the only `unsafe` code of the crate; everything else
//! sees the safe [`Kvm`] ([`Hypervisor`]) and [`KvmCpu`] ([`Cpu`]).
//!
//! A VM is set up the same way every time ([`Kvm::fresh`]): KVM's PMU off when the host can turn
//! it off, every MSR exit to user space (`KVM_CAP_X86_USER_SPACE_MSR`) and the filter of
//! [`crate::msr`], the TSS and identity-map pages, guest RAM as memory slot 0 from address 0, one
//! vCPU, the CPUID of [`crate::cpuid`]. No in-kernel interrupt controller and no in-kernel PIT
//! are created: the PIC and the timer are the engine's.
//!
//! Kicks: the spin watchdog sends the vCPU's thread a real-time signal whose handler does
//! nothing; `KVM_RUN` then returns `EINTR`, the guest having changed in no way.

use crate::abi::*;
use crate::cpu::{Cpu, CpuState, Exit, Hypervisor, Reply};
use std::fs::File;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `ioctl(fd, req, arg)`.
///
/// # Safety
/// `arg` must be what `req` expects (a value, or a pointer to memory of the request's size).
unsafe fn ioctl(fd: RawFd, req: u64, arg: u64) -> i32 {
    libc::ioctl(fd, req as _, arg)
}

fn chk(what: &str, r: i32) -> Result<i32, String> {
    if r < 0 {
        Err(format!(
            "hydo-kvm: {what}: {}",
            std::io::Error::from_raw_os_error(errno())
        ))
    } else {
        Ok(r)
    }
}

/// RFLAGS.RF (resume: no instruction breakpoint on the next instruction).
const RFLAGS_RF: u64 = 1 << 16;
/// RFLAGS.TF (the guest single-steps itself).
const RFLAGS_TF: u64 = 1 << 8;

/// The capabilities the engine cannot work without.
const REQUIRED: [(u32, &str); 10] = [
    (KVM_CAP_USER_MEMORY, "KVM_CAP_USER_MEMORY"),
    (KVM_CAP_SET_TSS_ADDR, "KVM_CAP_SET_TSS_ADDR"),
    (KVM_CAP_EXT_CPUID, "KVM_CAP_EXT_CPUID"),
    (KVM_CAP_VCPU_EVENTS, "KVM_CAP_VCPU_EVENTS"),
    (KVM_CAP_DEBUGREGS, "KVM_CAP_DEBUGREGS"),
    (KVM_CAP_XSAVE, "KVM_CAP_XSAVE"),
    (KVM_CAP_IMMEDIATE_EXIT, "KVM_CAP_IMMEDIATE_EXIT"),
    (
        KVM_CAP_X86_USER_SPACE_MSR,
        "KVM_CAP_X86_USER_SPACE_MSR (Linux 5.10)",
    ),
    (
        KVM_CAP_X86_MSR_FILTER,
        "KVM_CAP_X86_MSR_FILTER (Linux 5.10)",
    ),
    (
        KVM_CAP_ENFORCE_PV_FEATURE_CPUID,
        "KVM_CAP_ENFORCE_PV_FEATURE_CPUID (Linux 5.10)",
    ),
];

/// `/dev/kvm`, checked.
pub struct Kvm {
    sys: File,
    run_size: usize,
    pmu_off: bool,
    desc: String,
}

/// What the host is, for the diagnostics and the doctor.
pub fn host_line() -> String {
    let rel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();
    format!("Linux {rel}, {cpu}")
}

impl Kvm {
    /// Open the device and check the API, the capabilities and the host's CPUID.
    pub fn open(dev: &str) -> Result<Kvm, String> {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dev)
            .map_err(|e| format!("hydo-kvm: {dev}: {e}"))?;
        let fd = f.as_raw_fd();
        // SAFETY: an ioctl without an argument
        let v = unsafe { ioctl(fd, KVM_GET_API_VERSION, 0) };
        if v != KVM_API_VERSION {
            return Err(format!(
                "hydo-kvm: {dev}: KVM API version {v}, this engine needs {KVM_API_VERSION}"
            ));
        }
        let mut missing = Vec::new();
        for (cap, name) in REQUIRED {
            // SAFETY: KVM_CHECK_EXTENSION takes the capability number by value
            if unsafe { ioctl(fd, KVM_CHECK_EXTENSION, cap as u64) } <= 0 {
                missing.push(name);
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "hydo-kvm: this host's KVM lacks {}",
                missing.join(", ")
            ));
        }
        // SAFETY: as above
        let pmu = unsafe { ioctl(fd, KVM_CHECK_EXTENSION, KVM_CAP_PMU_CAPABILITY as u64) };
        // SAFETY: no argument
        let run_size = chk("KVM_GET_VCPU_MMAP_SIZE", unsafe {
            ioctl(fd, KVM_GET_VCPU_MMAP_SIZE, 0)
        })? as usize;
        let mut c = Box::<kvm_cpuid2>::default();
        c.h.nent = CPUID_MAX as u32;
        // SAFETY: a kvm_cpuid2 with room for nent entries
        chk("KVM_GET_SUPPORTED_CPUID", unsafe {
            ioctl(
                fd,
                KVM_GET_SUPPORTED_CPUID,
                &mut *c as *mut kvm_cpuid2 as u64,
            )
        })?;
        let n = (c.h.nent as usize).min(CPUID_MAX);
        crate::cpuid::check(&c.entries[..n])?;
        let desc = format!(
            "hydo-kvm: {dev}, KVM API {v}, {}{}",
            host_line(),
            if pmu as u64 & KVM_PMU_CAP_DISABLE != 0 {
                ""
            } else {
                " (KVM's PMU cannot be turned off here; the guest's CPUID and MSR filter hide it)"
            }
        );
        Ok(Kvm {
            sys: f,
            run_size,
            pmu_off: pmu as u64 & KVM_PMU_CAP_DISABLE != 0,
            desc,
        })
    }
}

fn new_fd(r: i32) -> File {
    // SAFETY: a file descriptor the kernel just returned to us, owned by the File from now on
    unsafe { File::from_raw_fd(r) }
}

impl Hypervisor for Kvm {
    unsafe fn fresh(&mut self, ram: *mut u8, len: usize) -> Result<Box<dyn Cpu>, String> {
        let vm = new_fd(chk(
            "KVM_CREATE_VM",
            ioctl(self.sys.as_raw_fd(), KVM_CREATE_VM, 0),
        )?);
        let v = vm.as_raw_fd();
        if self.pmu_off {
            let mut cap = kvm_enable_cap {
                cap: KVM_CAP_PMU_CAPABILITY,
                ..kvm_enable_cap::default()
            };
            cap.args[0] = KVM_PMU_CAP_DISABLE;
            chk(
                "KVM_ENABLE_CAP(PMU_CAPABILITY)",
                ioctl(v, KVM_ENABLE_CAP, &cap as *const _ as u64),
            )?;
        }
        let mut cap = kvm_enable_cap {
            cap: KVM_CAP_X86_USER_SPACE_MSR,
            ..kvm_enable_cap::default()
        };
        cap.args[0] =
            KVM_MSR_EXIT_REASON_INVAL | KVM_MSR_EXIT_REASON_UNKNOWN | KVM_MSR_EXIT_REASON_FILTER;
        chk(
            "KVM_ENABLE_CAP(X86_USER_SPACE_MSR)",
            ioctl(v, KVM_ENABLE_CAP, &cap as *const _ as u64),
        )?;
        let ranges = crate::msr::filter_ranges();
        let mut f = kvm_msr_filter {
            flags: KVM_MSR_FILTER_DEFAULT_DENY,
            ..kvm_msr_filter::default()
        };
        for (i, (base, n, bm)) in ranges.iter().enumerate() {
            f.ranges[i] = kvm_msr_filter_range {
                flags: KVM_MSR_FILTER_READ | KVM_MSR_FILTER_WRITE,
                nmsrs: *n,
                base: *base,
                bitmap: bm.as_ptr() as u64,
            };
        }
        chk(
            "KVM_X86_SET_MSR_FILTER",
            ioctl(v, KVM_X86_SET_MSR_FILTER, &f as *const _ as u64),
        )?;
        chk(
            "KVM_SET_TSS_ADDR",
            ioctl(v, KVM_SET_TSS_ADDR, crate::layout::KVM_TSS_ADDR),
        )?;
        let idmap: u64 = crate::layout::KVM_IDENTITY_MAP_ADDR;
        chk(
            "KVM_SET_IDENTITY_MAP_ADDR",
            ioctl(v, KVM_SET_IDENTITY_MAP_ADDR, &idmap as *const u64 as u64),
        )?;
        let region = kvm_userspace_memory_region {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: len as u64,
            userspace_addr: ram as u64,
        };
        chk(
            "KVM_SET_USER_MEMORY_REGION",
            ioctl(v, KVM_SET_USER_MEMORY_REGION, &region as *const _ as u64),
        )?;
        let vcpu = new_fd(chk("KVM_CREATE_VCPU", ioctl(v, KVM_CREATE_VCPU, 0))?);
        let p = libc::mmap(
            std::ptr::null_mut(),
            self.run_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            vcpu.as_raw_fd(),
            0,
        );
        if p == libc::MAP_FAILED {
            return Err(format!(
                "hydo-kvm: mmap of kvm_run: {}",
                std::io::Error::last_os_error()
            ));
        }
        // owned from here on: an error below drops it, which unmaps the page
        let cpu = KvmCpu {
            _vm: vm,
            vcpu,
            run: p.cast(),
            run_size: self.run_size,
            last: Last::None,
            thread: Arc::new(AtomicU64::new(0)),
            stepping: false,
            rf_at: None,
            ram: ram as u64,
            ram_len: len,
            dirty: None,
        };
        let mut c = Box::<kvm_cpuid2>::default();
        let t = crate::cpuid::table();
        c.h.nent = t.len() as u32;
        c.entries[..t.len()].copy_from_slice(&t);
        chk(
            "KVM_SET_CPUID2",
            ioctl(cpu.fd(), KVM_SET_CPUID2, &*c as *const kvm_cpuid2 as u64),
        )?;
        // KVM's paravirtual hypercalls and MSRs only as the guest's CPUID offers them: the table
        // has no KVM leaf, so none (KVM_HC_CLOCK_PAIRING would write the host's clock into
        // guest memory)
        let mut pv = kvm_enable_cap {
            cap: KVM_CAP_ENFORCE_PV_FEATURE_CPUID,
            ..kvm_enable_cap::default()
        };
        pv.args[0] = 1;
        chk(
            "KVM_ENABLE_CAP(ENFORCE_PV_FEATURE_CPUID)",
            ioctl(cpu.fd(), KVM_ENABLE_CAP, &pv as *const _ as u64),
        )?;
        install_kick_handler()?;
        Ok(Box::new(cpu))
    }

    fn describe(&self) -> String {
        self.desc.clone()
    }
}

/// What the last exit was, for [`Cpu::reply`].
#[derive(Clone, Copy)]
enum Last {
    None,
    Io { off: usize, len: usize },
    Mmio,
    Msr,
}

pub struct KvmCpu {
    _vm: File,
    vcpu: File,
    run: *mut u8,
    run_size: usize,
    last: Last,
    /// the pthread that runs the vCPU (0 before its first run)
    thread: Arc<AtomicU64>,
    /// the spin observer's breakpoints are armed ([`Cpu::step_to`])
    stepping: bool,
    /// where the observer set RFLAGS.RF for a jump to itself (cleared again on disarming when the
    /// instruction did not run)
    rf_at: Option<u64>,
    /// guest RAM (memory slot 0)
    ram: u64,
    ram_len: usize,
    /// the dirty-page log's bitmap while the log is on ([`Cpu::dirty_log`])
    dirty: Option<Vec<u64>>,
}

// SAFETY: the kvm_run page is owned by this value and used by one thread at a time (the engine's)
unsafe impl Send for KvmCpu {}

fn kick_signal() -> i32 {
    libc::SIGRTMIN() + 6
}

extern "C" fn on_kick(_: libc::c_int) {}

fn install_kick_handler() -> Result<(), String> {
    static ONCE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: a handler that does nothing. KVM_RUN returns EINTR when the signal arrives
        // (KVM does not restart it); SA_RESTART restarts any other system call the engine's
        // thread is in when a late kick arrives.
        let r = unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_kick as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(kick_signal(), &sa, std::ptr::null_mut())
        };
        // without the handler the signal's default action would end the process
        if r != 0 {
            return Err(format!(
                "hydo-kvm: cannot install the watchdog's signal handler: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    })
    .clone()
}

/// Unblock the watchdog's signal in the calling (vCPU) thread: a thread created with it blocked
/// would never be kicked out of an exit-free guest.
fn unblock_kick() {
    // SAFETY: a signal set on the stack and pthread_sigmask on the calling thread
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, kick_signal());
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

impl KvmCpu {
    fn head(&self) -> &kvm_run_head {
        // SAFETY: the kvm_run page starts with this head and lives as long as self
        unsafe { &*(self.run as *const kvm_run_head) }
    }
    fn head_mut(&mut self) -> &mut kvm_run_head {
        // SAFETY: as above, borrowed mutably through self
        unsafe { &mut *(self.run as *mut kvm_run_head) }
    }
    fn exit_union<T: Copy>(&self) -> T {
        // SAFETY: the union member T is at KVM_RUN_EXIT_OFFSET and fits in the page
        unsafe { std::ptr::read_unaligned(self.run.add(KVM_RUN_EXIT_OFFSET) as *const T) }
    }
    fn fd(&self) -> RawFd {
        self.vcpu.as_raw_fd()
    }

    fn get<T: Default>(&self, req: u64, what: &str) -> T {
        let mut v = T::default();
        // SAFETY: `req` reads a T into v
        let r = unsafe { ioctl(self.fd(), req, &mut v as *mut T as u64) };
        if let Err(e) = chk(what, r) {
            crate::fatal(1, &e);
        }
        v
    }
    fn set<T>(&self, req: u64, v: &T, what: &str) {
        // SAFETY: `req` reads a T from v
        let r = unsafe { ioctl(self.fd(), req, v as *const T as u64) };
        if let Err(e) = chk(what, r) {
            crate::fatal(1, &e);
        }
    }

    /// The exit in the kvm_run page after a KVM_RUN that returned 0.
    fn decode(&mut self) -> Exit {
        let reason = self.head().exit_reason;
        self.last = Last::None;
        match reason {
            KVM_EXIT_IO => {
                let io: kvm_run_io = self.exit_union();
                let len = io.size as usize * io.count as usize;
                let off = io.data_offset as usize;
                if off + len > self.run_size {
                    return Exit::Fail(format!(
                        "an I/O exit with data outside kvm_run ({off}+{len})"
                    ));
                }
                self.last = Last::Io { off, len };
                let out = io.direction == KVM_EXIT_IO_OUT;
                let data = if out {
                    // SAFETY: checked to be inside the page above
                    unsafe { std::slice::from_raw_parts(self.run.add(off), len) }.to_vec()
                } else {
                    Vec::new()
                };
                Exit::Io {
                    port: io.port,
                    size: io.size,
                    out,
                    count: io.count,
                    data,
                }
            }
            KVM_EXIT_MMIO => {
                let m: kvm_run_mmio = self.exit_union();
                self.last = Last::Mmio;
                Exit::Mmio {
                    addr: m.phys_addr,
                    len: m.len as u8,
                    write: m.is_write != 0,
                    data: m.data,
                }
            }
            KVM_EXIT_X86_RDMSR | KVM_EXIT_X86_WRMSR => {
                let m: kvm_run_msr = self.exit_union();
                self.last = Last::Msr;
                if reason == KVM_EXIT_X86_RDMSR {
                    Exit::RdMsr { index: m.index }
                } else {
                    Exit::WrMsr {
                        index: m.index,
                        value: m.data,
                    }
                }
            }
            KVM_EXIT_HLT => Exit::Hlt,
            KVM_EXIT_IRQ_WINDOW_OPEN => Exit::IrqWindow,
            KVM_EXIT_SHUTDOWN => Exit::Shutdown,
            KVM_EXIT_INTR => Exit::Kick,
            KVM_EXIT_DEBUG => {
                // only the observer's two instruction breakpoints (DR6 B0, B1) are expected: the
                // observer does not step a guest that uses the trap flag or its own breakpoints
                let d: kvm_debug_exit_arch = self.exit_union();
                if self.stepping && d.dr6 & 3 != 0 {
                    Exit::Debug
                } else {
                    Exit::Fail(format!(
                        "an unexpected debug exception (exception {}, dr6 {:#x}, at {:#x})",
                        d.exception, d.dr6, d.pc
                    ))
                }
            }
            KVM_EXIT_FAIL_ENTRY => {
                let r: u64 = self.exit_union();
                Exit::Fail(format!("VM entry failed (hardware reason {r:#x})"))
            }
            KVM_EXIT_INTERNAL_ERROR => {
                let i: kvm_run_internal = self.exit_union();
                Exit::Fail(format!(
                    "KVM internal error {} (data {:x?})",
                    i.suberror,
                    &i.data[..(i.ndata as usize).min(16)]
                ))
            }
            r => Exit::Fail(format!("unexpected KVM exit reason {r}")),
        }
    }

    /// KVM_RUN once: Ok(true) for an exit in the page, Ok(false) for EINTR.
    fn kvm_run(&mut self) -> Result<bool, String> {
        loop {
            // SAFETY: KVM_RUN takes no argument
            let r = unsafe { ioctl(self.fd(), KVM_RUN, 0) };
            if r >= 0 {
                return Ok(true);
            }
            match errno() {
                libc::EINTR => return Ok(false),
                libc::EAGAIN => continue,
                e => return Err(format!("KVM_RUN: {}", std::io::Error::from_raw_os_error(e))),
            }
        }
    }
}

impl Cpu for KvmCpu {
    fn run(&mut self) -> Exit {
        if self.thread.load(Ordering::Relaxed) == 0 {
            // SAFETY: pthread_self has no preconditions
            let t = unsafe { libc::pthread_self() } as u64;
            unblock_kick();
            self.thread.store(t, Ordering::Relaxed);
        }
        match self.kvm_run() {
            Ok(true) => self.decode(),
            Ok(false) => Exit::Kick,
            Err(e) => Exit::Fail(e),
        }
    }

    fn reply(&mut self, r: Reply) {
        match (self.last, r) {
            (Last::Io { off, len }, Reply::Io(b)) => {
                let n = len.min(b.len());
                // SAFETY: off+len was checked to be inside the page when the exit was decoded
                unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), self.run.add(off), n) };
            }
            (Last::Mmio, Reply::Mmio(d)) => {
                let mut m: kvm_run_mmio = self.exit_union();
                m.data = d;
                // SAFETY: the mmio member at its offset in the page
                unsafe {
                    std::ptr::write_unaligned(
                        self.run.add(KVM_RUN_EXIT_OFFSET) as *mut kvm_run_mmio,
                        m,
                    )
                };
            }
            (Last::Msr, Reply::Msr { value, fault }) => {
                let mut m: kvm_run_msr = self.exit_union();
                m.data = value;
                m.error = fault as u8;
                // SAFETY: the msr member at its offset in the page
                unsafe {
                    std::ptr::write_unaligned(
                        self.run.add(KVM_RUN_EXIT_OFFSET) as *mut kvm_run_msr,
                        m,
                    )
                };
            }
            _ => crate::fatal(1, "hydo-kvm: an answer for an exit that asked none"),
        }
    }

    fn complete(&mut self) -> Option<Exit> {
        self.head_mut().immediate_exit = 1;
        let r = self.kvm_run();
        self.head_mut().immediate_exit = 0;
        match r {
            Ok(false) => None,
            Ok(true) => match self.decode() {
                Exit::Kick => None,
                e => Some(e),
            },
            Err(e) => Some(Exit::Fail(e)),
        }
    }

    fn interruptible(&self) -> bool {
        self.head().ready_for_interrupt_injection != 0
    }

    fn if_flag(&self) -> bool {
        self.head().if_flag != 0
    }

    fn inject(&mut self, vector: u8) {
        let i = kvm_interrupt { irq: vector as u32 };
        self.set(KVM_INTERRUPT, &i, "KVM_INTERRUPT");
    }

    fn request_window(&mut self, on: bool) {
        self.head_mut().request_interrupt_window = on as u8;
    }

    fn save(&self) -> CpuState {
        let mut msrs = kvm_msrs::default();
        let list = crate::msr::SNAPSHOT;
        msrs.h.nmsrs = list.len() as u32;
        for (i, &m) in list.iter().enumerate() {
            msrs.entries[i].index = m;
        }
        // SAFETY: a kvm_msrs with nmsrs entries
        let n = unsafe { ioctl(self.fd(), KVM_GET_MSRS, &mut msrs as *mut kvm_msrs as u64) };
        if n != list.len() as i32 {
            crate::fatal(
                1,
                &format!("hydo-kvm: KVM_GET_MSRS read {n} of {}", list.len()),
            );
        }
        let xs: kvm_xsave = self.get(KVM_GET_XSAVE, "KVM_GET_XSAVE");
        CpuState {
            regs: self.get(KVM_GET_REGS, "KVM_GET_REGS"),
            sregs: self.get(KVM_GET_SREGS, "KVM_GET_SREGS"),
            fpu: crate::cpu::fpu_of_xsave(&xs.region),
            xsave: xs.region.to_vec(),
            msrs: list
                .iter()
                .enumerate()
                .map(|(i, &m)| (m, msrs.entries[i].data))
                .collect(),
            events: self.get(KVM_GET_VCPU_EVENTS, "KVM_GET_VCPU_EVENTS"),
            debugregs: self.get(KVM_GET_DEBUGREGS, "KVM_GET_DEBUGREGS"),
        }
    }

    fn load(&mut self, s: &CpuState) {
        self.set(KVM_SET_SREGS, &s.sregs, "KVM_SET_SREGS");
        self.set(KVM_SET_REGS, &s.regs, "KVM_SET_REGS");
        if s.xsave.len() == 1024 {
            let mut xs = kvm_xsave::default();
            xs.region.copy_from_slice(&s.xsave);
            self.set(KVM_SET_XSAVE, &xs, "KVM_SET_XSAVE");
        } else {
            self.set(KVM_SET_FPU, &s.fpu, "KVM_SET_FPU");
        }
        let mut msrs = kvm_msrs::default();
        msrs.h.nmsrs = s.msrs.len() as u32;
        for (i, (m, v)) in s.msrs.iter().enumerate() {
            msrs.entries[i].index = *m;
            msrs.entries[i].data = *v;
        }
        // SAFETY: a kvm_msrs with nmsrs entries
        let n = unsafe { ioctl(self.fd(), KVM_SET_MSRS, &msrs as *const kvm_msrs as u64) };
        if n != s.msrs.len() as i32 {
            crate::fatal(
                1,
                &format!("hydo-kvm: KVM_SET_MSRS wrote {n} of {}", s.msrs.len()),
            );
        }
        self.set(KVM_SET_VCPU_EVENTS, &s.events, "KVM_SET_VCPU_EVENTS");
        let d = kvm_debugregs {
            flags: 0,
            ..s.debugregs
        };
        self.set(KVM_SET_DEBUGREGS, &d, "KVM_SET_DEBUGREGS");
    }

    fn rip(&self) -> u64 {
        self.get::<kvm_regs>(KVM_GET_REGS, "KVM_GET_REGS").rip
    }

    fn regs(&self) -> kvm_regs {
        self.get(KVM_GET_REGS, "KVM_GET_REGS")
    }

    fn cr3(&self) -> u64 {
        self.get::<kvm_sregs>(KVM_GET_SREGS, "KVM_GET_SREGS").cr3
    }

    fn may_step(&self) -> bool {
        let r = self.regs();
        let s = self.get::<kvm_sregs>(KVM_GET_SREGS, "KVM_GET_SREGS");
        let d = self.get::<kvm_debugregs>(KVM_GET_DEBUGREGS, "KVM_GET_DEBUGREGS");
        s.cs.l == 1 && r.rflags & RFLAGS_TF == 0 && d.dr7 & 0xff == 0
    }

    fn step_to(&mut self, step: Option<&crate::spin::Step>) {
        let mut d = kvm_guest_debug::default();
        if let Some(s) = step {
            // execute breakpoints (R/W and LEN 0) in DR0 and DR1, local enables L0 and L1, and
            // DR7's fixed bit 10; KVM puts them in the hardware while the guest runs and keeps
            // the guest's own debug registers aside
            d.control = KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_USE_HW_BP;
            let mut dr7 = 0x400u64;
            for (k, &a) in s.addrs().iter().enumerate() {
                d.debugreg[k] = a;
                dr7 |= 1 << (2 * k);
            }
            d.debugreg[7] = dr7;
            // a breakpoint at the instruction about to run (a jump to itself) would stop before
            // it: RF lets it run once (the CPU clears RF when the instruction completes, and an
            // exception's frame has RF set for a fault anyway)
            let mut r = self.regs();
            if self.rf_at != Some(r.rip) {
                self.rf_at = None; // the instruction ran: the CPU cleared RF
            }
            if s.addrs().contains(&r.rip) && r.rflags & RFLAGS_RF == 0 {
                r.rflags |= RFLAGS_RF;
                self.set(KVM_SET_REGS, &r, "KVM_SET_REGS");
                self.rf_at = Some(r.rip);
            }
        } else if let Some(at) = self.rf_at.take() {
            // disarmed before the instruction ran (a kick before the entry): take RF back, so
            // that nothing of the observer stays in the guest's state
            let mut r = self.regs();
            if r.rip == at && r.rflags & RFLAGS_RF != 0 {
                r.rflags &= !RFLAGS_RF;
                self.set(KVM_SET_REGS, &r, "KVM_SET_REGS");
            }
        }
        self.stepping = step.is_some();
        self.set(KVM_SET_GUEST_DEBUG, &d, "KVM_SET_GUEST_DEBUG");
    }

    fn run_flags(&self) -> [u8; 3] {
        let h = self.head();
        [
            h.ready_for_interrupt_injection,
            h.if_flag,
            h.request_interrupt_window,
        ]
    }

    fn set_run_flags(&mut self, f: [u8; 3]) {
        let h = self.head_mut();
        h.ready_for_interrupt_injection = f[0];
        h.if_flag = f[1];
        h.request_interrupt_window = f[2];
    }

    fn dirty_log(&mut self, on: bool) {
        let region = kvm_userspace_memory_region {
            slot: 0,
            flags: if on { KVM_MEM_LOG_DIRTY_PAGES } else { 0 },
            guest_phys_addr: 0,
            memory_size: self.ram_len as u64,
            userspace_addr: self.ram,
        };
        // SAFETY: the region of `Kvm::fresh` again, with the log's flag
        let r = unsafe {
            ioctl(
                self._vm.as_raw_fd(),
                KVM_SET_USER_MEMORY_REGION,
                &region as *const _ as u64,
            )
        };
        if let Err(e) = chk("KVM_SET_USER_MEMORY_REGION (dirty log)", r) {
            crate::fatal(1, &e);
        }
        self.dirty = on.then(|| vec![0u64; (self.ram_len / 4096).div_ceil(64)]);
    }

    fn take_dirty(&mut self, out: &mut Vec<u64>) {
        let Some(bm) = self.dirty.as_mut() else {
            return;
        };
        let log = kvm_dirty_log {
            slot: 0,
            padding1: 0,
            dirty_bitmap: bm.as_mut_ptr() as u64,
        };
        // SAFETY: the bitmap has a bit for every page of the slot
        let r = unsafe {
            ioctl(
                self._vm.as_raw_fd(),
                KVM_GET_DIRTY_LOG,
                &log as *const _ as u64,
            )
        };
        if let Err(e) = chk("KVM_GET_DIRTY_LOG", r) {
            crate::fatal(1, &e);
        }
        for (i, w) in bm.iter().enumerate() {
            let mut w = *w;
            while w != 0 {
                let b = w.trailing_zeros() as u64;
                out.push(i as u64 * 64 + b);
                w &= w - 1;
            }
        }
    }

    fn kicker(&self) -> Box<dyn Fn() + Send + Sync> {
        let t = self.thread.clone();
        Box::new(move || {
            let id = t.load(Ordering::Relaxed);
            if id != 0 {
                // SAFETY: the vCPU's thread lives while the VMM holds this kicker (the VMM drops
                // it before its vCPU), and the signal's handler does nothing
                unsafe {
                    libc::pthread_kill(id as libc::pthread_t, kick_signal());
                }
            }
        })
    }
}

impl Drop for KvmCpu {
    fn drop(&mut self) {
        // SAFETY: the page mapped in Kvm::fresh, not used after this
        unsafe {
            libc::munmap(self.run.cast(), self.run_size);
        }
    }
}
