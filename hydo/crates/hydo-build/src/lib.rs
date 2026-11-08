//! hydo-build: compile a project manifest (hydo.yaml, see hydo/onboard/MANIFEST.md) into a hydo
//! guest image. The Rust port of hydo/onboard/hydo-build (x86_64), hydo_build_arm64.py (arm64),
//! hydo_oci.py (services from Docker/OCI images) and the release-time build patches that were in
//! hydo_bridge.py (`build_linux`, `build_mac`). It starts from [`hydo_manifest`] (reading,
//! `--set`, the composer's plan, validation) and gave the same `--check`, `--print-shell` and
//! `--print-world` output, the same refusals and the same image bytes.
//!
//! Since Milestone 136 every image's first process is the guest agent hydo-agent (`agent.rs`),
//! driven by the host controller (hydo-control); the shell init (`hydo-init.sh`, its spliced
//! `hydo-init-*.sh` extensions and the guest composer `hydo-composer.sh`) and `--print-init`
//! are gone. The compiled manifest (`/etc/hydo/manifest.sh`, `prelude.sh`) stays: the controller
//! reads it out of the image and runs the project's commands through it.
//!
//! It runs the same external tools the Python did: mke2fs and debugfs under the pinned
//! libfaketime (or, on the Mac release, mke2fs inside a helper VM), busybox (its applet list),
//! `cp -a` for store closures, `du`, `find` and `xz` for kernel modules, `nix-store -qR` for
//! `root.nix:` closures.
//!
//! Where the Python stopped with a traceback, this port stops with a plain message
//! ([`Fail::Crash`]); docs/HISTORY.md (Milestone 103) lists those cases.

pub mod agent;
pub mod arm64;
pub mod cpio;
pub mod elfdeps;
pub mod fsx;
pub mod image;
pub mod oci;
pub mod pyjson;
pub mod pyv;
pub mod record;
pub mod shell;
pub mod stage;
pub mod tarball;
pub mod tls;
pub mod x86;

use hydo_manifest::py;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Whether deflate is zlib's own (needed for byte-identical arm64 initramfs files). The static
/// musl build (the Linux release's `hydo`) has only the pure-Rust deflate.
pub const HAVE_ZLIB: bool = cfg!(not(target_env = "musl"));

/// How a build ends early.
#[derive(Debug)]
pub enum Fail {
    /// A ManifestError: printed as `hydo-build: <msg>`, exit status 1.
    Manifest(String),
    /// A SystemExit: the message is printed as it is, exit status 1.
    Exit(String),
    /// The release CLIs' die(): `hydo: <msg>`, exit status 1.
    Die(String),
    /// Where the Python hydo-build stopped with a traceback: `hydo-build: <msg>`, exit status 1.
    Crash(String),
}

impl Fail {
    pub fn message(&self) -> String {
        match self {
            Fail::Manifest(m) | Fail::Exit(m) | Fail::Die(m) | Fail::Crash(m) => m.clone(),
        }
    }
}

/// The release a build runs in: its patches (bundled store, owners, the Mac's helper VM).
#[derive(Clone, Debug)]
pub enum Release {
    /// A plain `hydo-build` (the repository's tools and the host's /nix/store).
    None,
    /// The Linux release at `root`: guest/store with guest/store-closures.txt stands in for the
    /// pinned /nix/store paths, and every root inode is owned by 0:0 (HYDO_KEEP_OWNERS=1 skips it).
    Linux { root: PathBuf },
    /// The Mac release at `root`: mke2fs runs in a helper VM (`<engine> hvm run`: the Mac engine
    /// `hydo-hvm`, signed with the hypervisor entitlement; until M147 the C `hv/hvm/hvm`), the
    /// aarch64 tc closure comes from guest/tc with guest/tc-closure.txt.
    Mac { root: PathBuf, engine: PathBuf },
}

/// What a build needs besides its arguments.
#[derive(Clone, Debug)]
pub struct Ctx {
    /// The product's onboard/ directory (MANIFEST.md; ../history/hydo_hist.sh;
    /// ../VERSION for a plan's hydo_version:).
    pub onboard: PathBuf,
    pub release: Release,
    /// The build's inputs: the tools and guest programs to stage, read from the configuration
    /// (`hydo_config::build::Build`; the `hydo` command fills the release's paths in).
    pub inputs: hydo_config::build::Build,
}

impl Ctx {
    /// The onboard/ directory of a release laid out around the running program, else of the
    /// source tree it was built in (as hydo_manifest::LoadOptions::default()); `HYDO_ONBOARD`
    /// overrides it.
    pub fn standalone(inputs: hydo_config::build::Build) -> Ctx {
        let onboard = inputs
            .onboard
            .clone()
            .unwrap_or_else(|| hydo_manifest::LoadOptions::default().onboard_dir);
        Ctx {
            onboard,
            release: Release::None,
            inputs,
        }
    }
}

