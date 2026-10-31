//! hydo-grpc: the generic gRPC driver that runs inside a hydo guest. It reads the project's
//! schema and plan (written by hydo-grpc-gen) and has one subcommand per job:
//!
//! ```text
//! hydo-grpc steps FROM TO        workload steps [FROM, TO) (manifest workload.run)
//! hydo-grpc retry                re-send operations whose outcome is unknown (workload.retry)
//! hydo-grpc drive <rpc> [N]      N operations of one RPC or scenario (a composer command;
//!                                exit 0 all ok, 1 one ended failed or unknown)
//! hydo-grpc call <rpc> [JSON]    one call with the given request (JSON, or "-" = stdin; none =
//!                                generated); prints the response; exit 0 ok, 1 definite
//!                                failure, 2 unknown outcome (as invariants use)
//! hydo-grpc gen <rpc> [N]        print N generated requests (no call)
//! hydo-grpc list                 the RPCs and scenarios of the plan
//! ```
//!
//! Files: $HYDO_GRPC_DIR (default /etc/hydo/grpc) holds desc.pb and plan.json; state shared by
//! the processes of one run (op counter, value pools, the calls record) lives in
//! $HYDO_GRPC_STATE (default /tmp/hydo/grpc). Seed: --seed, else $HYDO_GRPC_SEED, else the file
//! $HYDO_GRPC_STATE/seed, else hydo_grpc_seed= on the kernel command line, else the plan's.
//! $HYDO_GRPC_TARGET overrides the plan's target address.

use hydo_grpc::client::{Client, GenEnv, Options};
use hydo_grpc::outcome::{code_name, Kind};
use hydo_grpc::pjson;
use hydo_grpc::rng::{derive, R};
use hydo_grpc::valuegen::GenError;
use std::io::Read;
use std::process::exit;
use std::sync::Arc;

/// The client thread's stack.
const STACK: usize = 256 << 20;

fn env_or(k: &str, d: &str) -> String {
    match std::env::var(k) {
        Ok(v) if !v.is_empty() => v,
        _ => d.to_string(),
    }
}

fn die(msg: String) -> ! {
    eprintln!("hydo-grpc: {msg}");
    exit(3);
}

/// `strconv.ParseUint(s, 0, 64)`: decimal, 0x hex, 0o / leading-0 octal, 0b binary, underscores.
fn parse_uint0(s: &str) -> Result<u64, String> {
    let err = |what: &str| {
        format!(
            "strconv.ParseUint: parsing {}: {}",
            hydo_grpc::gofmt::quote(s),
            what
        )
    };
    let (base, body) = if let Some(b) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16, b)
    } else if let Some(b) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        (2, b)
    } else if let Some(b) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        (8, b)
    } else if s.len() > 1 && s.starts_with('0') {
        (8, &s[1..])
    } else {
        (10, s)
    };
    let digits: String = body.chars().filter(|&c| c != '_').collect();
    if digits.is_empty() || s.starts_with('_') || s.ends_with('_') || s.contains("__") {
        return Err(err("invalid syntax"));
    }
    match u64::from_str_radix(&digits, base) {
        Ok(v) => Ok(v),
        Err(e) if matches!(e.kind(), std::num::IntErrorKind::PosOverflow) => {
            Err(err("value out of range"))
        }
        Err(_) => Err(err("invalid syntax")),
    }
}

fn seed_from(flag: &str, state: &str) -> Option<u64> {
    let try_ = |s: &str| -> Option<u64> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        match parse_uint0(s) {
            Ok(v) => Some(v),
            Err(e) => die(format!("seed {}: {}", hydo_grpc::gofmt::quote(s), e)),
        }
    };
    if let Some(v) = try_(flag) {
        return Some(v);
    }
    if let Some(v) = try_(&std::env::var("HYDO_GRPC_SEED").unwrap_or_default()) {
        return Some(v);
    }
    if let Ok(b) = std::fs::read_to_string(format!("{state}/seed")) {
        if let Some(v) = try_(&b) {
            return Some(v);
        }
    }
    if let Ok(b) = std::fs::read_to_string("/proc/cmdline") {
        for t in b.split_whitespace() {
            if let Some(v) = t.strip_prefix("hydo_grpc_seed=") {
                return try_(v);
            }
        }
    }
    None
}

