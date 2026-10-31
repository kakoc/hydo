//! The generator's command line (`hydo-grpc-gen`):
//!
//! ```text
//! hydo-grpc-gen <proto dir> [--hints hints.yaml] --out <dir>
//! hydo-grpc-gen --descriptor-set set.pb [--hints ...] --out <dir>
//! hydo-grpc-gen --reflect host:port [--hints ...] --out <dir>
//! ```
//!
//! It writes, under `<dir>`: `grpc/desc.pb` (the schema, deterministic bytes), `grpc/plan.json`
//! (RPCs + hints, checked against the schema), the static guest client `hydo-grpc` (and
//! `hydo-grpc.arm64` with `--arch amd64,arm64`), `workload/parallel_driver_<rpc>` (one composer
//! command per RPC) and `hydo.grpc.yaml` (a starter section for hydo.yaml).
//!
//! The clients are not compiled here: they are copied from `--client DIR` ($HYDO_GRPC_CLIENT),
//! which holds `hydo-grpc` for amd64 and `hydo-grpc.<arch>` for another architecture (a release
//! ships them; in a tree, hydo/onboard/hydo-grpc-gen builds them with hydo/grpcgen/build-client.sh).
//! $HYDO_GRPC_ARCH sets the default of `--arch`. See hydo/onboard/MANIFEST.md, "gRPC services".

use crate::desc::{self, FileBytes};
use crate::load;
use hydo_grpc::spec::Plan;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A failure: the message and the exit status (1, or 2 for a usage error).
pub struct Fail(pub String, pub i32);

fn die<T>(msg: impl Into<String>) -> Result<T, Fail> {
    Err(Fail(msg.into(), 1))
}

#[derive(Default)]
struct Args {
    hints: String,
    out: String,
    desc_set: String,
    reflect: String,
    target: String,
    arch: String,
    client: String,
    no_build: bool,
    rel: String,
    steps: i64,
    imports: Vec<String>,
    pos: Vec<String>,
}

const USAGE: &str = "usage: hydo-grpc-gen <proto dir> [--hints hints.yaml] --out <dir>   (or --descriptor-set / --reflect instead of <proto dir>)";

fn env_or(k: &str, d: &str) -> String {
    match std::env::var(k) {
        Ok(v) if !v.is_empty() => v,
        _ => d.to_string(),
    }
}

/// The flag listing (Go's flag.PrintDefaults layout).
pub fn usage() -> String {
    let arch_def = env_or("HYDO_GRPC_ARCH", "amd64");
    let client_def = std::env::var("HYDO_GRPC_CLIENT").unwrap_or_default();
    let mut s = format!("{USAGE}\n");
    let q = |d: &str| {
        if d.is_empty() {
            String::new()
        } else {
            format!(" (default {})", hydo_grpc::gofmt::quote(d))
        }
    };
    s += "  -I value\n    \textra import path for .proto files (repeatable)\n";
    s += &format!(
        "  -arch string\n    \tclient architectures to copy: amd64, arm64 or amd64,arm64{}\n",
        q(&arch_def)
    );
    s += &format!("  -client string\n    \tdirectory with the prebuilt clients (hydo-grpc, hydo-grpc.<arch>){}\n", q(&client_def));
    s += "  -descriptor-set string\n    \tread a compiled FileDescriptorSet instead of .proto files\n";
    s += "  -hints string\n    \thints file (YAML or JSON)\n";
    s += "  -no-build\n    \tdo not copy the client binary\n";
    s += "  -out string\n    \toutput directory (required)\n";
    s +=
        "  -reflect string\n    \task a running server (gRPC server reflection) for its services\n";
    s += "  -rel string\n    \tdirectory the starter section's paths are relative to (default: the parent of --out)\n";
    s += "  -steps int\n    \tworkload.steps in the starter section (default 30)\n";
    s += "  -target string\n    \tserver address in the guest (overrides the hints' target)\n";
    s
}

fn usage_fail(msg: Option<String>) -> Fail {
    let mut s = String::new();
    if let Some(m) = msg {
        s += &m;
        s.push('\n');
    }
    s += &usage();
    Fail(s.trim_end().to_string(), 2)
}

