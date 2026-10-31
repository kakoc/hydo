//! Confirm each defect with its hand-written trigger schedules (confirm.py): every trigger twice on
//! the defect's image (both must be a VIOLATION with the same fingerprint) and once on the
//! control image, which must pass. Records go to `<out>/confirm.jsonl` (appended) and stdout.

use crate::defects::{crash_verdict, sig_match, Set, BASE};
use crate::pools::Engines;
use hydo_explore::gen::Ex;
use hydo_explore::py::{dumps_line, py_str};
use hydo_explore::report::verdict_kind;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

pub struct Opts {
    pub workers: usize,
    pub no_control: bool,
    pub base: bool,
}

fn emit(log: &mut std::fs::File, rec: Map<String, Value>) {
    let line = dumps_line(&Value::Object(rec));
    println!("{line}");
    let _ = std::io::stdout().flush();
    let _ = writeln!(log, "{line}");
    let _ = log.flush();
}

pub fn confirm(
    set: &Set,
    eng: &Engines,
    names: &[String],
    out: &Path,
    o: &Opts,
) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let lp = out.join("confirm.jsonl");
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&lp)
        .map_err(|e| format!("{}: {e}", lp.display()))?;
    let ex = Ex::default();
    for name in names {
        let image = set.image_of(name);
        let sig = Set::sig(name).ok_or_else(|| format!("no such defect: {name}"))?;
        let dir = out.join(name);
        let pool = eng.pool(&image, o.workers, &dir);
        let trig = set.triggers(name);
        let mut items = Vec::new();
        for (i, s) in trig.iter().enumerate() {
            items.push((format!("t{i}-a"), s.clone()));
            items.push((format!("t{i}-b"), s.clone()));
        }
        let res = pool.run_all(&items, false);
        pool.close();
        for (i, s) in trig.iter().enumerate() {
            let (ta, tb) = (format!("t{i}-a"), format!("t{i}-b"));
            let ra = res.get(&ta).map(|x| x.1.clone()).unwrap_or_default();
            let rb = res.get(&tb).map(|x| x.1.clone()).unwrap_or_default();
            let va = crash_verdict(&dir, &ta, &py_str(&ra["verdict"]), name);
            let class = ex
                .parse(s)
                .map(|p| ex.finding_class(&p))
                .map_err(|e| e.to_string())?;
            let mut rec = Map::new();
            rec.insert("defect".into(), json!(name));
            rec.insert("image".into(), json!(image));
            rec.insert("schedule".into(), json!(s));
            rec.insert("verdict".into(), json!(va));
            rec.insert("fp_a".into(), ra["fp"].clone());
            rec.insert("fp_b".into(), rb["fp"].clone());
            rec.insert("same".into(), json!(ra["fp"] == rb["fp"]));
            rec.insert("violation".into(), json!(verdict_kind(&va) == "violation"));
            rec.insert("class".into(), json!(class));
            rec.insert("signature_match".into(), json!(sig_match(sig, &va)));
            emit(&mut log, rec);
        }
    }
    if o.no_control {
        return Ok(());
    }
    // control image -> the defects it controls, in --defects order
    let mut controls: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in names {
        controls
            .entry(set.control_of(n))
            .or_default()
            .push(n.clone());
    }
    let main_control = set.control_of("d1-kafka-first");
    for (control, cnames) in &controls {
        let cdir = if *control == main_control {
            out.join("control")
        } else {
            out.join(control.replace("-disk.img", ""))
        };
        let pool = eng.pool(control, o.workers, &cdir);
        let mut items: Vec<(String, String)> = Vec::new();
        for n in cnames.iter().filter(|n| *n != "d0-old-front-door") {
            for (i, s) in set.triggers(n).iter().enumerate() {
                items.push((format!("{n}-t{i}"), s.clone()));
            }
        }
        if cnames.iter().any(|n| n == "d0-old-front-door") {
            items.push((
                "d0-old-front-door-t0".into(),
                set.triggers("d0-old-front-door")[0].clone(),
            ));
        }
        if o.base && *control == main_control {
            for (i, s) in BASE.iter().enumerate() {
                items.push((format!("base{i}-a"), s.to_string()));
                items.push((format!("base{i}-b"), s.to_string()));
            }
        }
        let res = pool.run_all(&items, false);
        pool.close();
        let sorted: BTreeMap<&String, &(String, _)> = res.iter().collect();
        for (tag, (s, r)) in sorted {
            let dn = match tag.rfind("-t") {
                Some(i) => &tag[..i],
                None => tag.as_str(),
            };
            let v = crash_verdict(&cdir, tag, &py_str(&r["verdict"]), dn);
            let mut rec = Map::new();
            rec.insert("control".into(), json!(control));
            rec.insert("for".into(), json!(tag));
            rec.insert("schedule".into(), json!(s));
            rec.insert("verdict".into(), json!(v));
            rec.insert("fp".into(), r["fp"].clone());
            rec.insert("ok".into(), json!(verdict_kind(&v) == "ok"));
            emit(&mut log, rec);
        }
    }
    Ok(())
}
