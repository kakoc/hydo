//! Linux KVM through its ioctls: `/dev/kvm`, a VM per fresh machine, its one vCPU and the shared
//! `kvm_run` page. With [`crate::mem`] this is the only `unsafe` code of the crate; everything else
//! sees the safe [`Kvm`] ([`Hypervisor`]) and [`KvmCpu`] ([`Cpu`]).
//!
//! A VM is set up the same way every time ([`Kvm::fresh`]): KVM's PMU off when the host can turn
//! it off, every MSR exit to user space (`KVM_CAP_X86_USER_SPACE_MSR`) and the filter of
//! [`crate::msr`], the TSS and identity-map pages, guest RAM as memory slot 0 from address 0 (and,
//! for a guest of more than 3 GiB, slot 1 from 4 GiB: [`crate::layout::ram_blocks`]), one vCPU,
//! the CPUID of [`crate::cpuid`]. No in-kernel interrupt controller and no in-kernel PIT are
//! created: the PIC and the timer are the engine's.
//!
//! Kicks: the spin watchdog sends the vCPU's thread a real-time signal whose handler does
//! nothing; `KVM_RUN` then returns `EINTR`, the guest having changed in no way.

use crate::abi::*;
use crate::cpu::{Cpu, CpuState, Exit, Hypervisor, Reply};
use crate::cpuid::Level;
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
    /// what the host's KVM supports (`KVM_GET_SUPPORTED_CPUID`)
    supported: Vec<kvm_cpuid_entry2>,
    /// `KVM_CAP_XCRS`
    xcrs: bool,
    /// the guest CPU level ([`Kvm::with_level`])
    level: Level,
    /// the CPUID cache leaves (`guest.cpu_cache`, [`Kvm::with_cache`])
    cache: bool,
    /// v3 on a hybrid CPU: the CPUs of one kind of core every vCPU thread runs on
    /// ([`crate::hybrid`])
    pin: Option<Vec<usize>>,
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
        crate::cpuid::check(&c.entries[..n], Level::V2)?;
        // SAFETY: as above
        let xcrs = unsafe { ioctl(fd, KVM_CHECK_EXTENSION, KVM_CAP_XCRS as u64) } > 0;
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
            supported: c.entries[..n].to_vec(),
            xcrs,
            level: Level::V2,
            cache: false,
            pin: None,
        })
    }

    /// Whether the guest's CPUID describes the fixed cache hierarchy (`guest.cpu_cache`, M203:
    /// [`crate::cpuid::table_with`]); no host requirement.
    pub fn with_cache(mut self, cache: bool) -> Kvm {
        if cache {
            self.desc = format!("{}; CPUID cache leaves", self.desc);
        }
        self.cache = cache;
        self
    }

    /// The guest CPU level of every VM this makes (x86-64-v2 unless set): Err when the host's KVM
    /// cannot give it (the features of [`crate::cpuid::table_for`], and for v3 the guest's XCR0
    /// in snapshots, `KVM_CAP_XCRS`).
    pub fn with_level(mut self, level: Level) -> Result<Kvm, String> {
        crate::cpuid::check(&self.supported, level).map_err(|e| format!("hydo-kvm: {e}"))?;
        if level == Level::V3 && !self.xcrs {
            return Err(
                "hydo-kvm: this host's KVM lacks KVM_CAP_XCRS (the guest's XCR0 at x86-64-v3)"
                    .into(),
            );
        }
        if level != Level::V2 {
            self.desc = format!("{}; guest CPU {}", self.desc, level.name());
        }
        if level == Level::V3 {
            self.pin = one_kind_of_core();
            if let Some(p) = &self.pin {
                self.desc = format!(
                    "{}, on one kind of core of this hybrid CPU: CPUs {}",
                    self.desc,
                    p.iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
        }
        self.level = level;
        Ok(self)
    }
}

/// The CPU list of a sysfs file (empty when it is not there).
fn sys_cpus(path: &str) -> Vec<usize> {
    std::fs::read_to_string(path)
        .map(|s| crate::hybrid::cpulist(&s))
        .unwrap_or_default()
}

/// The CPUs this thread may use now.
fn allowed_cpus() -> Vec<usize> {
    // SAFETY: a zeroed cpu_set_t is a valid empty set; sched_getaffinity fills it
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) };
    if r != 0 {
        return Vec::new();
    }
    // SAFETY: CPU_ISSET reads the set within its size
    (0..libc::CPU_SETSIZE as usize)
        .filter(|&c| unsafe { libc::CPU_ISSET(c, &set) })
        .collect()
}

