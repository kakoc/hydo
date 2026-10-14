//! The Linux boot path: a self-contained KVM machine that boots a real x86-64
//! kernel to a serial console. It deliberately does NOT touch the deterministic
//! engine — it uses KVM's in-kernel irqchip + PIT and the host clock. Its only
//! job is to get an ordinary kernel running; making this path deterministic is a
//! separate, later milestone.

use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use kvm_bindings::{kvm_pit_config, kvm_userspace_memory_region, KVM_MAX_CPUID_ENTRIES};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd, VmFd};

use crate::boot::{self, LinuxBootOpts};
use crate::memory::GuestMemory;
use crate::uart::{Uart, COM1_IRQ};

// Same low-4G scratch pages firecracker/kvmtool hand KVM for the Intel identity
// map and TSS (outside guest RAM; KVM keeps them internally).
const KVM_TSS_ADDRESS: u64 = 0xfffb_d000;
const KVM_IDENTITY_MAP_ADDRESS: u64 = 0xfffb_c000;

pub struct LinuxVm {
    #[allow(dead_code)]
    vm: VmFd,
    vcpu: VcpuFd,
    mem: GuestMemory,
    #[allow(dead_code)]
    kvm: Kvm,
}

impl LinuxVm {
    /// Build a VM configured for a real kernel: in-kernel irqchip + PIT, host
    /// CPUID, identity/TSS pages, guest RAM, and one vCPU.
    pub fn new(mem_size: usize) -> io::Result<Self> {
        let kvm = Kvm::new()?;
        let vm = kvm.create_vm()?;

        // These must be set before the vCPU is created.
        vm.set_identity_map_address(KVM_IDENTITY_MAP_ADDRESS)?;
        vm.set_tss_address(KVM_TSS_ADDRESS as usize)?;

        // In-kernel PIC/IOAPIC (and per-vCPU LAPIC) + PIT. The irqchip must exist
        // before the vCPU so that vCPU gets an in-kernel local APIC.
        vm.create_irq_chip()?;
        let pit = kvm_pit_config::default();
        vm.create_pit2(pit)?;

        // Guest RAM at GPA 0.
        let mem = GuestMemory::new(mem_size)?;
        let region = kvm_userspace_memory_region {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem.size() as u64,
            userspace_addr: mem.host_addr(),
        };
        // SAFETY: region describes our live, stable mapping owned by `mem`.
        unsafe { vm.set_user_memory_region(region)? };

        let vcpu = vm.create_vcpu(0)?;

        // Hand the vCPU the host's supported CPUID (includes the KVM paravirt
        // signature leaves, so the guest uses kvm-clock and skips fragile timer
        // calibration). Real kernels read CPUID and hang without it.
        let cpuid = kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?;
        vcpu.set_cpuid2(&cpuid)?;

        Ok(LinuxVm {
            vm,
            vcpu,
            mem,
            kvm,
        })
    }

    /// Load the kernel/initramfs/boot_params and set the vCPU to enter 64-bit
    /// long mode at the kernel entry point with RSI = boot_params.
    pub fn load_and_setup(&mut self, opts: &LinuxBootOpts) -> io::Result<()> {
        let info = boot::load_linux(&mut self.mem, opts)?;
        self.setup_long_mode(info.entry, info.boot_params)?;
        Ok(())
    }

