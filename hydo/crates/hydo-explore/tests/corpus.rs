//! The schedule language on the explorer's own output: every schedule string the explorer makes
//! over a large deterministic range (several hundred thousand) must parse with `hydo-schedule` and
//! print back byte for byte, with its host and guest halves where the engines split it (at the
//! first '|').
//!
//! Until M107 the corpus came from `hydo/crates/hydo-schedule/tests/corpus.py` (explore.py); now
//! hydo-explore makes it, with corpus.py's driver ported below: every generator with the option
//! streams switched per seed, no manifest (the stack's names) and every example manifest, hvx and
//! hvm (with and without a data disk and tc), every sweep, the shrink candidates of every 25th seed
//! (with a tick range, so seeded jitter becomes an explicit tjx= list, and one step further), the
//! branch variants of a BRANCH run (the request was deleted in M145; its strings stay in the
//! corpus, which is the recorded one, and are still valid schedules), the cf= probe, freeze-sweep points, and the composer's
//! resolved, saved and old index forms. Each line is `<backend>\t<schedule>`, deduplicated per
//! backend in first-seen order.
//!
//! That the Rust corpus is the same set of strings explore.py made is checked against
//! `tests/golden/corpus-{full,quick}.txt`: corpus.py's stream at commit 2a58c4b (Python 3.15.0 on
//! the Mac) as chunked SHA-256, recorded with two changes to the script: hvm never gets cf= (the
//! fix ident.py applies, docs/HISTORY.md M102; before it, 5,759 hvm strings carried a `cf=` that
//! hvx refuses after `disk=none`), and a manifest's group label is its path relative to the
//! repository (corpus.py used the absolute path, so the per-seed flags depended on the checkout's
//! location). Restore the script: `git show 2a58c4b:hydo/crates/hydo-schedule/tests/corpus.py`.
//! The example manifests are pinned in the golden. `HYDO_BLESS=1` re-records from the Rust output.
//! Default: the full corpus; HYDO_CORPUS_QUICK=1 for about a tenth.

mod support;

use hydo_explore::compose::{self, OItem};
use hydo_explore::gen::{Backend, Ex, TJ_MAX};
use hydo_explore::sched::{Fc, Tj, S};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use std::collections::HashSet;
use support::{crc32, manifest_inputs, manifests, mix, salt_of, top, Stream};

const FLAGS: [&str; 13] = [
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
];

const SWEEPS: [&str; 12] = [
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
];

struct Corpus {
    seen: HashSet<(&'static str, String)>,
    out: Stream,
    cur: Backend,
}

impl Corpus {
    fn emit(&mut self, x: String) {
        let b = match self.cur {
            Backend::Hvx => "hvx",
            Backend::Hvm => "hvm",
        };
        if self.seen.insert((b, x.clone())) {
            self.out.line(format!("{b}\t{x}"));
        }
    }
}

fn flags_for(sd: u64, salt: u64) -> [bool; 13] {
    let h = mix(sd.wrapping_mul(0x1000193) ^ salt);
    let mut f = [false; 13];
    for (i, x) in f.iter_mut().enumerate() {
        *x = match sd {
            1 => false,
            2 => true,
            _ => (h >> i) & 1 == 1,
        };
    }
    f
}

fn setflags(ex: &mut Ex, f: &[bool; 13]) {
    let b = |k: &str| f[FLAGS.iter().position(|x| *x == k).unwrap()];
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
    ex.grpc_on = b("GRPC");
    ex.compose = b("COMPOSE");
}

/// hydo_composer.with_w.
fn with_w(schedule: &str, w: &str) -> String {
    let (host, guest) = schedule.split_once('|').unwrap();
    let mut toks: Vec<String> = guest.split_whitespace().map(str::to_string).collect();
    let mut done = false;
    for t in toks.iter_mut() {
        if t.starts_with("W=") {
            *t = format!("W={w}");
            done = true;
        }
    }
    if !done {
        toks.push(format!("W={w}"));
    }
    format!("{host}| {}", toks.join(" "))
}

fn candidates(c: &mut Corpus, ex: &Ex, s: &S, sd: u64) {
    let tjk = (2 + (sd % 7) as i64, 40 + ((sd * 13) % 300) as i64);
    for tk in [None, Some(tjk)] {
        for cand in ex.candidates(s, tk) {
            let t = ex.render(&cand);
            c.emit(t.clone());
            if matches!(cand.tj, Some(Tj::X(_))) {
                let p = ex.parse(&t).expect("a rendered candidate parses");
                for c2 in ex.candidates(&p, tk) {
                    c.emit(ex.render(&c2));
                }
            }
        }
    }
}

/// The run budget a BRANCH run gave its variants (explore.py `BRANCH_BUDGET`; M145 deleted it).
const BRANCH_BUDGET: i64 = 16;

fn branch_variants(c: &mut Corpus, ex: &Ex, text: &str, sd: u64) {
    let (host, guest) = text.split_once('|').unwrap();
    let host = host.trim_end();
    let pm = ex.tj_pm[ex.tj_pm.len() / 2];
    c.emit(format!("{host} rb={BRANCH_BUDGET} |{guest}"));
    for v in [1u64, 2, 7, 31] {
        c.emit(format!(
            "{host} tj={v:x} tjpm={pm} tjmax={TJ_MAX} tjlo={} tjhi=0 rb={BRANCH_BUDGET} |{guest}",
            3 + (sd + v) % 50
        ));
    }
}

fn composer_forms(c: &mut Corpus, ex: &Ex, text: &str) {
    let meta = ex.space.as_ref().unwrap().compose.as_ref().unwrap();
    let Some(w) = compose::w_of(text) else {
        return;
    };
    let Ok(r) = compose::stable_w(&w, meta) else {
        return;
    };
    let mut xs = vec![r.clone()];
    xs.extend(compose::candidates(&r, meta).into_iter().take(8));
    for x in xs {
        let t = with_w(text, &x);
        if compose::fits(&t) {
            c.emit(t);
        } else {
            c.emit(with_w(text, &format!("@{}", compose::key_of(&x))));
        }
    }
    c.emit(with_w(text, &format!("@{}", compose::key_of(&r))));
    // the old index form of o (M86), which the shrinker wrote before M98
    let Ok(mut d) = compose::parse_w(&w, meta) else {
        return;
    };
    if !d.o.is_empty() {
        let cmds = compose::commands(meta);
        d.o =
            d.o.iter()
                .map(|o| match o {
                    OItem::S(id) => match cmds.iter().position(|(i, _)| i == id) {
                        Some(ix) => OItem::I(ix as i64),
                        None => o.clone(),
                    },
                    OItem::I(_) => o.clone(),
                })
                .collect();
        if d.o.iter().all(|o| matches!(o, OItem::I(_))) {
            c.emit(with_w(text, &compose::render_w(&d)));
        }
    }
}

fn freeze_points(c: &mut Corpus, ex: &Ex) {
    let matches: Vec<Option<String>> = std::iter::once(None)
        .chain(ex.fc_match.iter().map(|(m, _)| Some(m.clone())))
        .collect();
    for (svc, peer) in &ex.fc_pairs {
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
                        c.emit(ex.render(&s));
                    }
                }
            }
        }
    }
}

