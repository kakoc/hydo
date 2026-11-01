//! The test composer, host side (the Rust port of `hydo/onboard/hydo_composer.py`; MANIFEST.md
//! "The test composer" and "Stable compositions (command ids)").
//!
//! A plan's `test:` section (or the `hydo/` folder) is read, validated and expanded into the
//! generic init's workload, provisioning and invariants by `hydo-manifest` (`expand`,
//! `scan_folder`, `read_regression`, `regression_status`); this crate re-exports those and adds
//! everything else the host does with a plan:
//!
//! - the composition token `W=` in its three forms (M98): the seed form
//!   `W=<seed hex>,p..,q..,r..[,n..][,t..][,x<entries>][,o<commands>]` that the explorer draws and
//!   shrinks, the resolved form `W=:p<p>,r<ms>[,t<ms>],c<id>+<id>...,<entry>.<round>.<k>.<seed>,...`
//!   that names commands by id, and the saved form `W=@<key>` ([`render_w`], [`stable_w`],
//!   [`entries_of`], [`describe`], [`contents_of`], [`draw`], [`candidates`]);
//! - saved regressions: [`regression_status`] (what a replay does), [`regression_warning`],
//!   [`saved_regression_text`] (`hydo save`);
//! - what a plan puts into an image: [`shell_lines`] (manifest.sh) and [`stage`] (compose.list,
//!   compose.checks, compose.json, compose.saved/, hydo/regressions/).
//!
//! Until M136 the guest side was `hydo/onboard/hydo-composer.sh` with two awk programs that
//! mirrored [`plan`] exactly (the tests checked that they agreed); since M136 the host controller
//! (hydo-control `schedule/composer.rs`) runs the rounds with this crate's code, and images carry
//! no guest composer (`git show d9202e7:hydo/onboard/hydo-composer.sh` restores it).
//!
//! `meta` everywhere is the plan meta as `expand` returns it and an image carries it
//! (`/etc/hydo/compose.json`): a JSON object with `params`, `commands` (each with `id`, `kind`,
//! `content`, ...), `checks`, `regressions`, `steps`, `composed`, `stable_compose`.

use hydo_manifest::py::{int_of, json_dumps_indent, str_of, truthy};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub use hydo_manifest::composer::{
    check_version, engine_version, expand, fits, kept, key_of, kind_of, parse_resolved, parse_w,
    plan, read_regression, render_resolved, resolve_seed, resolved_problems, scan_folder, shell_of,
    shq, test_from_meta, w_of, with_w, Entry, OItem, Resolved, WSeed, CHECK_KINDS, DEFAULT_STEPS,
    FORMAT, GUEST_MAX, HISTORY_KINDS, PLAN_KINDS, SH, WORKLOAD_KINDS,
};
pub use hydo_manifest::{regression_status, ManifestError, Status};

/// The guest composer the expanded plan's commands name (`compose.sh step`, `first`, `setw`,
/// `verdict`, ...). Since M136 no image carries it: the host controller recognises those calls and
/// runs the rounds itself (hydo-control `schedule/composer.rs`).
pub const COMPOSE_SH: &str = "/etc/hydo/compose.sh";
/// The composer's schedule token.
pub const W_TOKEN: &str = "W";
/// Resolved compositions of saved regressions (`W=@<key>`) inside an image.
pub const SAVED_DIR: &str = "/etc/hydo/compose.saved";

/// Why a `W=` value cannot be used here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WError {
    /// The value is malformed or names what the plan does not have (Python: a ValueError, which
    /// the explorer and `regression_status` catch).
    Invalid(String),
    /// The Python reference stops with another exception here (for example `q0`, a modulo by
    /// zero); the message starts with `hydo_manifest::py::CRASH_PREFIX`.
    Crash(String),
}

