//! hydo-explore against explore.py itself, frozen. Until M107 `tests/ident.py` (explore.py and the
//! modules it imported) printed a deterministic stream of questions with explore.py's answers and
//! this test answered each with hydo-explore. The Python is gone: its stream was recorded once into
//! `tests/golden/identity-quick.txt` and `identity-full.txt` (chunked SHA-256, see
//! `tests/support/mod.rs`), and this test now produces the same stream itself, questions and
//! answers, with ident.py's driver ported below, and compares it chunk by chunk:
//!
//! - every schedule string main() composes, for every generator (random, swarm, swarm-pos,
//!   process), with the option streams switched per seed (timing, new faults, more faults,
//!   preempt, turns, bursts, buggify, concurrency, freeze-call, protocol faults, gRPC faults, the
//!   composer's W=, durable-only), without a manifest (the stack) and for every example manifest,
//!   on hvx and on hvm (with and without a data disk and tc);
//! - `render(parse(s))`, the finding class under both knob rules, and the shrink candidates of
//!   those schedules (with and without a tick range, so seeded jitter becomes a tjx= list, and
//!   one step further);
//! - every sweep (or its refusal message), with the per-point streams;
//! - freeze-sweep points, the composer's resolved form, description, contents, key and its
//!   resolved candidates;
//! - the F= n table (it depends on the last bit of exp/log, see `gen::fc_value`).
//!
//! The stream's lines (ident.py's format): `N <nmax>\t<table>`, `G <group json>`, `E\t<refusal>`,
//! `F <flags json>`, `S <strategy> <seed> <durable>\t<schedule>`,
//! `P <tjk>\t<schedule>\t<render>\t<class cut>\t<class any>\t<candidates>`,
//! `W <sweep> <timing>\t<points or !refusal>`, `Z\t<schedule>\t<composer forms>` and
//! `X <freeze point>\t<schedule>`.
//!
//! The reference was explore.py at commit 2a58c4b with one deliberate difference, applied in
//! ident.py: hvm never gets cf= (restore the script with
//! `git show 2a58c4b:hydo/crates/hydo-explore/tests/ident.py`). The example manifests (and their
//! `hydo/` folders) are pinned in the golden; if one changes or a new one appears, the test says so,
//! and after the new answers are checked, `HYDO_BLESS=1 cargo test -p hydo-explore --test it identity::`
//! (once more with HYDO_IDENT_FULL=1) re-records the goldens from the Rust output, which is the
//! reference from now on. By default the quick stream runs (ident.py --quick: the first four
//! groups and every fifth after them, 40 seeds); HYDO_IDENT_FULL=1 runs the full one (300 seeds,
//! every group).

use crate::support;

use hydo_explore::compose;
use hydo_explore::gen::{fc_value, Backend, Ex};
use hydo_explore::sched::{Fc, S};
use hydo_explore::space::{grpc_targets, Space};
use serde_json::Value;
use support::{crc32, manifest_inputs, manifests, mix, salt_of, top, Stream};

const US: &str = "\u{1f}";

struct State {
    ex: Ex,
    broken: Option<String>,
}

fn group(cfg: &Value) -> State {
    let mut ex = Ex::default();
    let mut broken = None;
    if let Some(m) = cfg["manifest"].as_str() {
        match Space::from_path(&top().join(m)) {
            Ok(sp) => {
                ex.grpc_targets = grpc_targets(Some(&sp.m));
                sp.apply(&mut ex);
            }
            Err(e) => broken = Some(e),
        }
    }
    ex.backend = if cfg["backend"] == "hvm" {
        Backend::Hvm
    } else {
        Backend::Hvx
    };
    ex.hvm_disk = cfg["disk"] == true;
    ex.hvm_tc = cfg["tc"] == true;
    ex.cf_range = Some((1, 300));
    State { ex, broken }
}

fn set_flags(ex: &mut Ex, f: &Value) {
    let b = |k: &str| f[k] == true;
    ex.timing = b("TIMING");
    ex.new_faults = b("NEW_FAULTS");
    ex.more_faults = b("MORE_FAULTS");
    ex.preempt = b("PREEMPT");
    ex.turns = b("TURNS");
    ex.bursts = b("BURSTS");
    ex.buggify = b("BUGGIFY");
    ex.concurrency = b("CONCURRENCY");
    ex.freeze_call = b("FREEZE_CALL");
    ex.protocol_faults = b("PROTOCOL_FAULTS");
    ex.compose = b("COMPOSE");
    ex.grpc_on = b("GRPC");
    ex.knob_rule_any = b("KNOB_ANY");
}