fn group(c: &mut Corpus, ex: &mut Ex, label: &str, strats: &[&str], seeds: u64) {
    let (bname, cand_every) = (
        match ex.backend {
            Backend::Hvx => "hvx",
            Backend::Hvm => "hvm",
        },
        25,
    );
    c.cur = ex.backend;
    ex.cf_range = Some((1, 300));
    let salt = salt_of(&format!("{label}/{bname}"));
    for strat in strats {
        for sd in 1..=seeds {
            let fl = flags_for(sd, salt ^ u64::from(crc32(strat.as_bytes())));
            setflags(ex, &fl);
            let text = ex.render(&ex.compose_seed(strat, sd, fl[2]));
            c.emit(text.clone());
            if sd % cand_every == 0 || sd <= 3 {
                let p = ex.parse(&text).expect("the explorer's schedule parses");
                candidates(c, ex, &p, sd);
                if ex.backend == Backend::Hvx {
                    let g = ex.render(&ex.generator(strat, sd));
                    branch_variants(c, ex, &g, sd);
                }
            }
            let composed = ex.space.as_ref().is_some_and(|s| s.composed());
            if composed && (sd % 5 == 0 || sd <= 3) {
                composer_forms(c, ex, &text);
            }
        }
    }
    let mut fl = [false; 13];
    for sw in SWEEPS {
        if let Some(sp) = &ex.space {
            if sp.check_sweep(sw).is_err() {
                continue;
            }
        }
        let Ok(points) = ex.sweep(sw) else {
            continue;
        };
        for timing in [false, true] {
            fl[0] = timing;
            fl[12] = true;
            setflags(ex, &fl);
            for (i, (_, sch)) in points.iter().enumerate() {
                c.emit(ex.render(&ex.add_point_streams(sch.clone(), i as u64 + 1)));
            }
        }
    }
    if !ex.fc_pairs.is_empty() {
        freeze_points(c, ex);
    }
    c.emit("disk=cut dseed=0 cfn | run".into());
}

fn fresh(manifest: Option<&str>) -> Ex {
    let mut ex = Ex::default();
    if let Some(m) = manifest {
        let sp = Space::from_path(&top().join(m)).expect("an example manifest loads");
        ex.grpc_targets = grpc_targets(Some(&sp.m));
        sp.apply(&mut ex);
    }
    ex
}