/// Go's flag package, applied repeatedly so that flags may follow the proto dir.
fn parse_args(argv: &[String]) -> Result<Args, Fail> {
    let mut a = Args {
        arch: env_or("HYDO_GRPC_ARCH", "amd64"),
        client: std::env::var("HYDO_GRPC_CLIENT").unwrap_or_default(),
        steps: 30,
        ..Default::default()
    };
    let mut i = 0;
    while i < argv.len() {
        let s = &argv[i];
        if s == "--" {
            // flag.Parse stops; the next argument is positional, then parsing goes on
            i += 1;
            if i < argv.len() {
                a.pos.push(argv[i].clone());
                i += 1;
            }
            continue;
        }
        if s.len() < 2 || !s.starts_with('-') {
            a.pos.push(s.clone());
            i += 1;
            continue;
        }
        let name = s.strip_prefix("--").unwrap_or(&s[1..]);
        if name.starts_with('-') || name.starts_with('=') {
            return Err(usage_fail(Some(format!("bad flag syntax: {s}"))));
        }
        let (name, val) = match name.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (name.to_string(), None),
        };
        if name == "h" || name == "help" {
            return Err(Fail(usage().trim_end().to_string(), 0));
        }
        if name == "no-build" {
            a.no_build = match val.as_deref() {
                None => true,
                Some(v) => hydo_grpc::gofmt::parse_bool(v).map_err(|_| {
                    usage_fail(Some(format!(
                        "invalid boolean value {} for -no-build: parse error",
                        hydo_grpc::gofmt::quote(v)
                    )))
                })?,
            };
            i += 1;
            continue;
        }
        let known = [
            "hints",
            "out",
            "descriptor-set",
            "reflect",
            "target",
            "arch",
            "client",
            "rel",
            "steps",
            "I",
        ];
        if !known.contains(&name.as_str()) {
            return Err(usage_fail(Some(format!(
                "flag provided but not defined: -{name}"
            ))));
        }
        let v = match val {
            Some(v) => v,
            None => {
                i += 1;
                match argv.get(i) {
                    Some(v) => v.clone(),
                    None => {
                        return Err(usage_fail(Some(format!("flag needs an argument: -{name}"))))
                    }
                }
            }
        };
        match name.as_str() {
            "hints" => a.hints = v,
            "out" => a.out = v,
            "descriptor-set" => a.desc_set = v,
            "reflect" => a.reflect = v,
            "target" => a.target = v,
            "arch" => a.arch = v,
            "client" => a.client = v,
            "rel" => a.rel = v,
            "I" => a.imports.push(v),
            _ => {
                a.steps = parse_int0(&v).ok_or_else(|| {
                    usage_fail(Some(format!(
                        "invalid value {} for flag -steps: parse error",
                        hydo_grpc::gofmt::quote(&v)
                    )))
                })?
            }
        }
        i += 1;
    }
    Ok(a)
}

/// strconv.ParseInt(s, 0, 64)
fn parse_int0(s: &str) -> Option<i64> {
    let (neg, b) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (base, d) = if let Some(d) = b.strip_prefix("0x").or_else(|| b.strip_prefix("0X")) {
        (16, d)
    } else if let Some(d) = b.strip_prefix("0b").or_else(|| b.strip_prefix("0B")) {
        (2, d)
    } else if let Some(d) = b.strip_prefix("0o").or_else(|| b.strip_prefix("0O")) {
        (8, d)
    } else if b.len() > 1 && b.starts_with('0') {
        (8, &b[1..])
    } else {
        (10, b)
    };
    let d: String = d.chars().filter(|&c| c != '_').collect();
    let v = i128::from_str_radix(&d, base).ok()?;
    let v = if neg { -v } else { v };
    i64::try_from(v).ok()
}

/// A composer command: its name, the rpc or scenario it drives, its message and timeout.
pub struct Command {
    pub name: String,
    pub target: String,
    pub msg: String,
    pub timeout: i64,
}

fn worst(r: &hydo_grpc::spec::Rpc) -> i64 {
    (r.deadline_ms + r.retry.backoff_ms) * (r.retry.attempts + 1)
}

