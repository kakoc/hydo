//! The two views of a CI run: `report.md` (the pull-request comment and the job summary,
//! docs/design/ci.md section 5.1) and `junit.xml` (every finding and every saved regression as a
//! test case, the one test-report format every CI system shows without extra code, section 8.4).
//! Both are made from `ci-run.json` alone, so `hydo ci compare` makes them offline too.

use hydo_results::{ci_state, CiFinding, CiRun};
use std::fmt::Write as _;

/// A code span that survives any text, also inside a table cell (`|` escaped).
pub fn code(s: &str) -> String {
    let s = s.replace('|', "\\|").replace('\n', " ");
    if s.contains('`') {
        format!("`` {s} ``")
    } else {
        format!("`{s}`")
    }
}

fn short(c: &str) -> &str {
    &c[..c.len().min(10)]
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn of_state<'a>(r: &'a CiRun, s: &str) -> Vec<&'a CiFinding> {
    r.findings.iter().filter(|f| f.state == s).collect()
}

/// The first line: what the change did, in a few words.
pub fn headline(r: &CiRun) -> String {
    let new = of_state(r, ci_state::NEW).len();
    let fixed = of_state(r, ci_state::FIXED).len();
    let regs = &r.summary.regressions;
    let mut parts = Vec::new();
    if r.base.as_ref().is_some_and(|b| b.findings_from != "none") {
        parts.push(if new == 0 {
            "no new findings".to_string()
        } else {
            plural(new, "new finding", "new findings")
        });
        if fixed > 0 {
            parts.push(format!("{fixed} fixed"));
        }
        let changed = of_state(r, ci_state::CHANGED).len();
        if changed > 0 {
            parts.push(format!("{changed} changed"));
        }
    } else {
        let open = r
            .findings
            .iter()
            .filter(|f| f.state != ci_state::ACCEPTED)
            .count();
        parts.push(plural(open, "finding", "findings"));
    }
    let back = of_state(r, ci_state::REAPPEARED).len();
    if back > 0 {
        parts.push(format!("{back} reappeared"));
    }
    let accepted = of_state(r, ci_state::ACCEPTED).len();
    if accepted > 0 {
        parts.push(format!("{accepted} accepted"));
    }
    if regs.replayed > 0 {
        parts.push(format!(
            "{} of {} regressions as expected",
            regs.as_expected, regs.replayed
        ));
    }
    if r.budget.partial {
        if let (Some(c), Some(p)) = (r.budget.completed, r.budget.planned) {
            parts.push(format!("partial: {c} of {p} seeds"));
        }
    }
    let verdict = match r.outcome.exit {
        0 => "passed",
        1 => "failed",
        _ => "could not complete",
    };
    format!("hydo: {} ({verdict})", parts.join(", "))
}

fn engine_words(r: &CiRun) -> String {
    let b = r.engine.backend.clone().unwrap_or_else(|| "?".into());
    let name = match b.as_str() {
        "kvm" => "KVM",
        "hvx" => "hvx",
        "hvm" => "hvm (Apple Silicon)",
        x => x,
    };
    match &r.engine.host_cpu {
        Some(c) if !c.model.is_empty() => format!("{name} on {}", c.model),
        Some(c) => format!("{name} on {}", c.vendor),
        None => name.to_string(),
    }
}

fn table(out: &mut String, fs: &[&CiFinding], sets: &str) {
    out.push_str("| Finding | Minimal schedule | Reproduce |\n|---|---|---|\n");
    for f in fs {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            code(&f.title),
            code(&f.minimal),
            code(&format!("hydo run \"{}\"", f.minimal))
        );
    }
    if !sets.is_empty() {
        let _ = writeln!(out, "\nBuild the image first with `hydo build{sets}`.");
    }
}

fn list(out: &mut String, fs: &[&CiFinding]) {
    for f in fs {
        let _ = writeln!(out, "- {} ({})", code(&f.title), code(&f.minimal));
    }
}