/// On a hybrid Intel CPU, the CPUs of one kind of core this process may use
/// ([`crate::hybrid::choose`]); None elsewhere.
fn one_kind_of_core() -> Option<Vec<usize>> {
    crate::hybrid::choose(
        &allowed_cpus(),
        &sys_cpus("/sys/devices/cpu_core/cpus"),
        &sys_cpus("/sys/devices/cpu_atom/cpus"),
    )
}

/// Bind the calling thread to `cpus`.
fn bind_thread(cpus: &[usize]) -> Result<(), String> {
    // SAFETY: a zeroed cpu_set_t is a valid empty set; CPU_SET writes within it
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for &c in cpus {
        unsafe { libc::CPU_SET(c, &mut set) };
    }
    // SAFETY: the set lives across the call
    let r = unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) };
    chk("sched_setaffinity (one kind of core of a hybrid CPU)", r).map(|_| ())
}

fn new_fd(r: i32) -> File {
    // SAFETY: a file descriptor the kernel just returned to us, owned by the File from now on
    unsafe { File::from_raw_fd(r) }
}

impl Hypervisor for Kvm {
    unsafe fn fresh(&mut self, ram: *mut u8, len: usize) -> Result<Box<dyn Cpu>, String> {
        let mut v = self.fresh_n(ram, len, 1)?;
        Ok(v.remove(0))
    }

    /// One VM and its `n` vCPUs (ids 0 to n-1), each with its CPUID table: the one-vCPU table
    /// for n = 1 (the machine of every recorded value), [`crate::cpuid::table_smp`] above.
    unsafe fn fresh_n(
        &mut self,
        ram: *mut u8,
        len: usize,
        n: usize,
    ) -> Result<Vec<Box<dyn Cpu>>, String> {
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
        // one memory slot per block of RAM (slot 0 from 0; slot 1 from 4 GiB above 3 GiB)
        for (slot, (at, off, size)) in crate::layout::ram_blocks(len as u64)
            .into_iter()
            .enumerate()
        {
            let region = kvm_userspace_memory_region {
                slot: slot as u32,
                flags: 0,
                guest_phys_addr: at,
                memory_size: size,
                userspace_addr: ram as u64 + off,
            };
            chk(
                "KVM_SET_USER_MEMORY_REGION",
                ioctl(v, KVM_SET_USER_MEMORY_REGION, &region as *const _ as u64),
            )?;
        }
        let vm = Arc::new(vm);
        let mut cpus: Vec<Box<dyn Cpu>> = Vec::with_capacity(n);
        for id in 0..n.max(1) {
            cpus.push(Box::new(self.vcpu(&vm, id, n, ram, len)?));
        }
        install_kick_handler()?;
        Ok(cpus)
    }

    fn describe(&self) -> String {
        self.desc.clone()
    }

    fn branches(&self) -> Result<Box<dyn crate::branches::Branches>, String> {
        Ok(Box::new(PerfBranches::open()?))
    }
}