/// One command per RPC with a weight and per scenario. The timeout bounds `drive.calls`
/// operations whose every attempt runs into its deadline.
pub fn commands(p: &Plan) -> Vec<Command> {
    let bound = |ms: i64| (p.drive.calls * ms / 1000 + 10).min(3600);
    let mut out = Vec::new();
    for r in &p.rpcs {
        if r.weight > 0 {
            out.push(Command {
                name: r.short.clone(),
                target: r.name.clone(),
                msg: format!("{}: generated requests", r.name),
                timeout: bound(worst(r)),
            });
        }
    }
    for sc in &p.scenarios {
        let mut ms = 0;
        for n in &sc.calls {
            if let Ok(i) = p.find(n) {
                ms += worst(&p.rpcs[i]);
            }
        }
        out.push(Command {
            name: sc.name.clone(),
            target: sc.name.clone(),
            msg: format!("scenario {}", sc.calls.join(" -> ")),
            timeout: bound(ms),
        });
    }
    out
}

/// The client's file name for one architecture: hydo-grpc for amd64, else hydo-grpc.<arch>.
pub fn client_file(arch: &str) -> String {
    if arch == "amd64" {
        "hydo-grpc".into()
    } else {
        format!("hydo-grpc.{arch}")
    }
}

/// The client the starter section stages: the amd64 one when it is copied, else the first one.
pub fn client_name(archs: &[String]) -> String {
    if archs.iter().any(|a| a == "amd64") {
        return client_file("amd64");
    }
    client_file(archs.first().map(String::as_str).unwrap_or("amd64"))
}

/// The starter section for hydo.yaml.
pub fn starter(p: &Plan, rel: &str, client: &str, steps: i64, retry_cmd: &str) -> String {
    let mut b = String::new();
    let mut w = |s: String| {
        b.push_str(&s);
        b.push('\n');
    };
    let sources = p.sources.clone().unwrap_or_default().join(", ");
    w(format!("# Generated by hydo-grpc-gen from {sources}."));
    w("# Merge into hydo.yaml; paths are relative to the directory hydo.yaml is in.".into());
    w("# RPCs (hydo-grpc list):".into());
    for r in &p.rpcs {
        w(format!(
            "#   {:<20} {} weight={} deadline={}ms retry={}",
            r.short, r.name, r.weight, r.deadline_ms, r.retry.attempts
        ));
    }
    w("root:".into());
    w("  files:".into());
    w(format!(
        "    - {{src: {rel}/{client}, dst: /bin/hydo-grpc, mode: \"0755\"}}"
    ));
    w(format!("    - {{src: {rel}/grpc, dst: /etc/hydo/grpc}}"));
    w(String::new());
    w("env:".into());
    w(format!("  HYDO_GRPC_SEED: \"{}\"           # requests: a function of this seed and the composer entry (HYDO_SEED) or the step", p.seed));
    w(String::new());
    w("# The test composer (MANIFEST.md \"The test composer\") composes these commands; the schedule".into());
    w(
        "# token W= decides which run when, and each entry's HYDO_SEED decides its requests."
            .into(),
    );
    w(format!(
        "# The same commands are files in {rel}/workload/ (the hydo/workload/ layout)."
    ));
    w("test:".into());
    w("  commands:".into());
    for c in commands(p) {
        w(format!("    - {{name: {}, kind: parallel_driver, run: [/bin/hydo-grpc, drive, {}], timeout: {}}}", c.name, c.target, c.timeout));
    }
    w(String::new());
    w("workload:".into());
    w(format!("  steps: {steps}                       # composer rounds; faults are placed at round numbers"));
    if !retry_cmd.is_empty() {
        w(format!("  retry: {retry_cmd}      # unknown outcomes, re-sent with the same request after the faults heal"));
    }
    w(String::new());
    // without the composer: numbered steps
    let mut worst_ms = 0;
    for r in &p.rpcs {
        if r.weight == 0 {
            continue;
        }
        worst_ms = worst_ms.max(worst(r));
    }
    let timeout = (steps * p.steps.calls * worst_ms / 1000 + 60).clamp(120, 3600);
    w("# Without the composer (numbered steps), drop test: and use instead:".into());
    w("# workload:".into());
    w(format!("#   steps: {steps}"));
    w(format!(
        "#   run: /bin/hydo-grpc steps $HYDO_FROM $HYDO_TO   # {} call(s) x {} worker(s) per step",
        p.steps.calls, p.steps.concurrency
    ));
    if !retry_cmd.is_empty() {
        w(format!("#   retry: {retry_cmd}"));
    }
    w(format!("#   timeout: {timeout}"));
    w("#   process: hydo-grpc".into());
    w(String::new());
    w("# Invariants stay yours. Call records: /tmp/hydo/grpc/calls (HYDO-HIST lines, history grpc).".into());
    w(
        "# hydo-grpc call <rpc> '<json>' prints a response and exits 0 ok / 1 failed / 2 unknown,"
            .into(),
    );
    w("# for read-backs in invariants. Outcomes of RPCs with outcome_id: also go to $HYDO_ACKED and $HYDO_OUTCOMES.".into());
    b
}

