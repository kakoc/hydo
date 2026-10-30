//! hydo-composer against the recorded answers of hydo/onboard/hydo_composer.py (the reference
//! until M107, when the Python was deleted).
//!
//! The driver below is tests/py_ref.py (removed in M107; `git show 2a58c4b:hydo/crates/hydo-composer/tests/py_ref.py`)
//! ported line for line: the M98 corpora of test_composer.py (Python's `random.Random(7)` stream,
//! reproduced by `common/pyrandom.rs`), and for every plan and hydo/ folder it read (pinned in
//! `tests/golden/composer-plans.json`: the directory, the manifest data before expansion, and a
//! SHA-256 of the folder's workload/, checks/ and regressions/ files) expand, shell_lines,
//! test_from_meta, stage, 40 drawn W values (`random.Random(101)`) with their resolved form, key,
//! entries, description, candidates and contents, and regressions in every form with
//! regression_status, regression_warning and saved_regression_text. Each case is one line
//! `{"op", "args", "out"}` exactly as py_ref.py printed it (paths as `<TOP>`/`<WORK>`, a crash's
//! message blank, a staged tree's files sorted); `tests/golden/composer.sha256` holds the SHA-256
//! of every 256 lines of Python's stream, recorded from 2a58c4b with Python 3.15.0 on the Mac
//! (59,262 cases). The stream must be identical.
//!
//! `HYDO_BLESS=1 cargo test -p hydo-composer --test equivalence` re-records both goldens from the
//! Rust answers (after a deliberate change, or when a pinned plan folder changed: the test says
//! which). `HYDO_RECORD_PY=<py_ref.py output> HYDO_RECORD_WORK=<its --work dir>` records from a
//! saved Python stream instead (how the goldens were made).

mod common;

use common::golden;
use common::pyrandom::PyRandom;
use hydo_composer as hc;
use hydo_manifest::py::{json_dumps, CRASH_PREFIX};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const CHUNK: usize = 256;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn top() -> PathBuf {
    crate_dir().join("../../..").canonicalize().unwrap()
}

fn golden_dir() -> PathBuf {
    crate_dir().join("tests/golden")
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-composer-eq-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

type Out = Result<Value, (&'static str, String)>;

fn werr(e: hc::WError) -> (&'static str, String) {
    match e {
        hc::WError::Invalid(m) => ("ValueError", m),
        hc::WError::Crash(m) => ("crash", m),
    }
}

fn merr(e: hc::ManifestError) -> (&'static str, String) {
    if e.message.starts_with(CRASH_PREFIX) {
        ("crash", e.message)
    } else {
        ("ValueError", e.message)
    }
}

fn entries(es: Vec<hc::Entry>) -> Value {
    Value::Array(
        es.into_iter()
            .map(|(e, r, c, s)| json!([e as i64, r as i64, c as u64, s as i64]))
            .collect(),
    )
}

fn steps(a: &Value) -> Option<i128> {
    a.get("steps").and_then(Value::as_i64).map(i128::from)
}

fn saved(a: &Value) -> Option<hc::Saved> {
    a.get("saved").and_then(Value::as_object).map(|m| {
        m.iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect()
    })
}

fn s<'a>(a: &'a Value, k: &str) -> &'a str {
    a[k].as_str().unwrap_or_default()
}