    /// 64-bit long-mode entry: identity-map the low 1 GiB with 2 MiB pages, a
    /// flat GDT (CS=0x08 code64, DS=0x10 data — the firecracker/kvmtool layout),
    /// long-mode control registers, RIP=entry, RSI=boot_params, interrupts off.
    fn setup_long_mode(&mut self, entry: u64, boot_params: u64) -> io::Result<()> {
        const PML4_ADDR: u64 = 0x1000;
        const PDPT_ADDR: u64 = 0x2000;
        const PD_ADDR: u64 = 0x3000;
        const GDT_ADDR: u64 = 0x4000;
        const PTE_PRESENT: u64 = 1 << 0;
        const PTE_RW: u64 = 1 << 1;
        const PTE_PS: u64 = 1 << 7;
        const CR0_PE: u64 = 1 << 0;
        const CR0_PG: u64 = 1 << 31;
        const CR4_PAE: u64 = 1 << 5;
        const EFER_LME: u64 = 1 << 8;
        const EFER_LMA: u64 = 1 << 10;

        self.write_u64(PML4_ADDR, PDPT_ADDR | PTE_PRESENT | PTE_RW);
        self.write_u64(PDPT_ADDR, PD_ADDR | PTE_PRESENT | PTE_RW);
        for i in 0..512u64 {
            self.write_u64(PD_ADDR + i * 8, (i * 0x20_0000) | PTE_PRESENT | PTE_RW | PTE_PS);
        }

        self.write_u64(GDT_ADDR, 0);
        self.write_u64(GDT_ADDR + 8, 0x00AF_9A00_0000_FFFF); // code64
        self.write_u64(GDT_ADDR + 16, 0x00CF_9200_0000_FFFF); // data

        let mut sregs = self.vcpu.get_sregs()?;
        sregs.cs = seg(0x08, 0xb, true);
        let d = seg(0x10, 0x3, false);
        sregs.ds = d;
        sregs.es = d;
        sregs.fs = d;
        sregs.gs = d;
        sregs.ss = d;
        sregs.gdt.base = GDT_ADDR;
        sregs.gdt.limit = 3 * 8 - 1;
        sregs.cr3 = PML4_ADDR;
        sregs.cr4 |= CR4_PAE;
        sregs.cr0 |= CR0_PE | CR0_PG;
        sregs.efer |= EFER_LME | EFER_LMA;
        self.vcpu.set_sregs(&sregs)?;

        let mut regs = self.vcpu.get_regs()?;
        regs.rip = entry;
        regs.rsi = boot_params;
        regs.rsp = 0x8000;
        regs.rflags = 0x2; // reserved bit set, interrupts disabled
        self.vcpu.set_regs(&regs)?;
        Ok(())
    }

    fn write_u64(&mut self, gpa: u64, val: u64) {
        self.mem.write(gpa, &val.to_le_bytes());
    }

    /// Run the kernel, streaming ttyS0 output to stdout and feeding stdin to the
    /// guest UART. Returns when the guest triple-faults, halts, or `max_exits` is
    /// reached (a watchdog against a silent hang).
    pub fn run(&mut self, max_exits: u64) -> io::Result<()> {
        let mut uart = Uart::new();
        let mut flushed = 0usize;
        let stdout = io::stdout();
        let mut exits: u64 = 0;

        // Best-effort raw non-blocking stdin so keystrokes reach the guest shell.
        let stdin_fd = io::stdin().as_raw_fd();
        set_nonblocking(stdin_fd);

        // When the guest idles it HLTs and KVM_RUN blocks in-kernel (in-kernel
        // irqchip delivers timer ticks without returning to us), so we would never
        // poll stdin. A periodic SIGALRM with a no-SA_RESTART handler makes KVM_RUN
        // return EINTR ~50x/sec so keystrokes reach the guest console.
        install_wakeup_timer();

        loop {
            exits += 1;
            if exits > max_exits {
                flush_uart(&uart, &mut flushed, &stdout)?;
                eprintln!("\n[hydo] watchdog: {max_exits} VM exits reached, stopping");
                break;
            }

            // Pump any waiting stdin bytes into the UART receive queue.
            let mut buf = [0u8; 64];
            let n = unsafe {
                libc::read(stdin_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
            if n > 0 {
                for &b in &buf[..n as usize] {
                    uart.push_rx(b);
                }
            }

            // Classify the exit into an owned decision, so the VcpuExit borrow of
            // self.vcpu is dropped before we touch self.vm / stdout below.
            let mut stop: Option<&'static str> = None;
            match self.vcpu.run() {
                Ok(VcpuExit::IoOut(port, data)) => {
                    if Uart::handles(port) {
                        uart.io_out(port, data);
                    }
                    // Everything else (0x80 delay port, POST codes, ...) is ignored.
                }
                Ok(VcpuExit::IoIn(port, data)) => {
                    if Uart::handles(port) {
                        uart.io_in(port, data);
                    } else {
                        port_in_default(port, data);
                    }
                }
                Ok(VcpuExit::MmioRead(_addr, data)) => {
                    for b in data.iter_mut() {
                        *b = 0;
                    }
                }
                Ok(VcpuExit::MmioWrite(_, _)) => {}
                // With the in-kernel irqchip, HLT normally blocks in-kernel; a
                // userspace HLT exit means the guest halted for good.
                Ok(VcpuExit::Hlt) => stop = Some("guest HLT"),
                Ok(VcpuExit::Shutdown) => stop = Some("guest triple-faulted / shutdown"),
                Ok(_) => stop = Some("unexpected exit"),
                Err(e) if e.errno() == libc::EINTR || e.errno() == libc::EAGAIN => {}
                Err(e) => {
                    flush_uart(&uart, &mut flushed, &stdout)?;
                    return Err(e.into());
                }
            }

            flush_uart(&uart, &mut flushed, &stdout)?;

            if let Some(reason) = stop {
                eprintln!("\n[hydo] {reason}");
                break;
            }

            // Drive the COM1 IRQ line to match the UART's pending state.
            let want = uart.irq_pending();
            if want != uart.irq_level {
                self.vm.set_irq_line(COM1_IRQ, want)?;
                uart.irq_level = want;
            }
        }
        Ok(())
    }
}

fn flush_uart(uart: &Uart, flushed: &mut usize, stdout: &io::Stdout) -> io::Result<()> {
    if uart.out.len() > *flushed {
        let mut lock = stdout.lock();
        lock.write_all(&uart.out[*flushed..])?;
        lock.flush()?;
        *flushed = uart.out.len();
    }
    Ok(())
}

extern "C" fn wakeup_handler(_sig: libc::c_int) {}

/// Install a no-op SIGALRM handler (no SA_RESTART) and arm a 20 ms repeating
/// timer, so a HLTed guest's KVM_RUN returns EINTR periodically and we can pump
/// stdin into the guest console.
fn install_wakeup_timer() {
    // SAFETY: installing a trivial signal handler and a standard interval timer.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = wakeup_handler as usize;
        sa.sa_flags = 0; // deliberately NOT SA_RESTART
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGALRM, &sa, std::ptr::null_mut());

        let it = libc::itimerval {
            it_interval: libc::timeval { tv_sec: 0, tv_usec: 20_000 },
            it_value: libc::timeval { tv_sec: 0, tv_usec: 20_000 },
        };
        libc::setitimer(libc::ITIMER_REAL, &it, std::ptr::null_mut());
    }
}

