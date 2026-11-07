//! The x86_64 image: a read-only ext4 root (p1) made with mke2fs -d from a staged tree, a
//! journaled ext4 data disk (p2), both behind a hand-made MBR. Port of hydo-build's `build()`,
//! with the Linux release's bundled /nix/store (hydo_bridge.py's `use_bundled_store`) and the
//! helpers both architectures share (root.files, root.text, the readers' detection).

use crate::fsx::{self, io_fail, lexists};
use crate::image::{self, MkfsTools};
use crate::pyv::{self, section};
use crate::shell::{self, ShellTools};
use crate::{outln, tool, Ctx, Fail, Io, Prepared, Release};
use hydo_manifest::py;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const NETMODS: &[&str] = &["sch_prio", "sch_netem", "cls_u32"];
pub const FCMODS: &[&str] = &[
    "x_tables",
    "ip_tables",
    "iptable_filter",
    "xt_tcpudp",
    "xt_NFQUEUE",
    "nfnetlink",
    "nfnetlink_queue",
];
pub const NSMODS: &[&str] = &["llc", "stp", "bridge", "veth"];

const BUSYBOX: &str = "/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox";
pub const MKE2FS: &str =
    "/nix/store/24s0phlx9kly7x7qb8gal3mmn51ndmlx-e2fsprogs-1.47.0-bin/bin/mke2fs";
pub const FAKETIME: &str =
    "/nix/store/fs0gp7zyfjnbvaq82yvwbcsvqh7876d1-libfaketime-0.9.10/bin/faketime";
const IPROUTE2: &str = "/nix/store/dx4fsbxs5v82l9lzmjkpkhx5ygqcwm69-iproute2-6.8.0";
const MODROOT: &str = "/run/current-system/kernel-modules/lib/modules/6.6.35";
const IPTABLES: &str = "/nix/store/xnrrq638db6170pg7dvz84f8nr1q2wvi-iptables-1.8.10";

/// The guest-side /nix/store closures: the host's /nix/store with `nix-store -qR`, or, in the
/// Linux release, guest/store with guest/store-closures.txt (only for the pinned paths the host
/// does not have).
pub struct Store {
    bundle: Option<PathBuf>,
    closures: HashMap<String, Vec<String>>,
}

impl Store {
    pub fn new(ctx: &Ctx) -> Result<Store, Fail> {
        let Release::Linux { root } = &ctx.release else {
            return Ok(Store {
                bundle: None,
                closures: HashMap::new(),
            });
        };
        let guest = root.join("guest");
        let mut closures: HashMap<String, Vec<String>> = HashMap::new();
        let list = guest.join("store-closures.txt");
        let t = crate::read_text(&list)?;
        for line in t.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = py::split_ws(line);
            if f.len() != 2 {
                return Err(Fail::Crash(format!(
                    "{}: a line that is not '<root> <member>': {line}",
                    list.display()
                )));
            }
            closures
                .entry(f[0].to_string())
                .or_default()
                .push(f[1].to_string());
        }
        Ok(Store {
            bundle: Some(guest.join("store")),
            closures,
        })
    }

    fn here(&self, p: &str) -> Option<PathBuf> {
        self.bundle
            .as_ref()
            .map(|b| b.join(p.trim_start_matches('/')))
    }

    /// The path an existence check looks at (hydo-build's view of os.path in the release).
    pub fn view(&self, p: &str) -> PathBuf {
        if p.starts_with("/nix/store/") && !lexists(Path::new(p)) {
            if let Some(h) = self.here(p).filter(|h| lexists(h)) {
                return h;
            }
        }
        PathBuf::from(p)
    }

    /// `closure(path)`: the store paths to copy (host paths), sorted.
    pub fn closure(&self, p: &str) -> Result<Vec<String>, Fail> {
        if self.bundle.is_some() {
            if let Some(ms) = self.closures.get(p) {
                if !Path::new(p).exists() {
                    let mut v: Vec<String> = ms
                        .iter()
                        .filter_map(|x| self.here(x))
                        .map(|x| x.to_string_lossy().into_owned())
                        .collect();
                    v.sort();
                    return Ok(v);
                }
            }
            if which("nix-store").is_none() {
                return Err(Fail::Manifest(format!(
                    "{p}: not in the release's bundled closures, and this machine has no nix-store (root.nix: needs Nix; list the files under root.files: instead)"
                )));
            }
        }
        let out = Command::new("nix-store")
            .arg("-qR")
            .arg(p)
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| {
                Fail::Exit(format!(
                    "hydo-build: cannot run nix-store -qR {p} ({e}): root.nix: and the pinned tool closures need Nix on this machine"
                ))
            })?;
        if !out.status.success() {
            return Err(Fail::Exit(format!(
                "hydo-build: nix-store -qR {p} failed ({})",
                image::exit_text(&out.status)
            )));
        }
        let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        v.sort();
        Ok(v)
    }
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file() && is_exec(p))
}

