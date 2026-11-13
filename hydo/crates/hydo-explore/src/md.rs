//! report.md, the exploration's report for people: a view of `exploration.json` (the results
//! format), made only from the document, so `hydo report` prints the same text from the file.

use crate::py::{dumps_line, fl, head};
use crate::report::cell;
use crate::results::{render_liveness, render_properties};
use hydo_results::{Exploration, Failure};
use serde_json::{json, Map, Value};

/// The summary as rows, in the order the report and the printed summary line show them.
pub fn summary_rows(doc: &Exploration) -> Vec<(String, Value)> {
    let s = &doc.summary;
    let e = &doc.engine;
    let mut r: Vec<(String, Value)> = Vec::new();
    let mut put = |k: &str, v: Value| r.push((k.to_string(), v));
    let f = |x: Option<f64>| fl(x.unwrap_or(0.0));
    if doc.mode == "branch" {
        put("schedules", json!(s.schedules));
        put("branch_points", json!(s.branch_points.unwrap_or(0)));
        put("runs", json!(s.runs.unwrap_or(0)));
        put(
            "variants_per_point",
            json!(s.variants_per_point.unwrap_or(0)),
        );
        put("tjpm", json!(s.tjpm.unwrap_or(0)));
        put("sweep_wall_s", f(s.sweep_wall_s));
        put("runs_per_hour", f(s.runs_per_hour));
        put("mean_run_s", f(s.mean_run_s));
        put("failures", json!(s.failures));
        put(
            "reproduced_standalone",
            json!(s.reproduced_standalone.unwrap_or(0)),
        );
        put(
            "timing_only_failures",
            json!(s.timing_only_failures.unwrap_or(0)),
        );
        put("total_wall_s", f(s.total_wall_s));
    } else {
        put("schedules", json!(s.schedules));
        put("strategy", json!(doc.config.strategy));
        put("image", json!(doc.image.path));
        put("workers", json!(e.workers.unwrap_or(0)));
        put("sweep_wall_s", f(s.sweep_wall_s));
        put("runs_per_hour", f(s.runs_per_hour));
        put("mean_run_s", f(s.mean_run_s));
        put("worker_boot_s", f(s.worker_boot_s));
        put("total_wall_s", f(s.total_wall_s));
        put("failures", json!(s.failures));
        put("reproduced", json!(s.reproduced.unwrap_or(0)));
        if let Some(n) = s.repro_runs {
            put("repro_runs", json!(n));
        }
        if let Some(Value::Object(info)) = doc.freeze_sweep.as_ref().and_then(|x| x.get("summary"))
        {
            for (k, v) in info {
                put(k, v.clone());
            }
        }
        if let Some(q) = s.quiet_wait_s {
            put("quiet_wait_s", fl(q));
        }
        if let Some(c) = s.cache_hits {
            put("cache_hits", json!(c));
        }
    }
    if e.ram_mib.is_some_and(|m| m != 1024) {
        put("ram_mib", json!(e.ram_mib));
    }
    if e.vcpus.is_some_and(|v| v != 1) {
        put("vcpus", json!(e.vcpus));
    }
    if e.cpu.as_deref().is_some_and(|c| c != "v2") {
        put("cpu", json!(e.cpu));
    }
    if let Some(k) = &e.guest_kernel {
        put("guest_kernel", json!(k));
    }
    if let Some(n) = s.error_kinds_new {
        put("error_kinds_new", json!(n));
    }
    if let Some(n) = s.runs_with_new_error_kinds {
        put("runs_with_new_error_kinds", json!(n));
    }
    if let Some(n) = s.vacuous_passes {
        put("vacuous_passes", json!(n));
    }
    if let Some(n) = s.refused {
        put("refused", json!(n));
    }
    // M209: a steered search's best value and its first failure (absent for other explorations)
    if let Some(st) = &doc.steer {
        put("steer", json!(format!("{} {}", st.goal, st.id)));
        put("steer_mutants", json!(st.mutants));
        put("steer_best", st.best.map(fl).unwrap_or(Value::Null));
        put(
            "steer_first_failure",
            st.first_failure.map(|n| json!(n)).unwrap_or(Value::Null),
        );
    }
    r
}

