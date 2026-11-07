//! The list of every recorded example run, `hydo/onboard/recorded.json` (Milestone 164): the
//! source of the recorded values since then (docs/REBASELINE.md keeps the history of how each was
//! made). `hydo-bench rebaseline` reads it, checks every value against the tree and, with
//! `--write`, writes the new values back.
//!
//! The file (`"v": 1`, `"type": "recorded-runs"`) holds `images`, each:
//! - `id`: `<platform>/<name>`; `platform`: `hvx` (x86_64 on the kernel module), `hvm` (arm64 on
//!   the Mac) or `kvm` (x86_64 on stock KVM);
//! - `recorded`: where the values were last recorded (a REBASELINE.md section, or the
//!   `hydo-bench rebaseline --write` that wrote them); `note`: anything a reader must know;
//! - `build`: how the image is made from the tree: `stage` (the step of
//!   `hydo/onboard/rebaseline/stage.sh prepare` that stages its inputs), `dir` (the folder, in the
//!   staged tree, the manifest is built from), `manifest`, `args` (more `hydo build` arguments;
//!   `{inputs}` is the staging inputs folder), `owners` (`keep`: built as uid 1000, gid 100 with
//!   the staged files' owners kept, as the images with Nix store paths are; absent: normalised);
//! - `sha256`: the image's sha256 (or the prefix that was recorded); on the Mac the initramfs's;
//! - `big`: a stack-sized guest (its runs go alone);
//! - `runs`: `schedule`, `verdict` (the verdict line as recorded, without `HYDO-ORACLE: `; a long
//!   one may end in `...`; on the Mac a run whose history check replaced the guest's verdict reads
//!   `<guest verdict>; history <check verdict>`), `fp`, and where recorded `ctl` and `wfp`;
//!   `pinned: true` marks the pinned reproducers.
//!
//! What counts as the same verdict ([`verdict_key`]): the class (OK, VIOLATION, ERROR, ...) and,
//! for a VIOLATION, the property in brackets after it; for any other class but OK the words of
//! the line with every digit blanked. Differences in the rest of the line (timings, counts) are
//! reported, not counted as a change.

use serde_json::Value;

/// The verdict part that decides: the check's verdict when a history check replaced the guest's.
fn effective(text: &str) -> &str {
    let t = text.trim();
    let t = t.strip_prefix("HYDO-ORACLE: ").unwrap_or(t);
    match t.rfind("; history ") {
        Some(i) => &t[i + "; history ".len()..],
        None => t,
    }
}

/// The verdict's class and its key: (`VIOLATION`, `redis.acked_writes_survive`), (`OK`, ``),
/// (`ERROR`, `restart after the faults (mysql)` with digits as `#`).
pub fn verdict_key(text: &str) -> (String, String) {
    let t = effective(text);
    let t = t.strip_suffix("...").unwrap_or(t);
    let class: String = t.split_whitespace().next().unwrap_or("").to_string();
    let rest = t[class.len()..].trim_start();
    let key = match class.as_str() {
        "OK" => String::new(),
        "VIOLATION" => match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((id, _)) => id.to_string(),
            None => String::new(),
        },
        _ => rest
            .chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect(),
    };
    (class, key)
}

/// Whether a new verdict line has the recorded one's class and key. A recorded line that leaves
/// the property out (an abbreviated table: `VIOLATION key k:1 lost`) compares by class; an ERROR
/// line compares by its words, the recorded one may be the start of the new one.
pub fn same_verdict(recorded: &str, now: &str) -> bool {
    let (rc, rk) = verdict_key(recorded);
    let (nc, nk) = verdict_key(now);
    if rc != nc {
        return false;
    }
    match rc.as_str() {
        "OK" => true,
        "VIOLATION" => rk.is_empty() || rk == nk,
        _ => nk.starts_with(&rk) || rk.starts_with(&nk),
    }
}

/// Whether the whole recorded line is the new one (a recorded line ending in `...` is a prefix).
pub fn same_text(recorded: &str, now: &str) -> bool {
    let r = recorded.trim();
    let n = now.trim();
    let r = r.strip_prefix("HYDO-ORACLE: ").unwrap_or(r);
    let n = n.strip_prefix("HYDO-ORACLE: ").unwrap_or(n);
    match r.strip_suffix("...") {
        Some(p) => n.starts_with(p),
        None => r == n,
    }
}

/// The verdict line to record from a run document's verdict (`text`, and the guest's own line
/// when a history check replaced it), in the list's form.
pub fn verdict_line(text: &str, guest: Option<&str>) -> String {
    let strip = |s: &str| {
        s.trim()
            .strip_prefix("HYDO-ORACLE: ")
            .unwrap_or(s.trim())
            .to_string()
    };
    match guest {
        Some(g) if strip(g) != strip(text) => format!("{}; history {}", strip(g), strip(text)),
        _ => strip(text),
    }
}

/// A recorded hex value against a new one: equal, or the recorded value is a prefix of it (the
/// tables kept prefixes of image hashes).
pub fn same_hex(recorded: &str, now: &str) -> bool {
    !recorded.is_empty() && now.starts_with(recorded)
}

