//! The command line's side: `ctl` (one command to the running world over its control socket)
//! and the side-effect checks.
//!
//! ```text
//! check at-most-once     <ep>.<route> [--by ref|idem]       no group has two effects
//! check exactly-once     <ep>.<route> [--ref FMT] [--acked FILE]   at most once, and every
//!                                                            acknowledged id has its effect
//! check effect-for-acked <ep>.<route> [--ref FMT] [--acked FILE] [--unless event:<name>|<ep>.<route>]
//!                                                            every acknowledged id has its effect
//!                                                            (use as `eventually`), except the refs
//!                                                            that have an --unless record
//! check none-after       <ep>.<route> --after event:<name>|<ep>.<route>   no effect on a ref
//!                                                            after that ref's first <after> record
//! check count            <ep>.<route>                        print how many effects (always 0)
//! check calls            <proxy>[.<route>]                   count a gRPC proxy's calls by outcome
//! ```
//!
//! FMT turns an acknowledged id (one line of $HYDO_ACKED) into the ref the effect carries: {id}
//! is replaced by the id (default "{id}"). Exit status: 0 holds, 1 violated, 2 cannot be decided
//! (no world log, or a power cut's carried log was too long). The last line is a HYDO-ORACLE
//! line, which the generic init uses as the invariant's details.

use crate::goish;
use crate::world::{get, parse_record, Rec, CARRY_MAX};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub fn env(k: &str, d: &str) -> String {
    match std::env::var(k) {
        Ok(v) if !v.is_empty() => v,
        _ => d.to_string(),
    }
}

/// the control socket and the record log; the config can name others, the CLI reads these
pub fn sock_path() -> String {
    env("HYDO_WORLD_SOCK", "/tmp/hydo/world.sock")
}

pub fn log_path() -> String {
    env("HYDO_WORLD_LOG", "/tmp/hydo/world.log")
}

fn out(b: &[u8]) {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(b);
    let _ = o.flush();
}

/// One command to the running world; prints the answer.
pub fn ctl_main(args: &[Vec<u8>]) -> i32 {
    let p = sock_path();
    let mut c = match UnixStream::connect(&p) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hydo-world ctl: dial unix {p}: connect: {}", goish::errno_text(&e));
            return 1;
        }
    };
    let _ = c.set_write_timeout(Some(Duration::from_secs(5)));
    let mut line = args.join(&b" "[..]);
    line.push(b'\n');
    let _ = c.write_all(&line);
    let mut s = Vec::new();
    let _ = c.read_to_end(&mut s);
    if args[0] == b"dump" || args[0] == b"summary" {
        // one per line, without the END line
        if s.ends_with(b"\n") {
            s.pop();
        }
        if s.ends_with(b"END") {
            s.truncate(s.len() - 3);
        }
    }
    out(&s);
    if s.starts_with(b"error") {
        1
    } else {
        0
    }
}

fn read_log() -> Result<(Vec<Rec>, bool), String> {
    let p = log_path();
    let b = std::fs::read(&p).map_err(|e| goish::path_err("open", &p, &e))?;
    let mut recs = Vec::new();
    let mut trunc = false;
    for ln in b.split(|&c| c == b'\n') {
        let Some(r) = parse_record(ln) else { continue };
        if get(&r, "kind") == b"meta" && get(&r, "trunc") == b"1" {
            trunc = true;
        }
        recs.push(r);
    }
    recs.sort_by_key(|r| atoi(get(r, "seq")));
    Ok((recs, trunc))
}