fn p_answer(ex: &mut Ex, tjk: &str, text: &str) -> String {
    let tjk = (tjk != "-")
        .then(|| tjk.split_once('-').unwrap())
        .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()));
    let s = ex.parse(text).expect("explore.py parsed it");
    ex.knob_rule_any = false;
    let cc = ex.finding_class(&s);
    ex.knob_rule_any = true;
    let ca = ex.finding_class(&s);
    let cands: Vec<String> = ex
        .candidates(&s, tjk)
        .iter()
        .map(|c| ex.render(c))
        .collect();
    format!("{}\t{}\t{}\t{}", ex.render(&s), cc, ca, cands.join(US))
}

fn w_answer(ex: &Ex, sw: &str) -> String {
    if let Some(sp) = &ex.space {
        if let Err(e) = sp.check_sweep(sw) {
            return format!("!{e}");
        }
    }
    match ex.sweep(sw) {
        Err(e) => format!("!{e}"),
        Ok(points) => points
            .into_iter()
            .enumerate()
            .map(|(i, (lab, s))| {
                format!(
                    "{lab}{US}{}",
                    ex.render(&ex.add_point_streams(s, i as u64 + 1))
                )
            })
            .collect::<Vec<_>>()
            .join("\t"),
    }
}

fn z_answer(ex: &Ex, text: &str) -> Option<String> {
    let meta = ex.space.as_ref()?.compose.as_ref()?;
    let w = compose::w_of(text)?;
    let r = compose::stable_w(&w, meta).ok()?;
    let desc = compose::describe(&w, meta).ok()?;
    let cont = compose::contents_of(&r, meta).ok()?;
    let mut sorted: Vec<(&String, &Value)> = cont.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let cont_json = format!(
        "{{{}}}",
        sorted
            .iter()
            .map(|(k, v)| format!(
                "{}: {}",
                hydo_explore::py::dumps_line(&Value::String((*k).clone())),
                hydo_explore::py::dumps_line(v)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let rc: Vec<String> = compose::candidates(&r, meta).into_iter().take(8).collect();
    let (host, _) = text.split_once('|').unwrap();
    let guest: Vec<String> = text
        .split_once('|')
        .unwrap()
        .1
        .split_whitespace()
        .map(|t| {
            if t.starts_with("W=") {
                format!("W={r}")
            } else {
                t.to_string()
            }
        })
        .collect();
    let st = format!("{host}| {}", guest.join(" "));
    Some(format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        r,
        desc,
        cont_json,
        compose::key_of(&r),
        if compose::fits(&st) { "1" } else { "0" },
        rc.join(US)
    ))
}

const FLAGS: [&str; 14] = [
    "TIMING",
    "NEW_FAULTS",
    "DURABLE",
    "MORE_FAULTS",
    "PREEMPT",
    "TURNS",
    "BURSTS",
    "BUGGIFY",
    "CONCURRENCY",
    "FREEZE_CALL",
    "PROTOCOL_FAULTS",
    "GRPC",
    "COMPOSE",
    "KNOB_ANY",
];

const SWEEPS: [&str; 13] = [
    "kill",
    "cut",
    "net",
    "killcut",
    "pause",
    "clock",
    "oneway",
    "frontkill",
    "tail",
    "depth1",
    "depth1-old",
    "protocol",
    "bogus",
];

/// ident.py `flags_for`: all off, all on, then a hash of the seed.
fn flags_for(sd: u64, salt: u64) -> Vec<(&'static str, bool)> {
    let h = mix(sd.wrapping_mul(0x1000193) ^ salt);
    FLAGS
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let on = match sd {
                1 => false,
                2 => true,
                _ => (h >> i) & 1 == 1,
            };
            (*f, on)
        })
        .collect()
}

/// `json.dumps(flags, sort_keys=True)`.
fn flags_json(fl: &[(&str, bool)]) -> Value {
    let mut v: Vec<(&str, bool)> = fl.to_vec();
    v.sort();
    let mut m = serde_json::Map::new();
    for (k, b) in v {
        m.insert(k.to_string(), Value::Bool(b));
    }
    Value::Object(m)
}

fn flag(fl: &[(&str, bool)], k: &str) -> bool {
    fl.iter().any(|(f, b)| *f == k && *b)
}

