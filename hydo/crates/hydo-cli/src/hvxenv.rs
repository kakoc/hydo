//! The Linux release's run environment: the device, the module lock and the environment every
//! subprocess gets (the Python CLI's module globals and tool_env()). The values are the
//! configuration `main` read (`hydo_config::engine::Engine`, M144).

use crate::ctx::{access, home_join, Ctx};
use hydo_config::engine::Engine;
use hydo_config::registry::{HVX_DEV, HYDO_LOCK};
use std::path::{Path, PathBuf};

pub fn base_lock(e: &Engine) -> String {
    match &e.lock {
        Some(l) => l.display().to_string(),
        None => home_join(".hvx-module.lock").display().to_string(),
    }
}

/// The device runs open (`HVX_DEV`, default /dev/hvx; another name: a side module).
pub fn dev(e: &Engine) -> String {
    e.dev().to_string()
}

pub fn devname(e: &Engine) -> String {
    Path::new(&dev(e))
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The main module shares the run lock; a side module (--name) gets its own lock file.
pub fn module_lock(e: &Engine, name: &str) -> String {
    if name == "hvx" {
        base_lock(e)
    } else {
        format!("{}.{}", base_lock(e), name)
    }
}

/// The lock a run on DEV holds (shared).
pub fn lock(e: &Engine) -> String {
    module_lock(e, &devname(e))
}

/// Some(true/false): whether this user can read and write the device; None: no device.
pub fn dev_usable_at(dev: &str) -> Option<bool> {
    let p = Path::new(dev);
    if !p.exists() {
        return None;
    }
    Some(access(p, libc::R_OK | libc::W_OK))
}

pub fn dev_usable(e: &Engine) -> Option<bool> {
    dev_usable_at(&dev(e))
}

/// The environment additions every hydo subprocess gets (tool_env()): the bundled debugfs first
/// on PATH, and the lock (and the device when it is not the default) for the wrapper `hvx-run`.
/// The explorer gets the release's paths through its configuration ([`explore_config`]).
pub fn tool_env(ctx: &Ctx) -> Vec<(String, String)> {
    let path = std::env::var("PATH").unwrap_or_default();
    let e = &ctx.config.engine;
    let mut v = vec![
        (
            "PATH".to_string(),
            format!("{}:{}", ctx.libexec().join("bin").display(), path),
        ),
        (HYDO_LOCK.to_string(), lock(e)),
    ];
    if dev(e) != hydo_config::engine::DEFAULT_DEV {
        v.push((HVX_DEV.to_string(), dev(e)));
    }
    v
}

/// The configuration an explorer started by `hydo` gets: the program's, with the release's
/// images (`guest/`), wrapper (`libexec/hvx-run`), lock file and engine program.
pub fn explore_config(ctx: &Ctx) -> hydo_config::Config {
    let mut c = ctx.config.clone();
    c.paths.build = ctx.guest();
    c.paths.hvx_run = ctx.libexec().join("hvx-run");
    c.engine.lock = Some(PathBuf::from(lock(&ctx.config.engine)));
    c.engine.program = Some(engine(ctx));
    c
}

/// The run engine's program (`bin/hydo-hvx`; `HYDO_HVX_EXPLORE` overrides it), started when the
/// engine cannot run in this process (see `hydo_engine::hvx`). A hydo built in a checkout takes
/// the `hydo-hvx` built next to it: its root is the install cache or `HYDO_RELEASE_DIR`'s
/// release tree, whose engine is another version's (Milestone 358).
pub fn engine(ctx: &Ctx) -> PathBuf {
    if ctx.config.engine.program.is_none() && ctx.checkout.is_some() {
        let exe = std::env::current_exe().and_then(|p| p.canonicalize());
        if let Some(p) = exe.ok().as_deref().and_then(sibling_engine) {
            return p;
        }
    }
    hydo_engine::hvx::program(&ctx.root, &ctx.config.engine)
}

/// The `hydo-hvx` program in the folder of the `hydo` program `exe`, when it is there.
fn sibling_engine(exe: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let p = exe.parent()?.join("hydo-hvx");
    let m = std::fs::metadata(&p).ok()?;
    (m.is_file() && m.permissions().mode() & 0o111 != 0).then_some(p)
}

pub fn bzimage(ctx: &Ctx) -> PathBuf {
    ctx.guest().join("bzImage")
}

pub fn initrd(ctx: &Ctx) -> PathBuf {
    ctx.guest().join("initramfs-kafka.cpio.gz")
}

#[cfg(test)]
mod tests {
    use super::sibling_engine;

    /// Milestone 358: a checkout's hydo takes the hydo-hvx built next to it, and nothing when
    /// that folder has none, or only a file that cannot run.
    #[test]
    fn a_checkout_build_takes_the_engine_built_next_to_it() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("hydo-hvxenv-sibling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("hydo");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(sibling_engine(&exe), None);
        let eng = d.join("hydo-hvx");
        std::fs::write(&eng, "").unwrap();
        std::fs::set_permissions(&eng, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(sibling_engine(&exe), None);
        std::fs::set_permissions(&eng, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(sibling_engine(&exe), Some(eng));
        let _ = std::fs::remove_dir_all(&d);
    }
}
