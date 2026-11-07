//! `hydo module build|install|uninstall|load|unload|status` and `hydo module udev install|remove|status`
//! (Linux). `install` (build for the running kernel when needed, then load under the exclusive
//! module lock) and `uninstall` (unload, delete the build) came with M154, when the release's
//! install.sh stopped building the module itself; it runs `hydo module install` unless --no-module.

use crate::ctx::{die, take, Ctx};
use crate::hvxenv::{dev_usable_at, module_lock};
use std::ffi::CStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const UDEV_DIR: &str = "/etc/udev/rules.d";

/// The directory that stands for `/` when hydo reads the host's kernel-module state
/// (`HYDO_CLI_HOST_ROOT`; `/` when unset). Set once by [`set_host_root`] from the configuration.
static HOST_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_host_root(root: Option<PathBuf>) {
    let _ = HOST_ROOT.set(root.unwrap_or_else(|| PathBuf::from("/")));
}

/// Where hydo reads the host file `path` (absolute): under the host root. Printed paths and
/// writes keep `path` itself; the CLI test points the root at an empty directory, so its
/// answers do not depend on which modules, devices and rules the machine running it has.
pub fn host(path: impl AsRef<Path>) -> PathBuf {
    let p = path.as_ref();
    match HOST_ROOT.get() {
        Some(r) if r != Path::new("/") => r.join(p.strip_prefix("/").unwrap_or(p)),
        _ => p.to_path_buf(),
    }
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some('a'..='z'))
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_')
}