/// Where the output goes (the command's stdout and stderr, or buffers in tests).
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

pub fn outln(io: &mut Io, s: &str) {
    let _ = writeln!(io.out, "{s}");
    let _ = io.out.flush();
}

pub fn errln(io: &mut Io, s: &str) {
    let _ = writeln!(io.err, "{s}");
    let _ = io.err.flush();
}

/// Python's text read: UTF-8, universal newlines.
pub fn read_text(p: &Path) -> Result<String, Fail> {
    let b = std::fs::read(p).map_err(|e| fsx::io_fail("cannot read", p, e))?;
    let t = String::from_utf8(b)
        .map_err(|_| Fail::Crash(format!("{}: not UTF-8 text", p.display())))?;
    Ok(t.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", s)`.
pub fn fullmatch_ident(s: &str) -> bool {
    let mut c = s.chars();
    match c.next() {
        Some(f) if f.is_ascii_alphabetic() || f == '_' => {}
        _ => return false,
    }
    c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// Python's `str.splitlines()`.
pub fn splitlines(t: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b: Vec<(usize, char)> = t.char_indices().collect();
    let mut i = 0;
    while i < b.len() {
        let (pos, c) = b[i];
        let brk = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if brk {
            out.push(&t[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' && i + 1 < b.len() && b[i + 1].1 == '\n' {
                next += 1;
                i += 1;
            }
            start = next;
        }
        i += 1;
    }
    if start < t.len() {
        out.push(&t[start..]);
    }
    out
}

/// `os.path.abspath` of a path.
pub fn abspath(p: &Path) -> PathBuf {
    PathBuf::from(py::abspath(&p.to_string_lossy()))
}

static UNIQ: AtomicU64 = AtomicU64::new(0);

pub fn unique() -> u64 {
    UNIQ.fetch_add(1, Ordering::SeqCst)
}

/// `tempfile.mkdtemp(prefix=...)`.
pub fn tempdir(prefix: &str) -> Result<PathBuf, Fail> {
    for _ in 0..100 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!(
            "{prefix}{}-{}-{nanos:x}",
            std::process::id(),
            unique()
        ));
        if std::fs::create_dir(&d).is_ok() {
            return Ok(d);
        }
    }
    Err(Fail::Crash("cannot create a temporary directory".into()))
}

/// `tool(env, default, check)`: the input when set and not empty, else default; must pass
/// `check`. Messages name the input's variable.
pub fn tool(
    input: &hydo_config::build::Input,
    default: &str,
    check: impl Fn(&str) -> bool,
) -> Result<String, Fail> {
    let env = input.var;
    let p = input.or(default);
    if !check(&p) {
        return Err(Fail::Exit(format!(
            "hydo-build: {env} not found at {p} (set {env})"
        )));
    }
    Ok(p)
}

pub const HELP: &str = "hydo-build -- compile a project manifest (hydo.yaml, see MANIFEST.md) into a hydo guest image.

    hydo-build <hydo.yaml> [--set KEY=VALUE ...] [--out NAME] [--build-dir DIR] [--arch x86_64|arm64]
               [--check] [--print-shell] [--print-world]

Produces, in --build-dir (default ./build):
    <NAME>-ext4.img   read-only ext4 root (p1): the project's files + closures, busybox, the
                      guest agent hydo-agent as /sbin/init, the compiled manifest in /etc/hydo/
    <NAME>-data.img   journaled ext4 data disk (p2), mounted on /data: the fault target
    <NAME>-disk.img   both behind a hand-made MBR -- the file the harnesses take
NAME defaults to explore-ob-<project name> (the \"explore\" prefix: hvx_explore serves it).
The image's first process is the guest agent hydo-agent (x86_64: /sbin/init; arm64: /init), and
/etc/hydo/control (\"agent <protocol>\") tells the engines to boot it under the host's lock-step
control: the host controller runs the services, the workload, the faults and the checks. (Until
Milestone 136 the default was a shell init; --control agent is still accepted and changes nothing.)
--arch arm64 builds <NAME>-arm64.cpio.gz instead, one initramfs for the Mac VMM
(hydo-hvm), and <NAME>-arm64-disk.img, the data disk.

Byte-reproducible like the hand-written image scripts (build-stack.sh): pinned mke2fs run
under a pinned libfaketime, fixed UUIDs and hash seeds derived from the project name,
non-lazy init, every superblock and inode time zeroed afterwards, all source mtimes fixed, and
the frozen clock read in a fixed time zone (UTC+4), so the machine's zone does not matter.

Host tools (override with the environment variables):
    HYDO_BUSYBOX  static busybox          HYDO_SEEDRNG   prebuilt static hvx_seedrng
    HYDO_MKE2FS   e2fsprogs mke2fs        HYDO_FAKETIME  libfaketime's faketime
    HYDO_IPROUTE2 iproute2 store path     HYDO_MODROOT   guest kernel module tree
    HYDO_READ_BIN hydo-read (hydo/crates/hydo-read/build.sh), staged only when a command calls
                  hydo-read or hydo-check
    HYDO_SQL_BIN  hydo-sql (hydo/crates/hydo-sql/build.sh), staged only when a command calls
                  hydo-sql (arm64: HYDO_SQL_BIN_ARM64, <build dir>/hydo-sql-arm64)
    HYDO_WORLDBIN hydo-world (hydo/crates/hydo-world/build.sh), staged only for fakes:/proxies:
    HYDO_AGENT_BIN the pinned hydo-agent (hydo/crates/hydo-agent/build.sh)
                  (default <build dir>/hydo-agent; arm64: HYDO_AGENT_BIN_ARM64, <build dir>/hydo-agent-arm64)
--arch arm64 takes the arm64 builds of the last two from HYDO_READ_BIN_ARM64 and
HYDO_WORLDBIN_ARM64 (default <build dir>/hydo-read-arm64 and hydo-world-arm64).
HYDO_ONBOARD names the onboard/ directory (../history/hydo_hist.sh, ../VERSION); the default is
the release's or the source tree's.
";

/// What the command line asked for.
struct Args {
    path: String,
    sets: Vec<(String, String)>,
    out: Option<String>,
    build_dir: String,
    arch: String,
    flags: Vec<String>,
}

fn parse_args(a: &[String], io: &mut Io) -> Result<Args, i32> {
    let mut r = Args {
        path: a[0].clone(),
        sets: Vec::new(),
        out: None,
        build_dir: "build".into(),
        arch: "x86_64".into(),
        flags: Vec::new(),
    };
    let mut i = 1;
    while i < a.len() {
        let x = a[i].as_str();
        if x == "--set" && i + 1 < a.len() && a[i + 1].contains('=') {
            let (k, v) = a[i + 1].split_once('=').unwrap_or_default();
            r.sets.push((k.to_string(), v.to_string()));
            i += 2;
        } else if x == "--control" {
            if let Err(m) = agent::parse_control(a.get(i + 1).map_or("", String::as_str)) {
                errln(io, &m);
                return Err(2);
            }
            i += 2;
        } else if x == "--out" || x == "--build-dir" || x == "--arch" {
            let Some(v) = a.get(i + 1) else {
                errln(io, &format!("hydo-build: {x} needs a value"));
                return Err(2);
            };
            match x {
                "--out" => r.out = Some(v.clone()),
                "--build-dir" => r.build_dir = v.clone(),
                _ => r.arch = v.clone(),
            }
            i += 2;
        } else if x == "--print-init" {
            errln(
                io,
                "hydo-build: --print-init was removed in Milestone 136: images have no shell init (their first process is hydo-agent, driven by the host)",
            );
            return Err(2);
        } else if ["--check", "--print-shell", "--print-world"].contains(&x) {
            r.flags.push(x.to_string());
            i += 1;
        } else {
            errln(io, &format!("hydo-build: unknown argument {x}"));
            return Err(2);
        }
    }
    Ok(r)
}

fn report(io: &mut Io, f: &Fail) -> i32 {
    match f {
        Fail::Manifest(m) => errln(io, &format!("hydo-build: {m}")),
        Fail::Crash(m) => errln(io, &format!("hydo-build: {m}")),
        Fail::Exit(m) => errln(io, m),
        Fail::Die(m) => errln(io, &format!("hydo: {m}")),
    }
    1
}

/// A manifest error from hydo-manifest, with its own words where the Python stopped with a
/// traceback.
fn manifest_fail(e: &hydo_manifest::ManifestError) -> Fail {
    match e.message.strip_prefix(py::CRASH_PREFIX) {
        Some(rest) => Fail::Crash(format!("the manifest cannot be read: {rest}")),
        None => Fail::Manifest(e.message.clone()),
    }
}

/// A loaded, image-prepared manifest: the normalised data as hydo-build held it after
/// `_oci_prepare`, with the plan's meta in `_compose`.
pub struct Prepared {
    pub data: serde_json::Value,
    pub loaded: hydo_manifest::Loaded,
    pub oci: Vec<oci::OciService>,
}

/// hydo-build's `main()` up to the flags: read, `--set`, the composer's plan, validate, the
/// architecture checks, the service order and the image services. Warnings are printed as
/// hydo-build printed them, also before a refusal.
pub fn prepare(
    ctx: &Ctx,
    path: &str,
    arch: &str,
    sets: &[(String, String)],
    io: &mut Io,
) -> Result<Prepared, Fail> {
    let opts = hydo_manifest::LoadOptions {
        arch: arch.to_string(),
        sets: sets.to_vec(),
        onboard_dir: ctx.onboard.clone(),
        version_check_warn: ctx.inputs.version_check_warn,
    };
    let (r, warnings) = hydo_manifest::load_until_images(Path::new(path), &opts);
    for w in &warnings {
        errln(io, &format!("hydo-build: warning: {w}"));
    }
    let loaded = r.map_err(|e| manifest_fail(&e))?;
    let mut data = loaded.manifest.data.clone();
    let oarch = if arch == "x86_64" || arch == "arm64" {
        arch
    } else {
        "x86_64"
    };
    let oci = oci::prepare(&mut data, path, oarch, io)?;
    // the project model (hydo-manifest model.rs) after the image reader's own checks
    hydo_manifest::check_document(&loaded).map_err(|e| manifest_fail(&e))?;
    Ok(Prepared { data, loaded, oci })
}

/// The `--print-shell` placeholders for the tool paths.
pub fn print_shell_tools(m: &serde_json::Value) -> shell::ShellTools {
    shell::ShellTools {
        tc: "/nix/store/...-iproute2/bin/tc".into(),
        fc: shell::has_freeze(m).then(|| {
            (
                "/bin/hydo_fc".to_string(),
                "/nix/store/...-iptables/bin/iptables-legacy".to_string(),
            )
        }),
        ip: shell::has_netns(m).then(|| "/nix/store/...-iproute2/bin/ip".to_string()),
    }
}

/// hydo-build's command line: `hydo-build <hydo.yaml> [--set K=V ...] [--out NAME]
/// [--build-dir DIR] [--arch x86_64|arm64] [--check] [--print-shell] [--print-world]`. Returns
/// the exit status.
pub fn main_with(ctx: &Ctx, argv: &[String], io: &mut Io) -> i32 {
    if argv.is_empty() || argv[0] == "-h" || argv[0] == "--help" {
        outln(io, HELP);
        return 0;
    }
    let a = match parse_args(argv, io) {
        Ok(a) => a,
        Err(rc) => return rc,
    };
    let p = match prepare(ctx, &a.path, &a.arch, &a.sets, io) {
        Ok(p) => p,
        Err(f) => return report(io, &f),
    };
    let m = &p.data;
    let order = &p.loaded.service_order;
    let has = |f: &str| a.flags.iter().any(|x| x == f);
    if has("--print-shell") {
        return match shell::compile_shell(ctx, m, order, &print_shell_tools(m)) {
            Ok((ms, pre)) => {
                let _ = write!(io.out, "{ms}\n# ---- prelude.sh\n{pre}\n");
                let _ = io.out.flush();
                0
            }
            Err(f) => report(io, &f),
        };
    }
    if has("--print-world") {
        let t = if shell::has_world(m) {
            shell::world_config(m)
        } else {
            "{}\n".to_string()
        };
        let _ = io.out.write_all(t.as_bytes());
        let _ = io.out.flush();
        return 0;
    }
    if has("--check") {
        for line in hydo_manifest::check_summary(&a.path, &p.loaded) {
            outln(io, &line);
        }
        return 0;
    }
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| format!("explore-ob-{}", py::str_of(&m["name"])));
    if let Err(e) = std::fs::create_dir_all(&a.build_dir) {
        return report(
            io,
            &fsx::io_fail("cannot create", Path::new(&a.build_dir), e),
        );
    }
    let mut p = p;
    let r = if a.arch == "arm64" {
        arm64::build(ctx, &mut p, &a.path, &out, Path::new(&a.build_dir), io)
    } else if a.arch != "x86_64" {
        errln(io, "hydo-build: --arch is x86_64 or arm64");
        return 2;
    } else {
        x86::build(ctx, &mut p, &a.path, &out, Path::new(&a.build_dir), io)
    };
    match r {
        Ok(()) => 0,
        Err(f) => report(io, &f),
    }
}

/// [`main_with`] on the process's own stdout and stderr.
pub fn main(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut o = std::io::stdout();
    let mut e = std::io::stderr();
    let mut io = Io {
        out: &mut o,
        err: &mut e,
    };
    main_with(ctx, argv, &mut io)
}
