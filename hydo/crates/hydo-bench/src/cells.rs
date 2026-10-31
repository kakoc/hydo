//! Benchmark cells (run.py `schedules`, `run_seeded`, `table`; bench_more_faults.py).
//!
//! A cell is one strategy (with its options) on one defect for some repetitions. Repetition r
//! uses the seeds r*1000+1 .. r*1000+budget (the M42/M44 repetitions), runs them in seed order
//! through a sliding window of schedules in flight, and stops at the first DETECTION: the lowest
//! seed, with every lower seed finished, whose run is in class `durable-defaults`, has a VIOLATION
//! verdict, and whose schedule passes on the control image (checked on a one-worker pool of the
//! control; a failure on both is a "base bug hit", not this defect). The detecting run is run
//! once more and its fingerprint compared.

use crate::defects::{crash_verdict, sig_match, Set};
use crate::pools::Engines;
use hydo_explore::gen::Ex;
use hydo_explore::pool::{Pool, Res};
use hydo_explore::py::{dumps_line, fl, py_str, round1};
use hydo_explore::report::{assert_failures, verdict_kind};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// One cell's options (run.py's command line, plus bench_more_faults.py's `--cf-range`).
#[derive(Clone, Debug)]
pub struct Cell {
    pub strategy: String,
    pub knob_rule_any: bool,
    pub new_faults: bool,
    pub buggify: bool,
    pub concurrency: bool,
    pub more_faults: bool,
    pub cf_range: Option<(i64, i64)>,
    pub vcpus: u32,
    pub turns: bool,
    pub window: usize,
    pub reps: Vec<i64>,
    pub budget: i64,
    pub workers: usize,
    pub out: PathBuf,
}

impl Cell {
    /// The explorer's state for this cell's schedules (explore.py's globals as run.py set them).
    pub fn ex(&self) -> Ex {
        Ex {
            knob_rule_any: self.knob_rule_any,
            new_faults: self.new_faults,
            buggify: self.buggify,
            concurrency: self.concurrency,
            more_faults: self.more_faults,
            cf_range: self.cf_range,
            turns: self.turns,
            ..Ex::default()
        }
    }

    /// The strategy's label in results.jsonl and the table.
    pub fn label(&self) -> String {
        let mut l = self.strategy.clone();
        if self.new_faults && (self.strategy == "random" || self.strategy == "swarm") {
            l.push_str("+nf");
        }
        if self.buggify {
            l.push_str("+bg");
        }
        if self.concurrency {
            l.push_str("+cc");
        }
        if self.vcpus != 1 {
            l.push_str(&format!("+v{}", self.vcpus));
        }
        if self.turns {
            l.push_str("+turns");
        }
        if self.more_faults {
            l.push_str("+xcf");
        }
        l
    }
}

/// run.py `schedules`: the schedule string of every seed, in run.py's composition order
/// turns(buggify(concurrency(new_faults(gen)))) (the M56 generators swarm-pos and process draw
/// every fault kind themselves, so they get no new_faults). With `more_faults`, the timed kill
/// `X=` and the k-th-flush cut `cf=` are added to the printed schedule from their own stream, as
/// bench_more_faults.py did (parse, add_more_faults, print), so every other field is unchanged.
pub fn schedules(ex: &Ex, strategy: &str, seeds: &[i64]) -> Result<Vec<(i64, String)>, String> {
    let mut out = Vec::with_capacity(seeds.len());
    for &sd in seeds {
        let u = sd as u64;
        let mut s = ex.generator(strategy, u);
        if strategy != "swarm-pos" && strategy != "process" {
            s = ex.add_new_faults(s, u, strategy == "swarm");
        }
        s = ex.add_turns(ex.add_buggify(ex.add_concurrency(s, u), u), u);
        let mut text = ex.render(&s);
        if ex.more_faults {
            let p = ex.parse(&text)?;
            text = ex.render(&ex.add_more_faults(p, u, strategy));
        }
        out.push((sd, text));
    }
    Ok(out)
}

/// The control image's pool and its answers (run.py `Control`): a candidate detection counts
/// only if the SAME schedule passes on the control.
pub struct Control {
    pool: Pool,
    cache: HashMap<String, Res>,
    dout: PathBuf,
    defect: String,
}