/// ident.py `setflags`: the F line and the flags on the explorer.
fn setflags(out: &mut Stream, ex: &mut Ex, fl: &[(&str, bool)]) {
    let v = flags_json(fl);
    set_flags(ex, &v);
    out.line(format!("F {}", hydo_explore::py::dumps_line(&v)));
}

/// ident.py `p_line`: the line and the candidates it printed.
fn p_line(out: &mut Stream, ex: &mut Ex, text: &str, tjk: Option<(i64, i64)>) -> Vec<String> {
    let tk = match tjk {
        None => "-".to_string(),
        Some((a, b)) => format!("{a}-{b}"),
    };
    let ans = p_answer(ex, &tk, text);
    let cands: Vec<String> = ans
        .splitn(4, '\t')
        .nth(3)
        .unwrap_or("")
        .split(US)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect();
    out.line(format!("P {tk}\t{text}\t{ans}"));
    cands
}

fn group_json(label: &str, manifest: Option<&str>, backend: &str, disk: bool, tc: bool) -> Value {
    serde_json::json!({
        "backend": backend,
        "disk": disk,
        "label": label,
        "manifest": manifest,
        "tc": tc,
    })
}

const CAND_EVERY: u64 = 10;

/// ident.py `group`.
fn run_group(out: &mut Stream, cfg: &Value, strats: &[&str], seeds: u64) {
    let label = cfg["label"].as_str().unwrap().to_string();
    out.group(&label);
    out.line(format!("G {}", hydo_explore::py::dumps_line(cfg)));
    let mut st = group(cfg);
    assert!(st.broken.is_none());
    let ex = &mut st.ex;
    let salt = salt_of(&label);
    for strat in strats {
        for sd in 1..=seeds {
            let fl = flags_for(sd, salt ^ u64::from(crc32(strat.as_bytes())));
            setflags(out, ex, &fl);
            let durable = flag(&fl, "DURABLE");
            let text = ex.render(&ex.compose_seed(strat, sd, durable));
            out.line(format!("S {strat} {sd} {}\t{text}", u8::from(durable)));
            if sd % CAND_EVERY == 0 || sd <= 3 {
                let tjk = (2 + (sd % 7) as i64, 40 + ((sd * 13) % 300) as i64);
                for tk in [None, Some(tjk)] {
                    for c in p_line(out, ex, &text, tk) {
                        if c.contains("tjx=") {
                            p_line(out, ex, &c, tk);
                            break;
                        }
                    }
                }
            }
            let composed = ex.space.as_ref().is_some_and(|s| s.composed());
            if composed && (sd % 5 == 0 || sd <= 3) {
                if let Some(z) = z_answer(ex, &text) {
                    out.line(format!("Z\t{text}\t{z}"));
                }
            }
        }
    }
    for timing in [false, true] {
        let fl: Vec<(&str, bool)> = FLAGS
            .iter()
            .map(|f| (*f, (*f == "TIMING" && timing) || *f == "COMPOSE"))
            .collect();
        setflags(out, ex, &fl);
        for sw in SWEEPS {
            out.line(format!("W {sw} {}\t{}", u8::from(timing), w_answer(ex, sw)));
        }
    }
    let matches: Vec<Option<String>> = std::iter::once(None)
        .chain(ex.fc_match.iter().map(|(m, _)| Some(m.clone())))
        .collect();
    for (svc, peer) in ex.fc_pairs.clone() {
        for d in ['<', '>'] {
            for m in &matches {
                for step in (0..ex.norders).step_by(7) {
                    for n in [0i64, 1, 5, 64] {
                        let mut s = S::new("none", 0);
                        s.f = Some(Fc {
                            svc: svc.clone(),
                            dir: d,
                            peer: peer.clone(),
                            step,
                            n,
                            matched: m.clone(),
                            dur: 1000,
                        });
                        out.line(format!(
                            "X {svc} {d} {peer} {step} {n} {} 1000\t{}",
                            m.as_deref().unwrap_or("-"),
                            ex.render(&s)
                        ));
                    }
                }
            }
        }
    }
}

