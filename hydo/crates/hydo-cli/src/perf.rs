//! Performance tests (Milestone 237): a run's `perf` measures and `hydo perf compare`.
//!
//! An image whose manifest has `perf:` runs its workload in marked calls (one per step by
//! default, hydo-control `schedule/perf.rs`). `hydo run` turns the marks into measures:
//!
//! - the engine's work between a call's two marks: on hvx the guest's retired conditional
//!   branches (`<tag>.work.jsonl`, which the engine writes with `HYDO_WORK_RECORD`, set for such
//!   an image) and the guest clock's nanoseconds; on KVM and the Mac the VM exits (the stops'
//!   clock in the control transcript);
//! - `workload`, the sum of the calls;
//! - the workload's own numbers (`HYDO-PERF <name> <value> [<unit>]` report lines).
//!
//! A run is deterministic, so the measures of one image and schedule are the same in every run.
//! `hydo perf compare A B` says which measures of run B differ from run A by more than the
//! threshold, in words.

use crate::ctx::{die_rc, read_text, take, Ctx};
use hydo_control::transcript::{Record, Transcript};
use hydo_results::{Perf, PerfMeasure, Run};
use std::collections::BTreeMap;
use std::path::Path;

pub const HELP: &str = r#"hydo perf -- performance tests from deterministic runs (hydo guide workflow)

    hydo perf compare <run A> <run B> [--threshold PCT] [--all]
    hydo perf show <run>

  An image whose hydo.yaml has `perf:` records performance measures in each run's result.json:
  the work of the whole workload and of each step, counted by the engine, and the numbers the
  workload wrote to $HYDO_PERF. On hvx the count is the guest's retired branches, which the CPU
  counts exactly, so an unchanged image gives the same number in every run and a change of
  0.1 % is real. On KVM and the Mac the count is VM exits, which do not see CPU work.

  compare   prints what changed from run A (the base) to run B, in words, for example
            "step 3 did 4.80% more work". A change smaller than the threshold (perf.threshold
            of the manifest, else 0.5 %) reads as no change. --all also lists the measures that
            did not change, and hvx's guest time, which does not follow the work.
  show      prints a run's measures.

  The exit status of compare is 1 when a measure of the engine grew by more than the
  threshold (more work: slower), 0 otherwise, and 2 when the runs cannot be compared.
"#;

/// The guest clock of hvx: its virtual TSC runs at 1.6 GHz (hvx.c `HVX_PMU_TICK_TSC`).
const HVX_TSC_PER_US: u64 = 1600;

/// One stop's counters in `<tag>.work.jsonl` (hvx).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    at: u64,
    br: u64,
    tsc: u64,
}

/// The work records of a file, by stop number (the last record of a number with the stop's
/// clock wins: a run resumed from a snapshot numbers its first stop as the snapshot's).
fn work_records(text: &str) -> BTreeMap<u64, Vec<Work>> {
    let mut m: BTreeMap<u64, Vec<Work>> = BTreeMap::new();
    for l in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        let g = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
        if let (Some(n), Some(at), Some(br), Some(tsc)) = (g("n"), g("at"), g("br"), g("tsc")) {
            m.entry(n).or_default().push(Work { at, br, tsc });
        }
    }
    m
}

/// One measured workload call: its steps and the stops of its two marks.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Window {
    a: i64,
    b: i64,
    start: (u64, u64),
    end: (u64, u64),
}

impl Window {
    fn name(&self) -> String {
        if self.b == self.a + 1 {
            format!("step {}", self.a)
        } else {
            format!("steps {}-{}", self.a, self.b - 1)
        }
    }
}