impl Control {
    pub fn new(eng: &Engines, set: &Set, out: &Path, defect: &str) -> Control {
        let dout = out.join(defect).join("control");
        let _ = std::fs::create_dir_all(&dout);
        Control {
            pool: eng.pool(&set.control_of(defect), 1, &dout),
            cache: HashMap::new(),
            dout,
            defect: defect.to_string(),
        }
    }

    /// (passes, the control's result) for a schedule; each schedule runs once.
    pub fn passes(&mut self, text: &str) -> (bool, Res) {
        if !self.cache.contains_key(text) {
            let tag = format!("c{}", self.cache.len());
            let got = self.pool.run_all(&[(tag.clone(), text.to_string())], false);
            let mut r = got.get(&tag).map(|(_, r)| r.clone()).unwrap_or_default();
            let v = crash_verdict(&self.dout, &tag, &py_str(&r["verdict"]), &self.defect);
            r.insert("verdict".into(), json!(v));
            self.cache.insert(text.to_string(), r);
        }
        let r = self.cache[text].clone();
        (verdict_kind(&py_str(&r["verdict"])) == "ok", r)
    }

    pub fn close(self) {
        self.pool.close();
    }
}

/// run.py `detects`.
pub fn detects(rec: &Map<String, Value>) -> bool {
    rec.get("class").and_then(Value::as_str) == Some("durable-defaults")
        && verdict_kind(&py_str(&rec["verdict"])) == "violation"
}

/// run.py `make_rec`: one run's record (runs-rep<r>.jsonl).
pub fn make_rec(
    ex: &Ex,
    outdir: &Path,
    sd: i64,
    text: &str,
    r: &Res,
    defect: &str,
) -> Map<String, Value> {
    let tag = format!("seed-{sd}");
    let class = ex
        .parse(text)
        .map(|s| ex.finding_class(&s))
        .unwrap_or_else(|e| panic!("cannot read the schedule {text:?}: {e}"));
    let mut rec = Map::new();
    rec.insert("seed".into(), json!(sd));
    rec.insert("schedule".into(), json!(text));
    rec.insert("class".into(), json!(class));
    for (k, v) in r {
        rec.insert(k.clone(), v.clone());
    }
    let v = crash_verdict(outdir, &tag, &py_str(&rec["verdict"]), defect);
    rec.insert("verdict".into(), json!(v));
    let af = assert_failures(outdir, &tag);
    let afl = !af.is_empty();
    let ids: Vec<String> = af.iter().map(py_str).collect();
    rec.insert("assert_failures".into(), Value::Array(af));
    if afl && verdict_kind(&py_str(&r["verdict"])) == "ok" {
        rec.insert(
            "verdict".into(),
            json!(format!("VIOLATION assertion {}", ids.join(","))),
        );
    }
    rec
}

fn obj(pairs: &[(&str, Value)]) -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v.clone());
    }
    m
}