pub fn is_exec(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// Run a command; a failure is the Python's CalledProcessError (a plain message here).
pub fn run_ok(cmd: &mut Command, what: &str) -> Result<std::process::Output, Fail> {
    let out = cmd
        .output()
        .map_err(|e| Fail::Exit(format!("hydo-build: cannot run {what}: {e}")))?;
    if !out.status.success() {
        let _ = std::io::Write::write_all(&mut std::io::stderr(), &out.stderr);
        return Err(Fail::Exit(format!(
            "hydo-build: {what} failed ({})",
            image::exit_text(&out.status)
        )));
    }
    Ok(out)
}

// ------------------------------------------------------------------ readers (hydo-read/hydo-check)
fn word_char_before(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_./-".contains(&c)
}

fn word_char_after(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_-".contains(&c)
}

/// `READERS_RE.search(text)`: `(?<![A-Za-z0-9_./-])hydo-(read|check)(?![A-Za-z0-9_-])`.
pub fn readers_search(b: &[u8]) -> bool {
    words_search(b, &[&b"hydo-read"[..], &b"hydo-check"[..]])
}

/// One of `words` as a command word (the same boundaries as `readers_search`).
pub fn words_search(b: &[u8], words: &[&[u8]]) -> bool {
    for &word in words {
        let mut i = 0;
        while i + word.len() <= b.len() {
            if &b[i..i + word.len()] == word {
                let before_ok = i == 0 || !word_char_before(b[i - 1]);
                let after_ok = i + word.len() == b.len() || !word_char_after(b[i + word.len()]);
                if before_ok && after_ok {
                    return true;
                }
            }
            i += 1;
        }
    }
    false
}

fn walk_strings(x: &Value, found: &dyn Fn(&[u8]) -> bool) -> bool {
    match x {
        Value::Object(o) => o.iter().any(|(k, v)| {
            !["description", "msg", "name"].contains(&k.as_str()) && walk_strings(v, found)
        }),
        Value::Array(a) => a.iter().any(|v| walk_strings(v, found)),
        Value::String(s) => found(s.as_bytes()),
        _ => false,
    }
}

/// `uses_readers(m)`: a command of the manifest (or a file of its hydo/ folder) calls hydo-read
/// or hydo-check.
pub fn uses_readers(m: &Value) -> Result<bool, Fail> {
    uses_program(m, &readers_search)
}

/// `uses_sql(m)`: a command of the manifest (or a file of its hydo/ folder) calls hydo-sql, the
/// ready-made SQL workloads and checks; /bin/hydo-sql is staged only then, so no other image
/// changes.
pub fn uses_sql(m: &Value) -> Result<bool, Fail> {
    uses_program(m, &|b: &[u8]| words_search(b, &[&b"hydo-sql"[..]]))
}

fn uses_program(m: &Value, found: &dyn Fn(&[u8]) -> bool) -> Result<bool, Fail> {
    if walk_strings(m, found) {
        return Ok(true);
    }
    let dirs: Vec<String> = m
        .get("_compose")
        .filter(|c| py::truthy(c))
        .and_then(|c| c.get("_folder"))
        .filter(|f| py::truthy(f))
        .and_then(|f| f.get("_dirs"))
        .and_then(Value::as_object)
        .map(|o| o.values().map(py::str_of).collect())
        .unwrap_or_default();
    let mut dirs = dirs;
    dirs.sort();
    for d in dirs {
        for w in fsx::walk(Path::new(&d))? {
            for f in &w.files {
                let p = w.dir.join(f);
                if let Ok(md) = std::fs::metadata(&p) {
                    if md.is_file() && md.len() <= (64 << 20) {
                        let b = std::fs::read(&p).map_err(|e| io_fail("cannot read", &p, e))?;
                        if found(&b) {
                            return Ok(true);
                        }
                    }
                }
            }
        }
    }
    Ok(false)
}

// ------------------------------------------------------------------ shared staging
fn expanduser(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return format!("{}{}", h.trim_end_matches('/'), &p[1..]);
        }
    }
    p.to_string()
}

