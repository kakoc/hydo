//! `configs:` (Milestone 299, a format-1 addition): the build parameters whose values `hydo
//! explore --configs` explores, one image per combination of values.
//!
//! An entry is keyed by a parameter that `params:` declares, and lists the values to try. The
//! parameter's default comes first, whether `values:` lists it or not. `service:` names the
//! service whose configuration the parameter is, `doc:` where the project documents it, and
//! `tradeoff:` the values whose documentation says that they weaken a promise, each with that
//! note:
//!
//! ```yaml
//! params:
//!   appendfsync: always
//! configs:
//!   appendfsync:
//!     values: [always, everysec, "no"]
//!     service: redis
//!     tradeoff:
//!       everysec: "fsync once a second: a power cut can lose the last second of writes"
//! ```
//!
//! The build ignores `configs:` (the image is made from `params:` and `--set` alone); the
//! validator checks its shape, and `apply_params` that every key is a declared parameter.

use serde_json::Value;

use crate::py::{self, repr_str, str_of};
use crate::validate::{err, unknown, R};

/// The keys of a `configs:` entry.
pub const KNOWN: &[&str] = &["values", "service", "doc", "tradeoff", "description"];

/// One setting to explore, as `hydo explore --configs` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// The parameter (a key of `params:`).
    pub param: String,
    /// Its default in `params:`.
    pub default: String,
    /// The values to explore, the default first.
    pub values: Vec<String>,
    /// The services whose configuration it is (`service:`, one name or a list).
    pub services: Vec<String>,
    /// Where the project documents the setting.
    pub doc: Option<String>,
    /// The values whose documentation says they weaken a promise, with that note, in the order
    /// of `values`.
    pub tradeoffs: Vec<(String, String)>,
}

impl Setting {
    /// The trade-off note of `value`, if it has one.
    pub fn tradeoff(&self, value: &str) -> Option<&str> {
        self.tradeoffs
            .iter()
            .find(|(v, _)| v == value)
            .map(|(_, n)| n.as_str())
    }
}

fn text_of(v: &Value, wh: &str) -> R<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(_) | Value::Bool(_) => Ok(str_of(v)),
        other => Err(err(format!(
            "{wh}: a string, a number or true/false, not {}",
            py::repr(other)
        ))),
    }
}