impl Kvm {
    /// vCPU `id` of `vm` (of `n`), its `kvm_run` page mapped and its CPUID set.
    ///
    /// # Safety
    /// `ram..ram+len` is the VM's guest RAM and stays mapped for the life of the vCPU.
    unsafe fn vcpu(
        &self,
        vm: &Arc<File>,
        id: usize,
        n: usize,
        ram: *mut u8,
        len: usize,
    ) -> Result<KvmCpu, String> {
        let v = vm.as_raw_fd();
        let vcpu = new_fd(chk(
            "KVM_CREATE_VCPU",
            ioctl(v, KVM_CREATE_VCPU, id as u64),
        )?);
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
            _vm: vm.clone(),
            vcpu,
            run: p.cast(),
            run_size: self.run_size,
            last: Last::None,
            thread: Arc::new(AtomicU64::new(0)),
            stepping: false,
            tf: false,
            rf_at: None,
            ct_bps: Vec::new(),
            ct_rf: None,
            ct_set: false,
            ct_hw: false,
            obs: Vec::new(),
            obs_mask: 0,
            dr6: 0,
            ram: ram as u64,
            ram_len: len,
            dirty: None,
            v3: self.level == Level::V3,
            pin: self.pin.clone(),
        };
        let mut c = Box::<kvm_cpuid2>::default();
        let t = if n <= 1 {
            crate::cpuid::table_with(self.level, self.cache)
        } else {
            crate::cpuid::table_smp(self.level, self.cache, n as u32, id as u32)
        };
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
        Ok(cpu)
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
    /// the VM, shared by its vCPUs
    _vm: Arc<File>,
    vcpu: File,
    run: *mut u8,
    run_size: usize,
    last: Last,
    /// the pthread that runs the vCPU (0 before its first run)
    thread: Arc<AtomicU64>,
    /// the spin observer's breakpoints are armed ([`Cpu::step_to`])
    stepping: bool,
    /// trap-flag single steps are on ([`Cpu::single_step`]: the walk of a full-state cycle)
    tf: bool,
    /// where the observer set RFLAGS.RF for a jump to itself (cleared again on disarming when the
    /// instruction did not run)
    rf_at: Option<u64>,
    /// the function contracts' breakpoints ([`Cpu::set_contract_bps`], Milestone 255)
    ct_bps: Vec<u64>,
    /// where [`Cpu::contract_resume`] set RFLAGS.RF
    ct_rf: Option<u64>,
    /// the contracts' breakpoints are in KVM ([`Cpu::set_contract_bps`])
    ct_set: bool,
    /// the contracts' breakpoints are in the debug registers now (after the observer's)
    ct_hw: bool,
    /// the observer's breakpoint addresses while it steps, and their DR6 bits
    obs: Vec<u64>,
    obs_mask: u64,
    /// DR6 of the last debug exit (the branch tick's landing, Milestone 274)
    dr6: u64,
    /// guest RAM (memory slot 0, and slot 1 above 3 GiB: `layout::ram_blocks`)
    ram: u64,
    ram_len: usize,
    /// the dirty-page log's bitmap while the log is on ([`Cpu::dirty_log`])
    dirty: Option<Vec<u64>>,
    /// x86-64-v3: the guest enables XSAVE components itself, so its XCR0 is part of the state
    /// ([`CpuState::xcr0`])
    v3: bool,
    /// the CPUs the vCPU's thread is bound to at its first run (v3 on a hybrid CPU)
    pin: Option<Vec<usize>>,
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

    /// The guest's XCR0 (`KVM_GET_XCRS`).
    fn xcr0(&self) -> u64 {
        let x: kvm_xcrs = self.get(KVM_GET_XCRS, "KVM_GET_XCRS");
        match x.xcrs[..(x.nr_xcrs as usize).min(16)]
            .iter()
            .find(|r| r.xcr == 0)
        {
            Some(r) => r.value,
            None => crate::fatal(1, "hydo-kvm: KVM_GET_XCRS gave no XCR0"),
        }
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

    /// The guest debugging of the moment: the spin observer's breakpoints first (`obs`, from
    /// DR0), then the function contracts' (Milestone 255), as execute breakpoints with their
    /// local enables and DR7's fixed bit 10. The contracts' breakpoints are in it while the
    /// observer runs to at most one address (they have DR1 to DR3 then); while it steps over
    /// two addresses or with the trap flag the engine checks the contracts' addresses at each
    /// step instead ([`crate::vmm::Vmm`]'s `ct_after_step`). Without any breakpoint debugging
    /// is off, as before contracts. Sets [`KvmCpu::ct_hw`].
    fn guest_debug(&mut self, obs: &[u64]) -> kvm_guest_debug {
        let mut d = kvm_guest_debug::default();
        let mut addrs: Vec<u64> = obs.to_vec();
        self.ct_hw = !self.tf && obs.len() <= 1 && !self.ct_bps.is_empty();
        if self.ct_hw {
            addrs.extend(self.ct_bps.iter().take(4 - obs.len()));
        }
        if !addrs.is_empty() {
            d.control = KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_USE_HW_BP;
            let mut dr7 = 0x400u64;
            for (k, &a) in addrs.iter().enumerate() {
                d.debugreg[k] = a;
                dr7 |= 1 << (2 * k);
            }
            d.debugreg[7] = dr7;
        }
        if self.tf {
            // with the branch tick's handler breakpoints (Milestone 274) the steps keep them;
            // the observer's walk steps without any (`addrs` empty), as before
            d.control = KVM_GUESTDBG_ENABLE
                | KVM_GUESTDBG_SINGLESTEP
                | if addrs.is_empty() {
                    0
                } else {
                    KVM_GUESTDBG_USE_HW_BP
                };
        }
        self.obs_mask = (1u64 << obs.len()) - 1;
        d
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
                self.dr6 = d.dr6;
                let ct_mask = if self.ct_hw { 0xf & !self.obs_mask } else { 0 };
                if (self.stepping && d.dr6 & self.obs_mask != 0) || (self.tf && d.dr6 & 0x4000 != 0)
                {
                    // the observer's stop (a contract's breakpoint at the same address too is
                    // seen by the engine's check after the step)
                    Exit::Debug
                } else if d.dr6 & ct_mask != 0 {
                    // a function contract's breakpoint (Milestone 255)
                    Exit::Contract(d.pc)
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
            if let Some(p) = &self.pin {
                if let Err(e) = bind_thread(p) {
                    crate::fatal(1, &e);
                }
            }
        }
        let e = match self.kvm_run() {
            Ok(true) => self.decode(),
            Ok(false) => Exit::Kick,
            Err(e) => Exit::Fail(e),
        };
        if !matches!(e, Exit::Kick | Exit::Contract(_)) {
            // the guest went on past the instruction a contract's stop resumed over
            self.ct_rf = None;
        }
        e
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
            xcr0: if self.v3 { self.xcr0() } else { 0 },
        }
    }

