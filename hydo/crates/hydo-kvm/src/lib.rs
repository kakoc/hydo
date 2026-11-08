//! hydo-kvm: the third run engine (docs/design/kvm-engine.md). It runs hydo's x86_64 agent images
//! deterministically on stock Linux KVM (`/dev/kvm`), so hydo runs where the hvx kernel module
//! cannot be loaded: cloud VMs and CI runners with nested virtualization, and AMD machines. It is
//! built like the Mac engine (hydo-hvm) and makes the Mac engine's trade-offs:
//!
//! - **the clock counts VM exits** that reach the engine, as on the Mac ([`vmm`]); a patched guest
//!   kernel (hv/kvm/linux/hydo-kvm-guest.patch) takes all of its time from that clock and never
//!   reads the TSC;
//! - **every interrupt is the engine's decision at such an exit**: no in-kernel interrupt
//!   controller, an 8259 PIC emulated by the engine ([`pic`]), injection only where the vCPU takes
//!   it before its next instruction, an interrupt-window exit otherwise;
//! - **nothing of the host reaches the guest**: a fixed CPUID ([`cpuid`]), a fixed MSR model with
//!   KVM's filter in front of every MSR KVM would answer from the host ([`msr`]), KVM's
//!   paravirtual features and PMU hidden;
//! - **the devices are the Mac engine's**: its virtio-mmio block device with hvx's seeded disk
//!   faults and its network device (hydo-hvm's `virtio`, unchanged), its 16550 ([`uart`]); the
//!   lock-step protocol is the Mac engine's ([`serve`]), so the host controller, agent images and
//!   the explorer work unchanged;
//! - **one vCPU** (several need an instruction-exact clock, which KVM does not give), and
//!   **exit-free loops**: a provable exact cycle (a thread spinning on a flag) becomes idle, at
//!   a point that does not depend on when it was noticed ([`spin`], the Mac engine's rule);
//!   anything else that runs without exits ends the run as a SPIN.
//!
//! Its fingerprints are its own (`hydo-kvm-v1`): not comparable with hvx's or hvm's.
//!
//! Two ways to use it: in-process ([`serve::run`] over a [`hydo_hvx::port::Port`], which
//! hydo-engine does on a thread of its own process; KVM allows several VMs per process), or the
//! program `hydo-kvm` (the Mac engine's command line, `serve::main_with`).
//!
//! `unsafe` is confined to `sys` (the ioctls and the `kvm_run` page) and [`mem`] (guest RAM).
//! The engine core is generic over the vCPU ([`cpu::Cpu`]), so everything but `sys` builds and
//! is tested on any system, with the scripted vCPU of [`script`].

// the port keeps the Mac engine's loops' shape (indices shared by several arrays)
#![allow(clippy::needless_range_loop)]

pub mod abi;
pub mod boot;
pub mod cpu;
pub mod cpuid;
pub mod hybrid;
pub mod kernel;
pub mod layout;
pub mod machine;
pub mod mem;
pub mod msr;
pub mod pic;
pub mod script;
pub mod serve;
pub mod spin;
pub mod uart;
pub mod vmm;

#[cfg(target_os = "linux")]
pub mod sys;

/// The hashes (hydo-hvm's: FNV-1a and the word-wise mix over RAM).
pub mod fnv {
    pub use hydo_hvm::fnv::{fnv, fnv_u64, mix64, OFFSET, PRIME};
}

/// True when this build can run guests (Linux with KVM; x86_64 hosts).
pub const SUPPORTED: bool = cfg!(all(target_os = "linux", target_arch = "x86_64"));

use std::io::Write;
use std::sync::{Arc, Mutex};

/// Where diagnostics go (stderr in the program).
pub type Diag = Arc<Mutex<Box<dyn Write + Send>>>;

pub fn say(d: &Diag, s: &str) {
    if let Ok(mut w) = d.lock() {
        let _ = writeln!(w, "{s}");
        let _ = w.flush();
    }
}

/// The end of the engine: an exit status after a message, unwound to [`catch_fatal`] so an
/// engine thread never ends the process it runs in.
pub struct Fatal {
    pub code: i32,
    pub msg: String,
}

pub fn fatal(code: i32, msg: &str) -> ! {
    std::panic::resume_unwind(Box::new(Fatal {
        code,
        msg: msg.to_string(),
    }))
}

pub fn catch_fatal(diag: &Diag, f: impl FnOnce() -> i32) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(rc) => rc,
        Err(p) => match p.downcast::<Fatal>() {
            Ok(f) => {
                if !f.msg.is_empty() {
                    say(diag, &f.msg);
                }
                f.code
            }
            Err(p) => std::panic::resume_unwind(p),
        },
    }
}

/// Whether `/dev/kvm` (or `dev`) can be used here: Ok with the host's line, or why not. Opens the
/// device and checks the API, the capabilities and the CPUID; creates no VM.
pub fn probe(dev: &str) -> Result<String, String> {
    probe_level(dev, cpuid::Level::V2)
}

/// As [`probe`], for guests at `level` (`guest.cpu: v3` needs the host's AVX2 and its XSAVE).
pub fn probe_level(dev: &str, level: cpuid::Level) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        use cpu::Hypervisor;
        sys::Kvm::open(dev)
            .and_then(|k| k.with_level(level))
            .map(|k| k.describe())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (dev, level);
        Err("the KVM engine needs Linux (/dev/kvm)".into())
    }
}