fn atoi(s: &[u8]) -> i64 {
    std::str::from_utf8(s).ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn say(code: i32, msg: &[u8]) -> i32 {
    let word: &[u8] = match code {
        0 => b"OK",
        1 => b"VIOLATION",
        _ => b"ERROR",
    };
    out(&[b"HYDO-ORACLE: ", word, b" ", msg, b"\n"].concat());
    code
}

fn is_effect(r: &Rec, target: &[u8]) -> bool {
    if get(r, "fx") != b"1" {
        return false;
    }
    if let Some(ev) = target.strip_prefix(b"event:") {
        return get(r, "kind") == b"event" && get(r, "route") == ev;
    }
    let (ep, route) = match target.iter().position(|&c| c == b'.') {
        Some(p) => (&target[..p], &target[p + 1..]),
        None => (target, &b""[..]),
    };
    get(r, "kind") == b"req" && get(r, "ep") == ep && (route.is_empty() || get(r, "route") == route)
}

fn replace(s: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

fn acked_refs(file: &[u8], format: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let f = String::from_utf8_lossy(file).into_owned();
    let b = std::fs::read(&f).map_err(|e| goish::path_err("open", &f, &e))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for id in b.split(|c| c.is_ascii_whitespace()).filter(|t| !t.is_empty()) {
        if id.starts_with(b"w:") || id.contains(&b'=') || seen.contains(id) {
            continue;
        }
        seen.insert(id.to_vec());
        out.push(replace(format, b"{id}", id));
    }
    Ok(out)
}

fn join_ints(s: &[i64]) -> String {
    s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
}

pub fn check_main(args: &[Vec<u8>]) -> i32 {
    if args.len() < 2 {
        crate::usage();
    }
    let (kind, target) = (args[0].as_slice(), args[1].as_slice());
    let mut opt: BTreeMap<&[u8], Vec<u8>> = BTreeMap::new();
    opt.insert(b"--by", b"ref".to_vec());
    opt.insert(b"--ref", b"{id}".to_vec());
    opt.insert(b"--acked", std::env::var_os("HYDO_ACKED").map(|v| {
        use std::os::unix::ffi::OsStrExt;
        v.as_bytes().to_vec()
    }).unwrap_or_default());
    opt.insert(b"--after", Vec::new());
    opt.insert(b"--unless", Vec::new());
    let mut i = 2;
    while i + 1 < args.len() {
        let k = args[i].as_slice();
        match opt.get_mut(k) {
            Some(v) => *v = args[i + 1].clone(),
            None => return say(2, &[b"check: unknown option ", k].concat()),
        }
        i += 2;
    }
    let o = |k: &[u8]| opt[k].clone();
    let (recs, trunc) = match read_log() {
        Ok(x) => x,
        Err(e) => return say(2, &[target, b": no world log (", e.as_bytes(), b")"].concat()),
    };
    if trunc {
        return say(
            2,
            &[
                target,
                format!(": the world log carried across the power cut was too long (more than {CARRY_MAX} bytes); cannot decide").as_bytes(),
            ]
            .concat(),
        );
    }
    let by = o(b"--by");
    let by_s = String::from_utf8_lossy(&by).into_owned();
    let mut effects: BTreeMap<Vec<u8>, Vec<i64>> = BTreeMap::new();
    let mut order: Vec<Vec<u8>> = Vec::new();
    let mut n = 0usize;
    for r in &recs {
        if !is_effect(r, target) {
            continue;
        }
        n += 1;
        let g = get(r, &by_s).to_vec();
        if !effects.contains_key(&g) {
            order.push(g.clone());
        }
        effects.entry(g).or_default().push(atoi(get(r, "seq")));
    }
    let at_most_once = || -> Option<i32> {
        for g in &order {
            if g.is_empty() {
                continue; // no ref/key: cannot be grouped
            }
            let s = &effects[g];
            if s.len() > 1 {
                return Some(say(
                    1,
                    &[
                        target,
                        b": ",
                        &by,
                        b" ",
                        g,
                        format!(" took effect {} times (records {})", s.len(), join_ints(s)).as_bytes(),
                    ]
                    .concat(),
                ));
            }
        }
        None
    };
    let for_acked = || -> i32 {
        let refs = match acked_refs(&o(b"--acked"), &o(b"--ref")) {
            Ok(r) => r,
            Err(e) => return say(2, &[target, b": acknowledged ids: ", e.as_bytes()].concat()),
        };
        // --unless: refs with such a record owe no effect (a cancelled order)
        let mut exempt = BTreeSet::new();
        let unless = o(b"--unless");
        if !unless.is_empty() {
            for r in &recs {
                if is_effect(r, &unless) {
                    exempt.insert(get(r, "ref").to_vec());
                }
            }
        }
        for rf in &refs {
            if effects.get(rf).is_none_or(Vec::is_empty) && !exempt.contains(rf) {
                return say(1, &[target, b": acknowledged ", rf, b" has no effect"].concat());
            }
        }
        say(
            0,
            &[target, format!(": all {} acknowledged have their effect ({n} effects)", refs.len()).as_bytes()].concat(),
        )
    };
    match kind {
        b"count" => {
            return say(0, &[target, format!(": {n} effects, {} refs", order.len()).as_bytes()].concat())
        }
        b"at-most-once" => {
            if let Some(rc) = at_most_once() {
                return rc;
            }
            return say(0, &[target, format!(": {n} effects, every {by_s} at most once").as_bytes()].concat());
        }
        b"exactly-once" => {
            if let Some(rc) = at_most_once() {
                return rc;
            }
            return for_acked();
        }
        b"effect-for-acked" => return for_acked(),
        b"none-after" => {
            let after = o(b"--after");
            if after.is_empty() {
                return say(2, b"check none-after needs --after event:<name> or <ep>.<route>");
            }
            let mut first: BTreeMap<Vec<u8>, i64> = BTreeMap::new();
            for r in &recs {
                if is_effect(r, &after) && !get(r, "ref").is_empty() {
                    first.entry(get(r, "ref").to_vec()).or_insert_with(|| atoi(get(r, "seq")));
                }
            }
            for r in &recs {
                if is_effect(r, target) {
                    if let Some(&f) = first.get(get(r, "ref")) {
                        if atoi(get(r, "seq")) > f {
                            return say(
                                1,
                                &[
                                    target,
                                    b": ref ",
                                    get(r, "ref"),
                                    b" took effect (record ",
                                    get(r, "seq"),
                                    b") after ",
                                    &after,
                                    format!(" (record {f})").as_bytes(),
                                ]
                                .concat(),
                            );
                        }
                    }
                }
            }
            return say(0, &[target, b": no effect after ", &after, format!(" ({} refs had it)", first.len()).as_bytes()].concat());
        }
        _ => {}
    }
    if let Some(rc) = crate::grpc::check_ext(kind, target, &recs) {
        return rc; // check calls
    }
    say(2, &[b"check: unknown kind ", kind].concat())
}

/// `say` for the module grpc.
pub fn say_ok(msg: &[u8]) -> i32 {
    say(0, msg)
}