/// ident.py `main`: the whole stream.
fn produce(quick: bool) -> Stream {
    let seeds = if quick { 40 } else { 300 };
    let mut out = Stream::default();
    out.group("fc_table");
    for nmax in [64i64, 32, 16, 100, 7] {
        let v: Vec<String> = (0..1000).map(|k| fc_value(k, nmax).to_string()).collect();
        out.line(format!("N {nmax}\t{}", v.join(" ")));
    }
    let mut plan: Vec<(Option<String>, &str)> = vec![(None, "hvx"), (None, "hvm")];
    for m in manifests() {
        plan.push((Some(m.clone()), "hvx"));
        plan.push((Some(m), "hvm"));
    }
    if quick {
        let mut q: Vec<_> = plan.iter().take(4).cloned().collect();
        q.extend(plan.iter().skip(4).step_by(5).cloned());
        plan = q;
    }
    let all = ["random", "swarm", "swarm-pos", "process"];
    let three = ["random", "swarm", "process"];
    for (manifest, backend) in plan {
        let mname = manifest.clone().unwrap_or_else(|| "None".into());
        let mut ex = Ex::default();
        if let Some(m) = &manifest {
            match Space::from_path(&top().join(m)) {
                Ok(sp) => sp.apply(&mut ex),
                Err(e) => {
                    let cfg = group_json(m, Some(m), backend, false, false);
                    out.group(m);
                    out.line(format!("G {}", hydo_explore::py::dumps_line(&cfg)));
                    out.line(format!("E\t{e}"));
                    continue;
                }
            }
        }
        let strats: &[&str] = if ex.norders >= 15 { &all } else { &three };
        if backend == "hvm" {
            for (disk, tc) in [(true, true), (false, false)] {
                let label = format!(
                    "{mname}/hvm/{}{}",
                    if disk { "True" } else { "False" },
                    if tc { "True" } else { "False" }
                );
                let cfg = group_json(&label, manifest.as_deref(), backend, disk, tc);
                run_group(&mut out, &cfg, strats, seeds);
            }
        } else {
            let cfg = group_json(
                &format!("{mname}/hvx"),
                manifest.as_deref(),
                backend,
                false,
                false,
            );
            run_group(&mut out, &cfg, strats, seeds);
        }
    }
    out
}

/// A flat stream (as ident.py printed it) cut into the same groups: one per `G` line.
fn regroup(lines: Vec<String>) -> Stream {
    let mut out = Stream::default();
    out.group("fc_table");
    for l in lines {
        if let Some(j) = l.strip_prefix("G ") {
            let v: Value = serde_json::from_str(j).unwrap();
            out.group(v["label"].as_str().unwrap());
        }
        out.line(l);
    }
    out
}

#[test]
fn explorer_identity() {
    let quick = std::env::var_os("HYDO_IDENT_FULL").is_none();
    let name = if quick {
        "identity-quick.txt"
    } else {
        "identity-full.txt"
    };
    let t0 = std::time::Instant::now();
    let stream = match support::recorded_from() {
        Some(lines) => regroup(lines),
        None => produce(quick),
    };
    let mut inputs = Vec::new();
    for m in manifests() {
        inputs.extend(manifest_inputs(&m));
    }
    let mut counts = std::collections::BTreeMap::<char, usize>::new();
    for (_, ls) in &stream.groups {
        for l in ls {
            *counts.entry(l.chars().next().unwrap()).or_default() += 1;
        }
    }
    eprintln!(
        "identity ({}): {counts:?} in {:.1} s",
        if quick { "quick" } else { "full" },
        t0.elapsed().as_secs_f64()
    );
    assert!(
        counts.get(&'S').copied().unwrap_or(0) > 1000,
        "too few schedules: {counts:?}"
    );
    let mode = if quick { "--quick" } else { "full" };
    let comments = if std::env::var_os("HYDO_GOLDEN_FROM").is_some() {
        vec![
            "# The answer stream of hydo/crates/hydo-explore/tests/ident.py (explore.py and the modules it".to_string(),
            format!("# imported, at commit 2a58c4b; Python 3.15.0 on the Mac), {mode} mode, recorded in M107 as"),
            "# chunked SHA-256 (tests/support/mod.rs). Restore the script: git show 2a58c4b:hydo/crates/hydo-explore/tests/ident.py".to_string(),
            "# Re-record from the Rust output (the reference since M107): HYDO_BLESS=1 cargo test -p hydo-explore --test identity".to_string(),
        ]
    } else {
        vec![
            format!("# The identity stream ({mode} mode) as hydo-explore produced it (HYDO_BLESS=1), chunked SHA-256"),
            "# (tests/support/mod.rs). Until M107 it was explore.py's own (git show 2a58c4b:hydo/crates/hydo-explore/tests/ident.py).".to_string(),
        ]
    };
    support::verify(
        &support::golden_dir().join(name),
        &comments,
        &inputs,
        &stream,
        256,
    );
}