pub fn loaded_modules() -> Vec<String> {
    std::fs::read_to_string(host("/proc/modules"))
        .map(|t| {
            t.lines()
                .filter_map(|l| l.split_whitespace().next().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn module_ko(ctx: &Ctx, name: &str) -> PathBuf {
    ctx.p("module/build").join(name).join(format!("{name}.ko"))
}

pub fn srcversion_loaded(name: &str) -> Option<String> {
    std::fs::read_to_string(host(format!("/sys/module/{name}/srcversion")))
        .ok()
        .map(|s| s.trim().to_string())
}

pub fn srcversion_file(ko: &Path) -> Option<String> {
    if !ko.is_file() {
        return None;
    }
    let out = Command::new("modinfo")
        .args(["-F", "srcversion"])
        .arg(ko)
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let mut t = String::from_utf8_lossy(&out.stdout).into_owned();
    t.push_str(&String::from_utf8_lossy(&out.stderr));
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// How many processes hold a flock on the lock file (from /proc/locks; takes no lock).
pub fn lock_holders(path: &str) -> usize {
    let Ok(st) = std::fs::metadata(path) else {
        return 0;
    };
    let (maj, min) = (dev_major(st.dev()), dev_minor(st.dev()));
    let ino = st.ino();
    let Ok(text) = std::fs::read_to_string(host("/proc/locks")) else {
        return 0;
    };
    let mut n = 0;
    for l in text.lines() {
        // r"\bFLOCK\b.*?\s([0-9a-f]+):([0-9a-f]+):(\d+)\s"
        let Some(i) = l.find("FLOCK") else { continue };
        for w in l[i..].split_whitespace() {
            let p: Vec<&str> = w.split(':').collect();
            if p.len() == 3 {
                if let (Ok(a), Ok(b), Ok(c)) = (
                    u64::from_str_radix(p[0], 16),
                    u64::from_str_radix(p[1], 16),
                    p[2].parse::<u64>(),
                ) {
                    if (a, b, c) == (maj, min, ino) {
                        n += 1;
                    }
                    break;
                }
            }
        }
    }
    n
}

fn dev_major(d: u64) -> u64 {
    ((d >> 32) & 0xffff_f000) | ((d >> 8) & 0xfff)
}

fn dev_minor(d: u64) -> u64 {
    ((d >> 12) & 0xffff_ff00) | (d & 0xff)
}

fn module_build(ctx: &Ctx, name: &str) -> i32 {
    if !is_name(name) {
        die("--name: lowercase letters, digits and '_' (it becomes the module and device name)");
    }
    let kdir = std::env::var("KDIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("/lib/modules/{}/build", uname_release()));
    if !Path::new(&kdir).is_dir() {
        die(&format!(
            "no kernel headers at {kdir} (install them, e.g. `sudo apt install linux-headers-$(uname -r)`)"
        ));
    }
    for t in ["cc", "make"] {
        if crate::ctx::which(t).is_none() {
            die(&format!(
                "building the kernel module needs a C compiler (cc) and make, and {t} is not on PATH \
                 (Debian/Ubuntu: sudo apt install build-essential)"
            ));
        }
    }
    let ko = module_ko(ctx, name);
    let bd = ko.parent().unwrap_or(Path::new(".")).to_path_buf();
    let _ = std::fs::create_dir_all(&bd);
    let src = ctx.p("module");
    if std::fs::copy(src.join("hvx.c"), bd.join(format!("{name}.c"))).is_err()
        || std::fs::copy(src.join("hvx_abi.h"), bd.join("hvx_abi.h")).is_err()
    {
        die(&format!(
            "cannot copy the module sources from {}",
            src.display()
        ));
    }
    let mut mk =
        format!("# generated by hydo module build (from hv/hvx/Makefile)\nobj-m := {name}.o\n");
    if name != "hvx" {
        mk.push_str(&format!("ccflags-y := -DHVX_DEVNAME='\"{name}\"'\n"));
    }
    mk.push_str(&format!("OBJECT_FILES_NON_STANDARD_{name}.o := y\n"));
    let _ = std::fs::write(bd.join("Makefile"), mk);
    let (pr, pw) = std::io::pipe().unwrap_or_else(|e| die(&e.to_string()));
    let child = Command::new("make")
        .arg("-C")
        .arg(&kdir)
        .arg(format!("M={}", bd.display()))
        .arg("modules")
        .stdout(pw.try_clone().unwrap_or_else(|e| die(&e.to_string())))
        .stderr(pw)
        .spawn();
    let mut text = String::new();
    let ok = match child {
        Ok(mut c) => {
            use std::io::Read;
            let mut r = pr;
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            text = String::from_utf8_lossy(&b).into_owned();
            c.wait().map(|s| s.success()).unwrap_or(false)
        }
        Err(_) => false,
    };
    if !ok || !ko.is_file() {
        eprint!("{}", crate::pyjson::tail(&text, 4000));
        die(&format!("building {name}.ko failed (output above)"));
    }
    println!(
        "hydo: built {} (srcversion {})",
        ko.display(),
        srcversion_file(&ko).unwrap_or_else(|| "None".into())
    );
    0
}

fn flock_x(lk: &str, script: &str) -> Option<i32> {
    Command::new("flock")
        .args(["-x", lk, "sh", "-c", script])
        .status()
        .ok()
        .and_then(|s| s.code())
}

fn module_load(ctx: &Ctx, name: &str, dry: bool, params: &[String]) -> i32 {
    let ko = module_ko(ctx, name);
    if !ko.is_file() && !dry {
        module_build(ctx, name);
    }
    let mods = loaded_modules();
    let has = |m: &str| mods.iter().any(|x| x == m);
    let mut steps: Vec<String> = Vec::new();
    if name == "hvx" {
        if has("hvx") {
            steps.push("sudo rmmod hvx".into());
        }
        if has("kvm_intel") {
            steps.push("sudo rmmod kvm_intel".into());
        }
        if has("kvm") {
            steps.push("sudo rmmod kvm".into());
        }
    } else {
        if has("kvm_intel") {
            die("kvm_intel is loaded; a side module cannot run VMs while KVM owns VT-x (load the main module first)");
        }
        if has(name) {
            steps.push(format!("sudo rmmod {name}"));
        }
    }
    let p: String = params.iter().map(|p| format!(" {p}")).collect();
    steps.push(format!("sudo insmod {}{}", ko.display(), p));
    steps.push(format!("test -c /dev/{name}"));
    if host(udev_rule_path(name)).exists() {
        steps.push("sudo udevadm settle".into());
    }
    let lk = module_lock(&ctx.config.engine, name);
    let script = steps.join(" && ");
    println!(
        "hydo: {} lock {} exclusively (waits for running VMs), then: {}",
        if dry { "would take" } else { "taking" },
        lk,
        script
    );
    if dry {
        return 0;
    }
    let rc = flock_x(&lk, &script).unwrap_or(-1);
    if rc != 0 {
        die(&format!("loading {name} failed (rc {rc})"));
    }
    println!(
        "hydo: {} loaded, /dev/{} ready (srcversion {})",
        name,
        name,
        srcversion_loaded(name).unwrap_or_else(|| "None".into())
    );
    0
}

fn module_unload(ctx: &Ctx, name: &str, dry: bool) -> i32 {
    let lk = module_lock(&ctx.config.engine, name);
    if !loaded_modules().iter().any(|m| m == name) {
        println!("hydo: {name} is not loaded");
        return 0;
    }
    let script = format!("sudo rmmod {name}");
    println!(
        "hydo: {} lock {} exclusively (waits for running VMs), then: {}",
        if dry { "would take" } else { "taking" },
        lk,
        script
    );
    if dry {
        return 0;
    }
    if flock_x(&lk, &script) != Some(0) {
        die(&format!("unloading {name} failed"));
    }
    if name == "hvx" {
        println!("hydo: hvx unloaded (`sudo modprobe kvm_intel` gives VT-x back to KVM)");
    }
    0
}

/// Why the module must be built (again) before it is loaded: None when this release's build is
/// current (built from the release's sources for the running kernel).
fn build_needed(ctx: &Ctx, name: &str) -> Option<String> {
    let ko = module_ko(ctx, name);
    if !ko.is_file() {
        return Some("not built yet".into());
    }
    let bd = ko.parent().unwrap_or(Path::new("."));
    let src = ctx.p("module");
    for (from, to) in [
        ("hvx.c".to_string(), format!("{name}.c")),
        ("hvx_abi.h".to_string(), "hvx_abi.h".to_string()),
    ] {
        if std::fs::read(src.join(&from)).ok() != std::fs::read(bd.join(&to)).ok() {
            return Some(format!(
                "module/{from} differs from the copy it was built from"
            ));
        }
    }
    let running = uname_release();
    let vermagic = Command::new("modinfo")
        .args(["-F", "vermagic"])
        .arg(&ko)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    match vermagic.split_whitespace().next() {
        Some(k) if k == running => None,
        Some(k) => Some(format!("built for kernel {k}, this machine runs {running}")),
        None => Some("its kernel version cannot be read (modinfo)".into()),
    }
}

/// `hydo module install`: build the module for the running kernel when needed, then load it
/// under the exclusive module lock (`hydo module load`). Nothing happens when this release's
/// build is loaded already, so it can run again safely.
fn module_install(ctx: &Ctx, name: &str, dry: bool, params: &[String]) -> i32 {
    if !is_name(name) {
        die("--name: lowercase letters, digits and '_' (it becomes the module and device name)");
    }
    if let Some(why) = build_needed(ctx, name) {
        println!(
            "hydo: {} {name}.ko ({why})",
            if dry { "would build" } else { "building" }
        );
        if !dry {
            module_build(ctx, name);
        }
    }
    let built = srcversion_file(&module_ko(ctx, name));
    let live = srcversion_loaded(name);
    if loaded_modules().iter().any(|m| m == name) && built.is_some() && built == live {
        println!(
            "hydo: {name} is installed: loaded from this release's build (srcversion {}), /dev/{name} {}",
            live.unwrap_or_default(),
            if host(format!("/dev/{name}")).exists() {
                "present"
            } else {
                "missing"
            }
        );
        return 0;
    }
    module_load(ctx, name, dry, params)
}

/// `hydo module uninstall`: unload the module (under the exclusive lock) and delete this
/// release's build of it. The udev rule, if any, stays (`hydo module udev remove`).
fn module_uninstall(ctx: &Ctx, name: &str, dry: bool) -> i32 {
    if !is_name(name) {
        die("--name: lowercase letters, digits and '_'");
    }
    let rc = module_unload(ctx, name, dry);
    if rc != 0 {
        return rc;
    }
    let bd = ctx.p("module/build").join(name);
    if bd.exists() {
        println!(
            "hydo: {} {}",
            if dry { "would delete" } else { "deleting" },
            bd.display()
        );
        if !dry {
            if let Err(e) = std::fs::remove_dir_all(&bd) {
                die(&format!("cannot delete {}: {e}", bd.display()));
            }
        }
    }
    if host(udev_rule_path(name)).exists() {
        println!(
            "hydo: the udev rule {} stays (`hydo module udev remove{}` deletes it)",
            udev_rule_path(name).display(),
            if name == "hvx" {
                String::new()
            } else {
                format!(" --name {name}")
            }
        );
    }
    0
}

fn module_status(ctx: &Ctx, name: &str) -> i32 {
    let mods = loaded_modules();
    let ko = module_ko(ctx, name);
    let (built, live) = (srcversion_file(&ko), srcversion_loaded(name));
    let loaded = mods.iter().any(|m| m == name);
    println!(
        "module {}: {}",
        name,
        if loaded {
            format!(
                "loaded (srcversion {})",
                live.clone().unwrap_or_else(|| "None".into())
            )
        } else {
            "not loaded".into()
        }
    );
    println!(
        "  release build: {}",
        match &built {
            Some(b) => format!("{} (srcversion {})", ko.display(), b),
            None => "not built (hydo module build)".into(),
        }
    );
    if let (Some(b), Some(l)) = (&built, &live) {
        println!(
            "  {}",
            if b == l {
                "same source as this release"
            } else {
                "DIFFERENT source from this release (the engine and the module may disagree on the ioctl ABI)"
            }
        );
    }
    let dev = format!("/dev/{name}");
    if host(&dev).exists() {
        println!("  {}", dev_line(&dev));
    }
    println!(
        "  kvm_intel: {}",
        if mods.iter().any(|m| m == "kvm_intel") {
            "LOADED (must be unloaded for hvx; `hydo module load` does it)"
        } else {
            "not loaded"
        }
    );
    let lk = module_lock(&ctx.config.engine, name);
    println!("  lock {}: {} holder(s) now", lk, lock_holders(&lk));
    0
}

pub fn group_name(gid: u32) -> String {
    // SAFETY: getgrgid returns a pointer into static storage or NULL; we copy the name at once.
    unsafe {
        let g = libc::getgrgid(gid);
        if g.is_null() {
            return gid.to_string();
        }
        CStr::from_ptr((*g).gr_name).to_string_lossy().into_owned()
    }
}

/// (gid, member names) of a group by name.
fn group_by_name(name: &str) -> Option<(u32, Vec<String>)> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: as above; the member list is a NULL-terminated array of C strings.
    unsafe {
        let g = libc::getgrnam(c.as_ptr());
        if g.is_null() {
            return None;
        }
        let mut mem = Vec::new();
        let mut p = (*g).gr_mem;
        while !p.is_null() && !(*p).is_null() {
            mem.push(CStr::from_ptr(*p).to_string_lossy().into_owned());
            p = p.add(1);
        }
        Some(((*g).gr_gid, mem))
    }
}

fn user_gid(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: getpwnam returns static storage or NULL.
    unsafe {
        let p = libc::getpwnam(c.as_ptr());
        (!p.is_null()).then(|| (*p).pw_gid)
    }
}

pub fn user_name(uid: u32) -> Option<String> {
    // SAFETY: getpwuid returns static storage or NULL.
    unsafe {
        let p = libc::getpwuid(uid);
        (!p.is_null()).then(|| CStr::from_ptr((*p).pw_name).to_string_lossy().into_owned())
    }
}

fn in_group(gid: u32) -> bool {
    // SAFETY: getgid has no side effects; getgroups fills a buffer we own.
    unsafe {
        if libc::getgid() == gid {
            return true;
        }
        let n = libc::getgroups(0, std::ptr::null_mut());
        if n <= 0 {
            return false;
        }
        let mut v = vec![0 as libc::gid_t; n as usize];
        let n = libc::getgroups(n, v.as_mut_ptr());
        v.truncate(n.max(0) as usize);
        v.contains(&gid)
    }
}

/// One line about who can open the device: mode, group, and whether runs need sudo.
pub fn dev_line(dev: &str) -> String {
    let Ok(st) = std::fs::metadata(host(dev)) else {
        return format!("{dev}: cannot stat");
    };
    let g = group_name(st.gid());
    let mode = st.permissions().mode();
    let who = if dev_usable_at(&host(dev).to_string_lossy()) == Some(true) {
        "you can open it: runs need no sudo".to_string()
    } else if mode & 0o060 == 0o060 && !in_group(st.gid()) {
        format!(
            "you are not in group {g}: log out and in again (or `newgrp {g}`) after being added"
        )
    } else {
        "only root can open it: runs use sudo (`hydo module udev install` changes that)".to_string()
    };
    let owner = if st.uid() == 0 {
        user_name(0).unwrap_or_else(|| "root".into())
    } else {
        st.uid().to_string()
    };
    format!(
        "{}: mode {:o}, owner {}, group {}; {}",
        dev,
        mode & 0o777,
        owner,
        g,
        who
    )
}

pub fn udev_rule_path(name: &str) -> PathBuf {
    Path::new(UDEV_DIR).join(format!("70-hydo-{name}.rules"))
}

fn udev_rule_text(name: &str, group: &str, mode: &str) -> String {
    format!(
        "# Installed by `hydo module udev install`; `hydo module udev remove` deletes it.\n\
         # /dev/{name} (the hvx kernel module) belongs to group {group} with mode {mode}, so members of the group\n\
         # run hydo guests without sudo. Anyone in the group can run guests through the module: it\n\
         # runs in the kernel, takes a CPU and up to 3 GiB of memory per guest, and is a test tool that\n\
         # was not hardened against hostile users. Give the group only to people you would give sudo.\n\
         KERNEL==\"{name}\", SUBSYSTEM==\"misc\", GROUP=\"{group}\", MODE=\"{mode}\"\n"
    )
}

fn invoking_user() -> String {
    if let Ok(u) = std::env::var("SUDO_USER") {
        if !u.is_empty() {
            return u;
        }
    }
    // getpass.getuser(): LOGNAME, USER, LNAME, USERNAME, then the password database
    for k in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Ok(u) = std::env::var(k) {
            if !u.is_empty() {
                return u;
            }
        }
    }
    user_name(crate::ctx::getuid()).unwrap_or_default()
}

fn is_group_name(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some('a'..='z') | Some('_'))
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_' || x == '-')
}

fn is_mode(s: &str) -> bool {
    // r"0?6[0-7]0"
    let s = s.strip_prefix('0').filter(|r| r.len() == 3).unwrap_or(s);
    let b = s.as_bytes();
    b.len() == 3 && b[0] == b'6' && (b'0'..=b'7').contains(&b[1]) && b[2] == b'0'
}

fn module_udev(verb: &str, name: &str, group: &str, mode: &str, dry: bool, adduser: bool) -> i32 {
    if !is_name(name) {
        die("--name: lowercase letters, digits and '_'");
    }
    if !is_group_name(group) {
        die("--group: a group name (lowercase letters, digits, '_' and '-')");
    }
    if !is_mode(mode) {
        die("--mode: 0660 (group read/write) or similar; the owner stays root");
    }
    let mode = if mode.starts_with('0') {
        mode.to_string()
    } else {
        format!("0{mode}")
    };
    let rule = udev_rule_path(name);
    let dev = format!("/dev/{name}");
    if verb == "status" {
        if host(&rule).exists() {
            let text = std::fs::read_to_string(host(&rule)).unwrap_or_default();
            let line = text.lines().find(|l| l.starts_with("KERNEL=="));
            println!(
                "udev rule {}: {}",
                rule.display(),
                line.map(|l| l.trim().to_string())
                    .unwrap_or_else(|| "present".into())
            );
        } else {
            println!(
                "udev rule {}: not installed (runs on {} use sudo unless you can open it)",
                rule.display(),
                dev
            );
        }
        println!(
            "  {}",
            if host(&dev).exists() {
                dev_line(&dev)
            } else {
                format!("{dev}: not present (module {name} not loaded)")
            }
        );
        return 0;
    }
    let mut steps: Vec<(String, Vec<String>)> = Vec::new();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut tmp: Option<PathBuf> = None;
    let mut member = true;
    let user = invoking_user();
    if verb == "install" {
        let g = group_by_name(group);
        if g.is_none() {
            steps.push((
                "create the group".into(),
                s(&["sudo", "groupadd", "--system", group]),
            ));
        }
        let t = std::env::temp_dir().join(format!("hydo-udev-{}.rules", std::process::id()));
        let _ = std::fs::write(&t, udev_rule_text(name, group, &mode));
        let _ = std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o644));
        steps.push((
            "install the rule".into(),
            s(&[
                "sudo",
                "install",
                "-m",
                "0644",
                "-o",
                "root",
                "-g",
                "root",
                &t.display().to_string(),
                &rule.display().to_string(),
            ]),
        ));
        tmp = Some(t);
        steps.push((
            "reload the udev rules".into(),
            s(&["sudo", "udevadm", "control", "--reload-rules"]),
        ));
        if host(&dev).exists() {
            steps.push((
                format!("apply it to {dev}"),
                s(&[
                    "sudo",
                    "udevadm",
                    "trigger",
                    "--action=change",
                    "--subsystem-match=misc",
                    &format!("--sysname-match={name}"),
                ]),
            ));
            steps.push(("wait for udev".into(), s(&["sudo", "udevadm", "settle"])));
        }
        member = match &g {
            Some((gid, mem)) => mem.contains(&user) || user_gid(&user) == Some(*gid),
            None => false,
        };
        if adduser && !member {
            steps.push((
                format!("add {user} to the group"),
                s(&["sudo", "usermod", "-aG", group, &user]),
            ));
        }
    } else {
        if !host(&rule).exists() {
            println!("hydo: no udev rule {}; nothing to remove", rule.display());
            return 0;
        }
        steps.push((
            "remove the rule".into(),
            s(&["sudo", "rm", "-f", &rule.display().to_string()]),
        ));
        steps.push((
            "reload the udev rules".into(),
            s(&["sudo", "udevadm", "control", "--reload-rules"]),
        ));
        if host(&dev).exists() {
            steps.push((
                format!("give {dev} back to root"),
                s(&["sudo", "chown", "root:root", &dev]),
            ));
            steps.push(("mode 0600".into(), s(&["sudo", "chmod", "0600", &dev])));
        }
    }
    for (what, argv) in &steps {
        println!(
            "hydo: {}{}: {}",
            if dry { "would " } else { "" },
            what,
            argv.join(" ")
        );
        if !dry
            && !Command::new(&argv[0])
                .args(&argv[1..])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        {
            die(&format!("{what} failed"));
        }
    }
    if verb == "install" {
        if let Some(t) = tmp {
            let _ = std::fs::remove_file(t);
        }
        if dry {
            return 0;
        }
        if host(&dev).exists() {
            println!("hydo: {}", dev_line(&dev));
        } else {
            println!(
                "hydo: {} will belong to group {} (mode {}) from the next `hydo module load{}`.",
                dev,
                group,
                mode,
                if name == "hvx" {
                    String::new()
                } else {
                    format!(" --name {name}")
                }
            );
        }
        println!(
            "hydo: anyone in group {group} can run guests through the hvx module without sudo. The module runs\n      \
             in the kernel and is a test tool not hardened against hostile users: give the group only\n      \
             to people you would give sudo. Loading and unloading the module still need sudo."
        );
        if adduser && !member {
            println!(
                "hydo: {user} was added to group {group}; log out and in again (or run `newgrp {group}`) before runs can use it."
            );
        } else if !adduser && !member {
            println!("hydo: {user} is not in group {group} (sudo usermod -aG {group} {user})");
        }
    } else if !dry {
        println!(
            "hydo: rule removed; {} is root's again (mode 0600). Group {} and its members were not changed{}.",
            dev,
            group,
            if group == "hydo" {
                " (`sudo groupdel hydo` removes the group that `install` created, if nothing else uses it)"
            } else {
                ""
            }
        );
    }
    0
}

pub fn cmd_module(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.is_empty()
        || ![
            "build",
            "install",
            "uninstall",
            "load",
            "unload",
            "status",
            "udev",
        ]
        .contains(&args[0].as_str())
    {
        die("usage: hydo module build|install|uninstall|load|unload|status [--name NAME] [--dry-run] [--param KEY=VALUE ...]\n       \
             hydo module udev install|remove|status [--name NAME] [--group GROUP] [--mode 0660] [--no-adduser] [--dry-run]");
    }
    let verb = args.remove(0);
    if verb == "udev" {
        if args.is_empty() || !["install", "remove", "status"].contains(&args[0].as_str()) {
            die("usage: hydo module udev install|remove|status [--name NAME] [--group GROUP] [--mode 0660] [--no-adduser] [--dry-run]");
        }
        let uverb = args.remove(0);
        let name = take(&mut args, "--name").unwrap_or_else(|| "hvx".into());
        let group = take(&mut args, "--group").unwrap_or_else(|| "hydo".into());
        let mode = take(&mut args, "--mode").unwrap_or_else(|| "0660".into());
        let dry = args.iter().any(|a| a == "--dry-run");
        let adduser = !args.iter().any(|a| a == "--no-adduser");
        args.retain(|a| a != "--dry-run" && a != "--no-adduser");
        if !args.is_empty() {
            die(&format!("unexpected arguments: {}", args.join(" ")));
        }
        return module_udev(&uverb, &name, &group, &mode, dry, adduser);
    }
    let name = take(&mut args, "--name").unwrap_or_else(|| "hvx".into());
    let dry = args.iter().any(|a| a == "--dry-run");
    let mut params = Vec::new();
    while let Some(p) = take(&mut args, "--param") {
        params.push(p);
    }
    args.retain(|a| a != "--dry-run");
    if !args.is_empty() {
        die(&format!("unexpected arguments: {}", args.join(" ")));
    }
    match verb.as_str() {
        "build" => module_build(ctx, &name),
        "install" => module_install(ctx, &name, dry, &params),
        "uninstall" => module_uninstall(ctx, &name, dry),
        "load" => module_load(ctx, &name, dry, &params),
        "unload" => module_unload(ctx, &name, dry),
        _ => module_status(ctx, &name),
    }
}

pub fn uname_release() -> String {
    // SAFETY: uname fills a struct we own.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return String::new();
        }
        CStr::from_ptr(u.release.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

pub fn uname_sys_machine() -> (String, String) {
    // SAFETY: as above.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return (String::new(), String::new());
        }
        (
            CStr::from_ptr(u.sysname.as_ptr())
                .to_string_lossy()
                .into_owned(),
            CStr::from_ptr(u.machine.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }
}