struct Flags {
    dir: String,
    state: String,
    seed: String,
    calls: String,
}

fn usage(f: &Flags) {
    let calls_default = std::env::var("HYDO_GRPC_CALLS").unwrap_or_default();
    let mut s = String::from("usage: hydo-grpc [flags] steps FROM TO | retry | drive <rpc> [N] | call <rpc> [JSON|-] | gen <rpc> [N] | list\n");
    let line = |name: &str, help: &str, def: &str| {
        let d = if def.is_empty() {
            String::new()
        } else {
            format!(" (default {})", hydo_grpc::gofmt::quote(def))
        };
        format!("  -{name} string\n    \t{help}{d}\n")
    };
    s += &line(
        "calls",
        "calls record file (default <state>/calls)",
        &calls_default,
    );
    s += &line(
        "dir",
        "directory with desc.pb and plan.json",
        &env_or("HYDO_GRPC_DIR", "/etc/hydo/grpc"),
    );
    s += &line(
        "seed",
        "seed (default: $HYDO_GRPC_SEED, <state>/seed, hydo_grpc_seed=, the plan's)",
        "",
    );
    s += &line(
        "state",
        "state directory shared by one run's processes",
        &env_or("HYDO_GRPC_STATE", "/tmp/hydo/grpc"),
    );
    let _ = f;
    eprint!("{s}");
}

/// Go's flag package: -name value, -name=value, --name ...; stops at the first non-flag.
fn parse_flags(args: &[String]) -> (Flags, Vec<String>) {
    let mut f = Flags {
        dir: env_or("HYDO_GRPC_DIR", "/etc/hydo/grpc"),
        state: env_or("HYDO_GRPC_STATE", "/tmp/hydo/grpc"),
        seed: String::new(),
        calls: std::env::var("HYDO_GRPC_CALLS").unwrap_or_default(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.len() < 2 || !a.starts_with('-') {
            break;
        }
        if a == "--" {
            i += 1;
            break;
        }
        let name = a.trim_start_matches('-');
        let (name, val) = match name.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (name.to_string(), None),
        };
        if name == "h" || name == "help" {
            usage(&f);
            exit(0);
        }
        if !["dir", "state", "seed", "calls"].contains(&name.as_str()) {
            eprintln!("flag provided but not defined: -{name}");
            usage(&f);
            exit(2);
        }
        let v = match val {
            Some(v) => v,
            None => {
                i += 1;
                match args.get(i) {
                    Some(v) => v.clone(),
                    None => {
                        eprintln!("flag needs an argument: -{name}");
                        usage(&f);
                        exit(2);
                    }
                }
            }
        };
        match name.as_str() {
            "dir" => f.dir = v,
            "state" => f.state = v,
            "seed" => f.seed = v,
            _ => f.calls = v,
        }
        i += 1;
    }
    (f, args[i..].to_vec())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (f, args) = parse_flags(&argv);
    if args.is_empty() {
        usage(&f);
        exit(3);
    }
    // Generation recurses into nested and recursive messages as deep as the stream says (a map
    // of messages that hold the map is a branching process); Go's stacks grow to 1 GiB, so the
    // client runs on a thread with a large stack (address space only; pages come when touched).
    // A small guest may refuse a large mapping (the kernel's overcommit heuristic): try smaller.
    let job = std::sync::Arc::new(std::sync::Mutex::new(Some((f, args))));
    for size in [STACK, STACK / 4, STACK / 32] {
        let j = job.clone();
        let t = std::thread::Builder::new().stack_size(size).spawn(move || {
            let (f, args) = j
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .expect("one run");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(run(f, args))
        });
        if let Ok(t) = t {
            exit(t.join().unwrap_or(3));
        }
    }
    die("cannot start the client thread".into());
}

