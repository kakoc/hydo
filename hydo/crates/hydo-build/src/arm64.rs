//! `--arch arm64`: the manifest compiled for the Mac VMM (hv/hvm/). Port of
//! hydo_build_arm64.py, with the Mac release's bundled aarch64 tc closure (hydo_bridge.py's
//! `use_bundled_tc`). Output, in the build dir:
//!     <NAME>-arm64.cpio.gz    the root, as one initramfs
//!     <NAME>-arm64-disk.img   the data disk for the virtio-blk device (HVM_DISK): an MBR with a
//!                             1 MiB placeholder p1 and the journaled ext4 data partition p2
//! The root tree is the x86 image's, with two differences: the whole root is the initramfs and a
//! small /init waits for /dev/vda2; tc is the pinned aarch64 iproute2 with exactly the shared
//! libraries its loader opens. A manifest with network: bridge, an addr: or the freeze fault boots
//! the second kernel, Image-explore-netns, and says so in /etc/hydo/kernel.

use crate::elfdeps::Elfdeps;
use crate::fsx::{self, io_fail, is_link, lexists};
use crate::image::{self, MkfsTools};
use crate::pyv::{self, section};
use crate::shell::{self, ShellTools};
use crate::x86::{self, is_exec, put_exec, run_ok};
use crate::{outln, tool, Ctx, Fail, Io, Prepared, Release};
use hydo_manifest::py;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BUSYBOX_ARM64: &str = "/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1/bin/busybox";
pub const TC_ARM64: &str = "/nix/store/qmhi9c99dpgxyfg6cipf8xbmcvsr1vx2-iproute2-6.8.0/bin/tc";
pub const NETNS_KERNEL: &str = "Image-explore-netns";
pub const NETNS_KERNEL_SHA256: &str =
    "806807e7be5f55dae342f796ccc6e20c35514b3684a8c0d4633901a8ac0aa609";
pub const IP_ARM64: &str = "/nix/store/qmhi9c99dpgxyfg6cipf8xbmcvsr1vx2-iproute2-6.8.0/bin/ip";
pub const IPTABLES_ARM64: &str = "/nix/store/y6j6xf1bfshzhdw7hip3swpgd6fqfs3b-iptables-1.8.10";
const IPT_FILES: &[&str] = &[
    "bin/xtables-legacy-multi",
    "lib/xtables/libxt_NFQUEUE.so",
    "lib/xtables/libxt_tcp.so",
    "lib/xtables/libxt_standard.so",
];
const LISTER: &str = "/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox";
const FIXED_APPLETS: &str = "sh ash cat echo grep egrep sed awk tr cut sort uniq wc head tail sleep date timeout pgrep pkill kill \
mount umount mkdir rm cp mv ln ls test [ [[ true false printf env expr seq tee touch sync usleep \
ifconfig ip wget nc httpd md5sum sha256sum xargs find basename dirname uname id hostname dd";

/// The Mac release's bundled aarch64 tc: guest/tc/<absolute paths> and guest/tc-closure.txt.
struct BundledTc {
    top: PathBuf,
    listing: Vec<String>,
}

impl BundledTc {
    fn new(ctx: &Ctx) -> Result<Option<BundledTc>, Fail> {
        let Release::Mac { root } = &ctx.release else {
            return Ok(None);
        };
        let guest = root.join("guest");
        let t = crate::read_text(&guest.join("tc-closure.txt"))?;
        Ok(Some(BundledTc {
            top: guest.join("tc"),
            listing: t
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
        }))
    }

    fn here(&self, p: &str) -> PathBuf {
        self.top.join(p.trim_start_matches('/'))
    }

    /// hydo_build_arm64's view of `os.path.isfile` in the Mac release.
    fn isfile(b: &Option<BundledTc>, p: &str) -> bool {
        if let Some(b) = b {
            if p.starts_with("/nix/store/") && lexists(&b.here(p)) {
                return b.here(p).is_file();
            }
        }
        Path::new(p).is_file()
    }
}