impl fmt::Display for WError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WError::Invalid(m) | WError::Crash(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for WError {}

impl From<ManifestError> for WError {
    fn from(e: ManifestError) -> Self {
        WError::Crash(e.message)
    }
}

/// `{key: resolved composition}` of the saved form `W=@<key>`.
pub type Saved = BTreeMap<String, String>;

// ------------------------------------------------------------------ the plan meta

fn commands(meta: &Value) -> Vec<Value> {
    meta.get("commands")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The ids and kinds of the plan's commands, in the plan's order.
pub fn ids_kinds(meta: &Value) -> (Vec<String>, Vec<String>) {
    let cs = commands(meta);
    (
        cs.iter().map(|c| str_of(&c["id"])).collect(),
        cs.iter().map(|c| str_of(&c["kind"])).collect(),
    )
}

fn params(meta: &Value) -> Value {
    meta.get("params").cloned().unwrap_or(Value::Null)
}

/// Python's `steps or meta["steps"]`.
fn steps_or(steps: Option<i128>, meta: &Value) -> i128 {
    match steps {
        Some(s) if s != 0 => s,
        _ => meta.get("steps").and_then(int_of).unwrap_or(0),
    }
}

fn param(p: &Value, k: &str) -> i128 {
    p.get(k).and_then(int_of).unwrap_or(0)
}

// ------------------------------------------------------------------ the seed form

fn rlist(xs: &[i128]) -> String {
    let mut xs = xs.to_vec();
    xs.sort();
    xs.dedup();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < xs.len() {
        let mut j = i;
        while j + 1 < xs.len() && xs[j + 1] == xs[j] + 1 {
            j += 1;
        }
        out.push(if i == j {
            xs[i].to_string()
        } else {
            format!("{}-{}", xs[i], xs[j])
        });
        i = j + 1;
    }
    out.join(".")
}

/// `render_w(d)`: a seed-form value from its fields (x and o are sets: sorted, without repeats).
pub fn render_w(d: &WSeed) -> String {
    let mut s = format!("{:08x},p{},q{},r{}", d.seed, d.p, d.q, d.r);
    if d.n != 0 {
        s.push_str(&format!(",n{}", d.n));
    }
    if d.t != 0 {
        s.push_str(&format!(",t{}", d.t));
    }
    if !d.x.is_empty() {
        s.push_str(&format!(",x{}", rlist(&d.x)));
    }
    if !d.o.is_empty() {
        let ints: Option<Vec<i128>> =
            d.o.iter()
                .map(|c| match c {
                    OItem::I(i) => Some(*i),
                    OItem::S(_) => None,
                })
                .collect();
        match ints {
            Some(v) => s.push_str(&format!(",o{}", rlist(&v))),
            None => {
                let mut v: Vec<String> =
                    d.o.iter()
                        .map(|c| match c {
                            OItem::I(i) => i.to_string(),
                            OItem::S(x) => x.clone(),
                        })
                        .collect();
                v.sort();
                v.dedup();
                s.push_str(&format!(",o{}", v.join("+")));
            }
        }
    }
    s
}

/// `parse_w(w, meta["params"])` with the error as a [`WError`].
pub fn parse_seed(w: &str, meta: &Value) -> Result<WSeed, WError> {
    parse_w(w, &params(meta)).map_err(WError::Invalid)
}

/// The entries a seed-form value runs on the plan's command list: (entry, round, command index,
/// HYDO_SEED).
pub fn kept_of(d: &WSeed, steps: i128, meta: &Value) -> Result<Vec<Entry>, WError> {
    let (ids, kinds) = ids_kinds(meta);
    Ok(kept(d, steps, &ids, &kinds)?)
}

// ------------------------------------------------------------------ stable compositions (M98)

/// `stable_w(w, meta, steps, saved)`: W in the resolved form, from any form.
pub fn stable_w(
    w: &str,
    meta: &Value,
    steps: Option<i128>,
    saved: Option<&Saved>,
) -> Result<String, WError> {
    if w.starts_with(':') {
        return Ok(w.to_string());
    }
    if let Some(k) = w.strip_prefix('@') {
        return saved.and_then(|s| s.get(k)).cloned().ok_or_else(|| {
            WError::Invalid(format!("W={w}: the saved composition is not known here"))
        });
    }
    let (ids, kinds) = ids_kinds(meta);
    let d = resolve_seed(w, &ids, &kinds, steps_or(steps, meta), &params(meta))?
        .map_err(WError::Invalid)?;
    Ok(render_resolved(&d))
}

/// `contents_of(w, meta, steps, saved)`: {command id: content hash} of the commands a composition
/// uses (the `# command-sha:` lines of a saved regression), sorted by id.
pub fn contents_of(
    w: &str,
    meta: &Value,
    steps: Option<i128>,
    saved: Option<&Saved>,
) -> Result<BTreeMap<String, String>, WError> {
    let mut have: BTreeMap<String, Value> = BTreeMap::new();
    for c in commands(meta) {
        have.insert(
            str_of(&c["id"]),
            c.get("content").cloned().unwrap_or(Value::Null),
        );
    }
    let d = parse_resolved(&stable_w(w, meta, steps, saved)?).map_err(WError::Invalid)?;
    let mut out = BTreeMap::new();
    for (_, _, cid, _) in d.entries {
        if let Some(v) = have.get(&cid).filter(|v| truthy(v)) {
            out.insert(cid, str_of(v));
        }
    }
    Ok(out)
}

/// `entries_of(w, meta, steps, saved)`: the entries (entry, round, command index in the plan's
/// list, HYDO_SEED) any form of W runs on this plan. A resolved or saved form that names a command
/// the plan lacks, or a round it does not run, is `WError::Invalid`.
pub fn entries_of(
    w: &str,
    meta: &Value,
    steps: Option<i128>,
    saved: Option<&Saved>,
) -> Result<Vec<Entry>, WError> {
    let steps = steps_or(steps, meta);
    if w.starts_with(':') || w.starts_with('@') {
        let d = parse_resolved(&stable_w(w, meta, Some(steps), saved)?).map_err(WError::Invalid)?;
        let errs = resolved_problems(&d, meta, Some(steps));
        if !errs.is_empty() {
            return Err(WError::Invalid(errs.join("; ")));
        }
        let (ids, _) = ids_kinds(meta);
        let mut ix: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, id) in ids.iter().enumerate() {
            ix.insert(id, i);
        }
        return Ok(d
            .entries
            .iter()
            .map(|(e, r, cid, s)| (*e, *r, ix[cid.as_str()], *s))
            .collect());
    }
    kept_of(&parse_seed(w, meta)?, steps, meta)
}

/// `describe(w, meta, steps)`: one line per round, e.g. `r0: parallel_driver_set, parallel_driver_set | r1: serial_driver_audit`.
pub fn describe(w: &str, meta: &Value, steps: Option<i128>) -> Result<String, WError> {
    let steps = steps_or(steps, meta);
    let (ids, _) = ids_kinds(meta);
    let mut rounds: BTreeMap<i128, Vec<String>> = BTreeMap::new();
    for (_, r, c, _) in entries_of(w, meta, Some(steps), None)? {
        rounds.entry(r).or_default().push(ids[c].clone());
    }
    Ok(rounds
        .iter()
        .map(|(r, v)| format!("r{}: {}", r, v.join(", ")))
        .collect::<Vec<_>>()
        .join(" | "))
}

// ------------------------------------------------------------------ explorer side

/// `draw(rng_next, meta)`: a seed-form W value for a new schedule, from the explorer's own random
/// stream (`rng_next()` gives the next 64-bit value): seed, concurrency and round length.
pub fn draw(rng_next: &mut dyn FnMut() -> u64, meta: &Value) -> String {
    let p = params(meta);
    let seed = (rng_next() & 0xFFFF_FFFF) as i128;
    let par = 1 + (rng_next() as i128).rem_euclid(param(&p, "parallel"));
    let q = 1 + (rng_next() as i128).rem_euclid(param(&p, "per_round"));
    let rm = param(&p, "round_ms");
    let choices = [(rm.div_euclid(2)).max(10), rm, (rm * 2).min(60000)];
    let r = choices[(rng_next() % 3) as usize];
    render_w(&WSeed {
        seed,
        p: par,
        q,
        r,
        n: param(&p, "max_commands"),
        t: param(&p, "budget_ms"),
        x: Vec::new(),
        o: Vec::new(),
    })
}

fn dedup_without(out: Vec<String>, w: &str) -> Vec<String> {
    let mut res: Vec<String> = Vec::new();
    for c in out {
        if c != w && !res.contains(&c) {
            res.push(c);
        }
    }
    res
}

fn union(a: &[i128], b: impl IntoIterator<Item = i128>) -> Vec<i128> {
    let mut v = a.to_vec();
    for x in b {
        if !v.contains(&x) {
            v.push(x);
        }
    }
    v
}

/// `candidates(w, meta, steps)`: simpler compositions than W for the shrinker, most aggressive
/// first: one command never run (o, by id), fewer commands (n halves), no concurrency (p=1, then
/// p-1), half of the remaining entries, then single entries (when few are left). A resolved form
/// gives resolved candidates; a saved form (`@<key>`) none.
pub fn candidates(w: &str, meta: &Value, steps: Option<i128>) -> Result<Vec<String>, WError> {
    let steps = steps_or(steps, meta);
    if w.starts_with(':') {
        return resolved_candidates(w);
    }
    if w.starts_with('@') {
        return Ok(Vec::new());
    }
    let d = parse_seed(w, meta)?;
    let (ids, _) = ids_kinds(meta);
    let ks = kept_of(&d, steps, meta)?;
    let mut o_ids: Vec<String> = Vec::new();
    for c in &d.o {
        let id = match c {
            // an index the list does not have is dropped (parse_w gives no negative index)
            OItem::I(i) if *i >= 0 && *i < ids.len() as i128 => ids[*i as usize].clone(),
            OItem::I(_) => continue,
            OItem::S(x) => x.clone(),
        };
        if !o_ids.contains(&id) {
            o_ids.push(id);
        }
    }
    let base = WSeed {
        o: o_ids.iter().cloned().map(OItem::S).collect(),
        ..d.clone()
    };
    let mut out: Vec<String> = Vec::new();
    let mut used: Vec<usize> = ks.iter().map(|e| e.2).collect();
    used.sort();
    used.dedup();
    if used.len() > 1 {
        for c in &used {
            let mut o = base.o.clone();
            if !o_ids.contains(&ids[*c]) {
                o.push(OItem::S(ids[*c].clone()));
            }
            out.push(render_w(&WSeed { o, ..base.clone() }));
        }
    }
    let n = ks.len() as i128;
    if ks.len() > 1 {
        out.push(render_w(&WSeed {
            n: (n / 2).max(1),
            ..base.clone()
        }));
    }
    if d.p > 1 {
        out.push(render_w(&WSeed {
            p: 1,
            ..base.clone()
        }));
        if d.p > 2 {
            out.push(render_w(&WSeed {
                p: d.p - 1,
                ..base.clone()
            }));
        }
    }
    if ks.len() > 3 {
        let half = ks.len() / 2;
        out.push(render_w(&WSeed {
            x: union(&d.x, ks[..half].iter().map(|e| e.0)),
            ..base.clone()
        }));
    }
    if ks.len() > 1 && ks.len() <= 8 {
        for e in &ks {
            out.push(render_w(&WSeed {
                x: union(&d.x, [e.0]),
                ..base.clone()
            }));
        }
    }
    Ok(dedup_without(out, w))
}

/// `_resolved_candidates(w)`: simpler resolved compositions: one command's entries dropped, the
/// second half dropped, no concurrency, the first half dropped, single entries (8 or fewer left).
pub fn resolved_candidates(w: &str) -> Result<Vec<String>, WError> {
    let d = parse_resolved(w).map_err(WError::Invalid)?;
    let es = d.entries.clone();
    let mut out: Vec<String> = Vec::new();
    let mut var = |entries: Vec<(i128, i128, String, i128)>, p: i128| {
        if !entries.is_empty() {
            out.push(render_resolved(&Resolved {
                p,
                r: d.r,
                t: d.t,
                entries,
            }));
        }
    };
    let mut used: Vec<String> = Vec::new();
    for e in &es {
        if !used.contains(&e.2) {
            used.push(e.2.clone());
        }
    }
    if used.len() > 1 {
        for cid in &used {
            var(es.iter().filter(|e| &e.2 != cid).cloned().collect(), d.p);
        }
    }
    if es.len() > 1 {
        var(es[..(es.len() / 2).max(1)].to_vec(), d.p);
    }
    if d.p > 1 {
        var(es.clone(), 1);
        if d.p > 2 {
            var(es.clone(), d.p - 1);
        }
    }
    if es.len() > 3 {
        var(es[es.len() / 2..].to_vec(), d.p);
    }
    if es.len() > 1 && es.len() <= 8 {
        for i in 0..es.len() {
            let mut v = es[..i].to_vec();
            v.extend_from_slice(&es[i + 1..]);
            var(v, d.p);
        }
    }
    Ok(dedup_without(out, w))
}

// ------------------------------------------------------------------ saved regressions

/// A [`Status`] as the JSON object Python's `regression_status` returns.
pub fn status_json(st: &Status) -> Value {
    let mut m = Map::new();
    m.insert("schedule".into(), Value::String(st.schedule.clone()));
    m.insert(
        "compose".into(),
        st.compose.clone().map(Value::String).unwrap_or(Value::Null),
    );
    m.insert(
        "error".into(),
        st.error.clone().map(Value::String).unwrap_or(Value::Null),
    );
    m.insert(
        "warnings".into(),
        Value::Array(st.warnings.iter().cloned().map(Value::String).collect()),
    );
    Value::Object(m)
}

/// `regression_warning(r, meta)`: the error and warnings of a replay as one line, or None.
pub fn regression_warning(r: &Value, meta: &Value) -> Result<Option<String>, ManifestError> {
    let st = regression_status(r, meta, None)?;
    let mut msgs: Vec<String> = st.error.into_iter().collect();
    msgs.extend(st.warnings);
    Ok(if msgs.is_empty() {
        None
    } else {
        Some(msgs.join("; "))
    })
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn req<'a>(x: &'a Value, k: &str) -> Result<&'a Value, String> {
    x.get(k)
        .ok_or_else(|| format!("the report's shrunk entry has no {k:?}"))
}