    fn host_dr6_bits(&self) -> u64 {
        crate::cpu::DR6_HOST_BITS
    }

    fn cpuid_now(&self) -> Vec<kvm_cpuid_entry2> {
        let mut c = Box::<kvm_cpuid2>::default();
        c.h.nent = CPUID_MAX as u32;
        // SAFETY: a kvm_cpuid2 with room for nent entries
        let r = unsafe { ioctl(self.fd(), KVM_GET_CPUID2, &mut *c as *mut kvm_cpuid2 as u64) };
        if r != 0 {
            return Vec::new();
        }
        c.entries[..(c.h.nent as usize).min(CPUID_MAX)].to_vec()
    }

    fn load(&mut self, s: &CpuState) {
        self.set(KVM_SET_SREGS, &s.sregs, "KVM_SET_SREGS");
        self.set(KVM_SET_REGS, &s.regs, "KVM_SET_REGS");
        if self.v3 && s.xcr0 != 0 {
            // before the XSAVE image: KVM checks XCR0 against the guest's CPUID only, and the
            // image's components against what the CPUID allows
            let mut x = kvm_xcrs {
                nr_xcrs: 1,
                ..kvm_xcrs::default()
            };
            x.xcrs[0] = kvm_xcr {
                xcr: 0,
                reserved: 0,
                value: s.xcr0,
            };
            self.set(KVM_SET_XCRS, &x, "KVM_SET_XCRS");
        }
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

    fn where_now(&self) -> hydo_hvm::machine::Where {
        let r = self.regs();
        let s = self.get::<kvm_sregs>(KVM_GET_SREGS, "KVM_GET_SREGS");
        hydo_hvm::machine::Where {
            pc: r.rip,
            kernel: s.cs.selector & 3 == 0,
            page_table: s.cr3,
            thread_pointer: s.fs.base,
            sp: r.rsp,
        }
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

    fn tsd(&self) -> bool {
        let s = self.get::<kvm_sregs>(KVM_GET_SREGS, "KVM_GET_SREGS");
        s.cr4 & (1 << 2) != 0
    }

    fn step_to(&mut self, step: Option<&crate::spin::Step>) {
        // execute breakpoints (R/W and LEN 0) from DR0, local enables, and DR7's fixed bit 10;
        // KVM puts them in the hardware while the guest runs and keeps the guest's own debug
        // registers aside. Without a step only the function contracts' breakpoints, if any
        // (none: debugging off).
        self.tf = false;
        let obs: Vec<u64> = step.map(|s| s.addrs().to_vec()).unwrap_or_default();
        let d = self.guest_debug(&obs);
        if let Some(s) = step {
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
        self.obs = obs;
        self.set(KVM_SET_GUEST_DEBUG, &d, "KVM_SET_GUEST_DEBUG");
        self.ct_set = true;
    }

    fn can_single_step(&self) -> bool {
        true
    }

    fn land_step(&mut self, bps: &[u64]) {
        // trap-flag steps with instruction breakpoints at `bps` (the branch tick's handlers)
        self.tf = true;
        self.stepping = !bps.is_empty();
        self.obs = bps.to_vec();
        let d = self.guest_debug(bps);
        self.set(KVM_SET_GUEST_DEBUG, &d, "KVM_SET_GUEST_DEBUG");
        self.ct_set = true;
    }

    fn last_dr6(&self) -> u64 {
        self.dr6
    }

    fn single_step(&mut self, on: bool) {
        // KVM sets RFLAGS.TF while the guest runs one instruction and takes the debug exception
        // (KVM_GET_REGS does not show the flag); turning the steps off takes the flag back
        // with the trap flag the function contracts' breakpoints are out: the engine checks
        // their addresses at every step
        self.tf = on;
        self.stepping = false;
        self.obs.clear();
        let d = self.guest_debug(&[]);
        self.set(KVM_SET_GUEST_DEBUG, &d, "KVM_SET_GUEST_DEBUG");
        self.ct_set = true;
    }

    fn set_contract_bps(&mut self, addrs: &[u64]) {
        let want: Vec<u64> = addrs.iter().take(3).copied().collect();
        if self.ct_bps == want && self.ct_set {
            return;
        }
        self.ct_bps = want;
        self.ct_set = false;
        if self.tf || self.obs.len() > 1 {
            // the observer's steps check the addresses themselves; its end puts them in
            self.ct_set = true;
            return;
        }
        {
            let obs = self.obs.clone();
            let d = self.guest_debug(&obs);
            // KVM refuses (EBUSY) while an exception waits to be delivered to the guest: the
            // breakpoints go in before the next run instead, which the guest's state decides
            // SAFETY: KVM_SET_GUEST_DEBUG reads a kvm_guest_debug from d
            let r = unsafe {
                ioctl(
                    self.fd(),
                    KVM_SET_GUEST_DEBUG,
                    &d as *const kvm_guest_debug as u64,
                )
            };
            self.ct_set = r == 0;
        }
    }

    fn contract_resume(&mut self, pc: u64) {
        let mut r = self.regs();
        if r.rip == pc && r.rflags & RFLAGS_RF == 0 {
            r.rflags |= RFLAGS_RF;
            self.set(KVM_SET_REGS, &r, "KVM_SET_REGS");
            self.ct_rf = Some(pc);
        }
    }

    fn contract_settle(&mut self) -> bool {
        if let Some(at) = self.ct_rf.take() {
            let mut r = self.regs();
            if r.rip == at && r.rflags & RFLAGS_RF != 0 {
                r.rflags &= !RFLAGS_RF;
                self.set(KVM_SET_REGS, &r, "KVM_SET_REGS");
                return true;
            }
        }
        false
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
        for (slot, (at, off, size)) in crate::layout::ram_blocks(self.ram_len as u64)
            .into_iter()
            .enumerate()
        {
            let region = kvm_userspace_memory_region {
                slot: slot as u32,
                flags: if on { KVM_MEM_LOG_DIRTY_PAGES } else { 0 },
                guest_phys_addr: at,
                memory_size: size,
                userspace_addr: self.ram + off,
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
        }
        self.dirty = on.then(|| vec![0u64; (self.ram_len / 4096).div_ceil(64)]);
    }

    fn take_dirty(&mut self, out: &mut Vec<u64>) {
        let Some(bm) = self.dirty.as_mut() else {
            return;
        };
        // a bitmap per slot, its pages numbered from the slot's offset in the RAM buffer
        for (slot, (_, off, size)) in crate::layout::ram_blocks(self.ram_len as u64)
            .into_iter()
            .enumerate()
        {
            let words = ((size / 4096) as usize).div_ceil(64);
            let bm = &mut bm[..words];
            let log = kvm_dirty_log {
                slot: slot as u32,
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
            let first = off / 4096;
            for (i, w) in bm.iter().enumerate() {
                let mut w = *w;
                while w != 0 {
                    let b = w.trailing_zeros() as u64;
                    out.push(first + i as u64 * 64 + b);
                    w &= w - 1;
                }
            }
        }
    }

    fn kvm_stats(&self) -> Vec<(String, u64)> {
        let mut v = stats_of(self.fd(), "");
        v.extend(stats_of(self._vm.as_raw_fd(), "vm."));
        v
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

/// KVM's statistics of a VM or vCPU file (`KVM_GET_STATS_FD`): each counter's name (after
/// `prefix`) and its first value. Empty when the kernel has no statistics files (before 5.14).
fn stats_of(fd: RawFd, prefix: &str) -> Vec<(String, u64)> {
    use std::os::unix::fs::FileExt;
    // SAFETY: KVM_GET_STATS_FD takes no argument and returns a new file descriptor
    let r = unsafe { ioctl(fd, KVM_GET_STATS_FD, 0) };
    if r < 0 {
        return Vec::new();
    }
    let f = new_fd(r);
    let mut h = [0u8; 24];
    if f.read_exact_at(&mut h, 0).is_err() {
        return Vec::new();
    }
    let w = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as u64;
    let (name_size, n, desc_off, data_off) = (w(&h, 4), w(&h, 8), w(&h, 16), w(&h, 20));
    let stride = 16 + name_size;
    let mut d = vec![0u8; (stride * n) as usize];
    if f.read_exact_at(&mut d, desc_off).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for k in 0..n as usize {
        let e = &d[k * stride as usize..(k + 1) * stride as usize];
        let off = w(e, 8);
        let name: String = e[16..]
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect();
        let mut x = [0u8; 8];
        if f.read_exact_at(&mut x, data_off + off).is_ok() {
            out.push((format!("{prefix}{name}"), u64::from_le_bytes(x)));
        }
    }
    out
}

impl Drop for KvmCpu {
    fn drop(&mut self) {
        // SAFETY: the page mapped in Kvm::fresh, not used after this
        unsafe {
            libc::munmap(self.run.cast(), self.run_size);
        }
    }
}

// ---------------- the branch tick's counter (Milestone 274) ----------------

/// `struct perf_event_attr` up to `sig_data` (`PERF_ATTR_SIZE_VER7`, Linux 5.13 and later).
#[repr(C)]
#[derive(Default)]
struct PerfAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
    config2: u64,
    branch_sample_type: u64,
    sample_regs_user: u64,
    sample_stack_user: u32,
    clockid: i32,
    sample_regs_intr: u64,
    aux_watermark: u32,
    sample_max_stack: u16,
    reserved2: u16,
    aux_sample_size: u32,
    reserved3: u32,
    sig_data: u64,
}

const PERF_TYPE_RAW: u32 = 4;
const PERF_FLAG_PINNED: u64 = 1 << 2;
const PERF_FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
const PERF_FLAG_EXCLUDE_HV: u64 = 1 << 6;
const PERF_FLAG_EXCLUDE_HOST: u64 = 1 << 19;
/// `_IOW('$', 4, __u64)`
const PERF_EVENT_IOC_PERIOD: u64 = 0x4008_2404;
const F_SETSIG: i32 = 10;
const F_SETOWN_EX: i32 = 15;
const F_OWNER_TID: i32 = 0;
/// no overflow in practice (a sampling event needs a period)
const NO_ALARM: u64 = 1 << 62;

/// The raw event that counts retired conditional branches, taken and not taken, on this host's
/// CPU, or why there is none. Intel from Ice Lake on (and the performance cores of hybrid CPUs):
/// `BR_INST_RETIRED.COND` (`0x11c4`); Skylake to Comet Lake: `BR_INST_RETIRED.CONDITIONAL`
/// (`0x01c4`). Exact counts were measured on a Xeon 6975P-C only (Milestone 274); AMD is refused:
/// its count is exact only with a setting that needs root (rr's `SpecLockMap` note), and no AMD
/// runner had a counter to measure.
pub fn branch_event() -> Result<u64, String> {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: CPUID is available on every x86_64 CPU
        let v = unsafe { std::arch::x86_64::__cpuid(0) };
        let vendor: Vec<u8> = [v.ebx, v.edx, v.ecx]
            .iter()
            .flat_map(|r| r.to_le_bytes())
            .collect();
        if vendor != b"GenuineIntel" {
            return Err(format!(
                "the branch tick (guest.branch_tick) counts on Intel CPUs only; this host's CPU is {}",
                String::from_utf8_lossy(&vendor)
            ));
        }
        // SAFETY: as above
        let l1 = unsafe { std::arch::x86_64::__cpuid(1) };
        let family = (l1.eax >> 8) & 0xf;
        let model = ((l1.eax >> 4) & 0xf) | (((l1.eax >> 16) & 0xf) << 4);
        if family != 6 {
            return Err(format!(
                "the branch tick (guest.branch_tick): no conditional-branch event known for Intel family {family}"
            ));
        }
        Ok(match model {
            0x4e | 0x5e | 0x55 | 0x8e | 0x9e | 0xa5 | 0xa6 => 0x01c4,
            _ => 0x11c4,
        })
    }
    #[cfg(not(target_arch = "x86_64"))]
    Err("the branch tick needs an x86_64 host".into())
}

/// The branch tick's counter: a `perf_event` on the calling thread that counts the guest's
/// retired conditional branches in user mode, pinned, with its overflow signal to this thread.
pub struct PerfBranches {
    fd: File,
    event: u64,
}

// SAFETY: the file is used from the engine's thread only
unsafe impl Send for PerfBranches {}

impl PerfBranches {
    /// Open the counter on the calling thread (the thread that runs the vCPUs).
    pub fn open() -> Result<PerfBranches, String> {
        let event = branch_event()?;
        install_kick_handler()?;
        unblock_kick();
        let mut a = PerfAttr {
            type_: PERF_TYPE_RAW,
            size: std::mem::size_of::<PerfAttr>() as u32,
            config: event,
            sample_period: NO_ALARM,
            flags: PERF_FLAG_PINNED
                | PERF_FLAG_EXCLUDE_KERNEL
                | PERF_FLAG_EXCLUDE_HV
                | PERF_FLAG_EXCLUDE_HOST,
            wakeup_events: 1,
            ..PerfAttr::default()
        };
        if std::path::Path::new("/sys/bus/event_source/devices/cpu_core").exists() {
            // a hybrid CPU: the performance cores' PMU (the engine binds a v3 vCPU to one kind)
            a.type_ = std::fs::read_to_string("/sys/bus/event_source/devices/cpu_core/type")
                .ok()
                .and_then(|t| t.trim().parse().ok())
                .unwrap_or(PERF_TYPE_RAW);
        }
        // SAFETY: perf_event_open with an attribute block of its stated size, on this thread
        let r = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &a as *const PerfAttr,
                0,
                -1,
                -1,
                0,
            )
        } as i32;
        if r < 0 {
            let e = std::io::Error::last_os_error();
            let p = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            return Err(format!(
                "the branch tick (guest.branch_tick) needs a performance counter of the guest's \
                 branches: perf_event_open: {e} (perf_event_paranoid {p}; it must be 2 or lower, \
                 and a virtual machine must have a virtual PMU)"
            ));
        }
        let fd = new_fd(r);
        // SAFETY: gettid has no preconditions
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
        let own = [F_OWNER_TID, tid];
        // SAFETY: fcntl on our own descriptor with the arguments each command takes
        let ok = unsafe {
            libc::fcntl(fd.as_raw_fd(), F_SETOWN_EX, own.as_ptr()) == 0
                && libc::fcntl(fd.as_raw_fd(), F_SETSIG, kick_signal()) == 0
                && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_ASYNC) == 0
        };
        if !ok {
            return Err(format!(
                "the branch tick: the counter's signal: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(PerfBranches { fd, event })
    }
}

impl crate::branches::Branches for PerfBranches {
    fn read(&mut self) -> u64 {
        let mut v = [0u8; 8];
        // SAFETY: an 8-byte read of the counter's value into v
        let n = unsafe { libc::read(self.fd.as_raw_fd(), v.as_mut_ptr().cast(), 8) };
        if n != 8 {
            crate::fatal(
                1,
                &format!(
                    "hydo-kvm: the branch tick's counter: {}",
                    std::io::Error::last_os_error()
                ),
            );
        }
        u64::from_le_bytes(v)
    }
    fn alarm(&mut self, after: Option<u64>) {
        let p: u64 = after.unwrap_or(NO_ALARM).max(1);
        // SAFETY: PERF_EVENT_IOC_PERIOD reads a u64
        let r = unsafe {
            ioctl(
                self.fd.as_raw_fd(),
                PERF_EVENT_IOC_PERIOD,
                &p as *const u64 as u64,
            )
        };
        if let Err(e) = chk("PERF_EVENT_IOC_PERIOD", r) {
            crate::fatal(1, &e);
        }
    }
    fn describe(&self) -> String {
        format!(
            "the branch tick counts raw event {:#x} (the guest's retired conditional branches in user mode)",
            self.event
        )
    }
}
