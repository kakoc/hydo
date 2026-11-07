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

/// `hydo doctor --backend kvm` (or with `HYDO_BACKEND=kvm`): the checks of the KVM engine
/// (docs/design/kvm-engine.md): x86_64 Linux, `/dev/kvm` usable with what the engine needs
/// (it opens the device and asks KVM; no VM is made), the guest kernel and the initramfs.
pub fn cmd_doctor_kvm(ctx: &Ctx) -> i32 {
    let mut c = Checks { fails: 0, warns: 0 };
    if let Some(note) = missing_release_note(ctx) {
        println!("{note}");
    }
    let (sys, mach) = uname_sys_machine();
    c.check(
        sys == "Linux" && mach == "x86_64",
        &format!("x86_64 Linux (found {sys} {mach})"),
        "the KVM engine needs x86_64 Linux",
        false,
    );
    let cpu = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let flags: Vec<&str> = cpu
        .lines()
        .find(|l| l.starts_with("flags"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, f)| f.split_whitespace().collect())
        .unwrap_or_default();
    let model = cpu
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, m)| m.trim().to_string())
        .unwrap_or_else(|| "?".into());
    println!("     CPU: {model}");
    c.check(
        flags.contains(&"vmx") || flags.contains(&"svm"),
        "hardware virtualization (vmx or svm in /proc/cpuinfo)",
        "enable VT-x or AMD-V in the firmware; in a cloud VM, choose an instance type with nested virtualization",
        false,
    );
    if flags.contains(&"hypervisor") {
        println!("     inside a VM: the KVM engine runs on nested virtualization (slower; results are this host's)");
    }
    let dev = ctx.config.kvm.dev();
    c.check(
        dev.exists(),
        &format!("{} exists", dev.display()),
        "load the KVM module of the CPU (kvm_intel or kvm_amd), or enable nested virtualization",
        false,
    );
    if dev.exists() {
        c.check(
            access(&dev, libc::R_OK | libc::W_OK),
            &format!("{} is usable by this user", dev.display()),
            "add the user to the group that owns the device (often kvm)",
            false,
        );
        match hydo_kvm::probe(&dev.display().to_string()) {
            Ok(line) => {
                c.check(true, "KVM has what the engine needs (API 12, user-space MSRs and the MSR filter, the CPUID)", "", false);
                println!("     {line}");
            }
            Err(e) => c.check(false, "KVM has what the engine needs", &e, false),
        }
    }
    let dirs = [ctx.guest(), ctx.p("build"), ctx.p("hv/kvm/linux/out")];
    let k = hydo_kvm::kernel::KERNEL;
    let want = hydo_image::FileRef {
        file: k.file.into(),
        sha256: k.sha256.into(),
    };
    let found = match &ctx.config.kvm.kernel {
        Some(p) => Some(p.clone()),
        None => hydo_image::kernels::locate(&want, hydo_image::kernels::Role::Kernel, &dirs),
    };
    c.check(
        found.is_some(),
        &format!(
            "the KVM guest kernel {} (sha256 {}){}",
            k.file,
            &k.sha256[..16],
            found
                .as_ref()
                .map(|p| format!(": {}", rel(ctx, p)))
                .unwrap_or_default()
        ),
        "build it with hv/kvm/linux/build-kernel.sh (docs/BUILD-PINS.md) and put it in guest/, or name it with HYDO_KVM_KERNEL",
        false,
    );
    c.check(
        ctx.guest().join("initramfs-kafka.cpio.gz").is_file()
            || ctx.p("build/initramfs-kafka.cpio.gz").is_file(),
        "the x86_64 initramfs (initramfs-kafka.cpio.gz)",
        "install the Linux release (guest/)",
        false,
    );
    c.summary()
}