/// One image of the list, as the tool reads it.
#[derive(Clone, Debug)]
pub struct Image {
    pub index: usize,
    pub id: String,
    pub platform: String,
    pub stage: String,
    pub dir: String,
    pub manifest: String,
    pub args: Vec<String>,
    pub keep_owners: bool,
    pub sha256: String,
    pub big: bool,
    pub runs: Vec<Run>,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub schedule: String,
    pub verdict: String,
    pub fp: String,
    pub ctl: Option<String>,
    pub wfp: Option<String>,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// The list's images (every field checked); an error names the entry.
pub fn images(doc: &Value) -> Result<Vec<Image>, String> {
    if doc["v"] != 1 || doc["type"] != "recorded-runs" {
        return Err("not a recorded-runs list of version 1".into());
    }
    let arr = doc["images"].as_array().ok_or("no images")?;
    let mut out = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for (i, v) in arr.iter().enumerate() {
        let id = s(v, "id").ok_or(format!("image {i}: no id"))?;
        let e = |m: &str| format!("{id}: {m}");
        if !ids.insert(id.clone()) {
            return Err(e("listed twice"));
        }
        let platform = s(v, "platform").ok_or(e("no platform"))?;
        if !["hvx", "hvm", "kvm"].contains(&platform.as_str()) {
            return Err(e("platform must be hvx, hvm or kvm"));
        }
        if !id.starts_with(&format!("{platform}/")) {
            return Err(e("the id must start with the platform"));
        }
        let b = &v["build"];
        let args = b["args"]
            .as_array()
            .ok_or(e("build.args is not a list"))?
            .iter()
            .map(|a| a.as_str().map(str::to_string).ok_or(e("build.args: text")))
            .collect::<Result<Vec<_>, _>>()?;
        let keep_owners = match s(b, "owners").as_deref() {
            None => false,
            Some("keep") => true,
            Some(o) => return Err(e(&format!("build.owners {o}: only keep"))),
        };
        let mut runs = Vec::new();
        for r in v["runs"].as_array().ok_or(e("no runs"))? {
            let schedule = s(r, "schedule").ok_or(e("a run without a schedule"))?;
            let fp = s(r, "fp").ok_or(e(&format!("{schedule}: no fp")))?;
            runs.push(Run {
                verdict: s(r, "verdict").ok_or(e(&format!("{schedule}: no verdict")))?,
                fp,
                ctl: s(r, "ctl"),
                wfp: s(r, "wfp"),
                schedule,
            });
        }
        if runs.is_empty() {
            return Err(e("no runs"));
        }
        out.push(Image {
            index: i,
            platform,
            stage: s(b, "stage").ok_or(e("no build.stage"))?,
            dir: s(b, "dir").ok_or(e("no build.dir"))?,
            manifest: s(b, "manifest").ok_or(e("no build.manifest"))?,
            args,
            keep_owners,
            sha256: s(v, "sha256").unwrap_or_default(),
            big: v["big"].as_bool().unwrap_or(false),
            runs,
            id,
        });
    }
    Ok(out)
}

/// The list's text: serde_json's pretty form (two spaces, keys in their order) and a newline,
/// the form the file is kept in, so that `--write` changes only the values.
pub fn to_text(doc: &Value) -> String {
    let mut t = serde_json::to_string_pretty(doc).expect("a JSON value");
    t.push('\n');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(
            verdict_key("HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:0 lost"),
            ("VIOLATION".into(), "redis.acked_writes_survive".into())
        );
        assert_eq!(
            verdict_key("OK invariants=0 held (); history VIOLATION [redis.kv_linearizable]"),
            ("VIOLATION".into(), "redis.kv_linearizable".into())
        );
        assert_eq!(
            verdict_key("ERROR restart after the faults (mysql)"),
            ("ERROR".into(), "restart after the faults (mysql)".into())
        );
        assert!(same_verdict(
            "OK invariants=1 held (redis.acked_writes_survive: acked=20 all present)",
            "HYDO-ORACLE: OK invariants=1 held (redis.acked_writes_survive: acked=19 all present)"
        ));
        assert!(!same_verdict(
            "ERROR restart after the faults (redis)",
            "ERROR restart after the faults (mysql)"
        ));
        assert!(same_verdict(
            "ERROR restart after the faults",
            "ERROR restart after the faults (redis)"
        ));
        assert!(same_verdict(
            "VIOLATION key k:1 lost",
            "VIOLATION [redis.acked_writes_survive] key k:0 lost"
        ));
        assert!(!same_verdict("VIOLATION [a] x", "VIOLATION [b] x"));
        assert!(!same_verdict("OK", "VIOLATION [a] x"));
        assert!(same_verdict(
            "OK invariants=0 held (); history VIOLATION [kv.linearizable]",
            "HYDO-ORACLE: VIOLATION [kv.linearizable] key k:3: lost"
        ));
        assert!(same_text(
            "VIOLATION [a] key k:1: an ack...",
            "VIOLATION [a] key k:1: an acknowledged"
        ));
        assert!(!same_text("OK x=1", "OK x=2"));
        assert_eq!(
            verdict_line(
                "HYDO-ORACLE: VIOLATION [h] k",
                Some("HYDO-ORACLE: OK invariants=0 held ()")
            ),
            "OK invariants=0 held (); history VIOLATION [h] k"
        );
        assert_eq!(verdict_line("HYDO-ORACLE: OK x", None), "OK x");
        assert!(same_hex("eb8a9cb1", "eb8a9cb1ffff"));
        assert!(!same_hex("", "eb"));
    }
}
