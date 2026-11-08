#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! hydo-hvx: the hvx run engine in Rust (M132; the only hvx engine since M137, which deleted the C
//! engine runner `hv/hvx/hvx_explore.c` it was ported from: `git show d9202e7:hv/hvx/hvx_explore.c`).
//! It drives `/dev/hvx` (the hvx kernel module, `hv/hvx/hvx.c`) directly through the ioctls of
//! `hv/hvx/hvx_abi.h` and does everything the C engine did: boot an image to its snapshot points S0 and S1, serve
//! schedules from S1 (every host token: `disk=`/`dseed=`, `tj*`, `pp=`/`xt*`, `cf=`, `rb=`,
//! `q=`/`qs=`/`qb=`/`qm=`), several vCPUs, `--cpu v3`, the RAM size, the power cut's recovery VM
//! from S0, `BRANCH`, the fingerprints `fp`/`wfp`, the lock-step control protocol (M118) and the
//! controlled boot of agent images (M121, M126), with the same output lines.
//!
//! Two ways to use it:
//!
//! - in-process (the default): `engine::run` runs the engine from a typed `engine::Start`
//!   and talks to the host through a [`port::Port`] in values (`hydo-engine-proto`, M142);
//!   hydo-engine runs one per worker on a thread of the `hydo` process itself, over a
//!   [`port::ChanPort`], whenever that process can open the device (`HYDO_ENGINE=inproc` forces
//!   it). The engine reads no schedule text: [`sched::Sched::from_schedule`] arms the module from
//!   hydo-schedule's typed schedule;
//! - the program `hydo-hvx`, with the C engine's command line and protocol (`engine::main_with`
//!   reads the command line, [`text::TextPort`] turns the lines into the same values), which hydo-engine
//!   starts under `hvx-run sudo` when the process cannot open the device (`HYDO_ENGINE=process`
//!   forces it; `HYDO_HVX_EXPLORE` names another program).
//!
//! What differs inside, never in a result: snapshots stay in this process's memory as their
//! non-zero 4 KiB blocks ([`pages`]); a fresh VM gets only the non-zero blocks of the base image
//! and of the snapshot restored into it (the module's pages start zeroed); fingerprints skip
//! all-zero pages exactly ([`fnv::fnv_zskip`]); and an old VM is torn down on a helper thread
//! while the next run starts. Coverage tracing (`HVX_EXPLORE_PT_*`, [`pt`]) is ported since M137;
//! its decoder needs libipt and is built with the cargo feature `pt` only.
//!
//! The module and its ABI are Linux-only; on other systems the pure parts (ABI types, the boot
//! recipe, fingerprints, schedule parsing) build and are tested, and the engine is not compiled.

pub mod abi;
pub mod boot;
pub mod cstr;
pub mod ctl;
pub mod fnv;
pub mod pages;
pub mod port;
pub mod pt;
pub mod sched;
pub mod text;

#[cfg(target_os = "linux")]
pub mod engine;
#[cfg(target_os = "linux")]
pub mod vm;

/// True when this build can drive `/dev/hvx` (Linux).
pub const SUPPORTED: bool = cfg!(target_os = "linux");