/// The KVM engine's state in the default (hvx) doctor (Milestone 172: the Linux release ships it):
/// its program and guest kernel, and whether this machine's `/dev/kvm` can be used now. A
/// missing file is a warning (hvx runs without it); `hydo doctor --backend kvm` checks the rest.
fn kvm_state(c: &mut Checks, ctx: &Ctx) {
    let k = hydo_kvm::kernel::KERNEL;
    let want = hydo_image::FileRef {
        file: k.file.into(),
        sha256: k.sha256.into(),
    };
    let dirs = [ctx.guest(), ctx.p("build"), ctx.p("hv/kvm/linux/out")];
    let kernel = hydo_image::kernels::locate(&want, hydo_image::kernels::Role::Kernel, &dirs);
    let program = crate::ctx::access(
        &hydo_engine::kvm::program(&ctx.root, &ctx.config.kvm),
        libc::X_OK,
    );
    c.check(
        program && kernel.is_some(),
        &format!(
            "the KVM engine's files (bin/hydo-kvm, the guest kernel {} sha256 {}): `--backend kvm` runs without the kernel module",
            k.file,
            &k.sha256[..16]
        ),
        reinstall(ctx),
        true,
    );
    let dev = ctx.config.kvm.dev();
    let state = if !dev.exists() {
        format!(
            "{} is not there (no KVM module loaded; on this machine hvx owns VT-x)",
            dev.display()
        )
    } else if access(&dev, libc::R_OK | libc::W_OK) {
        format!(
            "{} is usable by you: `hydo doctor --backend kvm` checks the rest",
            dev.display()
        )
    } else {
        format!("{} is there but not usable by you", dev.display())
    };
    println!("     KVM engine: {state}");
}

pub fn cmd_doctor_linux(ctx: &Ctx) -> i32 {
    if ctx.kvm {
        return cmd_doctor_kvm(ctx);
    }
    let mut c = Checks { fails: 0, warns: 0 };
    if let Some(note) = missing_release_note(ctx) {
        println!("{note}");
    }
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
        "hydo module install (or only build it: hydo module build)",
        true,
    );
    c.check(
        mods.iter().any(|m| m == "hvx") && Path::new("/dev/hvx").exists(),
        "hvx module loaded, /dev/hvx present",
        "hydo module install",
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
        g.join("hydo-agent"),
        ctx.p("tools/bin/mke2fs"),
        g.join("store-closures.txt"),
        g.join("store").join("nix").join("store"),
    ] {
        c.check(
            p.exists(),
            &format!("present: {}", rel(ctx, &p)),
            reinstall(ctx),
            false,
        );
    }
    c.check(
        access(&ctx.p("bin/hydo-hvx"), libc::X_OK),
        "run engine present (bin/hydo-hvx; it runs inside hydo when /dev/hvx is usable)",
        "unpack the release again and run its install.sh",
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
    kvm_state(&mut c, ctx);
    installation(&mut c, ctx);
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

/// The fix for a missing installation file: unpack the release again, or, for a `hydo` installed
/// by `cargo install` (M169), fetch this version's files.
/// The first lines of `hydo doctor` for a hydo without its release files yet, built in a
/// checkout or installed by cargo: which hydo this is and the one command that gets the guest
/// kernels, engine programs and tools the later checks look for. `None` when the files are there.
pub fn missing_release_note(ctx: &Ctx) -> Option<String> {
    if !ctx.cargo_installed || ctx.root.join("SHA256SUMS").exists() {
        return None;
    }
    Some(missing_release_text(
        hydo_image::hydo_version(),
        hydo_image::hydo_commit(),
        ctx.checkout.as_deref(),
        &ctx.root,
    ))
}

fn missing_release_text(
    v: &str,
    commit: Option<&str>,
    checkout: Option<&Path>,
    root: &Path,
) -> String {
    let who = match (checkout, commit) {
        (Some(t), Some(c)) => format!("built in the checkout {} (commit {c})", t.display()),
        (Some(t), None) => format!("built in the checkout {}", t.display()),
        (None, _) => "installed by cargo".to_string(),
    };
    format!(
        "doctor: this is hydo {v}, {who}. A checkout or a cargo install has none of a release's \
         files (guest kernels, engine programs, tools), so the checks that need them fail.\n\
         doctor: get them with   hydo setup --get {v}   (the {v} release into {}; this hydo then \
         uses it), or install a release and run its bin/hydo.",
        root.display()
    )
}

fn reinstall(ctx: &Ctx) -> &'static str {
    if ctx.cargo_installed {
        "hydo setup --get (installs this version's files into the install cache)"
    } else {
        "reinstall"
    }
}