/// run.py `run_seeded`: every repetition of one defect; each repetition's result is printed and
/// returned (the caller appends them to results.jsonl).
pub fn run_defect(
    c: &Cell,
    set: &Set,
    eng: &Engines,
    defect: &str,
) -> Result<Vec<Map<String, Value>>, String> {
    let image = set.image_of(defect);
    let sig = Set::sig(defect).ok_or_else(|| format!("no such defect: {defect}"))?;
    let ex = c.ex();
    let label = c.label();
    let dout = c.out.join(defect);
    std::fs::create_dir_all(&dout).map_err(|e| format!("{}: {e}", dout.display()))?;
    let mut ctl = Control::new(eng, set, &c.out, defect);
    let pool = eng.pool(&image, c.workers, &dout);
    let window = if c.window > 0 {
        c.window
    } else {
        2 * c.workers
    };
    let mut results = Vec::new();
    for &rep in &c.reps {
        let seeds: Vec<i64> = (rep * 1000 + 1..=rep * 1000 + c.budget).collect();
        let items = schedules(&ex, &c.strategy, &seeds)?;
        let mut done: BTreeMap<i64, Map<String, Value>> = BTreeMap::new();
        let (mut nxt, mut outstanding) = (0usize, 0usize);
        let mut found: Option<(usize, Map<String, Value>)> = None;
        let mut base: Vec<Value> = Vec::new();
        let t0 = Instant::now();
        let mut batch = pool.batch(&[], false);
        loop {
            while nxt < items.len() && outstanding < window && found.is_none() {
                let (sd, text) = &items[nxt];
                batch.add(&format!("seed-{sd}"), text);
                nxt += 1;
                outstanding += 1;
            }
            if outstanding == 0 {
                break;
            }
            let Some((tag, text, r)) = batch.next_done() else {
                break;
            };
            outstanding -= 1;
            let sd: i64 = tag
                .split('-')
                .nth(1)
                .and_then(|x| x.parse().ok())
                .ok_or_else(|| format!("unexpected tag {tag}"))?;
            done.insert(sd, make_rec(&ex, &dout, sd, &text, &r, defect));
            if found.is_some() {
                continue;
            }
            // first detection = lowest detecting seed with all lower seeds done
            for (i, (s2, _)) in items.iter().enumerate() {
                let Some(rec) = done.get(s2) else { break };
                if !detects(rec) || rec.get("base_bug").is_some_and(|b| b == &json!(true)) {
                    continue;
                }
                let sched = py_str(&rec["schedule"]);
                let (ok, cr) = ctl.passes(&sched);
                if ok {
                    found = Some((i + 1, rec.clone()));
                    break;
                }
                let rec = done.get_mut(s2).expect("present");
                rec.insert("base_bug".into(), json!(true));
                rec.insert("control_verdict".into(), cr["verdict"].clone());
                base.push(Value::Object(obj(&[
                    ("run", json!(i + 1)),
                    ("seed", json!(s2)),
                    ("schedule", rec["schedule"].clone()),
                    ("verdict", rec["verdict"].clone()),
                    ("control_verdict", cr["verdict"].clone()),
                ])));
            }
        }
        drop(batch);
        let rp = dout.join(format!("runs-rep{rep}.jsonl"));
        let mut f = std::fs::File::create(&rp).map_err(|e| format!("{}: {e}", rp.display()))?;
        for rec in done.values() {
            let _ = writeln!(f, "{}", dumps_line(&Value::Object(rec.clone())));
        }
        drop(f);
        let mut res = obj(&[
            ("defect", json!(defect)),
            ("strategy", json!(label)),
            ("rep", json!(rep)),
            ("budget", json!(c.budget)),
            ("image", json!(image)),
            ("runs_done", json!(done.len())),
            ("wall_s", fl(round1(t0.elapsed().as_secs_f64()))),
            ("base_bug_hits", Value::Array(base)),
        ]);
        if c.vcpus != 1 {
            res.insert("vcpus".into(), json!(c.vcpus));
        }
        match found {
            Some((n, rec)) => {
                let seed = rec["seed"].as_i64().unwrap_or(0);
                let tag = format!("repro-{seed}");
                let sched = py_str(&rec["schedule"]);
                let rr = pool.run_all(&[(tag.clone(), sched.clone())], false);
                let rfp = rr
                    .get(&tag)
                    .map(|(_, r)| r["fp"].clone())
                    .unwrap_or(Value::Null);
                let verdict = py_str(&rec["verdict"]);
                res.insert("detected_at".into(), json!(n));
                res.insert("seed".into(), json!(seed));
                res.insert("schedule".into(), json!(sched));
                res.insert("verdict".into(), json!(verdict));
                res.insert("fp".into(), rec["fp"].clone());
                res.insert("repro_fp".into(), rfp.clone());
                res.insert("reproduced".into(), json!(rfp == rec["fp"]));
                res.insert("signature_match".into(), json!(sig_match(sig, &verdict)));
            }
            None => {
                res.insert("detected_at".into(), Value::Null);
            }
        }
        println!("{}", dumps_line(&Value::Object(res.clone())));
        let _ = std::io::stdout().flush();
        results.push(res);
    }
    pool.close();
    ctl.close();
    Ok(results)
}