fn sorted_contents(x: &Value) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = match x.get("contents") {
        Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), str_of(v))).collect(),
        _ => Vec::new(),
    };
    v.sort();
    v
}

fn joined(v: &Value) -> String {
    match v {
        Value::Array(a) => a.iter().map(str_of).collect::<Vec<_>>().join(" "),
        other => str_of(other),
    }
}

/// `saved_regression_text(x)`: the `hydo save` file for one shrunk failure of a report
/// (`report.json` `shrunk[]`). Err names a missing field (Python: a KeyError).
pub fn saved_regression_text(x: &Value) -> Result<String, String> {
    let mut l = vec![
        format!(
            "# found by `hydo explore` (seed {}): {}",
            str_of(req(x, "from_seed")?),
            str_of(req(x, "class")?)
        ),
        format!("# verdict: {}", head(&str_of(req(x, "min_verdict")?), 200)),
    ];
    let get = |k: &str| x.get(k).filter(|v| truthy(v));
    if let Some(c) = get("composition") {
        l.push(format!("# composition: {}", head(&str_of(c), 400)));
    }
    let minimal = str_of(req(x, "minimal")?);
    let mut sched = get("stable_schedule")
        .map(str_of)
        .unwrap_or(minimal.clone());
    let seedform = w_of(&minimal).unwrap_or_default();
    let commands = x.get("commands").filter(|c| !c.is_null());
    if get("stable").is_some()
        && get("stable_note").is_some()
        && commands.is_some()
        && !(seedform.starts_with(':') || seedform.starts_with('@'))
    {
        sched = minimal;
        l.push(format!(
            "# commands: {}",
            joined(commands.unwrap_or(&Value::Null))
        ));
        for (k, v) in sorted_contents(x) {
            l.push(format!("# command-sha: {k} {v}"));
        }
        l.push(format!(
            "# note: {}",
            str_of(get("stable_note").unwrap_or(&Value::Null))
        ));
    } else if let Some(stable) = get("stable") {
        if w_of(&sched).is_some_and(|w| w.starts_with('@')) {
            l.push(format!("# compose: {}", str_of(stable)));
        }
        for (k, v) in sorted_contents(x) {
            l.push(format!("# command-sha: {k} {v}"));
        }
        if let Some(n) = get("stable_note") {
            l.push(format!("# note: {}", str_of(n)));
        }
    } else if let Some(c) = commands {
        l.push(format!("# commands: {}", joined(c)));
    }
    l.push("# expect: violation      (change to ok when the bug is fixed)".into());
    l.push(sched);
    Ok(l.join("\n") + "\n")
}

