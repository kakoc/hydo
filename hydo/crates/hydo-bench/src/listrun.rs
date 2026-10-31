//! A fixed list of schedules on one or more images (listrun.py): one schedule per line of the
//! list file ("disk=none dseed=0 | run P=kafka@28+2"; blank lines and `#` comments skipped).
//! Writes `<out>/<image>.jsonl` (one record per schedule: schedule, verdict, fp, qn = vCPU turns in
//! the workload, crash = the order service's first panic/fatal-error line) and prints a table,
//! one line per schedule, with each image's verdict. `repeat` 2 runs every schedule twice; the
//! record gets fp2, same (fingerprints equal) and crash2.

use crate::defects::crash_line;
use crate::pools::Engines;
use hydo_explore::py::{dumps_line, py_str};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

pub fn items_of(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn opt_str(v: Option<String>) -> Value {
    v.map(Value::String).unwrap_or(Value::Null)
}

/// Run the list; returns the printed table.
pub fn run(
    eng: &Engines,
    images: &[String],
    items: &[String],
    workers: usize,
    out: &Path,
    repeat: u32,
) -> Result<String, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut table: HashMap<String, Vec<Map<String, Value>>> = HashMap::new();
    for img in images {
        let base = img.replace("-disk.img", "");
        let d = out.join(&base);
        let pool = eng.pool(img, workers, &d);
        let mut jobs: Vec<(String, String)> = items
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("s{i}"), t.clone()))
            .collect();
        if repeat == 2 {
            jobs.extend(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (format!("s{i}b"), t.clone())),
            );
        }
        let res = pool.run_all(&jobs, false);
        pool.close();
        let p = out.join(format!("{base}.jsonl"));
        let mut f = std::fs::File::create(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        for (i, t) in items.iter().enumerate() {
            let r = res
                .get(&format!("s{i}"))
                .map(|x| x.1.clone())
                .unwrap_or_default();
            let mut rec = Map::new();
            rec.insert("schedule".into(), json!(t));
            rec.insert(
                "verdict".into(),
                r.get("verdict").cloned().unwrap_or(json!("?")),
            );
            rec.insert("fp".into(), r.get("fp").cloned().unwrap_or(Value::Null));
            rec.insert("qn".into(), r.get("qn").cloned().unwrap_or(Value::Null));
            rec.insert(
                "crash".into(),
                opt_str(crash_line(&d, &format!("s{i}"), 120)),
            );
            if repeat == 2 {
                let r2 = res
                    .get(&format!("s{i}b"))
                    .map(|x| x.1.clone())
                    .unwrap_or_default();
                let fp2 = r2.get("fp").cloned().unwrap_or(Value::Null);
                let same = fp2 == r.get("fp").cloned().unwrap_or(Value::Null);
                rec.insert("fp2".into(), fp2);
                rec.insert("same".into(), json!(same));
                rec.insert(
                    "crash2".into(),
                    opt_str(crash_line(&d, &format!("s{i}b"), 120)),
                );
            }
            let _ = writeln!(f, "{}", dumps_line(&Value::Object(rec.clone())));
            table.entry(t.clone()).or_default().push(rec);
        }
    }
    let mut lines = String::new();
    for t in items {
        let cells: Vec<String> = table[t]
            .iter()
            .map(|r| {
                let v: String = py_str(&r["verdict"])
                    .replace("HYDO-ORACLE: ", "")
                    .chars()
                    .take(44)
                    .collect();
                let fp = match &r["fp"] {
                    Value::Null => "None".to_string(),
                    x => py_str(x),
                };
                let crash = if r["crash"].is_null() { "" } else { " CRASH" };
                let differs = if r.get("same") == Some(&json!(false)) {
                    " DIFFERS"
                } else {
                    ""
                };
                format!("{v} {fp}{crash}{differs}")
            })
            .collect();
        // `%-62s`: padded to 62 characters
        lines.push_str(&format!("{t:<62} {}\n", cells.join(" | ")));
    }
    Ok(lines)
}