/// Every defect of a cell, appended to `<out>/results.jsonl` as each finishes.
pub fn run_cell(c: &Cell, set: &Set, eng: &Engines, names: &[String]) -> Result<(), String> {
    std::fs::create_dir_all(&c.out).map_err(|e| format!("{}: {e}", c.out.display()))?;
    let p = c.out.join("results.jsonl");
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .map_err(|e| format!("{}: {e}", p.display()))?;
    for name in names {
        for r in run_defect(c, set, eng, name)? {
            let _ = writeln!(out, "{}", dumps_line(&Value::Object(r)));
        }
        let _ = out.flush();
    }
    Ok(())
}

/// Python's `"%g" % x` for the medians of the table (positive numbers below 10^6).
fn g(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        // %g keeps 6 significant digits
        let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
        if digits > 6 {
            let int_digits = s.split('.').next().map(str::len).unwrap_or(0);
            let prec = 6usize.saturating_sub(int_digits);
            let t = format!("{x:.prec$}");
            return t.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        s.to_string()
    }
}

/// statistics.median of a non-empty list of whole numbers.
fn median(v: &[i64]) -> f64 {
    let mut s = v.to_vec();
    s.sort();
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2] as f64
    } else {
        (s[n / 2 - 1] + s[n / 2]) as f64 / 2.0
    }
}

/// run.py `table`: one row per defect, one column per strategy label, from the results.jsonl of
/// every directory given (directories without one are skipped).
pub fn table(dirs: &[PathBuf]) -> Result<String, String> {
    let mut rows: BTreeMap<String, BTreeMap<String, Vec<Map<String, Value>>>> = BTreeMap::new();
    for d in dirs {
        let p = d.join("results.jsonl");
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        for line in text.lines() {
            let r: Value =
                serde_json::from_str(line).map_err(|e| format!("{}: {e}", p.display()))?;
            let r = r.as_object().cloned().unwrap_or_default();
            rows.entry(py_str(&r["defect"]))
                .or_default()
                .entry(py_str(&r["strategy"]))
                .or_default()
                .push(r);
        }
    }
    let mut strats: Vec<String> = rows.values().flat_map(|v| v.keys().cloned()).collect();
    strats.sort();
    strats.dedup();
    let mut out = String::new();
    out.push_str(&format!("| defect | {} |\n", strats.join(" | ")));
    out.push_str(&format!("|---|{}\n", "---|".repeat(strats.len())));
    let mut found: BTreeMap<&str, usize> = strats.iter().map(|s| (s.as_str(), 0)).collect();
    for (dfc, by) in &rows {
        let mut cells = Vec::new();
        for s in &strats {
            let mut rs = by.get(s).cloned().unwrap_or_default();
            if rs.is_empty() {
                cells.push("-".to_string());
                continue;
            }
            rs.sort_by_key(|r| r["rep"].as_i64().unwrap_or(0));
            let at = |r: &Map<String, Value>| r["detected_at"].as_i64().filter(|n| *n != 0);
            let hits: Vec<i64> = rs.iter().filter_map(at).collect();
            if !hits.is_empty() {
                *found.get_mut(s.as_str()).expect("a strategy") += 1;
            }
            let runs: Vec<String> = rs
                .iter()
                .map(|r| at(r).map(|n| n.to_string()).unwrap_or_else(|| "-".into()))
                .collect();
            let med = if hits.is_empty() {
                String::new()
            } else {
                format!(" med {}", g(median(&hits)))
            };
            cells.push(format!(
                "{}/{} ({}){}",
                hits.len(),
                rs.len(),
                runs.join(", "),
                med
            ));
        }
        out.push_str(&format!("| {} | {} |\n", dfc, cells.join(" | ")));
    }
    out.push_str(&format!(
        "| defects found (any rep) | {} |\n",
        strats
            .iter()
            .map(|s| found[s.as_str()].to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_print_as_percent_g() {
        assert_eq!(g(median(&[7])), "7");
        assert_eq!(g(median(&[7, 10])), "8.5");
        assert_eq!(g(median(&[7, 9])), "8");
        assert_eq!(g(median(&[14, 1, 10])), "10");
        assert_eq!(g(1234.5), "1234.5");
        assert_eq!(g(12.3456789), "12.3457");
    }
}
