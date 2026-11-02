//! `hydo doctor`: check this machine and the installation (both platforms). Each line is `ok`,
//! `WARN` (works, with a limit the fix explains) or `FAIL` (does not work until fixed); the last
//! line sums them up, and the exit status is 1 when a check failed. Nothing needs Python (M107).

use crate::ctx::{access, which, Ctx};
use crate::hvxenv::{dev, dev_usable, devname, lock};
use crate::module::{
    dev_line, loaded_modules, lock_holders, module_ko, srcversion_file, srcversion_loaded,
    uname_release, uname_sys_machine,
};
use std::path::Path;
use std::process::{Command, Stdio};

struct Checks {
    fails: usize,
    warns: usize,
}

impl Checks {
    /// The summary line and the exit status.
    fn summary(&self) -> i32 {
        println!(
            "doctor: {}",
            if self.fails == 0 && self.warns == 0 {
                "all checks passed".to_string()
            } else {
                format!("{} failed, {} warnings", self.fails, self.warns)
            }
        );
        i32::from(self.fails > 0)
    }

    fn check(&mut self, cond: bool, what: &str, fix: &str, warn: bool) {
        let tag = if cond {
            "ok  "
        } else if warn {
            "WARN"
        } else {
            "FAIL"
        };
        if cond {
            println!("{tag} {what}");
        } else {
            println!("{tag} {what}  -> {fix}");
            if warn {
                self.warns += 1;
            } else {
                self.fails += 1;
            }
        }
    }
}

fn rel(ctx: &Ctx, p: &Path) -> String {
    p.strip_prefix(&ctx.root).unwrap_or(p).display().to_string()
}