/// Copy guest path `p` from `src` into the root: a link as a link, a file with mode 755 when
/// the source is executable, else 644.
fn copy_one(src: &Path, dst: &Path) -> Result<(), Fail> {
    if lexists(dst) {
        return Ok(());
    }
    if let Some(par) = dst.parent() {
        fsx::makedirs(par)?;
    }
    if is_link(src) {
        let t = std::fs::read_link(src).map_err(|e| io_fail("cannot read link", src, e))?;
        std::os::unix::fs::symlink(t, dst).map_err(|e| io_fail("cannot link", dst, e))?;
    } else {
        fsx::copy_file(src, dst)?;
        fsx::chmod(dst, if is_exec(src) { 0o755 } else { 0o644 })?;
    }
    Ok(())
}

/// `_copy_elf_closure(binary, R)`: binary and every file its loader opens, at their own paths.
fn copy_elf_closure(bt: &Option<BundledTc>, binary: &str, r: &Path) -> Result<(), Fail> {
    let mut all: Vec<String> = match bt {
        Some(b) => b.listing.clone(),
        None => Elfdeps::new("")
            .closure(&[binary.to_string()])
            .map_err(Fail::Exit)?,
    };
    all.push(binary.to_string());
    all.sort();
    all.dedup();
    for p in all {
        let src = match bt {
            Some(b) => b.here(&p),
            None => PathBuf::from(&p),
        };
        copy_one(&src, &x86::under(r, &p))?;
    }
    Ok(())
}

/// `_copy_store_files(paths, R)`: store files and every file their loaders open, from
/// `store` (`HYDO_ARM64_STORE`: a directory holding nix/store/...; unset = the host's /nix/store).
fn copy_store_files(
    paths: &[String],
    r: &Path,
    store: &hydo_config::build::Input,
) -> Result<(), Fail> {
    let root = match store.nonempty() {
        Some(v) => crate::abspath(Path::new(&v))
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string(),
        None => String::new(),
    };
    for p in paths {
        if !lexists(Path::new(&format!("{root}{p}"))) {
            let under = if root.is_empty() {
                String::new()
            } else {
                format!(" under {root}")
            };
            return Err(Fail::Exit(format!(
                "hydo-build: {p} not found{under} (the aarch64 store paths: hydo-nix-fetch --closure <dir> <path> (hydo/crates/hydo-tools), then HYDO_ARM64_STORE=<dir>)"
            )));
        }
    }
    let mut all = Elfdeps::new(&root).closure(paths).map_err(Fail::Exit)?;
    all.extend(paths.iter().cloned());
    all.sort();
    all.dedup();
    for p in all {
        copy_one(Path::new(&format!("{root}{p}")), &x86::under(r, &p))?;
    }
    Ok(())
}

/// `_aarch64_tool(env, default)`: an existing aarch64 ELF executable (e_machine 0xb7).
fn aarch64_tool(
    bt: &Option<BundledTc>,
    input: &hydo_config::build::Input,
    default: &str,
) -> Result<String, Fail> {
    let env = input.var;
    let p = input.or(default);
    if !BundledTc::isfile(bt, &p) {
        return Err(Fail::Exit(format!(
            "hydo-build: {env} not found at {p} (set {env})"
        )));
    }
    let mut hdr = Vec::new();
    if let Ok(f) = std::fs::File::open(&p) {
        use std::io::Read;
        let _ = f.take(20).read_to_end(&mut hdr);
    }
    if hdr.len() < 20 || &hdr[..4] != b"\x7fELF" || hdr[18..20] != [0xb7, 0] {
        return Err(Fail::Exit(format!(
            "hydo-build: {env}={p} is not an aarch64 Linux binary (build it with GOARCH=arm64)"
        )));
    }
    Ok(p)
}

/// `needs_netns_kernel(m)`.
pub fn needs_netns_kernel(m: &serde_json::Value) -> bool {
    shell::has_netns(m) || shell::has_links(m) || shell::has_freeze(m)
}