/// `report.md`.
pub fn markdown(r: &CiRun, sets: &[(String, String)]) -> String {
    let sets: String = sets
        .iter()
        .map(|(k, v)| format!(" --set {k}={v}"))
        .collect();
    let mut o = String::new();
    let _ = writeln!(o, "### {}\n", headline(r));
    let at = r.commit.as_deref().map(short).unwrap_or("the working tree");
    let base = r.base.as_ref();
    let base_c = base
        .and_then(|b| b.commit.as_deref())
        .map(short)
        .unwrap_or("the base");
    match base {
        Some(b) if b.findings_from != "none" => {
            let _ = writeln!(
                o,
                "Project `{}` at {at}, compared with {}{base_c}.\n",
                r.project.name,
                b.branch
                    .as_deref()
                    .map(|br| format!("{br} at "))
                    .unwrap_or_default()
            );
        }
        Some(b) => {
            let _ = writeln!(
                o,
                "Project `{}` at {at}. No comparison with a base: {}.\n",
                r.project.name,
                b.missing.as_deref().unwrap_or("none was given")
            );
        }
        None => {
            let _ = writeln!(
                o,
                "Project `{}` at {at}. No base was given, so every finding is listed without a \
                 comparison.\n",
                r.project.name
            );
        }
    }
    let new = of_state(r, ci_state::NEW);
    if !new.is_empty() {
        let proven = new
            .iter()
            .all(|f| f.proof.as_deref() == Some("base-replay-ok"));
        if base.is_some_and(|b| b.findings_from != "none") {
            let _ = writeln!(
                o,
                "**New in this change** ({})\n",
                if proven {
                    format!("the base {base_c} passes the same schedule")
                } else {
                    "not found on the base; see each finding's proof in ci-run.json".into()
                }
            );
        } else {
            o.push_str("**Findings**\n\n");
        }
        table(&mut o, &new, &sets);
        for f in &new {
            if let Some(d) = &f.diff {
                let _ = writeln!(
                    o,
                    "\nWhere the base and this change first differ on {} (`hydo diff`): {d}",
                    code(&f.minimal)
                );
            }
            if let Some(r) = &f.renamed_from {
                let _ = writeln!(
                    o,
                    "\n{} fails on the same schedule as the fixed {}: probably the same finding, \
                     with a reworded check.",
                    code(&f.title),
                    code(r)
                );
            }
            if f.proof.as_deref() == Some("base-refused") {
                let why = f
                    .replays
                    .iter()
                    .find_map(|x| x.refused.clone())
                    .unwrap_or_default();
                let _ = writeln!(o, "\nThe base cannot run {}: {why}", code(&f.minimal));
            }
        }
        o.push('\n');
    }
    let back = of_state(r, ci_state::REAPPEARED);
    if !back.is_empty() {
        o.push_str(
            "**Reappeared** (the history recorded it fixed; it fails again here, and the base \
             passes its schedule):\n\n",
        );
        table(&mut o, &back, &sets);
        for f in &back {
            if let Some(at) = &f.fixed_at {
                let _ = writeln!(
                    o,
                    "\n{} was recorded fixed at {}.",
                    code(&f.title),
                    short(at)
                );
            }
        }
        o.push('\n');
    }
    let fixed = of_state(r, ci_state::FIXED);
    if !fixed.is_empty() {
        o.push_str("**Fixed by this change** (replayed here and passed):\n\n");
        list(&mut o, &fixed);
        o.push('\n');
    }
    let changed = of_state(r, ci_state::CHANGED);
    if !changed.is_empty() {
        o.push_str(
            "**Changed** (the base and this change both fail the schedule, differently; not \
             caused by this change):\n\n",
        );
        for f in &changed {
            // a finding of this change replayed on the base, or one of the base replayed here
            let first = f.replays.first();
            let other = first
                .and_then(|x| x.verdict.as_deref())
                .map(hydo_explore::report::sig_key)
                .unwrap_or_default();
            let _ = if first.is_some_and(|x| x.on == "base") {
                writeln!(
                    o,
                    "- {} here, where the base gives {} ({})",
                    code(&f.title),
                    code(&other),
                    code(&f.minimal)
                )
            } else {
                writeln!(
                    o,
                    "- {} on the base now gives {} ({})",
                    code(&f.title),
                    code(&other),
                    code(&f.minimal)
                )
            };
            if let Some(d) = &f.diff {
                let _ = writeln!(o, "  Where they first differ: {d}");
            }
        }
        o.push('\n');
    }
    let old: Vec<&CiFinding> = r
        .findings
        .iter()
        .filter(|f| f.state == ci_state::PRESENT || f.state == ci_state::PRE_EXISTING)
        .collect();
    if !old.is_empty() {
        let _ = writeln!(
            o,
            "**Already on the base** (not caused by this change): {}\n",
            plural(old.len(), "finding", "findings")
        );
        list(&mut o, &old);
        o.push('\n');
    }
    let accepted = of_state(r, ci_state::ACCEPTED);
    if !accepted.is_empty() {
        o.push_str(
            "**Accepted** (`hydo/known-findings.yaml`; reported, never failing the run):\n\n",
        );
        for f in &accepted {
            let a = f.accepted.as_ref();
            let _ = writeln!(
                o,
                "- {} ({}): {}{}{}",
                code(&f.title),
                code(&f.minimal),
                a.map(|a| a.state.as_str()).unwrap_or("?"),
                a.map(|a| format!(", accepted: {}", a.reason.replace('\n', " ")))
                    .unwrap_or_default(),
                a.filter(|a| !a.who.is_empty())
                    .map(|a| format!(" ({})", a.who))
                    .unwrap_or_default()
            );
        }
        o.push('\n');
    }
    if !r.stale_accepted.is_empty() {
        o.push_str(
            "**Accepted findings this run did not find** (remove an entry from \
             `hydo/known-findings.yaml` when it no longer reproduces):\n\n",
        );
        for s in &r.stale_accepted {
            let what = match s.why.as_str() {
                "replay-ok" => "its schedule passes here: it no longer reproduces".to_string(),
                "replay-other" => format!(
                    "its schedule now fails differently: {}",
                    code(
                        &s.replay
                            .as_ref()
                            .and_then(|r| r.verdict.as_deref())
                            .map(hydo_explore::report::sig_key)
                            .unwrap_or_default()
                    )
                ),
                _ => "not found by the exploration, and its schedule could not be replayed"
                    .to_string(),
            };
            let _ = writeln!(
                o,
                "- `{}` {}: {what}",
                s.key,
                if s.title.is_empty() {
                    String::new()
                } else {
                    code(&s.title)
                }
            );
        }
        o.push('\n');
    }
    let unv = of_state(r, ci_state::UNVERIFIED);
    if !unv.is_empty() {
        o.push_str("**Could not be replayed** (a person should look):\n\n");
        for f in &unv {
            let why = if f.proof.as_deref() == Some("head-replay-ok-partial") {
                "its schedule passes here, but the run's budget cut its exploration short, so it is \
                 not called fixed"
                    .to_string()
            } else {
                f.replays
                    .iter()
                    .find_map(|x| x.refused.clone())
                    .unwrap_or_else(|| "the replay gave no result".into())
            };
            let _ = writeln!(o, "- {} ({}): {why}", code(&f.title), code(&f.minimal));
        }
        o.push('\n');
    }
    if !r.regressions.is_empty() {
        let bad: Vec<_> = r.regressions.iter().filter(|g| !g.as_expected).collect();
        if bad.is_empty() {
            let _ = writeln!(
                o,
                "**Saved regressions**: {} of {} as expected.\n",
                r.summary.regressions.as_expected, r.summary.regressions.replayed
            );
        } else {
            o.push_str("**Saved regressions not as expected**:\n\n");
            for g in bad {
                let _ = writeln!(
                    o,
                    "- `{}`: expected {}, got {} ({})",
                    g.name,
                    g.expect,
                    code(
                        &g.error
                            .clone()
                            .or_else(|| g.verdict.clone())
                            .unwrap_or_default()
                    ),
                    code(&g.schedule)
                );
            }
            o.push('\n');
        }
    }
    if r.summary.vacuous_passes > 0 {
        let _ = writeln!(
            o,
            "**Warning**: {} of this run's {} passed without checking anything (the workload \
             acknowledged nothing, or a history was empty). A pass there proves nothing; see the \
             run records' warnings in explore/events.jsonl.\n",
            plural(r.summary.vacuous_passes as usize, "schedule", "schedules"),
            plural(r.summary.runs as usize, "run", "runs")
        );
    }
    if !r.nondeterminism.is_empty() {
        let _ = writeln!(
            o,
            "**hydo problem**: {} gave another fingerprint when run again. This is a bug in hydo \
             or its environment, not a finding of the project; please report it.\n",
            plural(r.nondeterminism.len(), "failure", "failures")
        );
    }
    // the outcome, with the policy
    match r.outcome.exit {
        0 => {
            let _ = writeln!(
                o,
                "Result: passed (fails on: {}).\n",
                r.outcome.fail_on.join(", ")
            );
        }
        1 => {
            let _ = writeln!(
                o,
                "Result: **failed** (fails on: {}): {}.\n",
                r.outcome.fail_on.join(", "),
                r.outcome.failed_by.join("; ")
            );
        }
        _ => o.push_str("Result: the run could not be completed.\n\n"),
    }
    let b = &r.budget;
    if let Some(s) = &b.shard {
        let _ = writeln!(
            o,
            "Shard {s}: seeds {} of {}. `hydo ci merge` makes the run's report from all shards.\n",
            b.seeds,
            b.range.as_deref().unwrap_or("?")
        );
    }
    if b.partial {
        let done = b.completed_seeds.as_deref().filter(|c| !c.is_empty());
        let _ = writeln!(
            o,
            "**Partial run**: {} of {} seeds explored ({}){}. The seeds explored do not depend \
             on the machine's speed: they are the first ones in seed order, so `--seeds {}` \
             explores the same schedules again. A partial run calls no finding fixed.\n",
            b.completed.unwrap_or(0),
            b.planned.unwrap_or(0),
            done.map(|c| format!("seeds {c}"))
                .unwrap_or_else(|| "none".into()),
            match (b.time_limit_s, r.shards.is_empty()) {
                (Some(t), true) => format!(", stopped by the time budget of {}", wall(t)),
                (_, false) => ", from the shards given".to_string(),
                _ => String::new(),
            },
            done.unwrap_or("-"),
        );
    } else if let Some(t) = b.time_limit_s {
        let _ = writeln!(
            o,
            "The time budget of {} let the run explore all {} seeds.\n",
            wall(t),
            b.completed.unwrap_or(0)
        );
    }
    o.push_str("<details><summary>What ran</summary>\n\n");
    let _ = writeln!(
        o,
        "- {} (seeds {}, strategy {}{}), {}, {}, {}.",
        plural(r.summary.runs as usize, "run", "runs"),
        match b.completed_seeds.as_deref() {
            Some(c) if c != b.seeds =>
                format!("{} of {}", if c.is_empty() { "none" } else { c }, b.seeds),
            _ => b.seeds.clone(),
        },
        b.strategy,
        if b.options.is_empty() {
            String::new()
        } else {
            format!(", {}", b.options.join(" "))
        },
        plural(r.summary.replays as usize, "replay", "replays"),
        wall(r.summary.wall_s),
        engine_words(r)
    );
    let words = |m: &std::collections::BTreeMap<String, u64>, f: &dyn Fn(&str) -> String| {
        m.iter()
            .map(|(k, n)| format!("{} {n}", f(k)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !r.coverage.faults.is_empty() {
        let _ = writeln!(
            o,
            "- Faults drawn: {}.",
            words(&r.coverage.faults, &|k| super::key::fault_word(k)
                .to_string())
        );
    }
    if !r.coverage.services.is_empty() {
        let _ = writeln!(
            o,
            "- Services targeted: {}.",
            words(&r.coverage.services, &|k| k.to_string())
        );
    }
    if let Some(p) = &r.coverage.properties {
        if p.not_exercised.is_empty() {
            let _ = writeln!(
                o,
                "- Properties: {} declared, every one reached.",
                p.declared
            );
        } else {
            let _ = writeln!(
                o,
                "- Properties never reached in this run: {}.",
                p.not_exercised
                    .iter()
                    .map(|x| code(x))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let img = &r.image;
    let _ = writeln!(
        o,
        "- Image {}{}.",
        if img.given {
            "given ready-made".to_string()
        } else if img.reused {
            "reused from the image cache".to_string()
        } else {
            "built".to_string()
        },
        img.input_key
            .as_deref()
            .map(|k| format!(" (input key {k})"))
            .unwrap_or_default()
    );
    if let Some(bi) = base.and_then(|b| b.image.as_ref()) {
        let same = bi.sha256.is_some() && bi.sha256 == img.sha256;
        let _ = writeln!(
            o,
            "- Base image {}.",
            if same {
                "the same as this change's (same bytes)".to_string()
            } else if bi.given {
                "given ready-made".to_string()
            } else if bi.reused {
                "reused from the image cache".to_string()
            } else {
                "built".to_string()
            }
        );
    }
    let _ = writeln!(
        o,
        "- hydo {} (fingerprint epoch {}).",
        r.hydo.version, r.hydo.epoch
    );
    for s in &r.shards {
        let _ = writeln!(
            o,
            "- Shard {}: seeds {}{}, {}, {}, {}, exit status {}.",
            s.shard,
            s.seeds,
            s.completed_seeds
                .as_deref()
                .filter(|c| *c != s.seeds)
                .map(|c| format!(" (explored {})", if c.is_empty() { "none" } else { c }))
                .unwrap_or_default(),
            plural(s.runs as usize, "run", "runs"),
            plural(s.replays as usize, "replay", "replays"),
            wall(s.wall_s),
            s.exit
        );
    }
    if let Some(h) = &r.history {
        let _ = writeln!(
            o,
            "- History {}{}: {} open and {} fixed findings before this run.",
            h.store,
            h.branch
                .as_deref()
                .map(|b| format!(", branch {b}"))
                .unwrap_or_default(),
            h.open,
            h.fixed
        );
    }
    if let Some(u) = &r.ci.run_url {
        let _ = writeln!(o, "- Job: {u}");
    }
    o.push_str("\n</details>\n");
    o
}

pub fn wall(s: f64) -> String {
    let s = s.round() as u64;
    if s >= 60 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

/// XML text and attribute values.
pub fn xml(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            // characters XML 1.0 does not allow
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// `junit.xml`: a test suite of findings (a finding in a state the policy fails on is a failed
/// test; one already on the base is skipped; one fixed passes) and one of saved regressions.
pub fn junit(r: &CiRun) -> String {
    let fails = |s: &str| r.outcome.fail_on.iter().any(|f| f == s);
    let class_f = format!("hydo.{}.findings", r.project.name);
    let class_r = format!("hydo.{}.regressions", r.project.name);
    let mut cases_f = String::new();
    let (mut nf, mut ff, mut sf) = (0, 0, 0);
    for f in &r.findings {
        nf += 1;
        let detail = format!(
            "{}\nfinding key: {}\nstate: {}{}\nminimal schedule: {}\nreproduce: hydo run \"{}\"\n",
            f.title,
            f.key,
            f.state,
            f.proof
                .as_deref()
                .map(|p| format!(" (proof: {p})"))
                .unwrap_or_default(),
            f.minimal,
            f.minimal
        );
        let _ = writeln!(
            cases_f,
            "    <testcase classname=\"{}\" name=\"{}\">",
            xml(&class_f),
            xml(&f.title)
        );
        if fails(&f.state) {
            ff += 1;
            let _ = writeln!(
                cases_f,
                "      <failure type=\"{}\" message=\"{} finding: {} (minimal schedule: {})\">{}</failure>",
                xml(&f.state),
                xml(&f.state),
                xml(&f.title),
                xml(&f.minimal),
                xml(&detail)
            );
        } else if f.state == ci_state::ACCEPTED {
            sf += 1;
            let _ = write!(
                cases_f,
                "      <skipped message=\"accepted ({}): {}\"/>\n      <system-out>{}</system-out>\n",
                xml(f.accepted.as_ref().map(|a| a.state.as_str()).unwrap_or("")),
                xml(f.accepted.as_ref().map(|a| a.reason.as_str()).unwrap_or("")),
                xml(&detail)
            );
        } else if f.state == ci_state::PRESENT || f.state == ci_state::PRE_EXISTING {
            sf += 1;
            let _ = write!(
                cases_f,
                "      <skipped message=\"{} on the base, not caused by this change\"/>\n      <system-out>{}</system-out>\n",
                xml(&f.state),
                xml(&detail)
            );
        } else {
            let _ = writeln!(cases_f, "      <system-out>{}</system-out>", xml(&detail));
        }
        cases_f.push_str("    </testcase>\n");
    }
    let mut cases_r = String::new();
    let (mut nr, mut fr) = (0, 0);
    for g in &r.regressions {
        nr += 1;
        let _ = writeln!(
            cases_r,
            "    <testcase classname=\"{}\" name=\"{}\">",
            xml(&class_r),
            xml(&g.name)
        );
        let got = g
            .error
            .clone()
            .or_else(|| g.verdict.clone())
            .unwrap_or_default();
        let detail = format!(
            "schedule: {}\nexpected: {}\ngot: {} (fp={})\nreproduce: hydo run \"{}\"\n",
            g.schedule,
            g.expect,
            got,
            g.fp.as_deref().unwrap_or("-"),
            g.schedule
        );
        if !g.as_expected {
            if fails("regression") {
                fr += 1;
                let _ = writeln!(
                    cases_r,
                    "      <failure type=\"regression\" message=\"expected {}, got {}\">{}</failure>",
                    xml(&g.expect),
                    xml(&got),
                    xml(&detail)
                );
            } else {
                let _ = writeln!(
                    cases_r,
                    "      <skipped message=\"not as expected; the policy does not fail on regressions\"/>"
                );
            }
        } else {
            let _ = writeln!(cases_r, "      <system-out>{}</system-out>", xml(&detail));
        }
        cases_r.push_str("    </testcase>\n");
    }
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        o,
        "<testsuites name=\"hydo\" tests=\"{}\" failures=\"{}\" time=\"{:.1}\">",
        nf + nr,
        ff + fr,
        r.summary.wall_s
    );
    let _ = write!(
        o,
        "  <testsuite name=\"hydo findings: {}\" tests=\"{nf}\" failures=\"{ff}\" skipped=\"{sf}\">\n{cases_f}  </testsuite>\n",
        xml(&r.project.name)
    );
    let _ = write!(
        o,
        "  <testsuite name=\"hydo regressions: {}\" tests=\"{nr}\" failures=\"{fr}\">\n{cases_r}  </testsuite>\n",
        xml(&r.project.name)
    );
    o.push_str("</testsuites>\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> CiRun {
        let mut r = CiRun {
            id: "local:1".into(),
            project: hydo_results::CiProject {
                name: "shop".into(),
                dir: ".".into(),
                manifest: "hydo/hydo.yaml".into(),
                manifest_sha256: None,
            },
            trigger: "pull_request".into(),
            commit: Some("9c41e07aaaaaaaa".into()),
            base: Some(hydo_results::CiBase {
                branch: Some("main".into()),
                commit: Some("4b2d11a".into()),
                findings_from: "explored".into(),
                ..Default::default()
            }),
            hydo: hydo_results::CiHydo {
                version: "0.2.0".into(),
                commit: None,
                epoch: 1,
            },
            budget: hydo_results::CiBudget {
                mode: "pr".into(),
                seeds: "1-60".into(),
                strategy: "process".into(),
                options: vec!["--protocol-faults".into()],
                ..Default::default()
            },
            outcome: hydo_results::CiOutcome {
                exit: 1,
                fail_on: vec!["new".into(), "reappeared".into(), "regression".into()],
                failed_by: vec!["new finding VIOLATION [a.b]".into()],
            },
            ..Default::default()
        };
        r.findings.push(CiFinding {
            key: "f1:invariant:0123456789".into(),
            kind: "invariant".into(),
            title: "VIOLATION [a.b] o# charged # times".into(),
            state: "new".into(),
            proof: Some("base-replay-ok".into()),
            minimal: "disk=none | run H=p.c@0-1:drop".into(),
            ..Default::default()
        });
        r.findings.push(CiFinding {
            key: "f1:invariant:9999999999".into(),
            kind: "invariant".into(),
            title: "VIOLATION <x> & y".into(),
            state: "present".into(),
            minimal: "disk=cut | run C=3".into(),
            ..Default::default()
        });
        r
    }

    #[test]
    fn the_report_names_new_findings_with_their_proof() {
        let m = markdown(&run(), &[("variant".into(), "buggy".into())]);
        assert!(m.starts_with("### hydo: 1 new finding (failed)\n"), "{m}");
        assert!(m.contains("**New in this change** (the base 4b2d11a passes the same schedule)"));
        assert!(m.contains(
            "| `VIOLATION [a.b] o# charged # times` | `disk=none \\| run H=p.c@0-1:drop` |"
        ));
        assert!(m.contains("`hydo build --set variant=buggy`"));
        assert!(m.contains("**Already on the base** (not caused by this change): 1 finding"));
        assert!(m.contains("Result: **failed**"));
    }

    #[test]
    fn a_pass_that_checked_nothing_is_said() {
        let mut r = run();
        r.summary.runs = 5;
        r.summary.vacuous_passes = 5;
        let m = markdown(&r, &[]);
        assert!(
            m.contains("**Warning**: 5 schedules of this run's 5 runs passed without checking"),
            "{m}"
        );
    }

    #[test]
    fn junit_fails_only_by_the_policy_and_escapes() {
        let j = junit(&run());
        assert!(j.contains("<testsuites name=\"hydo\" tests=\"2\" failures=\"1\""));
        assert!(j.contains("<failure type=\"new\""));
        assert!(j.contains("name=\"VIOLATION &lt;x&gt; &amp; y\""));
        assert!(j.contains("<skipped message=\"present on the base"));
        assert_eq!(xml("a\u{1}b"), "a b");
    }

    /// Stage 2: an accepted finding, an entry that no longer reproduces, and a budget's cut.
    #[test]
    fn accepted_stale_and_partial_are_said() {
        let mut r = run();
        r.findings[0].accepted = Some(hydo_results::CiAccepted {
            reason: "documented window".into(),
            state: "new".into(),
            ..Default::default()
        });
        r.findings[0].state = "accepted".into();
        r.stale_accepted.push(hydo_results::CiStaleAcceptance {
            key: "f1:invariant:00000000aa".into(),
            title: "VIOLATION [old]".into(),
            reason: "r".into(),
            why: "replay-ok".into(),
            replay: None,
        });
        r.budget.time_limit_s = Some(90.0);
        r.budget.completed_seeds = Some("1-8".into());
        r.budget.planned = Some(60);
        r.budget.completed = Some(8);
        r.budget.partial = true;
        r.outcome = hydo_results::CiOutcome {
            fail_on: vec!["new".into()],
            ..Default::default()
        };
        super::super::run::decide(&mut r);
        let m = markdown(&r, &[]);
        assert!(
            m.starts_with("### hydo: no new findings, 1 accepted, partial: 8 of 60 seeds (passed)"),
            "{m}"
        );
        assert!(m.contains("new, accepted: documented window"), "{m}");
        assert!(m.contains("`f1:invariant:00000000aa` `VIOLATION [old]`: its schedule passes here"));
        assert!(m.contains("**Partial run**: 8 of 60 seeds explored (seeds 1-8), stopped by the time budget of 1 min 30 s"));
        assert!(m.contains("(seeds 1-8 of 1-60, strategy"), "{m}");
        let j = junit(&r);
        assert!(
            j.contains("<skipped message=\"accepted (new): documented window\"/>"),
            "{j}"
        );
        assert!(j.contains("failures=\"0\""));
    }
}