pub fn cmd_doctor_linux(ctx: &Ctx) -> i32 {
    let mut c = Checks { fails: 0, warns: 0 };
    let (sys, mach) = uname_sys_machine();
    c.check(
        sys == "Linux" && mach == "x86_64",
        &format!("x86_64 Linux (found {sys} {mach})"),
        "hydo's Linux backend needs x86_64 Linux (Apple Silicon: the Mac release)",
        false,
    );
    let cpu = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let flags: Vec<&str> = cpu
        .lines()
        .find(|l| l.starts_with("flags"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, f)| f.split_whitespace().collect())
        .unwrap_or_default();
    let has = |f: &str| flags.contains(&f);
    let model = cpu
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, m)| m.trim().to_string())
        .unwrap_or_else(|| "?".into());
    println!("     CPU: {model}");
    c.check(
        cpu.contains("GenuineIntel"),
        "an Intel CPU (hvx is a VT-x hypervisor)",
        "AMD (SVM) is not supported",
        false,
    );
    c.check(
        has("vmx"),
        "VT-x present and enabled in firmware (vmx in /proc/cpuinfo)",
        "enable Intel VT-x in the BIOS/UEFI settings (the kernel hides vmx when firmware disables it)",
        false,
    );
    c.check(
        !has("hypervisor"),
        "bare metal (not inside a VM)",
        "inside a VM hvx needs nested VT-x, runs much slower and has no PMU: the exit-count clock is used",
        true,
    );
    let hybrid = Path::new("/sys/devices/cpu_atom").is_dir() || has("hybrid_cpu");
    let pmu = has("arch_perfmon") && !hybrid && !has("hypervisor");
    c.check(
        pmu,
        "a usable PMU (non-hybrid CPU with arch_perfmon): the PMU branch clock is used",
        &format!(
            "{}: hvx falls back to the exit-count clock -- runs stay deterministic, but fingerprints differ \
             from ones recorded with the PMU clock and a guest that computes without exits can stall",
            if hybrid { "hybrid P-core/E-core CPU" } else { "no architectural PMU" }
        ),
        true,
    );
    for (t, why) in [
        ("cc", "builds the kernel module"),
        ("make", "builds the kernel module"),
        ("flock", "the module lock, util-linux"),
        ("xz", "unpacks guest kernel modules for network faults"),
        (
            "sudo",
            "loads the module; runs use it only if you cannot open the device",
        ),
        ("modinfo", "module checks, kmod"),
    ] {
        c.check(
            which(t).is_some(),
            &format!("{t} ({why})"),
            &format!("install {t}"),
            false,
        );
    }
    let sudo_ok = which("sudo").is_some()
        && Command::new("sudo")
            .args(["-n", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    let e = &ctx.config.engine;
    let d = dev(e);
    let usable = dev_usable(e).map(|u| u && !e.sudo);
    if usable.is_some() && Path::new(&d).exists() {
        println!("     {}", dev_line(&d));
    }
    if usable == Some(true) {
        c.check(
            true,
            &format!("{d} can be opened by you: runs do not use sudo, and the run engine runs inside hydo"),
            "",
            false,
        );
        c.check(
            sudo_ok,
            "sudo without a password prompt (only module load/unload need it)",
            "`hydo module load/unload` will ask for a password",
            true,
        );
    } else {
        c.check(
            sudo_ok,
            &format!("runs use sudo (you cannot open {d} yourself: the run engine runs as bin/hydo-hvx under sudo), and sudo asks no password"),
            &format!(
                "a password prompt can block the explorer's workers: `hydo module udev install` (once, with \
                 sudo) gives {d} to a group, after which runs need no sudo -- see README"
            ),
            true,
        );
    }
    let rel_ = uname_release();
    let kdir = format!("/lib/modules/{rel_}/build");
    c.check(
        Path::new(&kdir).is_dir(),
        &format!("kernel headers for {rel_}"),
        "install them (Debian/Ubuntu: sudo apt install linux-headers-$(uname -r))",
        false,
    );
    let lockdown = std::fs::read_to_string("/sys/kernel/security/lockdown").unwrap_or_default();
    c.check(
        lockdown.is_empty() || lockdown.contains("[none]"),
        "kernel lockdown off (an unsigned module can be loaded)",
        &format!(
            "Secure Boot lockdown ({}): sign hvx.ko with an enrolled key or disable Secure Boot",
            lockdown.trim()
        ),
        true,
    );
    let mods = loaded_modules();
    let (built, live) = (
        srcversion_file(&module_ko(ctx, "hvx")),
        srcversion_loaded("hvx"),
    );
    c.check(
        built.is_some(),
        "hvx.ko built by this release",
        "hydo module build",
        true,
    );
    c.check(
        mods.iter().any(|m| m == "hvx") && Path::new("/dev/hvx").exists(),
        "hvx module loaded, /dev/hvx present",
        "hydo module load",
        false,
    );
    if d != "/dev/hvx" {
        c.check(
            Path::new(&d).exists(),
            &format!("{d} present (HVX_DEV: runs use this side module)"),
            &format!("hydo module load --name {}", devname(e)),
            false,
        );
    }
    if let (Some(b), Some(l)) = (&built, &live) {
        c.check(
            b == l,
            &format!("the loaded hvx is this release's source (srcversion {l})"),
            &format!(
                "the loaded module (srcversion {l}) is not this release's ({b}): run `hydo module load` \
                 when no one else is using it"
            ),
            false,
        );
    }
    c.check(
        !mods.iter().any(|m| m == "kvm_intel"),
        "kvm_intel not loaded (KVM and hvx cannot both own VT-x)",
        "`hydo module load` unloads kvm and kvm_intel (stop KVM virtual machines first)",
        false,
    );
    let lk = lock(e);
    println!(
        "     module lock: {} ({} holder(s) now)",
        lk,
        lock_holders(&lk)
    );
    let g = ctx.guest();
    for p in [
        g.join("bzImage"),
        g.join("initramfs-kafka.cpio.gz"),
        g.join("busybox"),
        g.join("hvx_seedrng"),
        ctx.p("tools/bin/mke2fs"),
        g.join("store-closures.txt"),
        g.join("store").join("nix").join("store"),
    ] {
        c.check(
            p.exists(),
            &format!("present: {}", rel(ctx, &p)),
            "reinstall",
            false,
        );
    }
    c.check(
        access(&ctx.p("bin/hydo-hvx"), libc::X_OK),
        "run engine present (bin/hydo-hvx; it runs inside hydo when /dev/hvx is usable)",
        "run install.sh again",
        false,
    );
    let out = Command::new(ctx.libexec().join("faketime"))
        .arg("2020-09-13 16:26:40")
        .arg(ctx.p("tools/bin/mke2fs"))
        .arg("-V")
        .output();
    // a release without its libexec/faketime (or mke2fs) is reported here, not a crash (M107)
    let (text, fix) = match out {
        Ok(o) => {
            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            let fix = format!("output: {}", crate::pyjson::head(t.trim(), 200));
            (t, fix)
        }
        Err(e) => (
            String::new(),
            format!(
                "cannot run {}: {e} (reinstall)",
                rel(ctx, &ctx.libexec().join("faketime"))
            ),
        ),
    };
    c.check(
        text.contains("1.47.0"),
        "bundled mke2fs runs (under the frozen clock)",
        &fix,
        false,
    );
    let cwd = std::env::current_dir().unwrap_or_default();
    // (until M144 also the snapshot directory, which the deleted C engine wrote)
    if let Some(free) = free_gib(&cwd) {
        c.check(
            free >= 2.0,
            &format!(
                "{:.0} GiB free in {} (2 GiB wanted: images)",
                free,
                cwd.display()
            ),
            "free some space",
            true,
        );
    }
    c.summary()
}

fn free_gib(p: &Path) -> Option<f64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills a struct we own.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return None;
        }
        Some(s.f_bavail as f64 * s.f_frsize as f64 / (1u64 << 30) as f64)
    }
}

