//! The Linux release's run environment: the device, the module lock, the snapshot directory and
//! the environment every subprocess gets (the Python CLI's module globals and tool_env()).

use crate::ctx::{access, getuid, home_join, Ctx};
use std::path::{Path, PathBuf};

pub fn base_lock() -> String {
    std::env::var("HYDO_LOCK")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| home_join(".hvx-module.lock").display().to_string())
}

/// The device runs open (`$HVX_DEV`, default /dev/hvx; another name: a side module).
pub fn dev() -> String {
    std::env::var("HVX_DEV")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/dev/hvx".into())
}

pub fn devname() -> String {
    Path::new(&dev())
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The main module shares the run lock; a side module (--name) gets its own lock file.
pub fn module_lock(name: &str) -> String {
    if name == "hvx" {
        base_lock()
    } else {
        format!("{}.{}", base_lock(), name)
    }
}

/// The lock a run on DEV holds (shared).
pub fn lock() -> String {
    module_lock(&devname())
}

/// Some(true/false): whether this user can read and write the device; None: no device.
pub fn dev_usable_at(dev: &str) -> Option<bool> {
    let p = Path::new(dev);
    if !p.exists() {
        return None;
    }
    Some(access(p, libc::R_OK | libc::W_OK))
}

pub fn dev_usable() -> Option<bool> {
    dev_usable_at(&dev())
}

pub fn uses_sudo() -> bool {
    std::env::var("HYDO_SUDO").as_deref() == Ok("1") || dev_usable() != Some(true)
}

/// Where runs keep their snapshots (no effect on any result).
pub fn snap_dir() -> String {
    if let Ok(d) = std::env::var("HYDO_SNAP_DIR") {
        if !d.is_empty() {
            return d;
        }
    }
    let d = "/var/tmp/hvx-snap";
    if !uses_sudo() && Path::new(d).is_dir() && !access(Path::new(d), libc::W_OK | libc::X_OK) {
        return format!("/var/tmp/hvx-snap-{}", getuid());
    }
    d.into()
}

/// The environment additions every hydo subprocess gets (tool_env()): the bundled debugfs first
/// on PATH, the release paths for explore.py, the lock, the snapshot directory, HYDO_CLI=1.
pub fn tool_env(ctx: &Ctx) -> Vec<(String, String)> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut e = vec![
        (
            "PATH".to_string(),
            format!("{}:{}", ctx.libexec().join("bin").display(), path),
        ),
        ("HYDO_BUILD".to_string(), ctx.guest().display().to_string()),
        (
            "HYDO_HVX_RUN".to_string(),
            ctx.libexec().join("hvx-run").display().to_string(),
        ),
        ("HYDO_LOCK".to_string(), lock()),
        ("HYDO_SNAP_DIR".to_string(), snap_dir()),
    ];
    if dev() != "/dev/hvx" {
        e.push(("HVX_DEV".to_string(), dev()));
    }
    e.push(("HYDO_CLI".to_string(), "1".to_string()));
    e
}

/// The run engine's program (`bin/hydo-hvx`; `$HYDO_HVX_EXPLORE` overrides it), started when the
/// engine cannot run in this process (see `hydo_engine::hvx`).
pub fn engine(ctx: &Ctx) -> PathBuf {
    hydo_engine::hvx::program(&ctx.root)
}

pub fn bzimage(ctx: &Ctx) -> PathBuf {
    ctx.guest().join("bzImage")
}

pub fn initrd(ctx: &Ctx) -> PathBuf {
    ctx.guest().join("initramfs-kafka.cpio.gz")
}