/// hydo_build_arm64's `build(m, manifest_path, out, bdir, H, oci, params)`.
pub fn build(
    ctx: &Ctx,
    p: &mut Prepared,
    manifest_path: &str,
    out: &str,
    bdir: &Path,
    io: &mut Io,
) -> Result<(), Fail> {
    let bt = BundledTc::new(ctx)?;
    let isfile = |x: &str| BundledTc::isfile(&bt, x);
    let busybox = tool(&ctx.inputs.busybox_arm64, BUSYBOX_ARM64, isfile)?;
    let mke2fs = tool(&ctx.inputs.mke2fs, x86::MKE2FS, isfile)?;
    let faketime = tool(&ctx.inputs.faketime, x86::FAKETIME, isfile)?;
    let m = p.data.clone();
    let world = if shell::has_world(&m) {
        Some(aarch64_tool(
            &bt,
            &ctx.inputs.worldbin_arm64,
            &bdir.join("hydo-world-arm64").to_string_lossy(),
        )?)
    } else {
        None
    };
    let read = if x86::uses_readers(&m)? {
        Some(aarch64_tool(
            &bt,
            &ctx.inputs.read_bin_arm64,
            &bdir.join("hydo-read-arm64").to_string_lossy(),
        )?)
    } else {
        None
    };
    let netns_k = needs_netns_kernel(&m);
    let netns = shell::has_netns(&m);
    let mut fcbin = None;
    let mut fc = None;
    if shell::has_freeze(&m) {
        fcbin = Some(aarch64_tool(
            &bt,
            &ctx.inputs.fcbin_arm64,
            &bdir.join("hydo_fc-arm64").to_string_lossy(),
        )?);
        fc = Some((
            "/bin/hydo_fc".to_string(),
            format!("{IPTABLES_ARM64}/bin/iptables-legacy"),
        ));
    }
    let ip = netns.then(|| IP_ARM64.to_string());
    let seedrng = ctx.inputs.seedrng_arm64.nonempty().map(str::to_string);
    let rootsec = section(&m, "root")?;
    let r = bdir.join(format!("{out}-arm64-root"));
    if r.is_dir() {
        fsx::force_remove(&r)?;
    }
    std::fs::create_dir_all(r.join("nix/store")).map_err(|e| io_fail("cannot create", &r, e))?;
    let store = x86::Store::new(&Ctx {
        onboard: ctx.onboard.clone(),
        release: Release::None,
        inputs: ctx.inputs.clone(),
    })?;
    let mut set = std::collections::BTreeSet::new();
    for n in pyv::items(pyv::get(rootsec, "nix")?, "root.nix")? {
        set.extend(store.closure(&py::str_of(&n))?);
    }
    for sp in &set {
        run_ok(
            Command::new("cp")
                .arg("-a")
                .arg(sp)
                .arg(format!("{}/", r.join("nix/store").display())),
            &format!("cp -a {sp}"),
        )?;
    }
    x86::make_dirs(&m, &r)?;
    put_exec(&busybox, &r.join("bin/busybox"))?;
    if let Some(s) = &seedrng {
        put_exec(s, &r.join("bin/hvx_seedrng"))?;
    }
    let lister = ctx.inputs.busybox.or(LISTER);
    let applets: Vec<String> = match Command::new(&lister).arg("--list").output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        _ => FIXED_APPLETS
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    };
    x86::link_applets(&r, &applets)?;
    if let Some(rb) = &read {
        put_exec(rb, &r.join("bin/hydo-read"))?;
        std::os::unix::fs::symlink("hydo-read", r.join("bin/hydo-check"))
            .map_err(|e| io_fail("cannot link", &r, e))?;
        outln(
            io,
            &format!(
                "hydo-build: staged /bin/hydo-read and /bin/hydo-check (arm64, sha256 {})",
                &fsx::sha(Path::new(rb))?[..16]
            ),
        );
    }
    if let Some(w) = &world {
        put_exec(w, &r.join("bin/hydo-world"))?;
        fsx::write_text(&r.join("etc/hydo/world.json"), &shell::world_config(&m))?;
        outln(
            io,
            &format!(
                "hydo-build: staged /bin/hydo-world (arm64, sha256 {})",
                &fsx::sha(Path::new(w))?[..16]
            ),
        );
    }
    x86::stage_files(&m, manifest_path, &r, &|s| {
        if let Some(b) = &bt {
            if s.starts_with("/nix/store/") && lexists(&b.here(s)) {
                return b.here(s).exists();
            }
        }
        Path::new(s).exists()
    })?;
    let hosts = if netns {
        shell::default_hosts(&m)
    } else {
        "127.0.0.1 localhost\n".to_string()
    };
    x86::default_etc(&r, &hosts)?;
    let tc = match &ctx.inputs.tc_arm64.value {
        Some(v) => v.clone(),
        None => TC_ARM64.to_string(),
    };
    if !tc.is_empty() {
        if !BundledTc::isfile(&bt, &tc) {
            return Err(Fail::Manifest(format!(
                "arm64 tc {tc} missing: nix build --max-jobs 0 --system aarch64-linux nixpkgs#iproute2 (or HYDO_TC_ARM64= for an image without network faults)"
            )));
        }
        copy_elf_closure(&bt, &tc, &r)?;
    }
    if ip.is_some() || fc.is_some() {
        let mut ps: Vec<String> = ip.iter().cloned().collect();
        if fc.is_some() {
            ps.extend(IPT_FILES.iter().map(|f| format!("{IPTABLES_ARM64}/{f}")));
        }
        copy_store_files(&ps, &r, &ctx.inputs.arm64_store)?;
    }
    if let Some(fb) = &fcbin {
        let l = x86::under(&r, &format!("{IPTABLES_ARM64}/bin/iptables-legacy"));
        std::os::unix::fs::symlink("xtables-legacy-multi", &l)
            .map_err(|e| io_fail("cannot link", &l, e))?;
        put_exec(fb, &r.join("bin/hydo_fc"))?;
        outln(
            io,
            &format!(
                "hydo-build: staged iptables-legacy and /bin/hydo_fc (arm64, sha256 {})",
                &fsx::sha(Path::new(fb))?[..16]
            ),
        );
    }
    if netns_k {
        fsx::write_text(
            &r.join("etc/hydo/kernel"),
            &format!("{NETNS_KERNEL} {NETNS_KERNEL_SHA256}\n"),
        )?;
    }
    let tools = ShellTools {
        tc: tc.clone(),
        fc: fc.clone(),
        ip: ip.clone(),
    };
    x86::write_compiled(ctx, p, manifest_path, &r, &tools)?;
    let (agent_bin, agent_sha) = crate::agent::binary(ctx, bdir, true, io)?;
    crate::stage::compose_stage(&m, &r)?;
    let oci_meta = crate::oci::stage(
        &mut p.oci,
        &r,
        Path::new(&busybox),
        &bdir.join(format!("{out}-arm64-oci-tmp")),
        netns,
        io,
    )?;
    // the agent is the kernel's first process itself (rdinit=/init): no shell before it
    crate::agent::install(&agent_bin, &agent_sha, &r, "/init", io)?;
    let dsec = section(&m, "data")?;
    let dmib = pyv::get(dsec, "size_mib")?
        .map(|v| pyv::int(v, "data.size_mib"))
        .transpose()?
        .unwrap_or(64);
    let name = py::str_of(&m["name"]);
    let data = bdir.join(format!("{out}-arm64-data.img"));
    let mk = MkfsTools { mke2fs, faketime };
    image::mkfs(
        ctx,
        &mk,
        &data,
        dmib as u64,
        &image::uuid_from(&name, "data"),
        &image::uuid_from(&name, "data-hash"),
        None,
        true,
    )?;
    image::zero_times(&data, true)?;
    let disk = bdir.join(format!("{out}-arm64-disk.img"));
    let (p1, n1) = (2048u64, 2048u64);
    let (p2, n2) = (p1 + n1, dmib as u64 * 2048);
    image::assemble_disk(
        &disk,
        p2 + n2,
        &[(p1 as u32, n1 as u32), (p2 as u32, n2 as u32)],
        &[(&data, p2)],
    )?;
    fsx::fix_mtimes(&r)?;
    let cpio = bdir.join(format!("{out}-arm64.cpio.gz"));
    let meta = (!oci_meta.is_empty()).then(|| crate::oci::cpio_meta(&oci_meta));
    crate::cpio::write_cpio_gz(&r, &cpio, fsx::FIXED_MTIME as u64, meta.as_ref(), true)?;
    outln(
        io,
        &format!(
            "hydo-build: {} (arm64 initramfs) + {} (data {} MiB)",
            cpio.display(),
            disk.display(),
            dmib
        ),
    );
    image::print_shas(io, &[&cpio, &disk])?;
    if netns_k {
        let mut why = Vec::new();
        if netns {
            why.push("network: bridge");
        }
        if shell::has_links(&m) {
            why.push("addr:");
        }
        if shell::has_freeze(&m) {
            why.push("faults: [freeze]");
        }
        outln(
            io,
            &format!(
                "hydo-build: this image boots the guest kernel {NETNS_KERNEL} (sha256 {}), not Image-explore: it needs {}; recorded in the image as /etc/hydo/kernel",
                &NETNS_KERNEL_SHA256[..16],
                why.join(", ")
            ),
        );
    }
    Ok(())
}
