//! `hydo/known-findings.yaml`: the findings a project accepts (`hydo ci accept`; stage 2 of
//! docs/design/ci.md, section 4.4), format 1.
//!
//! An accepted finding is reported as accepted and never fails a CI run by `fail_on`, so a known
//! failure does not make every pull request red. The file is reviewed like code. Like
//! `hydo/ci.yaml` it is **not** staged into the image (`hydo build` skips it), so accepting a
//! finding keeps the image and every fingerprint.
//!
//! ```yaml
//! format: 1
//! accepted:
//!   - key: f1:invariant:5ef675c8c9
//!     title: "VIOLATION [redis.acked_writes_survive] key k:# lost"
//!     schedule: "disk=cut dseed=70c9101f818fe74 | run C=14"
//!     reason: "appendfsync everysec: Redis documents the loss of up to one second of writes"
//!     who: ""
//!     date: "2026-11-13"
//! ```
//!
//! `key` is a finding key (`f1:<kind>:<10 hex>`, `hydo ci run`'s `ci-run.json`); `schedule` is the
//! minimal schedule it was accepted with, which `hydo ci run` replays when its exploration does
//! not find the key, to tell whether the entry can be removed. `who` names whoever accepted it;
//! hydo never fills it from a login or a host name. Within format 1 keys are only added (the
//! committed schema `hydo/schema/known-findings-v1.json`); an unknown key is an error.

use crate::ManifestError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The newest format of the file this hydo reads.
pub const FORMAT: u64 = 1;

/// Where `hydo ci run` and `hydo ci accept` look for the file, relative to the project's
/// directory.
pub const DEFAULT_PATH: &str = "hydo/known-findings.yaml";

/// One accepted finding.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Accepted {
    /// The finding key, `f1:<kind>:<10 hex>`.
    pub key: String,
    /// The finding's title when it was accepted (for people; the key decides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The minimal schedule it was accepted with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Why it is expected (required).
    pub reason: String,
    /// Who accepted it (optional; hydo never fills it in by itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
    /// The day it was accepted, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// `hydo/known-findings.yaml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KnownFindings {
    /// The format of this file: 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<u64>,
    /// The accepted findings.
    #[serde(default)]
    pub accepted: Vec<Accepted>,
}

impl KnownFindings {
    /// The entry of a key.
    pub fn get(&self, key: &str) -> Option<&Accepted> {
        self.accepted.iter().find(|a| a.key == key)
    }
}

fn err(m: String) -> ManifestError {
    ManifestError { message: m }
}

