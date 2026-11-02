//! hydo-hvm: the Mac run engine in Rust (M143). It is `hv/hvm/hvm.c` (the deterministic arm64 VMM
//! on Hypervisor.framework: the exit-count clock, the GICv2, the UART, PSCI, the interrupt
//! delivery rule, tick jitter and forced preemption points), `hv/hvm/virtio*.c` (the block and
//! network devices with hvx's seeded faults), `hv/hvm/spin_observer.c` (exit-free spin loops,
//! the rewind mode) and `hv/hvm/explore/serve.c` (the explorer engine `hvmx`: S0/S1, RUN, CTL,
//! RESUME, the lock-step protocol, the controlled boot of agent images, DISKOFF/CUT, the power
//! cut's recovery boot) ported function by function, with the same command line, protocol,
//! output lines and fingerprints.
//!
//! Two ways to use it:
//!
//! - the program `hydo-hvm` (the default engine on the Mac): hvmx's command line
//!   (`hydo-hvm [--ram-mib N] [--control [boot]] serve|one <Image> <initramfs> <outdir>
//!   ["<schedule>"]`), plus hvm's (`hydo-hvm hvm run|print|branch <Image> <initrd> <markers>`);
//!   it must be signed with the hypervisor entitlement (`hv/hvm/hv.entitlements`);
//! - in-process: [`serve::main_with`] runs the engine over any streams, with its environment
//!   from a lookup function; Hypervisor.framework allows one VM per process, so one engine at a
//!   time per process, and the process itself must carry the entitlement.
//!
//! The core is callable through Rust types ([`vmm::Vmm`]: boot, run, snapshot, restore,
//! fingerprints; [`serve::Engine`]: one schedule, a lock-step stop); the text protocol is the
//! adapter in [`serve`].
//!
//! `unsafe` is confined to [`hv`] (the framework's functions) and [`mem`] (page mappings). The
//! engine runs on Apple Silicon only; elsewhere the pure parts (the device tree, the devices,
//! the machine state's layout, the hashes) build and are tested.

pub mod fdt;
pub mod fnv;
pub mod layout;
pub mod machine;
pub mod virtio;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod hv;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod hvm_main;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod mem;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod serve;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod spin;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod vmm;

/// True when this build can run guests (Apple Silicon macOS).
pub const SUPPORTED: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

use std::io::Write;
use std::sync::{Arc, Mutex};

/// Where diagnostics go (stderr in the program), shared by the engine's parts.
pub type Diag = Arc<Mutex<Box<dyn Write + Send>>>;

/// A diagnostic line.
pub fn say(d: &Diag, s: &str) {
    if let Ok(mut w) = d.lock() {
        let _ = writeln!(w, "{s}");
        let _ = w.flush();
    }
}

/// The end of the engine: the C engine's `exit(code)` after a message. Unwinds to
/// [`serve::main_with`] (or the hvm main), which writes `msg` and returns `code`, so an engine
/// thread never ends the process it runs in.
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

/// Run `f`, turning a [`fatal`] into its exit status (the message goes to `diag`).
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