fn produce(quick: bool) -> Stream {
    let seeds = if quick { 60 } else { 600 };
    let mut c = Corpus {
        seen: HashSet::new(),
        out: Stream::default(),
        cur: Backend::Hvx,
    };
    c.out.group("corpus");
    let mut plan: Vec<(Option<String>, Backend)> = vec![(None, Backend::Hvx), (None, Backend::Hvm)];
    for m in manifests() {
        plan.push((Some(m.clone()), Backend::Hvx));
        plan.push((Some(m), Backend::Hvm));
    }
    if quick {
        plan = plan.into_iter().step_by(4).collect();
    }
    let all = ["random", "swarm", "swarm-pos", "process"];
    let three = ["random", "swarm", "process"];
    for (manifest, backend) in plan {
        let mname = manifest.clone().unwrap_or_else(|| "None".into());
        if backend == Backend::Hvm {
            for (disk, tc) in [(true, true), (false, false)] {
                let mut ex = fresh(manifest.as_deref());
                ex.backend = backend;
                ex.hvm_disk = disk;
                ex.hvm_tc = tc;
                let strats: &[&str] = if ex.norders >= 15 { &all } else { &three };
                let label = format!(
                    "{mname}/{}{}",
                    if disk { "True" } else { "False" },
                    if tc { "True" } else { "False" }
                );
                group(&mut c, &mut ex, &label, strats, seeds);
            }
        } else {
            let mut ex = fresh(manifest.as_deref());
            ex.backend = backend;
            let strats: &[&str] = if ex.norders >= 15 { &all } else { &three };
            group(&mut c, &mut ex, &mname, strats, seeds);
        }
    }
    c.out
}

#[test]
fn explorer_corpus_round_trips() {
    let quick = std::env::var_os("HYDO_CORPUS_QUICK").is_some();
    let t0 = std::time::Instant::now();
    let stream = match support::recorded_from() {
        Some(lines) => {
            let mut s = Stream::default();
            s.group("corpus");
            for l in lines {
                s.line(l);
            }
            s
        }
        None => produce(quick),
    };
    let gen_s = t0.elapsed().as_secs_f64();
    let (mut n, mut nhvm, mut bad) = (0usize, 0usize, Vec::new());
    for line in &stream.groups[0].1 {
        let (backend, s) = line.split_once('\t').expect("<backend>\\t<schedule>");
        n += 1;
        nhvm += usize::from(backend == "hvm");
        match Schedule::parse(s) {
            Ok(p) => {
                let back = p.to_string();
                if back != s {
                    bad.push(format!("printed differently:\n  in:  {s}\n  out: {back}"));
                } else {
                    let (h, g) = s.split_once('|').expect("a '|'");
                    if p.host_text() != h.trim_end() || p.guest_text() != g.trim_start() {
                        bad.push(format!("host/guest split differs: {s}"));
                    }
                }
            }
            Err(e) => bad.push(format!("refused ({backend}): {s}\n  {e}")),
        }
    }
    eprintln!(
        "corpus ({}): {n} schedule strings ({nhvm} for hvm), {} round-tripped byte for byte; \
         generated in {gen_s:.1} s, checked in {:.1} s",
        if quick { "quick" } else { "full" },
        n - bad.len(),
        t0.elapsed().as_secs_f64() - gen_s
    );
    assert!(n > 10_000, "the corpus is too small ({n})");
    assert!(
        bad.is_empty(),
        "{} of {n} failed; first ones:\n{}",
        bad.len(),
        bad.iter().take(30).cloned().collect::<Vec<_>>().join("\n")
    );
    let mut inputs = Vec::new();
    for m in manifests() {
        inputs.extend(manifest_inputs(&m));
    }
    let mode = if quick { "--quick" } else { "full" };
    let comments = if std::env::var_os("HYDO_GOLDEN_FROM").is_some() {
        vec![
            "# The schedule corpus of hydo/crates/hydo-schedule/tests/corpus.py (explore.py at commit 2a58c4b;".to_string(),
            format!("# Python 3.15.0 on the Mac), {mode} mode, recorded in M107 as chunked SHA-256 (tests/support/mod.rs),"),
            "# with two changes to the script: no cf= on hvm (as ident.py), relative manifest paths as group labels.".to_string(),
            "# Restore the script: git show 2a58c4b:hydo/crates/hydo-schedule/tests/corpus.py".to_string(),
            "# Re-record from the Rust output (the reference since M107): HYDO_BLESS=1 cargo test -p hydo-explore --test corpus".to_string(),
        ]
    } else {
        vec![
            format!("# The schedule corpus ({mode} mode) as hydo-explore produced it (HYDO_BLESS=1), chunked SHA-256"),
            "# (tests/support/mod.rs). Until M107 it was explore.py's (git show 2a58c4b:hydo/crates/hydo-schedule/tests/corpus.py).".to_string(),
        ]
    };
    let name = if quick {
        "corpus-quick.txt"
    } else {
        "corpus-full.txt"
    };
    support::verify(
        &support::golden_dir().join(name),
        &comments,
        &inputs,
        &stream,
        4096,
    );
}