/// filepath.Clean
pub fn clean(p: &str) -> String {
    let abs = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            x => parts.push(x),
        }
    }
    let s = parts.join("/");
    match (abs, s.is_empty()) {
        (true, _) => format!("/{s}"),
        (false, true) => ".".into(),
        _ => s,
    }
}

/// filepath.Dir
fn dir(p: &str) -> String {
    match p.rfind('/') {
        Some(i) => clean(&p[..=i]),
        None => ".".into(),
    }
}

/// filepath.Rel (lexical)
pub fn rel(base: &str, targ: &str) -> Result<String, String> {
    let (b, t) = (clean(base), clean(targ));
    if b == t {
        return Ok(".".into());
    }
    let (ba, ta) = (b.starts_with('/'), t.starts_with('/'));
    if ba != ta {
        return Err(format!("Rel: can't make {targ} relative to {base}"));
    }
    let bs: Vec<&str> = if b == "." {
        vec![]
    } else {
        b.trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect()
    };
    let ts: Vec<&str> = if t == "." {
        vec![]
    } else {
        t.trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect()
    };
    let mut i = 0;
    while i < bs.len() && i < ts.len() && bs[i] == ts[i] {
        i += 1;
    }
    if bs[i..].contains(&"..") {
        return Err(format!("Rel: can't make {targ} relative to {base}"));
    }
    let mut parts: Vec<&str> = vec![".."; bs.len() - i];
    parts.extend(&ts[i..]);
    Ok(parts.join("/"))
}

fn write_file(path: &Path, data: &[u8], mode: u32) -> Result<(), Fail> {
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(mode)
        .open(path)
        .map_err(|e| {
            Fail(
                format!("open {}: {}", path.display(), hydo_grpc::io_msg(&e)),
                1,
            )
        })?;
    f.write_all(data).map_err(|e| {
        Fail(
            format!("write {}: {}", path.display(), hydo_grpc::io_msg(&e)),
            1,
        )
    })
}

fn mkdir_all(p: &Path) -> Result<(), Fail> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(p)
        .map_err(|e| {
            Fail(
                format!("mkdir {}: {}", p.display(), hydo_grpc::io_msg(&e)),
                1,
            )
        })
}

use std::os::unix::fs::DirBuilderExt;

fn base_name(p: &str) -> String {
    let c = clean(p);
    if c == "/" {
        return "/".into();
    }
    c.rsplit('/').next().unwrap_or(&c).to_string()
}