/// Whether a text is a finding key: `f<version>:<kind>:<10 lowercase hex>`.
pub fn is_key(k: &str) -> bool {
    let mut p = k.split(':');
    let (Some(v), Some(kind), Some(h), None) = (p.next(), p.next(), p.next(), p.next()) else {
        return false;
    };
    v.len() > 1
        && v.starts_with('f')
        && v[1..].chars().all(|c| c.is_ascii_digit())
        && !kind.is_empty()
        && kind.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        && h.len() == 10
        && h.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Read the file's text (`name` for messages).
pub fn parse(text: &str, name: &str) -> Result<KnownFindings, ManifestError> {
    let v = crate::yaml::parse_yaml(text, name)?;
    let v = if v.is_null() {
        Value::Object(Default::default())
    } else {
        v
    };
    if !v.is_object() {
        return Err(err(format!("{name}: known findings must be a mapping")));
    }
    if let Some(f) = v.get("format") {
        match f.as_u64() {
            Some(n) if (1..=FORMAT).contains(&n) => {}
            Some(n) => {
                return Err(err(format!(
                    "{name}: format {n}: this hydo reads hydo/known-findings.yaml format {FORMAT}; a newer hydo is needed"
                )))
            }
            None => return Err(err(format!("{name}: format must be a whole number"))),
        }
    }
    let k: KnownFindings = serde_path_to_error::deserialize(v)
        .map_err(|e| err(format!("{name}: {}: {}", e.path(), e.inner())))?;
    let mut seen = std::collections::BTreeSet::new();
    for (i, a) in k.accepted.iter().enumerate() {
        if !is_key(&a.key) {
            return Err(err(format!(
                "{name}: accepted[{i}].key: {:?} is not a finding key like f1:invariant:5ef675c8c9 (ci-run.json names each finding's key)",
                a.key
            )));
        }
        if a.reason.trim().is_empty() {
            return Err(err(format!(
                "{name}: accepted[{i}].reason: say why {} is expected",
                a.key
            )));
        }
        if !seen.insert(a.key.as_str()) {
            return Err(err(format!(
                "{name}: accepted[{i}].key: {} is accepted twice",
                a.key
            )));
        }
    }
    Ok(k)
}

/// A YAML scalar for any text: a double-quoted string (JSON's escapes are YAML's).
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The head of a new file.
pub const NEW_FILE_HEAD: &str = "# Findings this project accepts: `hydo ci run` reports each as accepted and never fails a\n# run on it (hydo guide ci). Add entries with `hydo ci accept <key> --reason ...`; remove an\n# entry when a run reports that it no longer reproduces.\nformat: 1\naccepted:\n";

/// The entry's lines, appended to the file's text.
pub fn entry_text(a: &Accepted) -> String {
    let mut e = vec![format!("  - key: {}", a.key)];
    if let Some(t) = &a.title {
        e.push(format!("    title: {}", q(t)));
    }
    if let Some(s) = &a.schedule {
        e.push(format!("    schedule: {}", q(s)));
    }
    e.push(format!("    reason: {}", q(&a.reason)));
    e.push(format!("    who: {}", q(a.who.as_deref().unwrap_or(""))));
    if let Some(d) = &a.date {
        e.push(format!("    date: {}", q(d)));
    }
    e.join("\n") + "\n"
}

/// The file's text with one more entry: `text` is the file as it is (None: a new file). The text
/// above the entry is kept as it is, comments included; the result is read back before it is
/// returned, so a file that would not load is never written.
pub fn append(text: Option<&str>, a: &Accepted, name: &str) -> Result<String, ManifestError> {
    let mut t = match text {
        None => NEW_FILE_HEAD.to_string(),
        Some(t) => {
            let have = parse(t, name)?;
            if have.get(&a.key).is_some() {
                return Err(err(format!(
                    "{name}: {} is already accepted; edit its entry there",
                    a.key
                )));
            }
            let mut t = t.to_string();
            if !t.lines().any(|l| l.starts_with("accepted:")) {
                if !t.ends_with('\n') && !t.is_empty() {
                    t.push('\n');
                }
                t.push_str("accepted:\n");
            } else if t.lines().any(|l| l.trim_end() == "accepted: []") {
                t = t
                    .lines()
                    .map(|l| {
                        if l.trim_end() == "accepted: []" {
                            "accepted:"
                        } else {
                            l
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
            }
            t
        }
    };
    if !t.ends_with('\n') {
        t.push('\n');
    }
    t.push_str(&entry_text(a));
    let back = parse(&t, name)?;
    if back.get(&a.key).as_ref().map(|x| &x.reason) != Some(&a.reason) {
        return Err(err(format!(
            "{name}: the new entry does not read back (is `accepted:` the file's last key?); add it by hand:\n{}",
            entry_text(a)
        )));
    }
    Ok(t)
}

/// The JSON Schema of the file, generated from the types (committed as
/// `hydo/schema/known-findings-v1.json`).
pub fn schema() -> Value {
    let g = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let s = g.into_root_schema_for::<KnownFindings>();
    let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
    if let Value::Object(o) = &mut v {
        o.insert(
            "title".into(),
            Value::String("hydo/known-findings.yaml, format 1".into()),
        );
        o.insert(
            "$comment".into(),
            Value::String(
                "Generated from hydo-manifest's known findings (hydo/crates/hydo-manifest/src/known_findings.rs); do not edit. Within format 1 keys are only added. HYDO_BLESS=1 cargo test -p hydo-manifest --test it ci_format:: rewrites it."
                    .into(),
            ),
        );
    }
    v
}

/// The schema as the committed file holds it.
pub fn schema_text() -> String {
    serde_json::to_string_pretty(&schema()).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(k: &str) -> Accepted {
        Accepted {
            key: k.into(),
            title: Some("VIOLATION [r.a] key k:# lost".into()),
            schedule: Some("disk=cut | run C=14".into()),
            reason: "everysec: \"one second\" of writes".into(),
            who: None,
            date: Some("2026-11-13".into()),
        }
    }

    #[test]
    fn entries_append_and_read_back() {
        let t = append(None, &entry("f1:invariant:0123456789"), "k").unwrap();
        assert!(t.starts_with("# Findings"), "{t}");
        assert!(t.contains("    who: \"\"\n"), "{t}");
        let t2 = append(Some(&t), &entry("f1:run-error:abcdefabcd"), "k").unwrap();
        let k = parse(&t2, "k").unwrap();
        assert_eq!(k.accepted.len(), 2);
        assert_eq!(k.accepted[0].reason, "everysec: \"one second\" of writes");
        assert_eq!(k.accepted[0].who.as_deref(), Some(""));
        let e = append(Some(&t2), &entry("f1:invariant:0123456789"), "k").unwrap_err();
        assert!(e.message.contains("already accepted"), "{}", e.message);
        // a file with an empty list, and one without the list
        let t3 = append(
            Some("format: 1\naccepted: []\n"),
            &entry("f1:invariant:0123456789"),
            "k",
        );
        assert_eq!(parse(&t3.unwrap(), "k").unwrap().accepted.len(), 1);
        let t4 = append(Some("format: 1"), &entry("f1:invariant:0123456789"), "k");
        assert_eq!(parse(&t4.unwrap(), "k").unwrap().accepted.len(), 1);
    }

    #[test]
    fn what_is_refused_says_why() {
        let msg = |t: &str| parse(t, "k").unwrap_err().message;
        assert!(msg("format: 2\n").contains("a newer hydo is needed"));
        assert!(msg("accepted:\n  - key: nope\n    reason: x\n").contains("is not a finding key"));
        assert!(
            msg("accepted:\n  - key: f1:invariant:0123456789\n    reason: \" \"\n")
                .contains("say why")
        );
        assert!(msg(
            "accepted:\n  - {key: f1:invariant:0123456789, reason: a}\n  - {key: f1:invariant:0123456789, reason: b}\n"
        )
        .contains("accepted twice"));
        assert!(
            msg("accepted:\n  - {key: f1:invariant:0123456789, reason: a, by: me}\n")
                .contains("by")
        );
        assert!(is_key("f1:history-check:00ff00ff00"));
        assert!(!is_key("f1:invariant:00FF00FF00"));
    }
}
