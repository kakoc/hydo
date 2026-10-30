//! hydo-explore against explore.py itself: `tests/ident.py` (the reference, explore.py and the
//! modules it imports) prints a deterministic stream of questions with explore.py's answers; this
//! test answers each with hydo-explore and compares byte for byte:
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
//! The one deliberate difference (hvm never gets cf=) is applied in ident.py, see its docstring.
//! Needs python3; without it the test says so and passes. HYDO_IDENT_FULL=1 runs the full stream
//! (about ten times the default).

use hydo_explore::compose;
use hydo_explore::gen::{fc_value, Backend, Ex};
use hydo_explore::sched::{Fc, S};
use hydo_explore::space::{grpc_targets, Space};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const US: &str = "\u{1f}";

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root")
}

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

#[test]
fn explorer_identity() {
    let mut cmd = Command::new("python3");
    cmd.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ident.py"));
    if std::env::var_os("HYDO_IDENT_FULL").is_none() {
        cmd.arg("--quick");
    }
    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIPPED: python3 is not available ({e}); the reference is explore.py");
            return;
        }
    };
    let rd = BufReader::new(child.stdout.take().unwrap());
    let mut st: Option<State> = None;
    let (mut n, mut bad) = (
        std::collections::BTreeMap::<char, usize>::new(),
        Vec::<String>::new(),
    );
    let mut check = |what: &str, want: &str, got: String| {
        if want != got && bad.len() < 20 {
            bad.push(format!(
                "{what}\n  explore.py:   {want}\n  hydo-explore: {got}"
            ));
        }
    };
    for line in rd.lines() {
        let line = line.unwrap();
        let op = line.chars().next().unwrap();
        *n.entry(op).or_default() += 1;
        let (q, want) = line.split_once('\t').unwrap_or((&line, ""));
        match op {
            'G' => st = Some(group(&serde_json::from_str(&q[2..]).unwrap())),
            'E' => {
                let s = st.as_ref().unwrap();
                check(
                    &format!("refused group: {q}"),
                    want,
                    s.broken.clone().unwrap_or_default(),
                );
            }
            'F' => set_flags(
                &mut st.as_mut().unwrap().ex,
                &serde_json::from_str(&q[2..]).unwrap(),
            ),
            'N' => {
                let nmax: i64 = q[2..].parse().unwrap();
                let got: Vec<String> = (0..1000).map(|k| fc_value(k, nmax).to_string()).collect();
                check(&format!("fc_n table nmax={nmax}"), want, got.join(" "));
            }
            _ => {
                let s = st.as_mut().unwrap();
                assert!(s.broken.is_none(), "a refused group got questions");
                let ex = &mut s.ex;
                match op {
                    'S' => {
                        let w: Vec<&str> = q[2..].split(' ').collect();
                        let got =
                            ex.render(&ex.compose_seed(w[0], w[1].parse().unwrap(), w[2] == "1"));
                        check(&format!("schedule {q}"), want, got);
                    }
                    'P' => {
                        let (text, rest) = want.split_once('\t').unwrap();
                        let got = p_answer(ex, &q[2..], text);
                        check(
                            &format!("parse/class/candidates of {text} (tjk {})", &q[2..]),
                            rest,
                            got,
                        );
                    }
                    'W' => {
                        let sw = q[2..].split(' ').next().unwrap().to_string();
                        ex.timing = q.ends_with(" 1");
                        ex.compose = true;
                        check(&format!("sweep {}", &q[2..]), want, w_answer(ex, &sw));
                    }
                    'Z' => {
                        let (text, rest) = want.split_once('\t').unwrap();
                        check(
                            &format!("composer forms of {text}"),
                            rest,
                            z_answer(ex, text).unwrap_or_default(),
                        );
                    }
                    'X' => {
                        let w: Vec<&str> = q[2..].split(' ').collect();
                        let mut s = S::new("none", 0);
                        s.f = Some(Fc {
                            svc: w[0].into(),
                            dir: w[1].chars().next().unwrap(),
                            peer: w[2].into(),
                            step: w[3].parse().unwrap(),
                            n: w[4].parse().unwrap(),
                            matched: (w[5] != "-").then(|| w[5].to_string()),
                            dur: w[6].parse().unwrap(),
                        });
                        check(&format!("freeze point {q}"), want, ex.render(&s));
                    }
                    _ => panic!("unknown question {line}"),
                }
            }
        }
    }
    let status = child.wait().unwrap();
    assert!(status.success(), "ident.py failed");
    eprintln!("identity: {n:?}");
    assert!(
        bad.is_empty(),
        "{} differences:\n{}",
        bad.len(),
        bad.join("\n")
    );
    assert!(
        n.get(&'S').copied().unwrap_or(0) > 1000,
        "too few schedules: {n:?}"
    );
}
