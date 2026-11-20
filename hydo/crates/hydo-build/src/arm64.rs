//! `--arch arm64`: the manifest compiled for the Mac VMM (hv/hvm/). Port of
//! hydo_build_arm64.py, with the Mac release's bundled aarch64 tc closure (hydo_bridge.py's
//! `use_bundled_tc`). Output, in the build dir:
//!     `<NAME>-arm64.cpio.gz`    the root, as one initramfs
//!     `<NAME>-arm64-disk.img`   the data disk for the virtio-blk device (HVM_DISK): an MBR with a
//!                             1 MiB placeholder p1 and the journaled ext4 data partition p2
//! The root tree is the x86 image's, with two differences: the whole root is the initramfs and a
//! small /init waits for /dev/vda2; tc is the pinned aarch64 iproute2 with exactly the shared
//! libraries its loader opens. A manifest with network: bridge, an addr: or the freeze fault boots
//! the second kernel, Image-explore-netns-cg since the fingerprint epoch 3 (Image-explore-netns
//! before), and says so in /etc/hydo/kernel.

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
/// The second arm64 kernel of this hydo's images (with cgroups since the fingerprint epoch 3,
/// Milestone 234; the image record names it too).
pub const NETNS_KERNEL: &str = hydo_image::kernels::ARM64_NETNS_KERNEL_CG.file;
pub const NETNS_KERNEL_SHA256: &str = hydo_image::kernels::ARM64_NETNS_KERNEL_CG.sha256;
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
        let Release::Mac { root, .. } = &ctx.release else {
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

/// Whether this hydo's arm64 roots keep their hard links as links (Milestone 338, from the
/// fingerprint epoch 5, [`hydo_image::EpochDefaults::cpio_hard_links`]).
fn links() -> bool {
    hydo_image::defaults(hydo_image::EPOCH, "arm64").cpio_hard_links
}

/// `guest.root` (Milestone 284): the root is a read-only file system on the disk, not the
/// initramfs. The default is the disk for an image with `browser:` (the Playwright image is
/// about 3 GB unpacked, more than guest RAM should hold), else memory, as before.
pub fn root_on_disk(m: &serde_json::Value) -> bool {
    match m.get("guest").and_then(|g| g.get("root")) {
        Some(serde_json::Value::String(s)) => s == "disk",
        Some(serde_json::Value::Null) | None => m.get(hydo_manifest::browser::META).is_some(),
        Some(_) => false,
    }
}

/// The paths of the root that the host reads from an image's initramfs
/// (`hydo_engine::image::embedded_file`): its records and plans under /etc/hydo, the busybox
/// whose messages the controller reads, and a SQL helper's configuration. The initramfs of an
/// image whose root is on its disk carries copies of them.
const HOST_READS: &[&str] = &["etc/hydo", "bin/busybox", "hydo/workload/helper_sql.conf"];

/// The init of an image whose root is on its disk: mount the read-only ext4 root of /dev/vda1,
/// move /dev into it, and hand PID 1 to the agent there.
const DISK_ROOT_INIT: &str = "#!/bin/busybox sh\n\
b=/bin/busybox\n\
$b mount -t devtmpfs none /dev\n\
n=0; while [ ! -b /dev/vda1 ] && [ $n -lt 1000 ]; do $b usleep 1000; n=$((n+1)); done\n\
if ! $b mount -t ext4 -o ro,noatime /dev/vda1 /root; then\n\
  echo 'hydo: the root file system on /dev/vda1 did not mount'\n\
  while :; do $b sleep 3600; done\n\
fi\n\
$b mount --move /dev /root/dev\n\
exec $b switch_root /root /init\n";

/// `guest.root: disk`: the root tree `r` as a read-only ext4 file system `<out>-arm64-rootfs.img`
/// of its size (`root.size_mib`, else 1.3 times the tree, in steps of 64 MiB), and the small
/// initramfs `cpio` that mounts it. The tree is removed once it is in the archive. Returns the
/// file system and its size in MiB.
#[allow(clippy::too_many_arguments)]
fn disk_root(
    ctx: &Ctx,
    rootsec: Option<&serde_json::Value>,
    r: &Path,
    name: &str,
    bdir: &Path,
    out: &str,
    cpio: &Path,
    meta: Option<&crate::cpio::Meta>,
    io: &mut Io,
) -> Result<(PathBuf, u64), Fail> {
    let size = pyv::get(rootsec, "size_mib")?
        .map(|v| pyv::int(v, "root.size_mib"))
        .transpose()?
        .unwrap_or(0);
    let archive = bdir.join(format!("{out}-arm64-root.cpio"));
    let alen = crate::cpio::write_cpio_file(r, &archive, fsx::FIXED_MTIME as u64, meta, links())?;
    let mib: u64 = if size != 0 {
        size as u64
    } else {
        // the archive is the files' bytes and small headers; the file system needs room for
        // its inodes, directories and block maps besides
        let used = alen.div_ceil(1 << 20);
        64.max((used * 13 / 10).div_ceil(64) * 64)
    };
    // the initramfs: busybox, the init above, and copies of what the host reads
    let b = bdir.join(format!("{out}-arm64-boot"));
    if b.is_dir() {
        fsx::force_remove(&b)?;
    }
    for d in ["dev", "proc", "sys", "root", "bin"] {
        fsx::makedirs(&b.join(d))?;
    }
    for p in HOST_READS {
        let src = r.join(p);
        if !lexists(&src) {
            continue;
        }
        if src.is_dir() && !is_link(&src) {
            let par = b.join(p).parent().unwrap_or(&b).to_path_buf();
            fsx::makedirs(&par)?;
            run_ok(
                Command::new("cp").arg("-a").arg(&src).arg(&par),
                &format!("cp -a {}", src.display()),
            )?;
        } else {
            copy_one(&src, &b.join(p))?;
        }
    }
    fsx::write_text(&b.join("init"), DISK_ROOT_INIT)?;
    fsx::chmod(&b.join("init"), 0o755)?;
    fsx::fix_mtimes(&b)?;
    crate::cpio::write_cpio_gz(&b, cpio, fsx::FIXED_MTIME as u64, None, true, links())?;
    fsx::force_remove(&b)?;
    // the tree is in the archive now: a root of gigabytes is not kept beside the helper VM's disk
    fsx::force_remove(r)?;
    let rootfs = bdir.join(format!("{out}-arm64-rootfs.img"));
    let made = image::mac_mkfs_root(
        ctx,
        &rootfs,
        mib,
        &image::uuid_from(name, "root"),
        &image::uuid_from(name, "root-hash"),
        &archive,
        alen,
        io,
    );
    let _ = std::fs::remove_file(&archive);
    made?;
    image::zero_times(&rootfs, false)?;
    Ok((rootfs, mib))
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
    let tls_eps = crate::tls::endpoints(&m);
    let world = if shell::has_world(&m) {
        Some(if tls_eps.is_empty() {
            aarch64_tool(
                &bt,
                &ctx.inputs.worldbin_arm64,
                &bdir.join("hydo-world-arm64").to_string_lossy(),
            )?
        } else {
            aarch64_tool(
                &bt,
                &ctx.inputs.worldbin_tls_arm64,
                &bdir.join("hydo-world-tls-arm64").to_string_lossy(),
            )?
        })
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
    let sql = if x86::uses_sql(&m)? {
        Some(aarch64_tool(
            &bt,
            &ctx.inputs.sql_bin_arm64,
            &bdir.join("hydo-sql-arm64").to_string_lossy(),
        )?)
    } else {
        None
    };
    let netns_k = needs_netns_kernel(&m);
    let netns = shell::has_netns(&m);
    // guest.kernel (Milestone 256): the variant's kernel, which serves service networks too
    let variant = crate::record::kernel_variant(&m);
    let containers = variant.as_deref() == Some(hydo_image::kernels::CONTAINERS);
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
    if let Some(sb) = &sql {
        put_exec(sb, &r.join("bin/hydo-sql"))?;
        outln(
            io,
            &format!(
                "hydo-build: staged /bin/hydo-sql (arm64, sha256 {})",
                &fsx::sha(Path::new(sb))?[..16]
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
    let trust = x86::tls_stage(&mut p.data, &tls_eps, &r, io)?;
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
    if containers {
        let k = &hydo_image::kernels::ARM64_KERNEL_CONTAINERS;
        fsx::write_text(
            &r.join("etc/hydo/kernel"),
            &format!("{} {}\n", k.file, k.sha256),
        )?;
    } else if netns_k {
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
    x86::write_compiled(ctx, p, manifest_path, &r, &tools, "arm64")?;
    let (agent_bin, agent_sha) = crate::agent::binary(ctx, bdir, true, io)?;
    crate::stage::compose_stage(&m, &r)?;
    let (oci_meta, _) = crate::oci::stage_with(
        &mut p.oci,
        &r,
        Path::new(&busybox),
        &bdir.join(format!("{out}-arm64-oci-tmp")),
        netns,
        pyv::get(rootsec, "share_images")? == Some(&serde_json::Value::Bool(true)),
        None,
        trust.as_ref(),
        io,
    )?;
    // `writable: true`: the launcher copies the tree to the data disk at the first start, so the
    // data disk gets room for it besides data.size_mib
    let mut layer_mib: i128 = 0;
    for o in p.oci.iter().filter(|o| o.writable) {
        let d = r.join("oci").join(&o.svc);
        let du = run_ok(Command::new("du").arg("-sm").arg(&d), "du -sm")?;
        let used: i128 = String::from_utf8_lossy(&du.stdout)
            .split_whitespace()
            .next()
            .and_then(|x| x.parse().ok())
            .ok_or_else(|| Fail::Exit("hydo-build: du -sm printed no size".into()))?;
        layer_mib += (used * 13 / 10 + 63) / 64 * 64;
    }
    for w in crate::oci::ready_warnings(
        &p.oci,
        &r,
        &m,
        pyv::get(rootsec, "share_images")? == Some(&serde_json::Value::Bool(true)),
        None,
    ) {
        crate::errln(io, &format!("hydo-build: warning: {w}"));
    }
    // the agent is the kernel's first process itself (rdinit=/init): no shell before it
    crate::agent::install(&agent_bin, &agent_sha, &r, "/init", io)?;
    crate::record::write_inside(&r, "arm64", netns_k, variant.as_deref())?;
    let dsec = section(&m, "data")?;
    let dmib = pyv::get(dsec, "size_mib")?
        .map(|v| pyv::int(v, "data.size_mib"))
        .transpose()?
        .unwrap_or(64)
        + layer_mib;
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
    // root.fix_pyc (Milestone 202): Python's bytecode caches point at the fixed time below; on
    // unless the manifest says false (fingerprint epoch 2, Milestone 204)
    if pyv::get(rootsec, "fix_pyc")? != Some(&serde_json::Value::Bool(false)) {
        crate::pyc::fix_tree(&r, fsx::FIXED_MTIME)?;
    }
    fsx::fix_mtimes(&r)?;
    let cpio = bdir.join(format!("{out}-arm64.cpio.gz"));
    let meta = (!oci_meta.is_empty()).then(|| crate::oci::cpio_meta(&oci_meta));
    // guest.root: disk (Milestone 284): the root is a read-only ext4 file system in p1, and the
    // initramfs only mounts it; else p1 is a 1 MiB placeholder and the root is the initramfs
    let rootfs = if root_on_disk(&m) {
        Some(disk_root(
            ctx,
            rootsec,
            &r,
            &name,
            bdir,
            out,
            &cpio,
            meta.as_ref(),
            io,
        )?)
    } else {
        None
    };
    let (p1, n1) = match &rootfs {
        Some((_, mib)) => (2048u64, *mib * 2048),
        None => (2048u64, 2048u64),
    };
    let (p2, n2) = (p1 + n1, dmib as u64 * 2048);
    let mut parts: Vec<(&Path, u64)> = Vec::new();
    if let Some((f, _)) = &rootfs {
        parts.push((f.as_path(), p1));
    }
    parts.push((&data, p2));
    let vols = crate::x86::volume_images(ctx, &mk, &m, &name, bdir, &format!("{out}-arm64-vol"))?;
    if vols.is_empty() {
        image::assemble_disk(
            &disk,
            p2 + n2,
            &[(p1 as u32, n1 as u32), (p2 as u32, n2 as u32)],
            &parts,
        )?;
    } else {
        let v: Vec<(&Path, u64)> = vols.iter().map(|(p, n)| (p.as_path(), *n)).collect();
        image::assemble_disk_volumes(
            &disk,
            [(p1 as u32, n1 as u32), (p2 as u32, n2 as u32)],
            &parts,
            &v,
        )?;
    }
    match &rootfs {
        Some((f, mib)) => {
            // the file system is in the disk now; a root of gigabytes is not kept twice
            let _ = std::fs::remove_file(f);
            outln(
                io,
                &format!(
                    "hydo-build: {} (arm64 initramfs that mounts the root) + {} (root {} MiB, data {} MiB)",
                    cpio.display(),
                    disk.display(),
                    mib,
                    dmib
                ),
            );
        }
        None => {
            crate::cpio::write_cpio_gz(
                &r,
                &cpio,
                fsx::FIXED_MTIME as u64,
                meta.as_ref(),
                true,
                links(),
            )?;
            outln(
                io,
                &format!(
                    "hydo-build: {} (arm64 initramfs) + {} (data {} MiB)",
                    cpio.display(),
                    disk.display(),
                    dmib
                ),
            );
        }
    }
    image::print_shas(io, &[&cpio, &disk])?;
    crate::record::write_sidecar(
        &cpio,
        Some(&disk),
        hydo_image::kernels::record_with("arm64", netns_k, variant.as_deref()),
        manifest_path,
        &p.oci,
        io,
    )?;
    crate::contracts::write_plan(&m, manifest_path, &cpio, io)?;
    crate::stage::write_regressions(&cpio, &p.regressions, io)?;
    if containers {
        let k = &hydo_image::kernels::ARM64_KERNEL_CONTAINERS;
        outln(
            io,
            &format!(
                "hydo-build: this image boots the guest kernel variant containers (guest.kernel), {} (sha256 {}); recorded in the image as /etc/hydo/kernel",
                k.file,
                &k.sha256[..16]
            ),
        );
    } else if netns_k {
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
                "hydo-build: this image boots the guest kernel {NETNS_KERNEL} (sha256 {}), not {}: it needs {}; recorded in the image as /etc/hydo/kernel",
                &NETNS_KERNEL_SHA256[..16],
                hydo_image::kernels::arm64_kernel(false).file,
                why.join(", ")
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_root_goes_on_the_disk_with_browser_or_the_key() {
        assert!(!root_on_disk(&json!({"name": "x"})));
        assert!(root_on_disk(&json!({"name": "x", "_browser": {}})));
        assert!(root_on_disk(
            &json!({"name": "x", "guest": {"root": "disk"}})
        ));
        assert!(!root_on_disk(
            &json!({"name": "x", "_browser": {}, "guest": {"root": "memory"}})
        ));
        assert!(DISK_ROOT_INIT.starts_with("#!/bin/busybox sh\n"));
        assert!(DISK_ROOT_INIT.contains("switch_root /root /init"));
    }
}