/// The generator; `argv` without the program name. Returns the summary to print on stdout.
pub fn run(argv: &[String]) -> Result<String, Fail> {
    let a = parse_args(argv)?;
    if a.out.is_empty()
        || a.pos.len() > 1
        || a.pos.is_empty() == (a.desc_set.is_empty() && a.reflect.is_empty())
    {
        return Err(usage_fail(None));
    }
    let (files, sources): (Vec<FileBytes>, Vec<String>) = if a.pos.len() == 1 {
        let extra: Vec<PathBuf> = a.imports.iter().map(PathBuf::from).collect();
        let (set, names) =
            desc::from_proto_dir(Path::new(&a.pos[0]), &extra).map_err(|e| Fail(e, 1))?;
        (set, names.iter().map(|n| format!("proto:{n}")).collect())
    } else if !a.desc_set.is_empty() {
        let set = desc::from_descriptor_set(Path::new(&a.desc_set)).map_err(|e| Fail(e, 1))?;
        (
            set,
            vec![format!("descriptor-set:{}", base_name(&a.desc_set))],
        )
    } else {
        let set = crate::reflect::from_reflection(&a.reflect, Duration::from_secs(10))
            .map_err(|e| Fail(e, 1))?;
        (set, vec![format!("reflection:{}", a.reflect)])
    };
    let set_bytes = desc::encode_set(&files);
    let pool = prost_reflect::DescriptorPool::decode(set_bytes.as_slice())
        .map_err(|e| Fail(format!("schema: {e}"), 1))?;
    let mut h = load::read_hints(&a.hints).map_err(|e| Fail(e, 1))?;
    if !a.target.is_empty() {
        h.target = a.target.clone();
    }
    let plan = load::build(&pool, &h, Some(sources)).map_err(|e| Fail(format!("hints: {e}"), 1))?;

    let out = Path::new(&a.out);
    mkdir_all(&out.join("grpc"))?;
    mkdir_all(&out.join("workload"))?;
    write_file(&out.join("grpc").join("desc.pb"), &set_bytes, 0o644)?;
    write_file(
        &out.join("grpc").join("plan.json"),
        plan.marshal().as_bytes(),
        0o644,
    )?;
    if let Ok(rd) = fs::read_dir(out.join("workload")) {
        for e in rd.flatten() {
            let p = e.path();
            if fs::remove_file(&p).is_err() {
                let _ = fs::remove_dir(&p);
            }
        }
    }
    for c in commands(&plan) {
        let body = format!("#!/bin/busybox sh\n# hydo: timeout={}\n# hydo-msg: {} (hydo-grpc-gen)\nexec /bin/hydo-grpc drive {}\n", c.timeout, c.msg, c.target);
        write_file(
            &out.join("workload")
                .join(format!("parallel_driver_{}", c.name)),
            body.as_bytes(),
            0o755,
        )?;
    }
    let retry_cmd = if plan.rpcs.iter().any(|r| r.retry.at_end) {
        "/bin/hydo-grpc retry"
    } else {
        ""
    };
    let base = if a.rel.is_empty() {
        dir(&clean(&a.out))
    } else {
        a.rel.clone()
    };
    let rel_out = rel(&base, &a.out).map_err(|e| Fail(e, 1))?;
    let archs: Vec<String> = a.arch.split(',').map(str::to_string).collect();
    write_file(
        &out.join("hydo.grpc.yaml"),
        starter(&plan, &rel_out, &client_name(&archs), a.steps, retry_cmd).as_bytes(),
        0o644,
    )?;

    if !a.no_build {
        if a.client.is_empty() {
            return die("--client (or HYDO_GRPC_CLIENT) must name a directory with the prebuilt clients (hydo-grpc, hydo-grpc.<arch>; hydo/grpcgen/build-client.sh builds them), or pass --no-build");
        }
        for arch in &archs {
            let name = client_file(arch);
            let src = Path::new(&a.client).join(&name);
            let b = fs::read(&src).map_err(|e| {
                Fail(
                    format!(
                        "no prebuilt client for {} in {} (open {}: {})",
                        arch,
                        a.client,
                        src.display(),
                        hydo_grpc::io_msg(&e)
                    ),
                    1,
                )
            })?;
            let dst = out.join(&name);
            write_file(&dst, &b, 0o755)?;
            let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o755));
        }
    }
    let mut s = format!(
        "hydo-grpc-gen: {} RPCs, {} scenarios -> {}\n",
        plan.rpcs.len(),
        plan.scenarios.len(),
        a.out
    );
    for r in &plan.rpcs {
        s += &format!("  {:<22} {} weight={}\n", r.short, r.name, r.weight);
    }
    Ok(s)
}