// ------------------------------------------------------------------ the image

/// `shell_lines(meta)`: the manifest.sh lines of a composed plan (appended last, so other
/// manifests compile unchanged).
pub fn shell_lines(meta: &Value) -> Vec<String> {
    let p = params(meta);
    let mut l = vec!["HYDO_COMPOSE=1".to_string()];
    for k in [
        "parallel",
        "per_round",
        "round_ms",
        "command_timeout",
        "drain_timeout",
        "max_commands",
        "budget_ms",
    ] {
        l.push(format!(
            "HYDO_COMPOSE_{}={}",
            k.to_uppercase(),
            param(&p, k)
        ));
    }
    l.push("HYDO_KNOB_W_default='-'".into());
    l.push(format!(
        "HYDO_KNOB_W_set={}",
        shq(&format!("{SH} setw \"$HYDO_VALUE\""))
    ));
    l.push("HYDO_KNOB_W_name='test composer (W=)'".into());
    l
}

fn io_err(p: &Path, e: std::io::Error) -> String {
    format!("{}: {}", p.display(), e)
}

#[cfg(unix)]
fn copy_times(src_meta: &std::fs::Metadata, dst: &Path, follow: bool) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let c = std::ffi::CString::new(dst.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let ts = [
        libc::timespec {
            tv_sec: src_meta.atime() as _,
            tv_nsec: src_meta.atime_nsec() as _,
        },
        libc::timespec {
            tv_sec: src_meta.mtime() as _,
            tv_nsec: src_meta.mtime_nsec() as _,
        },
    ];
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    // SAFETY: c is a valid NUL-terminated path and ts holds two initialised timespecs.
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), flags) };
    if rc != 0 {
        return Err(io_err(dst, std::io::Error::last_os_error()));
    }
    Ok(())
}