fn set_nonblocking(fd: i32) {
    // SAFETY: fcntl on a valid fd with standard flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// Default reads for ports we do not emulate. Most return 0xff (open bus); the
/// PIT/PIC/serial live in-kernel or in the UART. Port 0x61 toggles its refresh
/// bit so any BIOS-style timing loop that watches it makes progress.
fn port_in_default(port: u16, data: &mut [u8]) {
    use std::sync::atomic::{AtomicU8, Ordering};
    static P61: AtomicU8 = AtomicU8::new(0);
    let val = match port {
        0x61 => {
            let v = P61.fetch_add(0x10, Ordering::Relaxed);
            v & 0x30
        }
        _ => 0xff,
    };
    if let Some(b) = data.first_mut() {
        *b = val;
    }
    for b in data.iter_mut().skip(1) {
        *b = 0;
    }
}

fn seg(selector: u16, type_: u8, code: bool) -> kvm_bindings::kvm_segment {
    kvm_bindings::kvm_segment {
        base: 0,
        limit: 0xffff_ffff,
        selector,
        type_,
        present: 1,
        dpl: 0,
        db: if code { 0 } else { 1 },
        s: 1,
        l: if code { 1 } else { 0 },
        g: 1,
        avl: 0,
        unusable: 0,
        padding: 0,
    }
}

/// Boot a Linux kernel to a serial console.
pub fn boot_linux(
    kernel: &Path,
    initramfs: Option<&Path>,
    cmdline: &str,
    mem_size: usize,
    max_exits: u64,
) -> io::Result<()> {
    let mut vm = LinuxVm::new(mem_size)?;
    let opts = LinuxBootOpts {
        kernel,
        initramfs,
        cmdline,
        mem_size,
    };
    vm.load_and_setup(&opts)?;
    eprintln!(
        "[hydo] booting {} (mem {} MiB){}",
        kernel.display(),
        mem_size / (1024 * 1024),
        match initramfs {
            Some(p) => format!(", initramfs {}", p.display()),
            None => String::new(),
        }
    );
    eprintln!("[hydo] cmdline: {cmdline}");
    eprintln!("[hydo] ---- serial console ----");
    vm.run(max_exits)?;
    Ok(())
}