fn snapshot(root: &Path) -> Value {
    use std::os::unix::fs::PermissionsExt;
    fn walk(d: &Path, root: &Path, out: &mut BTreeMap<String, Value>) {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        v.sort_by_key(|e| e.file_name());
        for e in v {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            let md = std::fs::symlink_metadata(&p).unwrap();
            let mode = format!("0o{:o}", md.permissions().mode() & 0o7777);
            if md.file_type().is_symlink() {
                out.insert(
                    rel,
                    json!({"link": std::fs::read_link(&p).unwrap().display().to_string()}),
                );
            } else if md.is_dir() {
                out.insert(rel, json!({ "dir": mode }));
                walk(&p, root, out);
            } else {
                let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
                out.insert(rel, json!({"mode": mode, "text": text}));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    serde_json::to_value(out).unwrap()
}

/// One case, answered by hydo-composer (the op names and args are py_ref.py's).
fn run(op: &str, a: &Value, work: &Path, n: usize) -> Out {
    let meta = &a["meta"];
    Ok(match op {
        "plan" => {
            let kinds: Vec<String> = a["cmds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["kind"].as_str().unwrap().to_string())
                .collect();
            let i = |k: &str| i128::from(a[k].as_i64().unwrap());
            entries(hc::plan(i("seed"), i("q"), i("steps"), &kinds).map_err(merr)?)
        }
        "render_w" => json!(hc::render_w(
            &hc::parse_seed(s(a, "w"), meta).map_err(werr)?
        )),
        "kept" => {
            let d = hc::parse_seed(s(a, "w"), meta).map_err(werr)?;
            entries(hc::kept_of(&d, steps(a).unwrap(), meta).map_err(werr)?)
        }
        "candidates" => json!(hc::candidates(s(a, "w"), meta, steps(a)).map_err(werr)?),
        "stable_w" => {
            json!(hc::stable_w(s(a, "w"), meta, steps(a), saved(a).as_ref()).map_err(werr)?)
        }
        "key_of" => json!(hc::key_of(s(a, "x"))),
        "render_resolved" => json!(hc::render_resolved(
            &hc::parse_resolved(s(a, "v")).map_err(|e| ("ValueError", e))?
        )),
        "entries_of" => {
            entries(hc::entries_of(s(a, "w"), meta, steps(a), saved(a).as_ref()).map_err(werr)?)
        }
        "describe" => json!(hc::describe(s(a, "w"), meta, steps(a)).map_err(werr)?),
        "contents_of" => {
            json!(hc::contents_of(s(a, "w"), meta, steps(a), saved(a).as_ref()).map_err(werr)?)
        }
        "expand" => {
            let mut m = a["m"].clone();
            let folder = hc::scan_folder(s(a, "dir")).map_err(merr)?;
            let mut warnings = Vec::new();
            let meta = hc::expand(&mut m, folder.as_ref(), &mut warnings).map_err(merr)?;
            json!({"m": m, "meta": meta, "warnings": warnings})
        }
        "shell_lines" => json!(hc::shell_lines(meta)),
        "test_from_meta" => hc::test_from_meta(meta),
        "stage" => {
            let folder = hc::scan_folder(s(a, "dir")).map_err(merr)?;
            let root = work.join(format!("rs-stage-{n}"));
            let onboard = crate_dir().join("../../onboard");
            hc::stage(meta, &root, &onboard, folder.as_ref()).map_err(|e| ("crash", e))?;
            let snap = snapshot(&root);
            let _ = std::fs::remove_dir_all(&root);
            snap
        }
        "regression_status" => {
            hc::status_json(&hc::regression_status(&a["r"], meta, steps(a)).map_err(merr)?)
        }
        "regression_warning" => json!(hc::regression_warning(&a["r"], meta).map_err(merr)?),
        "draw" => {
            let seq: Vec<u64> = a["seq"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap())
                .collect();
            let mut it = seq.into_iter();
            let mut next = || it.next().expect("draw took more values than recorded");
            json!(hc::draw(&mut next, meta))
        }
        "with_w" => {
            let w = hc::with_w(s(a, "schedule"), s(a, "w"));
            let f = hc::fits(&w);
            json!([w, f])
        }
        "w_of" => json!(hc::w_of(s(a, "schedule"))),
        "saved_regression_text" => {
            json!(hc::saved_regression_text(&a["x"]).map_err(|e| ("crash", e))?)
        }
        other => panic!("unknown op {other}"),
    })
}

// ------------------------------------------------------------------ the stream's canonical form

/// A key for "the same meta" (Python: `json.dumps(m, sort_keys=True)`).
fn sorted_key(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let b: BTreeMap<String, Value> =
                    m.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                Value::Object(b.into_iter().collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            x => x.clone(),
        }
    }
    serde_json::to_string(&sort(v)).unwrap()
}

/// The line as compared: py_ref.py's line with the paths named, a crash's message blank (the
/// reference's traceback text is not part of the contract) and a staged tree's files sorted (os.walk
/// and read_dir list in other orders).
fn canonical(case: &Value, paths: &[(String, &str)]) -> String {
    let mut c = case.clone();
    if c["out"]["err"] == "crash" {
        c["out"]["msg"] = json!("");
    }
    if c["op"] == "stage" {
        if let Some(Value::Object(m)) = c["out"].get("ok") {
            let b: BTreeMap<String, Value> = m.clone().into_iter().collect();
            c["out"]["ok"] = Value::Object(b.into_iter().collect());
        }
    }
    let mut t = json_dumps(&c);
    for (p, name) in paths {
        t = t.replace(p.as_str(), name);
    }
    t
}

fn section_of(line: &str, cur: &str) -> String {
    if line.starts_with("{\"op\": \"expand\"") {
        let case: Value = serde_json::from_str(line).unwrap();
        format!("plan {}", case["args"]["dir"].as_str().unwrap())
    } else if line.starts_with("{\"op\": \"summary\"") {
        "summary".into()
    } else {
        cur.to_string()
    }
}

/// Split a stream of canonical lines into the golden's sections.
struct Stream {
    sections: golden::Sections,
    cur: String,
}

impl Stream {
    fn new() -> Stream {
        Stream {
            sections: vec![("corpora".into(), Vec::new())],
            cur: "corpora".into(),
        }
    }
    fn push(&mut self, line: String) {
        let sec = section_of(&line, &self.cur);
        if sec != self.cur {
            self.sections.push((sec.clone(), Vec::new()));
            self.cur = sec;
        }
        self.sections.last_mut().unwrap().1.push(line);
    }
}

// ------------------------------------------------------------------ the driver (py_ref.py)

struct Driver {
    work: PathBuf,
    paths: Vec<(String, &'static str)>,
    metas: HashMap<String, i64>,
    stream: Stream,
    n_ops: usize,
}

impl Driver {
    /// `emit(op, args, fn)`: answer with hydo-composer, print the meta once, return the answer.
    fn emit(&mut self, op: &str, args: Value) -> Option<Value> {
        let got = run(op, &args, &self.work, self.n_ops);
        let out = match &got {
            Ok(v) => json!({ "ok": v }),
            Err((k, m)) => json!({"err": k, "msg": m}),
        };
        let mut args = args;
        if let Some(m) = args.get("meta").filter(|m| m.is_object()).cloned() {
            let k = sorted_key(&m);
            let id = match self.metas.get(&k) {
                Some(id) => *id,
                None => {
                    let id = self.metas.len() as i64;
                    self.metas.insert(k, id);
                    let line =
                        json!({"op": "meta", "args": {"id": id, "meta": m}, "out": {"ok": null}});
                    let c = canonical(&line, &self.paths);
                    self.stream.push(c);
                    id
                }
            };
            args["meta"] = json!({ "$meta": id });
        }
        let case = json!({"op": op, "args": args, "out": out});
        let c = canonical(&case, &self.paths);
        self.stream.push(c);
        self.n_ops += 1;
        got.ok()
    }
}

fn cmd_sets() -> Vec<Vec<Value>> {
    let k = |kind: &str, n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| json!({"kind": kind, "id": format!("{kind}_{i}")}))
            .collect()
    };
    let mut sets = Vec::new();
    for npar in [0, 1, 3] {
        for nser in [0, 1, 2] {
            for nsg in [0, 1, 2] {
                for nany in [0, 1, 2] {
                    if npar + nser + nsg + nany == 0 {
                        continue;
                    }
                    let mut cs = [
                        k("parallel_driver", npar),
                        k("serial_driver", nser),
                        k("singleton_driver", nsg),
                        k("anytime", nany),
                        k("first", 1),
                        k("eventually", 1),
                    ]
                    .concat();
                    cs.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
                    sets.push(cs);
                }
            }
        }
    }
    sets
}

fn defaults() -> Value {
    let mut m = Map::new();
    for (k, d, _, _) in hydo_manifest::composer::PARAMS {
        m.insert(k.to_string(), json!(*d as i64));
    }
    Value::Object(m)
}

fn kinds_ids(cmds: &[Value]) -> (Vec<String>, Vec<String>) {
    (
        cmds.iter().map(|c| s(c, "id").to_string()).collect(),
        cmds.iter().map(|c| s(c, "kind").to_string()).collect(),
    )
}

fn wseed(seed: i128, p: i128, q: i128, n: i128, t: i128) -> hc::WSeed {
    hc::WSeed {
        seed,
        p,
        q,
        r: 100,
        n,
        t,
        x: Vec::new(),
        o: Vec::new(),
    }
}

/// `corpora(n)`: the random stream of test_composer.py main(), case for case.
fn corpora(dr: &mut Driver, n: usize) -> (usize, usize, usize) {
    let mut rnd = PyRandom::new(7);
    let sets = cmd_sets();
    let params = defaults();
    let mut nw = 0;
    for i in 0..n {
        let cmds = sets[i % sets.len()].clone();
        let (ids, kinds) = kinds_ids(&cmds);
        let steps = rnd.choice(&[1i128, 4, 20, 57]);
        let seed = if i % 5 != 0 {
            i128::from(rnd.getrandbits(32))
        } else {
            i as i128
        };
        let p = 1 + rnd.below(4) as i128;
        let q = 1 + rnd.below(3) as i128;
        let nn = rnd.choice(&[0i128, 0, 3, 10]);
        let mut d = wseed(seed, p, q, nn, 0);
        let full = hc::plan(seed, q, steps, &kinds).unwrap();
        if !full.is_empty() && i % 3 == 0 {
            let k = full.len().min(rnd.range(1, 6) as usize);
            let pop: Vec<i128> = (0..full.len() as i128).collect();
            d.x = rnd.sample(&pop, k);
        }
        if i % 4 == 0 {
            d.o = vec![hc::OItem::I(rnd.below(cmds.len() as u64) as i128)];
        } else if i % 4 == 1 {
            let k = cmds.len().min(rnd.range(1, 3) as usize);
            d.o = rnd.sample(&ids, k).into_iter().map(hc::OItem::S).collect();
        }
        let w = hc::render_w(&d);
        let meta = json!({"params": params, "commands": cmds, "steps": steps as i64});
        dr.emit(
            "plan",
            json!({"seed": seed as i64, "q": q as i64, "steps": steps as i64, "cmds": cmds}),
        );
        dr.emit("render_w", json!({"w": w, "meta": meta}));
        dr.emit("kept", json!({"w": w, "meta": meta, "steps": steps as i64}));
        if i % 10 == 0 {
            dr.emit("candidates", json!({"w": w, "meta": meta, "steps": null}));
        }
        nw += 1;
    }
    let meta = json!({"params": params, "commands": sets.last().unwrap(), "steps": 20});
    for seed in 0..50i128 {
        let w = hc::render_w(&wseed(seed * 7919, 4, 2, 0, 0));
        dr.emit("candidates", json!({"w": w, "meta": meta, "steps": null}));
    }
    let (mut nres, mut nerr) = (0, 0);
    for i in 0..200usize.max(n / 5) {
        let cmds = sets[i % sets.len()].clone();
        let (_, kinds) = kinds_ids(&cmds);
        let steps = rnd.choice(&[4i128, 20, 57]);
        let seed = i128::from(rnd.getrandbits(32));
        let p = 1 + rnd.below(4) as i128;
        let q = 1 + rnd.below(3) as i128;
        let nn = rnd.choice(&[0i128, 3]);
        let t = rnd.choice(&[0i128, 0, 500]);
        let w = hc::render_w(&wseed(seed, p, q, nn, t));
        let meta = json!({"params": params, "commands": cmds, "steps": steps as i64});
        let (ids, _) = kinds_ids(&cmds);
        let d = hc::parse_w(&w, &params).unwrap();
        if hc::kept(&d, steps, &ids, &kinds).unwrap().is_empty() {
            continue;
        }
        let res = dr
            .emit(
                "stable_w",
                json!({"w": w, "meta": meta, "steps": null, "saved": null}),
            )
            .unwrap_or(Value::Null);
        dr.emit("key_of", json!({ "x": res }));
        dr.emit("render_resolved", json!({ "v": res }));
        let res = res.as_str().unwrap().to_string();
        let mut now: Vec<Value> = cmds.clone();
        now.push(json!({"kind": "parallel_driver", "id": format!("parallel_driver_new{i}")}));
        now.push(json!({"kind": "anytime", "id": "anytime_0new"}));
        let mut drop = None;
        if i % 7 == 3 {
            let used: BTreeSet<String> = hc::parse_resolved(&res)
                .unwrap()
                .entries
                .into_iter()
                .map(|e| e.2)
                .collect();
            let used: Vec<String> = used.into_iter().collect();
            let dv = rnd.choice(&used);
            now.retain(|c| s(c, "id") != dv);
            drop = Some(dv);
        }
        now.sort_by(|a, b| s(a, "id").cmp(s(b, "id")));
        if i % 2 == 1 {
            rnd.shuffle(&mut now);
        }
        let meta2 = json!({"params": params, "commands": now, "steps": steps as i64});
        dr.emit(
            "entries_of",
            json!({"w": res, "meta": meta2, "steps": null, "saved": null}),
        );
        if drop.is_some() {
            nerr += 1;
            continue;
        }
        dr.emit(
            "candidates",
            json!({"w": res, "meta": meta2, "steps": null}),
        );
        dr.emit("describe", json!({"w": res, "meta": meta2, "steps": null}));
        nres += 1;
    }
    (nw, nres, nerr)
}

fn opt(steps: Option<i128>) -> Value {
    steps.map(|x| json!(x as i64)).unwrap_or(Value::Null)
}

/// Python's `dict(base, **kw)`: existing keys keep their place, new ones go to the end.
fn with(base: &Value, kw: &[(&str, Value)]) -> Value {
    let mut m = base.as_object().unwrap().clone();
    for (k, v) in kw {
        m.insert(k.to_string(), v.clone());
    }
    Value::Object(m)
}

/// `plan_cases`: every op on one plan; `m` is the manifest data before expand.
fn plan_cases(dr: &mut Driver, mdir: &str, m: &Value, rnd: &mut PyRandom) -> usize {
    let out = dr.emit("expand", json!({"m": m, "dir": mdir}));
    let Some(out) = out else { return 0 };
    let meta = out["meta"].clone();
    if !hydo_manifest::py::truthy(&meta) {
        return 0;
    }
    dr.emit("shell_lines", json!({ "meta": meta }));
    dr.emit("test_from_meta", json!({ "meta": meta }));
    let root = dr.work.join(format!("stage-{}", dr.n_ops));
    dr.emit(
        "stage",
        json!({"meta": meta, "dir": mdir, "root": root.display().to_string()}),
    );
    for r in meta["regressions"].as_array().cloned().unwrap_or_default() {
        dr.emit(
            "regression_status",
            json!({"r": r, "meta": meta, "steps": null}),
        );
        dr.emit("regression_warning", json!({"r": r, "meta": meta}));
    }
    if !meta.get("composed").is_some_and(hydo_manifest::py::truthy) {
        return 1;
    }
    let ids: Vec<String> = meta["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| s(c, "id").to_string())
        .collect();
    for k in 0..40usize {
        let mut seq: Vec<u64> = Vec::new();
        let mut w = {
            let mut next = || {
                let v = rnd.getrandbits(64);
                seq.push(v);
                v
            };
            hc::draw(&mut next, &meta)
        };
        dr.emit("draw", json!({"seq": seq, "meta": meta}));
        if k % 3 == 1 {
            let cs = hc::candidates(&w, &meta, None).unwrap();
            if !cs.is_empty() {
                w = cs[k % cs.len()].clone();
            }
        }
        let steps: Option<i128> = if k % 4 != 0 {
            None
        } else {
            Some(rnd.choice(&[3i128, 20, 57]))
        };
        let st = opt(steps);
        dr.emit("describe", json!({"w": w, "meta": meta, "steps": st}));
        dr.emit("candidates", json!({"w": w, "meta": meta, "steps": st}));
        let res = dr.emit(
            "stable_w",
            json!({"w": w, "meta": meta, "steps": st, "saved": null}),
        );
        dr.emit(
            "contents_of",
            json!({"w": w, "meta": meta, "steps": st, "saved": null}),
        );
        let Some(res) = res.and_then(|r| r.as_str().map(str::to_string)) else {
            continue;
        };
        let Ok(parsed) = hc::parse_resolved(&res) else {
            continue;
        };
        let key = hc::key_of(&res);
        dr.emit("key_of", json!({ "x": res }));
        let mut saved = Map::new();
        saved.insert(key.clone(), json!(res));
        let saved = Value::Object(saved);
        for v in [res.clone(), format!("@{key}")] {
            dr.emit(
                "entries_of",
                json!({"w": v, "meta": meta, "steps": st, "saved": saved}),
            );
            dr.emit("describe", json!({"w": v, "meta": meta, "steps": st}));
            dr.emit("candidates", json!({"w": v, "meta": meta, "steps": st}));
        }
        dr.emit(
            "contents_of",
            json!({"w": format!("@{key}"), "meta": meta, "steps": null, "saved": saved}),
        );
        let sched = format!("disk=none dseed={k:x} | run K=redis@3+1 W={w}");
        dr.emit("with_w", json!({"schedule": sched, "w": res}));
        dr.emit("w_of", json!({ "schedule": sched }));
        let contents = json!(hc::contents_of(&w, &meta, steps, None).unwrap());
        let mut other = ids.clone();
        rnd.shuffle(&mut other);
        let mut other2: Vec<String> = other.clone();
        other2.push("parallel_driver_zz".into());
        let mut bad_list = vec!["bogus".to_string()];
        bad_list.extend(ids.iter().cloned());
        let zeros: Map<String, Value> = contents
            .as_object()
            .unwrap()
            .keys()
            .map(|c| (c.clone(), json!("0".repeat(16))))
            .collect();
        let rkey: String = key.chars().rev().collect();
        let regs = vec![
            json!({"name": "seed", "schedule": sched, "expect": "ok"}),
            json!({"name": "same-list", "schedule": sched, "expect": "violation", "commands": ids}),
            json!({"name": "other-list", "schedule": sched, "expect": "ok", "commands": other2}),
            json!({"name": "bad-list", "schedule": sched, "expect": "ok", "commands": bad_list}),
            json!({"name": "resolved", "schedule": hc::with_w(&sched, &res), "expect": "ok", "contents": contents}),
            json!({"name": "saved", "schedule": hc::with_w(&sched, &format!("@{key}")), "expect": "ok", "compose": res}),
            json!({"name": "edited-key", "schedule": hc::with_w(&sched, &format!("@{rkey}")), "expect": "ok", "compose": res}),
            json!({"name": "no-compose", "schedule": hc::with_w(&sched, &format!("@{key}")), "expect": "ok"}),
            json!({"name": "changed", "schedule": hc::with_w(&sched, &res), "expect": "ok", "contents": zeros}),
            json!({"name": "garbage", "schedule": hc::with_w(&sched, ":p0,zz"), "expect": "ok"}),
        ];
        let used: BTreeSet<String> = parsed.entries.iter().map(|e| e.2.clone()).collect();
        let first = used.iter().next().unwrap().clone();
        let smaller = with(
            &meta,
            &[(
                "commands",
                Value::Array(
                    meta["commands"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|c| s(c, "id") != first)
                        .cloned()
                        .collect(),
                ),
            )],
        );
        let late = with(&meta, &[("steps", json!(2))]);
        for r in &regs {
            for (mt, stp) in [
                (&meta, st.clone()),
                (&smaller, Value::Null),
                (&late, Value::Null),
            ] {
                dr.emit(
                    "regression_status",
                    json!({"r": r, "meta": mt, "steps": stp}),
                );
            }
            dr.emit("regression_warning", json!({"r": r, "meta": meta}));
        }
        let mut base = json!({"from_seed": k, "class": "violation", "signature": "x",
                              "min_verdict": format!("VIOLATION [p] {}", "x".repeat(300)),
                              "minimal": sched});
        if k % 2 == 1 {
            base["composition"] = json!(format!(
                "{}{}",
                hc::describe(&w, &meta, steps).unwrap(),
                " ".repeat(400)
            ));
        }
        let xs = vec![
            base.clone(),
            with(&base, &[("commands", json!(ids))]),
            with(
                &base,
                &[
                    ("stable", json!(res)),
                    ("stable_schedule", json!(hc::with_w(&sched, &res))),
                    ("contents", contents.clone()),
                ],
            ),
            with(
                &base,
                &[
                    ("stable", json!(res)),
                    (
                        "stable_schedule",
                        json!(hc::with_w(&sched, &format!("@{key}"))),
                    ),
                    ("contents", contents.clone()),
                    ("stable_note", json!("n")),
                ],
            ),
            with(
                &base,
                &[
                    ("stable", json!(res)),
                    ("stable_note", json!("did not fail the same way")),
                    ("commands", json!(ids)),
                    ("contents", contents.clone()),
                ],
            ),
            with(
                &base,
                &[
                    ("stable", json!(res)),
                    ("stable_note", json!("x")),
                    ("commands", json!(ids)),
                    ("minimal", json!(hc::with_w(&sched, &res))),
                ],
            ),
        ];
        for x in xs {
            dr.emit("saved_regression_text", json!({ "x": x }));
        }
    }
    1
}

/// Copy a tree with the modes a checkout made with umask 022 has (directories 0755, files 0755
/// with an execute bit, else 0644): the recording's modes on any machine and checkout.
fn copytree(src: &Path, dst: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dst).unwrap();
    let mut es: Vec<_> = std::fs::read_dir(src)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    es.sort_by_key(|e| e.file_name());
    for e in es {
        let p = e.path();
        let to = dst.join(e.file_name());
        if p.is_dir() {
            copytree(&p, &to);
        } else {
            std::fs::copy(&p, &to).unwrap();
            let x = std::fs::metadata(&p).unwrap().permissions().mode() & 0o111 != 0;
            let mode = if x { 0o755 } else { 0o644 };
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    }
    std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The inputs under the work directory: each repository plan folder's workload/, checks/ and
/// regressions/ under `<work>/plans/<path>` (named `<TOP>/<path>` in the stream), and
/// test_composer_ids.py's redis-composed variants (py_ref.py's `folder_variants`) under
/// `<work>/variants`. Files are created with umask 022, as when the goldens were recorded.
fn prepare(work: &Path, plans: &[(String, Value)]) {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: umask only sets the process's file creation mask.
    unsafe {
        libc::umask(0o022);
    }
    for (dir, _) in plans {
        let Some(rel) = dir.strip_prefix("<TOP>/") else {
            continue;
        };
        let dst = work.join("plans").join(rel);
        std::fs::create_dir_all(&dst).unwrap();
        for sub in ["workload", "checks", "regressions"] {
            let src = top().join(rel).join(sub);
            if src.is_dir() {
                copytree(&src, &dst.join(sub));
            }
        }
    }
    let src = top().join("hydo/onboard/examples/redis-composed/hydo");
    for v in ["v1", "v2", "v3", "v4"] {
        let d = work.join("variants").join(v).join("hydo");
        copytree(&src, &d);
        if v == "v2" || v == "v3" {
            for name in ["anytime_aaa_ping", "parallel_driver_incr"] {
                let p = d.join("workload").join(name);
                std::fs::write(&p, "#!/bin/sh\ntrue\n").unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        if v == "v3" {
            std::fs::remove_file(d.join("workload/parallel_driver_transfer")).unwrap();
        }
        if v == "v4" {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(d.join("workload/parallel_driver_transfer"))
                .unwrap();
            f.write_all(b"# edited\n").unwrap();
        }
    }
}

/// SHA-256 over what scan_folder reads under a plan directory: workload/, checks/ and
/// regressions/ (relative path, execute bit, bytes of every file, sorted).
fn folder_pin(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    fn walk(d: &Path, root: &Path, out: &mut Vec<(String, bool, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                let x = std::fs::metadata(&p).unwrap().permissions().mode() & 0o111 != 0;
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                out.push((rel, x, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    for sub in ["workload", "checks", "regressions"] {
        walk(&dir.join(sub), dir, &mut files);
    }
    files.sort();
    let mut all = Vec::new();
    for (rel, x, b) in files {
        all.extend_from_slice(format!("{rel}\0{}\0{}\0", u8::from(x), b.len()).as_bytes());
        all.extend_from_slice(&b);
    }
    golden::sha256_hex(&all)
}

/// The names of the paths in the stream: the copied plan folders first (`<TOP>`), then the work
/// directory, then the repository itself (the guest files staged from hydo/onboard).
fn paths(top: &Path, work: &Path) -> Vec<(String, &'static str)> {
    vec![
        (work.join("plans").display().to_string(), "<TOP>"),
        (work.display().to_string(), "<WORK>"),
        (top.display().to_string(), "<TOP>"),
    ]
}

fn resolve(dir: &str, work: &Path) -> String {
    dir.replace("<WORK>", &work.display().to_string())
        .replace("<TOP>", &work.join("plans").display().to_string())
}

fn header() -> Vec<String> {
    vec![
        "# hydo-composer equivalence: SHA-256 of every 256 lines of the answer stream of".into(),
        "# hydo/crates/hydo-composer/tests/py_ref.py (hydo/onboard/hydo_composer.py), recorded at".into(),
        "# commit 2a58c4b with Python 3.15.0 on the Mac; restore the script with".into(),
        "# `git show 2a58c4b:hydo/crates/hydo-composer/tests/py_ref.py`. Rows: section, chunk, lines, sha256.".into(),
    ]
}

/// The whole stream, answered by hydo-composer; the plans from `plans` (dir, data).
fn rust_stream(work: &Path, plans: &[(String, Value)]) -> (golden::Sections, Value) {
    prepare(work, plans);
    let mut dr = Driver {
        work: work.to_path_buf(),
        paths: paths(&top(), work),
        metas: HashMap::new(),
        stream: Stream::new(),
        n_ops: 0,
    };
    let (nw, nres, nerr) = corpora(&mut dr, 3000);
    let mut rnd = PyRandom::new(101);
    let mut nplans = 0;
    for (dir, m) in plans {
        nplans += plan_cases(&mut dr, &resolve(dir, work), m, &mut rnd);
    }
    let summary = json!({"w_values": nw, "resolved": nres, "removed": nerr, "plans": nplans,
                         "ops": dr.n_ops});
    let case = json!({"op": "summary", "args": {}, "out": {"ok": summary}});
    let c = canonical(&case, &dr.paths);
    dr.stream.push(c);
    (dr.stream.sections, summary)
}

/// The recording: a saved py_ref.py stream (HYDO_RECORD_PY, its --work dir in HYDO_RECORD_WORK).
fn record_from_python(path: &str, pywork: &str) {
    // the Python read the repository's folders in place
    let ps = vec![
        (pywork.to_string(), "<WORK>"),
        (top().display().to_string(), "<TOP>"),
    ];
    let mut st = Stream::new();
    let mut plans = Vec::new();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let case: Value = serde_json::from_str(line).unwrap();
        let c = canonical(&case, &ps);
        let norm: Value = serde_json::from_str(&c).unwrap();
        if case["op"] == "expand" {
            plans.push((
                norm["args"]["dir"].as_str().unwrap().to_string(),
                norm["args"]["m"].clone(),
            ));
        }
        st.push(c);
    }
    write_plans(&plans);
    golden::write(
        &golden_dir().join("composer.sha256"),
        &header(),
        &golden::digest(&st.sections, CHUNK),
    );
    eprintln!("recorded {} plans and the stream from {path}", plans.len());
}

fn write_plans(plans: &[(String, Value)]) {
    let work = scratch("pins");
    prepare(&work, plans);
    let list: Vec<Value> = plans
        .iter()
        .map(|(d, m)| json!({"dir": d, "pin": folder_pin(Path::new(&resolve(d, &work))), "m": m}))
        .collect();
    let doc = json!({
        "about": "The plans and hydo/ folders py_ref.py read (2a58c4b): the directory, the \
                  manifest data before expansion, and a SHA-256 of the folder's workload/, \
                  checks/ and regressions/ files (tests/equivalence.rs, folder_pin).",
        "plans": list,
    });
    std::fs::write(
        golden_dir().join("composer-plans.json"),
        serde_json::to_string_pretty(&doc).unwrap() + "\n",
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn pyrandom_is_pythons() {
    // values printed by Python 3.15: random.Random(7) / (101) / (2**40 + 5)
    let mut r = PyRandom::new(7);
    assert_eq!(
        [r.getrandbits(32), r.getrandbits(32), r.getrandbits(32)],
        [1390851128, 4071050724, 647892279]
    );
    assert_eq!(r.getrandbits(64), 12007621696699967246);
    assert_eq!(r.below(4), 0);
    assert_eq!(r.choice(&[1, 4, 20, 57]), 1);
    let mut l: Vec<u32> = (0..10).collect();
    r.shuffle(&mut l);
    assert_eq!(l, [3, 2, 7, 9, 6, 0, 4, 5, 1, 8]);
    let pop: Vec<u32> = (0..100).collect();
    assert_eq!(r.sample(&pop, 5), [55, 53, 8, 30, 11]);
    assert_eq!(r.sample(&["a", "b", "c"], 2), ["c", "b"]);
    assert_eq!(PyRandom::new(101).getrandbits(64), 15626562030168072909);
    assert_eq!(PyRandom::new((1 << 40) + 5).getrandbits(17), 66110);
}

#[test]
fn composer_matches_recorded_python() {
    if let (Ok(p), Ok(w)) = (
        std::env::var("HYDO_RECORD_PY"),
        std::env::var("HYDO_RECORD_WORK"),
    ) {
        record_from_python(&p, &w);
    }
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(golden_dir().join("composer-plans.json")).unwrap(),
    )
    .unwrap();
    let plans: Vec<(String, Value)> = doc["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (s(p, "dir").to_string(), p["m"].clone()))
        .collect();
    let work = scratch("work");
    let (sections, summary) = rust_stream(&work, &plans);
    if golden::bless() {
        write_plans(&plans);
    } else {
        let changed: Vec<String> = doc["plans"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| folder_pin(Path::new(&resolve(s(p, "dir"), &work))) != s(p, "pin"))
            .map(|p| s(p, "dir").to_string())
            .collect();
        assert!(
            changed.is_empty(),
            "pinned plan folders changed since the recording: {changed:?}; check the change, then \
             re-record with HYDO_BLESS=1"
        );
    }
    let _ = std::fs::remove_dir_all(&work);
    golden::check(
        &golden_dir().join("composer.sha256"),
        &header(),
        &sections,
        CHUNK,
        &|_| true,
    );
    // the M98 corpora as test_composer.py draws them
    assert_eq!(summary["w_values"], json!(3000));
    assert_eq!(summary["resolved"], json!(514));
    assert_eq!(summary["removed"], json!(86));
    assert!(summary["plans"].as_u64().unwrap() >= 15, "{summary}");
    let total = summary["ops"].as_u64().unwrap();
    assert_eq!(total, 59_262, "{summary}");
    eprintln!("composer equivalence: {total} cases identical to the recorded hydo_composer.py answers; {summary}");
}