/// The marked calls (with each mark's stop number and clock) and the workload's own records of
/// a run's transcript.
fn windows(t: &Transcript) -> (Vec<Window>, Vec<String>) {
    let mut at: BTreeMap<u64, u64> = BTreeMap::new();
    let mut open: BTreeMap<(i64, i64), (u64, u64)> = BTreeMap::new();
    let mut out = Vec::new();
    let mut recs = Vec::new();
    for r in &t.records {
        match r {
            Record::Stop { n, at: a, .. } => {
                at.insert(*n, *a);
            }
            Record::Report { n, line, .. } => {
                let here = (*n, at.get(n).copied().unwrap_or(0));
                if let Some(rest) = line.strip_prefix("HYDO-PERF-MARK ") {
                    let w: Vec<&str> = rest.split_whitespace().collect();
                    let ab = match (w.get(1), w.get(2)) {
                        (Some(a), Some(b)) => a.parse().ok().zip(b.parse().ok()),
                        _ => None,
                    };
                    match (w.first().copied(), ab) {
                        (Some("start"), Some(ab)) => {
                            open.insert(ab, here);
                        }
                        (Some("end"), Some((a, b))) => {
                            if let Some(start) = open.remove(&(a, b)) {
                                out.push(Window {
                                    a,
                                    b,
                                    start,
                                    end: here,
                                });
                            }
                        }
                        _ => {}
                    }
                } else if let Some(rest) = line.strip_prefix("HYDO-PERF ") {
                    recs.push(rest.to_string());
                }
            }
            _ => {}
        }
    }
    (out, recs)
}

/// The work record of stop `(n, at)`.
fn work_at(w: &BTreeMap<u64, Vec<Work>>, (n, at): (u64, u64)) -> Option<Work> {
    w.get(&n)?.iter().rev().find(|x| x.at == at).copied()
}

/// One workload record line (`<name> <value> [<unit>]`) as a measure.
fn record_measure(line: &str) -> Option<PerfMeasure> {
    let mut w = line.split_whitespace();
    let name = w.next()?.to_string();
    let value: f64 = w.next()?.parse().ok().filter(|v: &f64| v.is_finite())?;
    Some(PerfMeasure {
        name,
        source: "workload".into(),
        unit: w.next().unwrap_or("").to_string(),
        value,
    })
}

