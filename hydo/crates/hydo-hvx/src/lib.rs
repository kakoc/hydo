//! hydo-hvx: the hvx run engine in Rust (M132). It drives `/dev/hvx` (the hvx kernel module,
//! `hv/hvx/hvx.c`) directly through the ioctls of `hv/hvx/hvx_abi.h` and does everything the C
//! engine runner `hv/hvx/hvx_explore` does: boot an image to its snapshot points S0 and S1, serve
//! schedules from S1 (every host token: `disk=`/`dseed=`, `tj*`, `pp=`/`xt*`, `cf=`, `rb=`,
//! `q=`/`qs=`/`qb=`/`qm=`), several vCPUs, `--cpu v3`, the RAM size, the power cut's recovery VM
//! from S0, `BRANCH`, the fingerprints `fp`/`wfp`, the lock-step control protocol (M118) and the
//! controlled boot of agent images (M121, M126), with the same output lines.
//!
//! Two ways to use it:
//!
//! - the program `hydo-hvx`, with hvx_explore's command line and protocol (a drop-in for the C
//!   program: `HYDO_HVX_EXPLORE=.../hydo-hvx` makes the explorer start it);
//! - in-process: [`engine::main_with`] runs the same engine over any streams; hydo-engine's
//!   `HYDO_ENGINE=inproc` runs one per worker on a thread of the `hydo` process itself.
//!
//! What differs inside, never in a result: snapshots stay in this process's memory as their
//! non-zero 4 KiB blocks ([`pages`]); a fresh VM gets only the non-zero blocks of the base image
//! and of the snapshot restored into it (the module's pages start zeroed); fingerprints skip
//! all-zero pages exactly ([`fnv::fnv_zskip`]); and an old VM is torn down on a helper thread
//! while the next run starts. Coverage tracing (`HVX_EXPLORE_PT_*`) is not ported.
//!
//! The module and its ABI are Linux-only; on other systems the pure parts (ABI types, the boot
//! recipe, fingerprints, schedule parsing) build and are tested, and the engine is not compiled.

pub mod abi;
pub mod boot;
pub mod cstr;
pub mod ctl;
pub mod fnv;
pub mod pages;
pub mod sched;

#[cfg(target_os = "linux")]
pub mod engine;
#[cfg(target_os = "linux")]
pub mod vm;

/// True when this build can drive `/dev/hvx` (Linux).
pub const SUPPORTED: bool = cfg!(target_os = "linux");