fn cmd_out(cmd: &str, args: &[&str]) -> Option<(bool, String)> {
    let o = Command::new(cmd)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
    t.push_str(&String::from_utf8_lossy(&o.stderr));
    Some((o.status.success(), t))
}

pub fn cmd_doctor_mac(ctx: &Ctx) -> i32 {
    let mut c = Checks { fails: 0, warns: 0 };
    let (sys, mach) = uname_sys_machine();
    c.check(
        sys == "Darwin" && mach == "arm64",
        "Apple Silicon Mac",
        "hydo needs an M1 or later Mac",
        false,
    );
    let ver = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let v: Vec<u32> = ver
        .split('.')
        .take(2)
        .filter_map(|x| x.parse().ok())
        .collect();
    c.check(
        v.first().copied().unwrap_or(0) >= 13,
        &format!("macOS 13 or later (found {ver})"),
        "update macOS",
        false,
    );
    let cc = Command::new("xcrun")
        .args(["--find", "cc"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    c.check(
        cc,
        "a C compiler (the explorer compiles its property-table tool)",
        "xcode-select --install",
        false,
    );
    // the engine hydo runs (M143: bin/hydo-hvm, the Rust engine; HYDO_HVM=c: the C hvmx)
    let hvmx = hydo_engine::hvm::program(&ctx.root);
    for p in [
        hvmx.clone(),
        ctx.p("hv/hvm/hvm"),
        ctx.p("hv/hvm/explore/out/Image-explore"),
        ctx.guest().join("busybox"),
        ctx.guest().join("mke2fs"),
        ctx.guest().join("tc-closure.txt"),
    ] {
        c.check(
            p.exists(),
            &format!("present: {}", rel(ctx, &p)),
            "reinstall",
            false,
        );
    }
    for p in [
        ctx.p("hv/hvm/explore/out/Image-explore-netns"),
        ctx.guest().join("hydo_fc"),
    ] {
        c.check(
            p.exists(),
            &format!(
                "present: {} (service networks and the freeze)",
                rel(ctx, &p)
            ),
            "not in this installation: manifests with `network: bridge`, `addr:` or `faults: [freeze]` \
             cannot be built or run; everything else works",
            true,
        );
    }
    let hv = Command::new("sysctl")
        .args(["-n", "kern.hv_support"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    c.check(
        hv == "1",
        &format!("Hypervisor.framework available (kern.hv_support={hv})"),
        "not supported on this machine",
        false,
    );
    let sig = cmd_out(
        "codesign",
        &["-d", "--entitlements", "-", &hvmx.display().to_string()],
    )
    .map(|(_, t)| t)
    .unwrap_or_default();
    c.check(
        sig.contains("com.apple.security.hypervisor"),
        &format!(
            "{} signed with the hypervisor entitlement",
            hvmx.file_name().unwrap_or_default().to_string_lossy()
        ),
        "run install.sh again",
        false,
    );
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Some(free) = free_gib(&cwd) {
        c.check(
            free >= 2.0,
            &format!(
                "{:.0} GiB free in {} (2 GiB wanted: images)",
                free,
                cwd.display()
            ),
            "free some space",
            true,
        );
    }
    c.summary()
}