async fn run(f: Flags, args: Vec<String>) -> i32 {
    let seed = seed_from(&f.seed, &f.state);
    let mut c = match Client::new(Options {
        desc: format!("{}/desc.pb", f.dir),
        plan: format!("{}/plan.json", f.dir),
        state_dir: f.state.clone(),
        calls: f.calls.clone(),
        seed,
    }) {
        Ok(c) => c,
        Err(e) => die(e),
    };
    if let Ok(t) = std::env::var("HYDO_GRPC_TARGET") {
        if !t.is_empty() {
            c.plan.target = t;
        }
    }
    let calls_file = if f.calls.is_empty() {
        format!("{}/calls", f.state)
    } else {
        f.calls.clone()
    };
    let num = |i: usize, what: &str| -> i64 {
        let Some(a) = args.get(i) else {
            die(format!("{}: missing {}", args[0], what))
        };
        match hydo_grpc::gofmt::parse_int(a, 64) {
            Ok(n) => n,
            Err(e) => die(format!(
                "{}: {} {}: {}",
                args[0],
                what,
                hydo_grpc::gofmt::quote(a),
                e.replace("ParseInt", "Atoi")
            )),
        }
    };
    let c = Arc::new(c);
    match args[0].as_str() {
        "steps" => {
            let (a, b) = (num(1, "FROM"), num(2, "TO"));
            c.steps(a, b).await;
        }
        "retry" => {
            if let Err(e) = c.retry(&calls_file).await {
                die(format!("retry: {e}"));
            }
        }
        "drive" => {
            if args.len() < 2 {
                die("drive: which rpc?".into());
            }
            let n = if args.len() > 2 { num(2, "N") } else { 0 };
            match c.drive(&args[1], n).await {
                Err(e) => die(format!("drive: {e}")),
                Ok(false) => return 1, // a failed or unknown operation (the composer records it)
                Ok(true) => {}
            }
        }
        "call" => {
            if args.len() < 2 {
                die("call: which rpc?".into());
            }
            let ri = c.plan.find(&args[1]).unwrap_or_else(|e| die(e));
            let r = c.plan.rpcs[ri].clone();
            let reqs = if args.len() > 2 {
                let mut body = args[2].clone();
                if body == "-" {
                    let mut s = String::new();
                    let _ = std::io::stdin().read_to_string(&mut s);
                    body = s;
                }
                c.parse_json(&r, &body)
                    .unwrap_or_else(|e| die(format!("call: request: {e}")))
            } else {
                let mut rnd = R::new(derive(c.seed, &[3]));
                match c.generate(&r, &mut rnd, &mut GenEnv::default()) {
                    Ok(q) => q,
                    Err(e) => die(format!("call: {}", gen_err(&e))),
                }
            };
            let (res, resps) = c.call(&r, &reqs).await;
            match res.kind {
                Kind::Ok => {
                    println!("{}", pjson::json(&resps, r.server_streaming));
                    return 0;
                }
                Kind::Fail => {
                    println!("{} {}", code_name(res.code), res.msg);
                    return 1;
                }
                Kind::Unknown => {
                    println!("{} {}", code_name(res.code), res.msg);
                    return 2;
                }
            }
        }
        "gen" => {
            if args.len() < 2 {
                die("gen: which rpc?".into());
            }
            let ri = c.plan.find(&args[1]).unwrap_or_else(|e| die(e));
            let r = c.plan.rpcs[ri].clone();
            let n = if args.len() > 2 { num(2, "N") } else { 1 };
            let mut rnd = R::new(derive(c.seed, &[4]));
            let mut e = GenEnv::default();
            for _ in 0..n {
                match c.generate(&r, &mut rnd, &mut e) {
                    Ok(q) => println!("{}", pjson::json(&q, r.client_streaming)),
                    Err(err) => die(format!("gen: {}", gen_err(&err))),
                }
            }
        }
        "list" => {
            for r in &c.plan.rpcs {
                let kind = match (r.client_streaming, r.server_streaming) {
                    (true, true) => "bidi-stream",
                    (true, false) => "client-stream",
                    (false, true) => "server-stream",
                    _ => "unary",
                };
                println!(
                    "{:<24} {:<40} {} weight={} deadline={}ms retry={}",
                    r.short, r.name, kind, r.weight, r.deadline_ms, r.retry.attempts
                );
            }
            for s in &c.plan.scenarios {
                println!(
                    "{:<24} scenario: {} weight={}",
                    s.name,
                    s.calls.join(" -> "),
                    s.weight
                );
            }
        }
        _ => {
            usage(&f);
            return 3;
        }
    }
    0
}

fn gen_err(e: &GenError) -> String {
    e.to_string()
}