/// The one-line summary the explorer prints when it finishes.
pub fn summary_line(doc: &Exploration) -> String {
    let m: Map<String, Value> = summary_rows(doc).into_iter().collect();
    dumps_line(&Value::Object(m))
}

fn table(doc: &Exploration, l: &mut Vec<String>) {
    l.push("| | |".into());
    l.push("|---|---|".into());
    for (k, v) in summary_rows(doc) {
        l.push(format!("| {k} | {} |", cell(&v)));
    }
}

fn class(f: &Failure) -> String {
    f.run.class.clone().unwrap_or_default()
}

/// report.md of an exploration document.
pub fn report(doc: &Exploration) -> String {
    if doc.mode == "branch" {
        return branch_report(doc);
    }
    let mut l: Vec<String> = vec!["# Explorer report".into(), String::new()];
    table(doc, &mut l);
    if let Some(t) = &doc.turn_sweep {
        l.extend(turn_sweep_md(t));
    }
    if let Some(s) = &doc.steer {
        l.extend(steer_md(s));
    }
    let mut by: std::collections::BTreeMap<(String, String), Vec<&Failure>> = Default::default();
    for f in &doc.failures {
        by.entry((class(f), f.signature.clone()))
            .or_default()
            .push(f);
    }
    l.extend(
        [
            "",
            "## Failures by class and signature",
            "",
            "| class | signature | runs | seeds | reproduced |",
            "|---|---|---|---|---|",
        ]
        .map(String::from),
    );
    for ((c, sig), fs) in &by {
        l.push(format!(
            "| {c} | `{sig}` | {} | {} | {}/{} |",
            fs.len(),
            fs.iter()
                .take(8)
                .map(|f| f.run.seed.map(|s| s.to_string()).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(" "),
            fs.iter().filter(|f| f.reproduced).count(),
            fs.len()
        ));
    }
    l.extend(failure_outputs(&by));
    l.extend(["", "## Minimal reproducers", ""].map(String::from));
    let turns_re = regex::Regex::new(r"(?:^|\s)(?:qs?|qb)=").expect("p");
    for v in &doc.shrinks {
        let c = &v.class;
        let mc = v.min_class.clone().unwrap_or_else(|| c.clone());
        let hd = if mc == *c {
            mc.clone()
        } else {
            format!("{mc} (found as {c}; the minimal schedule needs only {mc})")
        };
        let minimal = &v.minimal;
        l.push(format!("### {hd}: `{}`", v.signature));
        l.push(String::new());
        l.push(format!(
            "- from seed {}, {} shrink runs",
            v.from_seed, v.tries
        ));
        if let Some(fk) = &v.fault_kinds {
            l.push(format!("- the failure's fault kinds: {fk}"));
        }
        l.push(format!("- minimal schedule: `{minimal}`"));
        l.push(format!(
            "- needs a tick-timing change: {}",
            if minimal.contains(" tj") { "yes" } else { "no" }
        ));
        if let Some(cmp) = v.composition.as_ref().filter(|x| !x.is_empty()) {
            l.push(format!("- composition (test composer, W=): {cmp}"));
        }
        if v.stable.as_ref().is_some_and(|x| !x.is_empty()) {
            let st = v.stable_schedule.as_deref();
            l.push(format!(
                "- stable form (commands by id; `hydo save` writes it): {}",
                if st == Some(minimal.as_str()) {
                    "the minimal schedule".to_string()
                } else {
                    format!(
                        "`{}` ({})",
                        st.unwrap_or("None"),
                        v.stable_note.clone().unwrap_or_default()
                    )
                }
            ));
        }
        if doc.engine.vcpus.is_some_and(|n| n > 1) {
            let host = minimal.split('|').next().unwrap_or("");
            l.push(format!(
                "- needs vCPU turn lengths: {}",
                if turns_re.is_match(host) { "yes" } else { "no" }
            ));
        }
        l.push(format!(
            "- verdict: `{}`",
            v.min_verdict.clone().unwrap_or_else(|| "None".into())
        ));
        let fp = |i: usize| {
            v.confirmations
                .get(i)
                .and_then(|c| c.fp.clone())
                .unwrap_or_else(|| "None".into())
        };
        l.push(format!(
            "- fingerprint, two runs: `{}` / `{}` ({})",
            fp(0),
            fp(1),
            if v.identical {
                "identical"
            } else {
                "DIFFERENT"
            }
        ));
        l.push(format!(
            "- by hand: {}",
            v.repro_command.clone().unwrap_or_default()
        ));
        l.push(String::new());
    }
    l.extend(
        [
            "## Property table over every schedule (hydo/ASSERTIONS.md)",
            "",
        ]
        .map(String::from),
    );
    match &doc.properties {
        None => l.extend(
            [
                "The guest emitted no HYDO-ASSERT records (an image built without the assertion SDK).",
                "",
            ]
            .map(String::from),
        ),
        Some(t) => {
            l.push("```".into());
            l.push(render_properties(t).trim_end().to_string());
            l.push("```".into());
            l.push(String::new());
            l.extend(render_liveness(&doc.liveness));
        }
    }
    if let Some(ek) = &doc.error_kinds {
        l.extend(hydo_errors::report_md_of(ek));
    }
    l.join("\n") + "\n"
}

/// `--sweep turns` (M205): the boundaries with both neighbours, then every length run.
/// The steered search's section (M209).
fn steer_md(s: &hydo_results::SteerSearch) -> Vec<String> {
    let v = |x: f64| {
        if x.fract() == 0.0 && x.abs() < 1e15 {
            format!("{}", x as i64)
        } else {
            format!("{x}")
        }
    };
    let what = match &s.pattern {
        Some(p) => format!("the first capture group of `{p}` in the consoles"),
        None => format!("the SDK's `HYDO-STEER` records of `{}`", s.id),
    };
    let mut l = vec![
        String::new(),
        "## Steered search (`--steer`)".into(),
        String::new(),
        format!(
            "The value: {what}, the {} of each run (goal `{}`). {} runs: {} random schedules and {} mutants \
             ({} mutants per 1000 schedules once the corpus had an entry, rounds of {}, a corpus of {}). \
             {} runs reported a value.",
            if s.goal == "max" { "largest" } else { "smallest" },
            s.goal,
            s.runs,
            s.random,
            s.mutants,
            s.mix_permille,
            s.batch,
            s.corpus_max,
            s.with_value
        ),
        String::new(),
    ];
    match (s.first_failure, &s.first_failure_tag) {
        (Some(n), Some(t)) => l.push(format!(
            "The first failure is run {n} (`{t}`), value {}.{}",
            s.first_failure_value
                .map(v)
                .unwrap_or_else(|| "none".into()),
            if s.stopped {
                " The search stopped after its round."
            } else {
                ""
            }
        )),
        _ => l.push(format!("No run failed in {} runs.", s.runs)),
    }
    l.push(String::new());
    if !s.progress.is_empty() {
        l.push("Every run whose value was better than every earlier run's:".into());
        l.push(String::new());
        l.push("| run | tag | value | schedule |".into());
        l.push("|---|---|---|---|".into());
        for p in &s.progress {
            l.push(format!(
                "| {} | {} | {} | `{}` |",
                p.run,
                p.tag,
                v(p.value),
                p.schedule.replace('|', "\\|")
            ));
        }
        l.push(String::new());
    }
    l
}

fn turn_sweep_md(t: &hydo_results::TurnSweep) -> Vec<String> {
    let mut l = vec![
        String::new(),
        "## vCPU turn lengths (`--sweep turns`)".into(),
        String::new(),
        format!(
            "The schedule `{}` ran with every turn length `q=` of a grid of {} lengths from {} to {} branches; \
             each pair of neighbouring lengths with different verdict kinds was bisected until they were at most {} branches apart.",
            t.base, t.grid, t.lo, t.hi, t.resolution
        ),
        String::new(),
    ];
    if t.boundaries.is_empty() {
        l.push("No boundary: every length gave the same verdict kind.".into());
        l.push(String::new());
    }
    for b in &t.boundaries {
        l.push(format!(
            "### Boundary between q={} ({}) and q={} ({})",
            b.below.q, b.below.kind, b.above.q, b.above.kind
        ));
        l.push(String::new());
        let fps = |p: &hydo_results::TurnPoint, i: usize| {
            format!(
                "`{}` / `{}`",
                p.fp.as_deref().unwrap_or("None"),
                b.reproductions
                    .get(i)
                    .and_then(|r| r.fp.as_deref())
                    .unwrap_or("None")
            )
        };
        for (i, p, cmd) in [
            (0, &b.below, &b.below_command),
            (1, &b.above, &b.above_command),
        ] {
            l.push(format!(
                "- q={}: `{}`, fingerprint, two runs: {}",
                p.q,
                p.verdict.as_deref().unwrap_or("None"),
                fps(p, i)
            ));
            if let Some(c) = cmd {
                l.push(format!("  - by hand: {c}"));
            }
        }
        l.push(format!(
            "- reproduced: {}",
            if b.reproduced { "yes" } else { "NO" }
        ));
        l.push(String::new());
    }
    l.push("| q | verdict kind | fingerprint |".into());
    l.push("|---|---|---|".into());
    for p in &t.points {
        l.push(format!(
            "| {} | {} | `{}` |",
            p.q,
            p.kind,
            p.fp.as_deref().unwrap_or("None")
        ));
    }
    l
}

fn branch_report(doc: &Exploration) -> String {
    let marker = doc.config.branch_marker.clone().unwrap_or_default();
    let mut l = vec![
        format!("# Branch report (marker `{marker}`)"),
        String::new(),
    ];
    table(doc, &mut l);
    l.extend(
        [
            "",
            "## Failures",
            "",
            "| run | class | timing only | verdict | reproduced standalone |",
            "|---|---|---|---|---|",
        ]
        .map(String::from),
    );
    for f in &doc.failures {
        l.push(format!(
            "| {} | {} | {} | `{}` | {} |",
            f.run.tag.clone().unwrap_or_default(),
            class(f),
            if f.timing_only == Some(true) {
                "yes"
            } else {
                "no"
            },
            head(&f.run.verdict.text, 90),
            if f.reproduced { "yes" } else { "NO" }
        ));
    }
    l.join("\n") + "\n"
}

/// A run document as text for people (`hydo report <run dir>`).
/// A failing check's output before its last line (`details.output`, Milestone 225), one line
/// each after `prefix`, and how many earlier lines were left out.
pub fn output_lines(f: &hydo_results::Finding, prefix: &str) -> Vec<String> {
    let Some(Value::Array(ls)) = f.details.get("output") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(n) = f.details.get("output_omitted").and_then(Value::as_u64) {
        out.push(format!("{prefix}({n} earlier lines left out)"));
    }
    out.extend(
        ls.iter()
            .map(|x| format!("{prefix}{}", x.as_str().unwrap_or(""))),
    );
    out
}

/// The failing checks' output of the first failure of each signature (Milestone 225), in the
/// order of the failures table. Empty when no failure has any.
fn failure_outputs(
    by: &std::collections::BTreeMap<(String, String), Vec<&Failure>>,
) -> Vec<String> {
    let mut l = Vec::new();
    for ((c, sig), fs) in by {
        let Some(f) = fs.first() else { continue };
        let mut body = Vec::new();
        let mut shown: Vec<&str> = Vec::new();
        for x in &f.run.findings {
            let o = output_lines(x, "    ");
            // a check is an invariant and an assertion property: its output once
            if !o.is_empty() && !shown.contains(&x.id.as_str()) {
                shown.push(&x.id);
                body.push(format!("- `{}`:", x.id));
                body.push(String::new());
                body.extend(o);
                body.push(String::new());
            }
        }
        if body.is_empty() {
            continue;
        }
        l.push(format!(
            "### {c}: `{sig}` (seed {})",
            f.run.seed.map(|s| s.to_string()).unwrap_or_default()
        ));
        l.push(String::new());
        l.extend(body);
    }
    if !l.is_empty() {
        let mut h = vec![
            String::new(),
            "## Output of the failing checks".into(),
            String::new(),
            "The lines each failing check printed before its last line, in the first run of each signature.".into(),
            String::new(),
        ];
        h.extend(l);
        return h;
    }
    l
}

pub fn run_report(r: &hydo_results::Run) -> String {
    let mut l = vec![
        format!("schedule: {}", r.schedule),
        format!("verdict:  {}", r.verdict.text),
        format!(
            "fp={}{}",
            r.fingerprints.fp,
            r.fingerprints
                .wfp
                .as_ref()
                .map(|w| format!(" wfp={w}"))
                .unwrap_or_default()
        ),
    ];
    if let Some(i) = &r.image {
        l.push(format!(
            "image:    {}{}",
            i.path,
            i.sha256
                .as_ref()
                .map(|s| format!(" (sha256 {})", head(s, 16)))
                .unwrap_or_default()
        ));
    }
    if r.findings.is_empty() {
        l.push("findings: none".into());
    } else {
        l.push(format!("findings: {}", r.findings.len()));
        for f in &r.findings {
            l.push(format!(
                "  {:<13} {:<9} {}  {}",
                f.kind,
                f.status,
                f.id,
                head(&f.message, 120)
            ));
            l.extend(output_lines(f, "    | "));
        }
    }
    for m in &r.messages {
        l.push(String::new());
        l.extend(messages_lines(m));
    }
    if let Some(t) = &r.properties {
        l.push(String::new());
        l.push(render_properties(t).trim_end().to_string());
    }
    l.join("\n") + "\n"
}

/// A messages check of a run for people: each claim's status, then the measures in a few lines.
pub fn messages_lines(m: &hydo_results::MessagesCheck) -> Vec<String> {
    let mut l = vec![format!("messages check {} (history {}):", m.id, m.history)];
    if let Some(e) = &m.error {
        l.push(format!("  error: {e}"));
        return l;
    }
    for c in &m.claims {
        l.push(format!(
            "  {:<13} {:<9} judged {}, undecided {}{}",
            c.claim,
            c.status,
            c.judged,
            c.undecided,
            if c.status == "held" {
                String::new()
            } else {
                format!(": {}", head(&c.summary, 160))
            }
        ));
    }
    let Some(x) = &m.measures else { return l };
    let get = |b: &Option<std::collections::BTreeMap<String, u64>>, k: &str| {
        b.as_ref()
            .and_then(|b| b.get(k).copied())
            .unwrap_or_default()
    };
    l.push(format!(
        "  sends: {} ok, {} failed, {} unknown ({} recovered); lost {}; unexpected {}, aborted {}",
        get(&x.sends, "ok"),
        get(&x.sends, "fail"),
        get(&x.sends, "info"),
        get(&x.sends, "recovered"),
        x.lost
            .as_ref()
            .map(|v| v.iter().map(|g| g.count).sum::<u64>())
            .unwrap_or_default(),
        x.unexpected.unwrap_or_default(),
        x.aborted.unwrap_or_default()
    ));
    if let Some(d) = &x.deliveries {
        l.push(format!(
            "  deliveries: {} of {} messages, duplicate rate {}, at most {} of one; redeliveries {}",
            d.total,
            d.messages,
            d.duplicate_rate,
            d.max_per_message,
            x.redeliveries
                .as_ref()
                .map(|r| r
                    .iter()
                    .filter(|(_, n)| **n > 0)
                    .map(|(k, n)| format!("{k} {n}"))
                    .collect::<Vec<_>>()
                    .join(", "))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "none".into())
        ));
    }
    if let Some(lat) = x.latency.as_ref().and_then(|t| t.get("first_delivery")) {
        let q: Vec<String> = lat
            .iter()
            .filter(|(k, _)| k.as_str() != "count")
            .map(|(k, v)| format!("{k} {v} ms"))
            .collect();
        l.push(format!(
            "  first delivery after the send: {} measured{}",
            lat.get("count").copied().unwrap_or_default(),
            if q.is_empty() {
                String::new()
            } else {
                format!(", {}", q.join(", "))
            }
        ));
    }
    for r in x.reorders.iter().flatten() {
        l.push(format!(
            "  reorders in {}/{}: {} inverted pairs, largest distance {}",
            r.channel, r.group, r.inverted_pairs, r.max_distance
        ));
    }
    l
}