/// `os.path.join(R, rel.lstrip("/"))`.
pub fn under(r: &Path, rel: &str) -> PathBuf {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        let mut s = r.as_os_str().to_os_string();
        s.push("/");
        PathBuf::from(s)
    } else {
        r.join(rel)
    }
}

/// The base directories, then root.dirs.
pub fn make_dirs(m: &Value, r: &Path) -> Result<(), Fail> {
    let root = section(m, "root")?;
    let mut ds: Vec<String> = [
        "bin",
        "sbin",
        "proc",
        "sys",
        "dev",
        "data",
        "tmp",
        "etc/hydo",
        "lib/modules",
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    for d in pyv::items(pyv::get(root, "dirs")?, "root.dirs")? {
        ds.push(
            pyv::need_str(&d, "root.dirs")?
                .trim_start_matches('/')
                .to_string(),
        );
    }
    for d in ds {
        fsx::makedirs(&under(r, &d))?;
    }
    Ok(())
}

/// The busybox applets the project's commands may call by name, as links to busybox.
pub fn link_applets(r: &Path, applets: &[String]) -> Result<(), Fail> {
    for a in applets {
        let p = r.join("bin").join(a);
        if !lexists(&p) {
            std::os::unix::fs::symlink("busybox", &p).map_err(|e| io_fail("cannot link", &p, e))?;
        }
    }
    Ok(())
}

/// root.files: and root.text: into the staged tree; `exists` is the view the source check uses.
pub fn stage_files(
    m: &Value,
    manifest_path: &str,
    r: &Path,
    exists: &dyn Fn(&str) -> bool,
) -> Result<(), Fail> {
    let root = section(m, "root")?;
    let mdir = py::dirname(&py::abspath(manifest_path));
    for e in pyv::items(pyv::get(root, "files")?, "root.files")? {
        let o = match &e {
            Value::Object(o) if o.contains_key("src") && o.contains_key("dst") => o,
            _ => {
                return Err(Fail::Manifest(
                    "root.files: each entry needs src and dst".into(),
                ))
            }
        };
        let mut src = expanduser(&py::str_of(&o["src"]));
        if !src.starts_with('/') {
            src = format!("{}/{}", mdir.trim_end_matches('/'), src);
        }
        if !exists(&src) {
            return Err(Fail::Manifest(format!("root.files: {src} does not exist")));
        }
        let dst = under(r, &py::str_of(&o["dst"]));
        fsx::copy_into(Path::new(&src), &dst)?;
        if let Some(mode) = o.get("mode") {
            let t = py::str_of(mode);
            let v = hydo_manifest::composer::py_int(&t, 8).map_err(|_| {
                Fail::Crash(format!(
                    "root.files: mode {} is not an octal number",
                    py::repr(mode)
                ))
            })?;
            fsx::chmod(&dst, v as u32)?;
        }
    }
    if let Some(t) = pyv::get(root, "text")? {
        match t {
            Value::Object(o) => {
                for (dst, content) in o {
                    let p = under(r, dst);
                    if let Some(par) = p.parent() {
                        fsx::makedirs(par)?;
                    }
                    let c = pyv::need_str(content, "root.text")?;
                    fsx::write_text(&p, c)?;
                }
            }
            v if !py::truthy(v) => {}
            v => {
                return Err(Fail::Crash(format!(
                    "root.text: a {} where a mapping of paths to contents was expected",
                    py::type_name(v)
                )))
            }
        }
    }
    Ok(())
}

/// /etc/hosts, /etc/passwd and /etc/group unless the project gave them.
pub fn default_etc(r: &Path, hosts: &str) -> Result<(), Fail> {
    for (name, content) in [
        ("etc/hosts", hosts),
        ("etc/passwd", "root:x:0:0:root:/tmp:/bin/sh\n"),
        ("etc/group", "root:x:0:\n"),
    ] {
        let p = r.join(name);
        if !p.exists() {
            fsx::write_text(&p, content)?;
        }
    }
    Ok(())
}

/// manifest.sh, prelude.sh, hydo.yaml and the params file.
pub fn write_compiled(
    ctx: &Ctx,
    p: &Prepared,
    manifest_path: &str,
    r: &Path,
    t: &ShellTools,
) -> Result<(), Fail> {
    let (ms, pre) = shell::compile_shell(ctx, &p.data, &p.loaded.service_order, t)?;
    fsx::write_text(&r.join("etc/hydo/manifest.sh"), &ms)?;
    fsx::write_text(&r.join("etc/hydo/prelude.sh"), &pre)?;
    fsx::copy_file(Path::new(manifest_path), &r.join("etc/hydo/hydo.yaml"))?;
    if !p.loaded.params.is_empty() {
        let mut kv = p.loaded.params.clone();
        kv.sort();
        let t: String = kv.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        fsx::write_text(&r.join("etc/hydo/params"), &t)?;
    }
    Ok(())
}

/// The prebuilt helper copied into the root with mode 755.
pub fn put_exec(src: &str, dst: &Path) -> Result<(), Fail> {
    fsx::copy_file(Path::new(src), dst)?;
    fsx::chmod(dst, 0o755)
}

// ------------------------------------------------------------------ build
/// hydo-build's `build(m, manifest_path, out, bdir, params, oci)`.
pub fn build(
    ctx: &Ctx,
    p: &mut Prepared,
    manifest_path: &str,
    out: &str,
    bdir: &Path,
    io: &mut Io,
) -> Result<(), Fail> {
    let store = Store::new(ctx)?;
    let isfile = |x: &str| store.view(x).is_file();
    let isdir = |x: &str| store.view(x).is_dir();
    let busybox = tool(&ctx.inputs.busybox, BUSYBOX, isfile)?;
    let seedrng = tool(
        &ctx.inputs.seedrng,
        &bdir.join("hvx_seedrng").to_string_lossy(),
        isfile,
    )?;
    let mke2fs = tool(&ctx.inputs.mke2fs, MKE2FS, isfile)?;
    let faketime = tool(&ctx.inputs.faketime, FAKETIME, isfile)?;
    let m = p.data.clone();
    let faults = shell::fault_kinds(&m);
    let want_tc = faults.iter().any(|f| f == "network" || f == "oneway");
    let netns = shell::has_netns(&m);
    let mut iproute2 = None;
    let mut modroot = None;
    if want_tc || netns {
        iproute2 = Some(tool(&ctx.inputs.iproute2, IPROUTE2, |x| {
            isfile(&format!("{x}/bin/tc"))
        })?);
        modroot = Some(tool(&ctx.inputs.modroot, MODROOT, isdir)?);
    }
    let mut iptables = None;
    let mut fcbin = None;
    if faults.iter().any(|f| f == "freeze") {
        iptables = Some(tool(&ctx.inputs.iptables, IPTABLES, |x| {
            isfile(&format!("{x}/bin/xtables-legacy-multi"))
        })?);
        fcbin = Some(tool(
            &ctx.inputs.fcbin,
            &bdir.join("hydo_fc").to_string_lossy(),
            isfile,
        )?);
        modroot = Some(tool(&ctx.inputs.modroot, MODROOT, isdir)?);
    }
    let mut world = None;
    if shell::has_world(&m) {
        world = Some(tool(
            &ctx.inputs.worldbin,
            &bdir.join("hydo-world").to_string_lossy(),
            isfile,
        )?);
    }
    let rootsec = section(&m, "root")?;
    let r = bdir.join(format!("{out}-root"));
    let mut roots: Vec<String> = Vec::new();
    for n in pyv::items(pyv::get(rootsec, "nix")?, "root.nix")? {
        roots.push(py::str_of(&n));
    }
    roots.extend(iproute2.clone());
    roots.extend(iptables.clone());
    // One entry per store path name, in name order (the order of a closure taken from /nix/store
    // alone). A machine with Nix and the release's bundled closures (M140) can reach the same store
    // path twice, once in /nix/store and once in the bundle; both hold the same content, and copying
    // it twice would write into the first copy's read-only directories.
    let mut by_name = std::collections::BTreeMap::new();
    for n in &roots {
        for p in store.closure(n)? {
            let name = Path::new(&p)
                .file_name()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone());
            let keep = by_name.get(&name).is_some_and(|q: &String| {
                q.starts_with("/nix/store/") || !p.starts_with("/nix/store/")
            });
            if !keep {
                by_name.insert(name, p);
            }
        }
    }
    let paths: Vec<String> = by_name.into_values().collect();
    let stamp = bdir.join(format!("{out}-root.closures"));
    let joined = paths.join("\n");
    let reuse = r.is_dir()
        && stamp.exists()
        && std::fs::read(&stamp)
            .map(|b| b == joined.as_bytes())
            .unwrap_or(false);
    if reuse {
        outln(
            io,
            &format!("hydo-build: reusing the staged closures in {}", r.display()),
        );
        for e in std::fs::read_dir(&r).map_err(|e| io_fail("cannot list", &r, e))? {
            let e = e.map_err(|er| io_fail("cannot list", &r, er))?;
            if e.file_name() != "nix" {
                fsx::force_remove(&e.path())?;
            }
        }
    } else {
        if r.is_dir() {
            fsx::force_remove(&r)?;
        }
        std::fs::create_dir_all(r.join("nix/store"))
            .map_err(|e| io_fail("cannot create", &r, e))?;
        for sp in &paths {
            run_ok(
                Command::new("cp")
                    .arg("-a")
                    .arg(sp)
                    .arg(format!("{}/", r.join("nix/store").display())),
                &format!("cp -a {sp}"),
            )?;
        }
        fsx::write_text(&stamp, &joined)?;
        outln(
            io,
            &format!("hydo-build: staged {} store paths", paths.len()),
        );
    }
    make_dirs(&m, &r)?;
    put_exec(&busybox, &r.join("bin/busybox"))?;
    put_exec(&seedrng, &r.join("bin/hvx_seedrng"))?;
    let lst = run_ok(Command::new(&busybox).arg("--list"), "busybox --list")?;
    let applets: Vec<String> = String::from_utf8_lossy(&lst.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    link_applets(&r, &applets)?;
    if uses_readers(&m)? {
        let rb = tool(
            &ctx.inputs.read_bin,
            &bdir.join("hydo-read").to_string_lossy(),
            isfile,
        )?;
        put_exec(&rb, &r.join("bin/hydo-read"))?;
        std::os::unix::fs::symlink("hydo-read", r.join("bin/hydo-check"))
            .map_err(|e| io_fail("cannot link", &r, e))?;
        outln(
            io,
            &format!(
                "hydo-build: staged /bin/hydo-read and /bin/hydo-check (sha256 {})",
                &fsx::sha(Path::new(&rb))?[..16]
            ),
        );
    }
    if uses_sql(&m)? {
        let sb = tool(
            &ctx.inputs.sql_bin,
            &bdir.join("hydo-sql").to_string_lossy(),
            isfile,
        )?;
        put_exec(&sb, &r.join("bin/hydo-sql"))?;
        outln(
            io,
            &format!(
                "hydo-build: staged /bin/hydo-sql (sha256 {})",
                &fsx::sha(Path::new(&sb))?[..16]
            ),
        );
    }
    if let Some(fc) = &fcbin {
        put_exec(fc, &r.join("bin/hydo_fc"))?;
    }
    if let Some(w) = &world {
        put_exec(w, &r.join("bin/hydo-world"))?;
        fsx::write_text(&r.join("etc/hydo/world.json"), &shell::world_config(&m))?;
    }
    let mut mods: Vec<&str> = Vec::new();
    if want_tc {
        mods.extend(NETMODS);
    }
    if iptables.is_some() {
        mods.extend(FCMODS);
    }
    if netns {
        mods.extend(NSMODS);
    }
    for md in mods {
        let mr = modroot.clone().unwrap_or_default();
        let f = Command::new("find")
            .arg(&mr)
            .arg("-name")
            .arg(format!("{md}.ko.xz"))
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Fail::Exit(format!("hydo-build: cannot run find: {e}")))?;
        let mut src: Vec<String> = String::from_utf8_lossy(&f.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        if src.is_empty() {
            return Err(Fail::Exit(format!(
                "hydo-build: kernel module {md}.ko.xz not under {mr}"
            )));
        }
        src.sort();
        let dst = r.join("lib/modules").join(format!("{md}.ko"));
        let fo = std::fs::File::create(&dst).map_err(|e| io_fail("cannot write", &dst, e))?;
        let st = Command::new("xz")
            .arg("-dc")
            .arg(&src[0])
            .stdout(fo)
            .status()
            .map_err(|e| Fail::Exit(format!("hydo-build: cannot run xz: {e}")))?;
        if !st.success() {
            return Err(Fail::Exit(format!(
                "hydo-build: xz -dc {} failed ({})",
                src[0],
                image::exit_text(&st)
            )));
        }
    }
    stage_files(&m, manifest_path, &r, &|s| store.view(s).exists())?;
    default_etc(&r, &shell::default_hosts(&m))?;
    let tools = ShellTools {
        tc: if want_tc {
            format!("{}/bin/tc", iproute2.clone().unwrap_or_default())
        } else {
            String::new()
        },
        fc: iptables.as_ref().map(|i| {
            (
                "/bin/hydo_fc".to_string(),
                format!("{i}/bin/iptables-legacy"),
            )
        }),
        ip: netns.then(|| format!("{}/bin/ip", iproute2.clone().unwrap_or_default())),
    };
    write_compiled(ctx, p, manifest_path, &r, &tools)?;
    let (bin, sha) = crate::agent::binary(ctx, bdir, false, io)?;
    crate::agent::install(&bin, &sha, &r, "/sbin/init", io)?;
    crate::record::write_inside(&r, "x86_64", false)?;
    crate::stage::compose_stage(&m, &r)?;
    let oci_meta = crate::oci::stage(
        &mut p.oci,
        &r,
        Path::new(&busybox),
        &bdir.join(format!("{out}-oci-tmp")),
        netns,
        io,
    )?;
    fsx::fix_mtimes(&r)?;

    let ext4 = bdir.join(format!("{out}-ext4.img"));
    let data = bdir.join(format!("{out}-data.img"));
    let disk = bdir.join(format!("{out}-disk.img"));
    let size = pyv::get(rootsec, "size_mib")?
        .map(|v| pyv::int(v, "root.size_mib"))
        .transpose()?
        .unwrap_or(0);
    let rmib: i128 = if size != 0 {
        size
    } else {
        let du = run_ok(Command::new("du").arg("-sm").arg(&r), "du -sm")?;
        let used: i128 = String::from_utf8_lossy(&du.stdout)
            .split_whitespace()
            .next()
            .and_then(|x| x.parse().ok())
            .ok_or_else(|| Fail::Exit("hydo-build: du -sm printed no size".into()))?;
        64.max((used * 13 / 10 + 63) / 64 * 64)
    };
    let name = py::str_of(&m["name"]);
    let mk = MkfsTools {
        mke2fs: mke2fs.clone(),
        faketime: faketime.clone(),
    };
    image::mkfs(
        ctx,
        &mk,
        &ext4,
        rmib as u64,
        &image::uuid_from(&name, "root"),
        &image::uuid_from(&name, "root-hash"),
        Some(&r),
        false,
    )?;
    if !oci_meta.is_empty() {
        crate::oci::fix_ext4(
            &oci_meta,
            &ctx.inputs.debugfs,
            &mke2fs,
            &faketime,
            &ext4,
            io,
        )?;
    }
    image::zero_times(&ext4, false)?;
    let dsec = section(&m, "data")?;
    let dmib = pyv::get(dsec, "size_mib")?
        .map(|v| pyv::int(v, "data.size_mib"))
        .transpose()?
        .unwrap_or(512);
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
    let (p1, n1) = (2048u64, rmib as u64 * 2048);
    let (p2, n2) = (p1 + n1, dmib as u64 * 2048);
    image::assemble_disk(
        &disk,
        p2 + n2,
        &[(p1 as u32, n1 as u32), (p2 as u32, n2 as u32)],
        &[(&ext4, p1), (&data, p2)],
    )?;
    outln(
        io,
        &format!(
            "hydo-build: {} (root {} MiB, data {} MiB)",
            disk.display(),
            rmib,
            dmib
        ),
    );
    image::print_shas(io, &[&ext4, &data, &disk])?;
    crate::record::write_sidecar(&disk, None, "x86_64", false, manifest_path, &p.oci, io)
}