/// The `perf` measures of a run: from its transcript (`control.jsonl`) and, on hvx, the engine's
/// work records (`one.work.jsonl`). None when the run has no marked call and no record.
pub fn of_transcript(
    t: &Transcript,
    work: Option<&str>,
    engine: &str,
    threshold: f64,
) -> Option<Perf> {
    let (ws, recs) = windows(t);
    if ws.is_empty() && recs.is_empty() {
        return None;
    }
    // without hvx's branch clock the counter stays 0: the stops' clock (exits) is the measure
    let work = work
        .map(work_records)
        .filter(|w| w.values().flatten().any(|x| x.br != 0));
    let mut per: Vec<(String, Vec<(&'static str, f64)>)> = Vec::new();
    for w in &ws {
        let mut v = Vec::new();
        if let Some(wk) = &work {
            if let (Some(s), Some(e)) = (work_at(wk, w.start), work_at(wk, w.end)) {
                v.push(("branches", e.br.wrapping_sub(s.br) as f64));
                let ns = e.tsc.wrapping_sub(s.tsc).saturating_mul(1000) / HVX_TSC_PER_US;
                v.push(("guest_ns", ns as f64));
            }
        }
        if work.is_none() {
            v.push(("exits", w.end.1.saturating_sub(w.start.1) as f64));
        }
        per.push((w.name(), v));
    }
    let unit = if work.is_some() { "branches" } else { "exits" };
    let mut measures = Vec::new();
    if !per.is_empty() {
        let mut total: Vec<(&'static str, f64)> = Vec::new();
        for (_, v) in &per {
            for (u, x) in v {
                match total.iter_mut().find(|(t, _)| t == u) {
                    Some(t) => t.1 += x,
                    None => total.push((u, *x)),
                }
            }
        }
        for (name, v) in std::iter::once(("workload".to_string(), total)).chain(per) {
            for (u, x) in v {
                measures.push(PerfMeasure {
                    name: name.clone(),
                    source: "engine".into(),
                    unit: u.into(),
                    value: x,
                });
            }
        }
    }
    measures.extend(recs.iter().map(String::as_str).filter_map(record_measure));
    Some(Perf {
        engine: engine.into(),
        unit: unit.into(),
        threshold,
        measures,
    })
}

/// A run directory's `perf`, for an image whose plan has `perf:` (`hydo run`'s result.json).
pub fn of_run_dir(outdir: &Path, image: &Path, engine: &str) -> Option<Perf> {
    let plan = hydo_control::agent::plan_of_image(image).ok()?;
    let p = plan.perf?;
    let t = Transcript::parse(&read_text(&outdir.join("control.jsonl")).ok()?).ok()?;
    let work = read_text(&outdir.join("one.work.jsonl")).ok();
    of_transcript(&t, work.as_deref(), engine, p.threshold_bp as f64 / 100.0)
}

// ------------------------------------------------------------------ the comparison

/// One measure of both runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub name: String,
    pub source: String,
    pub unit: String,
    pub a: f64,
    pub b: f64,
}

impl Change {
    /// The change in percent of A (None when A is 0 and B is not).
    pub fn pct(&self) -> Option<f64> {
        if self.a == 0.0 {
            return (self.b == 0.0).then_some(0.0);
        }
        Some((self.b - self.a) / self.a * 100.0)
    }

    /// Whether the change is at least `threshold` percent (a change from 0 always is).
    pub fn changed(&self, threshold: f64) -> bool {
        self.pct().is_none_or(|p| p.abs() >= threshold && p != 0.0)
    }

    /// The change in words for a reader who knows the system, not hydo.
    pub fn words(&self) -> String {
        let what = if self.name == "workload" {
            "the whole workload".to_string()
        } else {
            self.name.clone()
        };
        let nums = format!(
            "{} -> {}{}",
            num(self.a),
            num(self.b),
            if self.unit.is_empty() {
                String::new()
            } else {
                format!(" {}", self.unit_words())
            }
        );
        let Some(p) = self.pct() else {
            return format!("{what} is new: {nums}");
        };
        if p == 0.0 {
            return format!("{what} did not change: {nums}");
        }
        let more = if p > 0.0 { "more" } else { "less" };
        let pc = pct_text(p.abs());
        match (self.source.as_str(), self.unit.as_str()) {
            ("engine", "guest_ns") => format!("{what} took {pc} {more} guest time: {nums}"),
            ("engine", _) => format!(
                "{what} did {pc} {more} work ({}): {nums}",
                if p > 0.0 { "slower" } else { "faster" }
            ),
            _ => format!(
                "{what} {} {pc}: {nums}",
                if p > 0.0 { "rose by" } else { "fell by" }
            ),
        }
    }

    fn unit_words(&self) -> &str {
        match self.unit.as_str() {
            "branches" => "branches",
            "exits" => "VM exits",
            "guest_ns" => "ns of guest time",
            u => u,
        }
    }
}

/// A percentage with two decimals, or with the digits that show it when it is under 0.005 %
/// (one branch in 228 million is 0.0000004%, not 0.00%).
fn pct_text(p: f64) -> String {
    if p >= 0.005 {
        return format!("{p:.2}%");
    }
    let digits = (-p.log10()).ceil().clamp(2.0, 12.0) as usize;
    format!("{p:.digits$}%")
}

/// A number with thousands separators (and two decimals when it has a fraction).
fn num(x: f64) -> String {
    if x.fract() != 0.0 {
        return format!("{x:.2}");
    }
    let s = format!("{}", x.abs() as u64);
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if x < 0.0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Every measure of A and B, in A's order then B's new ones.
pub fn changes(a: &Perf, b: &Perf) -> Vec<Change> {
    let mut out: Vec<Change> = a
        .measures
        .iter()
        .map(|m| Change {
            name: m.name.clone(),
            source: m.source.clone(),
            unit: m.unit.clone(),
            a: m.value,
            b: f64::NAN,
        })
        .collect();
    for m in &b.measures {
        match out
            .iter_mut()
            .find(|c| c.name == m.name && c.unit == m.unit && c.source == m.source && c.b.is_nan())
        {
            Some(c) => c.b = m.value,
            None => out.push(Change {
                name: m.name.clone(),
                source: m.source.clone(),
                unit: m.unit.clone(),
                a: 0.0,
                b: m.value,
            }),
        }
    }
    out
}

/// What `hydo perf compare` prints, and whether a measure of the engine grew past the threshold.
pub fn report(a: &Perf, b: &Perf, threshold: f64, all: bool) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let unit = match a.unit.as_str() {
        "branches" => "the guest's retired branches, counted exactly by the CPU",
        _ => "VM exits (this engine does not count CPU work between two exits)",
    };
    lines.push(format!(
        "hydo perf: run B against run A on {}, counted in {unit}; a change under {threshold}% \
         reads as no change",
        a.engine
    ));
    let cs = changes(a, b);
    let mut slower = false;
    let (mut same, mut exact, mut gone, mut hidden) = (0, 0, Vec::new(), 0);
    for c in &cs {
        // the engine's other units (hvx's guest time) do not track the work: --all shows them
        if !all && c.source == "engine" && c.unit != a.unit {
            hidden += 1;
            continue;
        }
        if c.b.is_nan() {
            gone.push(c.name.clone());
            continue;
        }
        if c.changed(threshold) {
            if c.source == "engine" && c.unit == a.unit && c.pct().is_none_or(|p| p > 0.0) {
                slower = true;
            }
            lines.push(format!("  {}", c.words()));
        } else {
            same += 1;
            if c.a == c.b {
                exact += 1;
            }
            if all {
                lines.push(format!("  (no change) {}", c.words()));
            }
        }
    }
    if !gone.is_empty() {
        lines.push(format!("  only in run A: {}", gone.join(", ")));
    }
    let n = cs.len() - gone.len() - hidden;
    lines.push(if exact == n {
        format!("  no change: all {n} measures are exactly the same (0.00%)")
    } else if same == n {
        format!("  no change: all {n} measures are within {threshold}%")
    } else {
        format!("  no change in {same} of {n} measures (within {threshold}%)")
    });
    if hidden > 0 {
        lines.push(format!(
            "  ({hidden} measures of guest time are not shown: guest time does not follow the \
             work; --all shows them)"
        ));
    }
    (lines, slower)
}

/// A run's `perf` from its directory (or its result.json).
fn perf_of(p: &str) -> Result<(Run, Perf), String> {
    let path = Path::new(p);
    let doc = if path.is_dir() {
        path.join(hydo_results::RUN_DOC)
    } else {
        path.to_path_buf()
    };
    let run =
        hydo_results::reader::read_run(&doc).map_err(|e| format!("{}: {e}", doc.display()))?;
    let perf = run.perf.clone().ok_or_else(|| {
        format!(
            "{} has no performance measures: the image's hydo.yaml needs `perf:` (hydo guide workflow)",
            doc.display()
        )
    })?;
    Ok((run, perf))
}

/// `hydo perf compare|show ...`.
pub fn cmd_perf(_ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let threshold: Option<f64> = take(&mut args, "--threshold").map(|t| {
        t.parse()
            .ok()
            .filter(|x: &f64| *x >= 0.0)
            .unwrap_or_else(|| die_rc(&format!("--threshold: a percentage (got {t:?})"), 2))
    });
    let all = if let Some(i) = args.iter().position(|a| a == "--all") {
        args.remove(i);
        true
    } else {
        false
    };
    let sub = args.first().cloned().unwrap_or_default();
    match (sub.as_str(), args.len()) {
        ("compare", 3) => {
            let (ra, a) = perf_of(&args[1]).unwrap_or_else(|e| die_rc(&e, 2));
            let (rb, b) = perf_of(&args[2]).unwrap_or_else(|e| die_rc(&e, 2));
            if a.engine != b.engine {
                die_rc(
                    &format!(
                        "run A ran on {} and run B on {}: work counts of two engines are not comparable",
                        a.engine, b.engine
                    ),
                    2,
                );
            }
            if ra.epoch != rb.epoch {
                eprintln!(
                    "hydo perf: note: the runs come from two fingerprint epochs ({:?} and {:?}); \
                     an engine change between them can move the counts",
                    ra.epoch, rb.epoch
                );
            }
            let cpu = |r: &Run| {
                r.engine
                    .as_ref()
                    .and_then(|e| e.host_cpu.as_ref())
                    .map(|c| format!("{c:?}"))
            };
            if a.engine == "kvm" && cpu(&ra) != cpu(&rb) {
                eprintln!(
                    "hydo perf: note: the runs come from two host CPU models; KVM's counts can \
                     differ between models"
                );
            }
            let t = threshold.unwrap_or(a.threshold);
            let (lines, slower) = report(&a, &b, t, all);
            for l in lines {
                println!("{l}");
            }
            i32::from(slower)
        }
        ("show", 2) => {
            let (_, a) = perf_of(&args[1]).unwrap_or_else(|e| die_rc(&e, 2));
            println!(
                "hydo perf: {} measures on {} ({})",
                a.measures.len(),
                a.engine,
                a.unit
            );
            for m in &a.measures {
                println!("  {:<24} {:>18} {}", m.name, num(m.value), m.unit);
            }
            0
        }
        _ => {
            eprint!("{HELP}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(at: &[(u64, u64)], reports: &[(u64, &str)]) -> Transcript {
        let mut text = String::from(
            r#"{"cadence":"default","control":1,"controller":"schedule","conversation":"run","schedule":"disk=none | run","start":"resume S1"}"#,
        );
        text.push('\n');
        let mut recs: Vec<(u64, String)> = Vec::new();
        for (n, a) in at {
            recs.push((
                *n * 10,
                format!(r#"{{"at":{a},"n":{n},"seen":0,"t":"stop","why":"cadence"}}"#),
            ));
        }
        for (i, (n, l)) in reports.iter().enumerate() {
            recs.push((
                *n * 10 + 1 + i as u64,
                format!(
                    r#"{{"by":"main","line":{},"n":{n},"t":"report"}}"#,
                    serde_json::to_string(l).unwrap()
                ),
            ));
        }
        recs.sort();
        for (_, r) in recs {
            text.push_str(&r);
            text.push('\n');
        }
        Transcript::parse(&text).unwrap()
    }

    fn marks() -> Transcript {
        transcript(
            &[(3, 100), (5, 160), (7, 250)],
            &[
                (3, "HYDO-PERF-MARK start 0 1"),
                (5, "HYDO-PERF-MARK end 0 1 rc=0"),
                (5, "HYDO-PERF-MARK start 1 2"),
                (7, "HYDO-PERF-MARK end 1 2 rc=0"),
                (7, "HYDO-PERF put_p50_ns 1200 ns"),
            ],
        )
    }

    fn value(p: &Perf, name: &str, unit: &str) -> f64 {
        p.measures
            .iter()
            .find(|m| m.name == name && m.unit == unit)
            .map(|m| m.value)
            .unwrap_or_else(|| panic!("{name} {unit} in {p:?}"))
    }

    #[test]
    fn exits_per_step_come_from_the_stops_clock() {
        let p = of_transcript(&marks(), None, "kvm", 0.5).unwrap();
        assert_eq!(p.unit, "exits");
        assert_eq!(value(&p, "workload", "exits"), 150.0);
        assert_eq!(value(&p, "step 0", "exits"), 60.0);
        assert_eq!(value(&p, "step 1", "exits"), 90.0);
        let r = p.measures.last().unwrap();
        assert_eq!(
            (r.name.as_str(), r.source.as_str()),
            ("put_p50_ns", "workload")
        );
        assert_eq!((r.value, r.unit.as_str()), (1200.0, "ns"));
    }

    #[test]
    fn branches_come_from_the_work_records_of_the_same_stops() {
        let work = [
            r#"{"at":100,"br":1000,"n":3,"phase":"workload","rdtsc":0,"tsc":16000,"why":"cadence"}"#,
            // an earlier stop with the same number and another clock (a resumed snapshot's)
            r#"{"at":90,"br":7,"n":5,"phase":"workload","rdtsc":0,"tsc":0,"why":"cadence"}"#,
            r#"{"at":160,"br":5000,"n":5,"phase":"workload","rdtsc":0,"tsc":32000,"why":"cadence"}"#,
            r#"{"at":250,"br":5500,"n":7,"phase":"workload","rdtsc":0,"tsc":48000,"why":"cadence"}"#,
        ]
        .join("\n");
        let p = of_transcript(&marks(), Some(&work), "hvx", 0.5).unwrap();
        assert_eq!(p.unit, "branches");
        assert_eq!(value(&p, "step 0", "branches"), 4000.0);
        assert_eq!(value(&p, "step 1", "branches"), 500.0);
        assert_eq!(value(&p, "workload", "branches"), 4500.0);
        // 16,000 ticks of the 1.6 GHz guest TSC are 10 us
        assert_eq!(value(&p, "step 0", "guest_ns"), 10_000.0);
        assert!(p.measures.iter().all(|m| m.unit != "exits"));
    }

    #[test]
    fn without_the_branch_clock_the_measure_is_exits() {
        let work =
            r#"{"at":100,"br":0,"n":3,"phase":"workload","rdtsc":0,"tsc":16000,"why":"cadence"}"#;
        let p = of_transcript(&marks(), Some(work), "hvx", 0.5).unwrap();
        assert_eq!(p.unit, "exits");
        assert_eq!(value(&p, "workload", "exits"), 150.0);
    }

    #[test]
    fn a_run_without_marks_has_no_measures() {
        let t = transcript(&[(3, 100)], &[(3, "hydo-explore-acked: 1 2")]);
        assert_eq!(of_transcript(&t, None, "kvm", 0.5), None);
    }

    fn perf(vals: &[(&str, f64)]) -> Perf {
        Perf {
            engine: "hvx".into(),
            unit: "branches".into(),
            threshold: 0.5,
            measures: vals
                .iter()
                .map(|(n, v)| PerfMeasure {
                    name: n.to_string(),
                    source: "engine".into(),
                    unit: "branches".into(),
                    value: *v,
                })
                .collect(),
        }
    }

    #[test]
    fn the_same_counts_report_no_change() {
        let a = perf(&[("workload", 1000.0), ("step 0", 400.0), ("step 1", 600.0)]);
        let (lines, slower) = report(&a, &a, 0.5, false);
        assert!(!slower);
        assert_eq!(
            lines.last().unwrap(),
            "  no change: all 3 measures are exactly the same (0.00%)"
        );
    }

    #[test]
    fn a_step_with_more_work_is_named_in_words() {
        let a = perf(&[("workload", 1000.0), ("step 0", 400.0), ("step 3", 600.0)]);
        let b = perf(&[("workload", 1029.0), ("step 0", 400.0), ("step 3", 628.8)]);
        let (lines, slower) = report(&a, &b, 0.5, false);
        assert!(slower);
        assert_eq!(
            lines[1],
            "  the whole workload did 2.90% more work (slower): 1,000 -> 1,029 branches"
        );
        assert_eq!(
            lines[2],
            "  step 3 did 4.80% more work (slower): 600 -> 628.80 branches"
        );
        assert_eq!(lines[3], "  no change in 1 of 3 measures (within 0.5%)");
    }

    #[test]
    fn a_change_under_the_threshold_is_no_change_and_less_work_is_not_a_failure() {
        let a = perf(&[("workload", 100_000.0), ("step 0", 1000.0)]);
        let b = perf(&[("workload", 100_400.0), ("step 0", 900.0)]);
        let (lines, slower) = report(&a, &b, 0.5, false);
        assert!(!slower);
        assert_eq!(
            lines[1],
            "  step 0 did 10.00% less work (faster): 1,000 -> 900 branches"
        );
        let (lines, _) = report(&a, &b, 0.5, true);
        assert!(lines[1].starts_with("  (no change) the whole workload did 0.40% more work"));
    }

    #[test]
    fn guest_time_shows_only_with_all() {
        let mut a = perf(&[("workload", 1000.0)]);
        let mut b = perf(&[("workload", 1000.0)]);
        for (p, ns) in [(&mut a, 5000.0), (&mut b, 5300.0)] {
            p.measures.push(PerfMeasure {
                name: "workload".into(),
                source: "engine".into(),
                unit: "guest_ns".into(),
                value: ns,
            });
        }
        let (lines, slower) = report(&a, &b, 0.5, false);
        assert!(!slower);
        assert_eq!(
            lines[1],
            "  no change: all 1 measures are exactly the same (0.00%)"
        );
        assert!(lines[2].starts_with("  (1 measures of guest time are not shown"));
        let (lines, slower) = report(&a, &b, 0.5, true);
        assert!(!slower, "guest time never fails a comparison");
        assert!(lines.iter().any(|l| l
            == "  the whole workload took 6.00% more guest time: 5,000 -> 5,300 ns of guest time"));
    }

    #[test]
    fn a_tiny_change_shows_its_digits() {
        assert_eq!(pct_text(4.8), "4.80%");
        assert_eq!(pct_text(0.005), "0.01%");
        assert_eq!(pct_text(1.0 / 228_035_550.0 * 100.0), "0.0000004%");
        let c = Change {
            name: "workload".into(),
            source: "engine".into(),
            unit: "branches".into(),
            a: 228_035_550.0,
            b: 228_035_551.0,
        };
        assert_eq!(
            c.words(),
            "the whole workload did 0.0000004% more work (slower): 228,035,550 -> 228,035,551 \
             branches"
        );
    }

    #[test]
    fn workload_numbers_rise_and_fall() {
        let c = Change {
            name: "put_p99_ns".into(),
            source: "workload".into(),
            unit: "ns".into(),
            a: 2000.0,
            b: 2100.0,
        };
        assert_eq!(c.words(), "put_p99_ns rose by 5.00%: 2,000 -> 2,100 ns");
    }
}