/// The validator's check of `configs:` (the parameters themselves are checked by
/// `apply_params`, which still has `params:`). `services` are the manifest's service names.
pub fn validate(configs: Option<&Value>, services: &[String]) -> R<()> {
    let Some(c) = configs else {
        return Ok(());
    };
    if c.is_null() {
        return Ok(());
    }
    let Value::Object(o) = c else {
        return Err(err(
            "configs: a mapping of parameter name: {values: [...], service:, doc:, tradeoff:}",
        ));
    };
    for (k, e) in o {
        let wh = format!("configs.{k}");
        if !e.is_object() {
            return Err(err(format!(
                "{wh}: a mapping with values: (the values to explore), and optionally service:, doc: and tradeoff:"
            )));
        }
        unknown(e, KNOWN, &wh)?;
        let vals = match e.get("values") {
            Some(Value::Array(a)) if !a.is_empty() => a,
            _ => {
                return Err(err(format!(
                    "{wh}.values: a non-empty list of the values to explore"
                )))
            }
        };
        let mut seen: Vec<String> = Vec::new();
        for (i, v) in vals.iter().enumerate() {
            let s = text_of(v, &format!("{wh}.values[{i}]"))?;
            if s.is_empty() {
                return Err(err(format!("{wh}.values[{i}]: an empty value")));
            }
            if seen.contains(&s) {
                return Err(err(format!(
                    "{wh}.values: {} is listed twice",
                    repr_str(&s)
                )));
            }
            seen.push(s);
        }
        if let Some(s) = e.get("service") {
            let names = match s {
                Value::Array(a) if !a.is_empty() => a.clone(),
                Value::String(_) => vec![s.clone()],
                _ => {
                    return Err(err(format!(
                        "{wh}.service: a service's name, or a list of services' names"
                    )))
                }
            };
            for n in &names {
                let s = text_of(n, &format!("{wh}.service"))?;
                if !services.contains(&s) {
                    return Err(err(format!(
                        "{wh}.service: no service {} (services: {})",
                        repr_str(&s),
                        if services.is_empty() {
                            "none".to_string()
                        } else {
                            services.join(", ")
                        }
                    )));
                }
            }
        }
        if let Some(d) = e.get("doc") {
            text_of(d, &format!("{wh}.doc"))?;
        }
        if let Some(t) = e.get("tradeoff") {
            let Value::Object(t) = t else {
                return Err(err(format!(
                    "{wh}.tradeoff: a mapping of value: the documented trade-off of that value"
                )));
            };
            for (v, note) in t {
                let n = text_of(note, &format!("{wh}.tradeoff.{v}"))?;
                if n.trim().is_empty() {
                    return Err(err(format!(
                        "{wh}.tradeoff.{v}: say what the documentation says the value weakens"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// `apply_params`' check, with the defaults of `params:` (before `--set`): every key of
/// `configs:` is a declared parameter, and every value with a trade-off note is one of its values
/// or its default.
pub fn check_params(configs: Option<&Value>, declared: &[(String, String)]) -> R<()> {
    let Some(Value::Object(o)) = configs else {
        return Ok(());
    };
    for (k, e) in o {
        let Some((_, default)) = declared.iter().find(|(n, _)| n == k) else {
            let mut ks: Vec<&str> = declared.iter().map(|(n, _)| n.as_str()).collect();
            ks.sort();
            return Err(err(format!(
                "configs.{k}: {} is not a parameter of params: ({}); a setting to explore is a build parameter",
                repr_str(k),
                if ks.is_empty() {
                    "none".to_string()
                } else {
                    ks.join(", ")
                }
            )));
        };
        let mut vals: Vec<String> = vec![default.clone()];
        for v in e
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            vals.push(str_of(v));
        }
        if let Some(Value::Object(t)) = e.get("tradeoff") {
            for v in t.keys() {
                if !vals.contains(v) {
                    return Err(err(format!(
                        "configs.{k}.tradeoff: {} is not one of the values ({})",
                        repr_str(v),
                        vals.join(", ")
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The settings of a document as written (before `params:` is applied), each with the
/// parameter's default first. Empty without `configs:`. The document must be valid (`load`).
pub fn read(document: &Value) -> R<Vec<Setting>> {
    let Some(Value::Object(o)) = document.get("configs") else {
        return Ok(Vec::new());
    };
    let params = document.get("params").and_then(Value::as_object);
    let mut out = Vec::new();
    for (k, e) in o {
        let wh = format!("configs.{k}");
        let default = match params.and_then(|p| p.get(k)) {
            Some(Value::Null) => String::new(),
            Some(v) => text_of(v, &format!("params.{k}"))?,
            None => return Err(err(format!("{wh}: {k} is not a parameter of params:"))),
        };
        let mut values = vec![default.clone()];
        for (i, v) in e
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let s = text_of(v, &format!("{wh}.values[{i}]"))?;
            if !values.contains(&s) {
                values.push(s);
            }
        }
        let mut tradeoffs = Vec::new();
        if let Some(Value::Object(t)) = e.get("tradeoff") {
            for v in &values {
                if let Some(n) = t.get(v) {
                    tradeoffs.push((v.clone(), text_of(n, &format!("{wh}.tradeoff.{v}"))?));
                }
            }
        }
        out.push(Setting {
            param: k.clone(),
            default,
            values,
            services: match e.get("service") {
                Some(Value::Array(a)) => a.iter().map(str_of).collect(),
                Some(v) => vec![str_of(v)],
                None => Vec::new(),
            },
            doc: e.get("doc").map(str_of),
            tradeoffs,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::{apply_params, validate as validate_manifest};

    fn doc(configs: &str) -> Value {
        let text = format!(
            "name: db\nparams:\n  fsync: always\n  other: x\nservices:\n  - name: db\n    cmd: db\n    ready: 'true'\nworkload:\n  steps: 2\n  run: 'true'\ninvariants:\n  - id: db.ok\n    msg: m\n    cmd: 'true'\nconfigs:\n{configs}"
        );
        crate::yaml::parse_yaml(&text, "hydo.yaml").unwrap()
    }

    fn check(configs: &str) -> Result<(), String> {
        let mut m = doc(configs);
        apply_params(&mut m, &[]).map_err(|e| e.message.clone())?;
        validate_manifest(&mut m).map_err(|e| e.message)
    }

    #[test]
    fn a_setting_reads_with_its_default_first() {
        let c = "  fsync:\n    values: [everysec, always, \"no\"]\n    service: db\n    tradeoff:\n      everysec: may lose a second\n";
        assert_eq!(check(c), Ok(()));
        let s = read(&doc(c)).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].values, vec!["always", "everysec", "no"]);
        assert_eq!(s[0].default, "always");
        assert_eq!(s[0].services, vec!["db"]);
        let c2 = "  fsync:\n    values: [x]\n    service: [db]\n";
        assert_eq!(check(c2), Ok(()));
        assert_eq!(read(&doc(c2)).unwrap()[0].services, vec!["db"]);
        assert_eq!(s[0].tradeoff("everysec"), Some("may lose a second"));
        assert_eq!(s[0].tradeoff("always"), None);
    }

    #[test]
    fn refusals_name_the_place() {
        let e = check("  nope:\n    values: [a]\n").unwrap_err();
        assert!(
            e.contains("configs.nope") && e.contains("not a parameter"),
            "{e}"
        );
        let e = check("  fsync:\n    values: []\n").unwrap_err();
        assert!(e.contains("configs.fsync.values"), "{e}");
        let e = check("  fsync:\n    values: [a, a]\n").unwrap_err();
        assert!(e.contains("listed twice"), "{e}");
        let e = check("  fsync:\n    values: [a]\n    service: web\n").unwrap_err();
        assert!(e.contains("no service 'web'"), "{e}");
        let e = check("  fsync:\n    values: [a]\n    tradeoff:\n      b: c\n").unwrap_err();
        assert!(e.contains("not one of the values"), "{e}");
        let e = check("  fsync:\n    values: [a]\n    knob: R\n").unwrap_err();
        assert!(e.contains("knob"), "{e}");
        // the default may carry the trade-off (NATS: the default sync_interval)
        assert_eq!(
            check("  fsync:\n    values: [sync]\n    tradeoff:\n      always: lazy\n"),
            Ok(())
        );
    }
}