/// `shutil.copytree(src, dst, symlinks=True)`: symlinks stay symlinks; files and directories
/// keep their permission bits and times (copy2 / copystat; extended attributes are not copied).
#[cfg(unix)]
fn copytree(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::create_dir_all(dst).map_err(|e| io_err(dst, e))?;
    let mut names: Vec<_> = std::fs::read_dir(src)
        .map_err(|e| io_err(src, e))?
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    names.sort();
    for n in names {
        let s = src.join(&n);
        let d = dst.join(&n);
        let md = std::fs::symlink_metadata(&s).map_err(|e| io_err(&s, e))?;
        if md.file_type().is_symlink() {
            let to = std::fs::read_link(&s).map_err(|e| io_err(&s, e))?;
            std::os::unix::fs::symlink(&to, &d).map_err(|e| io_err(&d, e))?;
            copy_times(&md, &d, false)?;
        } else if md.is_dir() {
            copytree(&s, &d)?;
        } else {
            std::fs::copy(&s, &d).map_err(|e| io_err(&s, e))?;
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(md.mode() & 0o7777))
                .map_err(|e| io_err(&d, e))?;
            copy_times(&md, &d, true)?;
        }
    }
    let md = std::fs::metadata(src).map_err(|e| io_err(src, e))?;
    std::fs::set_permissions(dst, std::fs::Permissions::from_mode(md.mode() & 0o7777))
        .map_err(|e| io_err(dst, e))?;
    copy_times(&md, dst, true)
}