/// What `hydo setup --check` would say (M154): one line per damaged file (the fix is to unpack the
/// release again), one per step setup has not taken (the fix is `hydo setup`), or one line that
/// all matched. A development tree (no SHA256SUMS) gets a warning instead.
fn installation(c: &mut Checks, ctx: &Ctx) {
    let r = crate::setup::inspect_ctx(ctx);
    if !r.release && ctx.cargo_installed {
        let how = match &ctx.checkout {
            Some(t) => format!("hydo was built in the checkout {}", t.display()),
            None => "hydo was installed by cargo".to_string(),
        };
        c.check(
            false,
            &format!(
                "this version's release files in {} ({how})",
                ctx.root.display()
            ),
            &format!("hydo setup --get {}", hydo_image::hydo_version()),
            false,
        );
        return;
    }
    if !r.release {
        c.check(
            false,
            &format!(
                "an unpacked hydo release in {} (SHA256SUMS)",
                ctx.root.display()
            ),
            "not a release: the installation's files are not checked (`hydo setup --check` needs one)",
            true,
        );
        return;
    }
    for d in r.damaged.iter().take(10) {
        c.check(
            false,
            &format!("release file {}", d.line()),
            "unpack the release again and run its install.sh (hydo setup repairs no file)",
            false,
        );
    }
    if r.damaged.len() > 10 {
        println!("     ... and {} more damaged files", r.damaged.len() - 10);
    }
    for s in &r.pending {
        c.check(
            false,
            &format!("set up: {} is not done", s.describe()),
            "hydo setup",
            false,
        );
    }
    if r.damaged.is_empty() && r.pending.is_empty() {
        c.check(
            true,
            &format!(
                "the installation matches the release: {} files and store entries checked, {} pinned in this hydo (hydo setup --check)",
                r.checked, r.pinned
            ),
            "",
            false,
        );
    }
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
    if let Some(note) = missing_release_note(ctx) {
        println!("{note}");
    }
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
    // (no C compiler: nothing on the Mac compiles C since the property table moved into the
    // explorer, M115; codesign is part of macOS)
    // the engine hydo runs (bin/hydo-hvm, the Rust engine; it also runs `hydo build`'s mkfs VM)
    let hvmx = hydo_engine::hvm::program(&ctx.root, &ctx.config.hvm);
    for p in [
        hvmx.clone(),
        ctx.p("hv/hvm/explore/out/Image-explore"),
        ctx.guest().join("busybox"),
        ctx.guest().join("mke2fs"),
        ctx.guest().join("tc-closure.txt"),
        ctx.guest().join("hydo-agent"),
    ] {
        c.check(
            p.exists(),
            &format!("present: {}", rel(ctx, &p)),
            reinstall(ctx),
            false,
        );
    }
    for p in [
        ctx.p("hv/hvm/explore/out/Image-explore-netns"),
        ctx.guest().join("hydo_fc"),
        // the pinned aarch64 ip and iptables, in the release's store tree (HYDO_ARM64_STORE)
        ctx.guest()
            .join("tc")
            .join(hydo_build::arm64::IP_ARM64.trim_start_matches('/')),
        ctx.guest().join("tc").join(
            format!(
                "{}/bin/xtables-legacy-multi",
                hydo_build::arm64::IPTABLES_ARM64
            )
            .trim_start_matches('/'),
        ),
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
        "hydo setup",
        false,
    );
    // who signed the engine (Milestone 172): a release's Developer ID, or setup's ad-hoc signature
    if let Some((_, t)) = cmd_out("codesign", &["-dvv", &hvmx.display().to_string()]) {
        let authority = t.lines().find_map(|l| l.strip_prefix("Authority="));
        let team = t.lines().find_map(|l| l.strip_prefix("TeamIdentifier="));
        match (authority, team) {
            (Some(a), Some(team)) if a.contains(&format!("({team})")) => {
                println!("     signature: {a}")
            }
            (Some(a), Some(team)) => println!("     signature: {a} (team {team})"),
            _ if t.contains("Signature=adhoc") => {
                println!("     signature: ad hoc (signed on this Mac)")
            }
            _ => {}
        }
    }
    installation(&mut c, ctx);
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The note names the hydo, its checkout and commit, and the one command with the version.
    #[test]
    fn missing_release_note_names_the_command() {
        let t = super::missing_release_text(
            "0.1.0",
            Some("abc123"),
            Some(Path::new("/src/hydo")),
            Path::new("/h/.hydo/versions/0.1.0"),
        );
        assert!(t.contains("this is hydo 0.1.0, built in the checkout /src/hydo (commit abc123)"));
        assert!(t.contains("hydo setup --get 0.1.0"));
        assert!(t.contains("/h/.hydo/versions/0.1.0"));
        let c = super::missing_release_text("0.1.0", None, None, Path::new("/v"));
        assert!(c.contains("installed by cargo"));
    }
}