fn write(p: &Path, text: &str) -> Result<(), String> {
    std::fs::write(p, text).map_err(|e| io_err(p, e))
}

fn mkdirs(p: &Path) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| io_err(p, e))
}

/// `stage(meta, R, here, folder)`: copy the plan into the image root `root`: the `hydo/` folder
/// (whole, helpers included) under `/hydo/`, the command and check lists the guest composer reads
/// (`/etc/hydo/compose.list`, `compose.checks`), the plan for the explorer (`compose.json`), the
/// saved compositions (`compose.saved/<key>`), a plan's own regressions as
/// `/hydo/regressions/<name>.schedule`. (Until M136 also `hydo-composer.sh` as
/// `/etc/hydo/compose.sh`, the guest composer of the shell-init images.) `meta` is the plan meta
/// without `_folder`; `folder` is what `scan_folder` returned.
#[cfg(unix)]
pub fn stage(meta: &Value, root: &Path, folder: Option<&Value>) -> Result<(), String> {
    if let Some(Value::Object(dirs)) = folder.and_then(|f| f.get("_dirs")) {
        let mut subs: Vec<(&String, &Value)> = dirs.iter().collect();
        subs.sort_by(|a, b| a.0.cmp(b.0));
        for (sub, src) in subs {
            let dst = root.join("hydo").join(sub);
            if dst.is_dir() {
                std::fs::remove_dir_all(&dst).map_err(|e| io_err(&dst, e))?;
            }
            copytree(Path::new(&str_of(src)), &dst)?;
        }
    }
    let etc = root.join("etc/hydo");
    mkdirs(&etc)?;
    let cmds = commands(meta);
    let mut list = String::new();
    for (i, c) in cmds.iter().enumerate() {
        list.push_str(&format!(
            "{} {} {} {} {}\n",
            i,
            str_of(&c["kind"]),
            int_of(&c["timeout"]).unwrap_or(0),
            str_of(&c["id"]),
            shell_of(&c["run"]).replace('\n', " ")
        ));
    }
    write(&etc.join("compose.list"), &list)?;
    let ct = param(&params(meta), "command_timeout");
    let mut checks = String::new();
    for (i, c) in meta
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        checks.push_str(&format!(
            "{} {} {} {} {}\n",
            i,
            str_of(&c["kind"]),
            ct,
            str_of(&c["name"]),
            shell_of(&c["run"]).replace('\n', " ")
        ));
    }
    write(&etc.join("compose.checks"), &checks)?;
    write(
        &etc.join("compose.json"),
        &(json_dumps_indent(meta, 1, true) + "\n"),
    )?;
    let regs: Vec<Value> = meta
        .get("regressions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut saved: BTreeMap<String, String> = BTreeMap::new();
    for r in &regs {
        let w = w_of(&str_of(&r["schedule"]));
        let compose = r.get("compose").filter(|c| truthy(c)).map(str_of);
        if let (Some(w), Some(c)) = (w, compose) {
            if let Some(k) = w.strip_prefix('@') {
                if key_of(&c) == k {
                    saved.insert(k.to_string(), c);
                }
            }
        }
    }
    if !saved.is_empty() {
        let d = root.join(SAVED_DIR.trim_start_matches('/'));
        mkdirs(&d)?;
        for (k, c) in &saved {
            write(&d.join(k), &format!("{c}\n"))?;
        }
    }
    if !regs.is_empty() {
        let d = root.join("hydo").join("regressions");
        mkdirs(&d)?;
        for r in &regs {
            let p = d.join(format!("{}.schedule", str_of(&r["name"])));
            if p.exists() {
                continue;
            }
            let mut t = String::new();
            if let Some(cs) = r.get("commands").filter(|c| !c.is_null()) {
                t.push_str(&format!("# commands: {}\n", joined(cs)));
            }
            if let Some(c) = r.get("compose").filter(|c| truthy(c)) {
                t.push_str(&format!("# compose: {}\n", str_of(c)));
            }
            for (k, v) in sorted_contents(r) {
                t.push_str(&format!("# command-sha: {k} {v}\n"));
            }
            let sched = r
                .get("saved_schedule")
                .filter(|s| truthy(s))
                .unwrap_or(&r["schedule"]);
            t.push_str(&format!(
                "# expect: {}\n{}\n",
                str_of(&r["expect"]),
                str_of(sched)
            ));
            write(&p, &t)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta() -> Value {
        json!({"params": {"parallel": 4, "per_round": 2, "round_ms": 100, "command_timeout": 60,
                          "drain_timeout": 60, "check_every": 2, "check_within": 30,
                          "max_commands": 0, "budget_ms": 0},
               "commands": [
                   {"id": "anytime_read", "kind": "anytime", "content": "aaaaaaaaaaaaaaaa"},
                   {"id": "parallel_driver_set", "kind": "parallel_driver", "content": "bbbbbbbbbbbbbbbb"},
                   {"id": "parallel_driver_transfer", "kind": "parallel_driver", "content": "cccccccccccccccc"}],
               "steps": 20, "composed": true, "stable_compose": 1})
    }

    #[test]
    fn render_round_trip() {
        let m = meta();
        for w in [
            "0000002a,p4,q2,r100",
            "fc94c8ea,p2,q2,r200,n2,x4,o0.4.6",
            "0000002a,p4,q2,r100,oparallel_driver_set",
            "deadbeef,p1,q1,r50,n7,t300,x0.3-4,oanytime_read+parallel_driver_set",
        ] {
            assert_eq!(render_w(&parse_seed(w, &m).unwrap()), w);
        }
    }

    #[test]
    fn resolved_and_saved() {
        let m = meta();
        let res = stable_w("0000002a,p4,q2,r100", &m, None, None).unwrap();
        assert!(res.starts_with(":p4,r100,c"));
        let a: Vec<Entry> = entries_of("0000002a,p4,q2,r100", &m, None, None).unwrap();
        assert_eq!(entries_of(&res, &m, None, None).unwrap(), a);
        let mut saved = Saved::new();
        saved.insert(key_of(&res), res.clone());
        let at = format!("@{}", key_of(&res));
        assert_eq!(entries_of(&at, &m, None, Some(&saved)).unwrap(), a);
        assert!(matches!(
            entries_of(&at, &m, None, None),
            Err(WError::Invalid(_))
        ));
        assert_eq!(
            describe(&res, &m, None).unwrap(),
            describe("0000002a,p4,q2,r100", &m, None).unwrap()
        );
        for c in candidates(&res, &m, None).unwrap() {
            assert!(parse_resolved(&c).unwrap().entries.len() <= a.len());
        }
        assert!(candidates(&at, &m, None).unwrap().is_empty());
    }

    #[test]
    fn draw_is_a_seed_form() {
        let mut x = 0u64;
        let mut next = || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x
        };
        let w = draw(&mut next, &meta());
        assert_eq!(render_w(&parse_seed(&w, &meta()).unwrap()), w);
    }

    #[test]
    fn saved_text() {
        let x = json!({"from_seed": 6, "class": "violation", "min_verdict": "VIOLATION x",
                       "minimal": "disk=none | run W=:p1,r100,cparallel_driver_set,0.0.0.5",
                       "stable": ":p1,r100,cparallel_driver_set,0.0.0.5",
                       "contents": {"parallel_driver_set": "bbbbbbbbbbbbbbbb"}});
        assert_eq!(
            saved_regression_text(&x).unwrap(),
            "# found by `hydo explore` (seed 6): violation\n# verdict: VIOLATION x\n\
             # command-sha: parallel_driver_set bbbbbbbbbbbbbbbb\n\
             # expect: violation      (change to ok when the bug is fixed)\n\
             disk=none | run W=:p1,r100,cparallel_driver_set,0.0.0.5\n"
        );
        assert!(saved_regression_text(&json!({})).is_err());
    }
}
