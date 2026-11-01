//! A service's restart policy (`services[].restart:`, M134): what hydo does when the service's
//! process ends by itself while a test runs, named and meant as Docker Compose's `restart:`.
//!
//! | value | the service's process ends by itself (not by a hydo fault) |
//! |---|---|
//! | `no` (the default; a bare `no` in YAML reads as `false`, which means the same) | it stays down until the heal's restart pass, as without `restart:` |
//! | `always` | it is started again, whatever its exit status |
//! | `unless-stopped` | the same as `always` (nobody stops a service by hand in a test run) |
//! | `on-failure` | it is started again when its exit status is not 0 (a signal counts as a failure) |
//! | `on-failure:<max>` | the same, at most `<max>` times in one boot (`on-failure:0`: no limit, as Docker) |
//!
//! The value rules live here (the validator checks them after `params:` are replaced, and the
//! host controller reads the policy with [`Restart::parse`]); the model (`model.rs`) only fixes the
//! key and its kinds of value.

use serde_json::Value;

/// A restart policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Restart {
    /// `no`: today's behaviour.
    #[default]
    No,
    /// `always`.
    Always,
    /// `unless-stopped`: the same as `always` in a test run.
    UnlessStopped,
    /// `on-failure[:max]`: `max` 0 = no limit.
    OnFailure { max: u64 },
}

impl Restart {
    /// The policy of a `restart:` value: a string, or `false` (a bare YAML `no`).
    pub fn parse(v: &Value) -> Result<Restart, String> {
        let s = match v {
            Value::Bool(false) => return Ok(Restart::No),
            Value::String(s) => s.as_str(),
            other => {
                return Err(format!(
                    "no, always, on-failure, on-failure:<max> or unless-stopped (as Docker Compose's restart:), not {}",
                    crate::py::repr(other)
                ))
            }
        };
        match s {
            "no" => Ok(Restart::No),
            "always" => Ok(Restart::Always),
            "unless-stopped" => Ok(Restart::UnlessStopped),
            "on-failure" => Ok(Restart::OnFailure { max: 0 }),
            _ => match s.strip_prefix("on-failure:") {
                Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => n
                    .parse()
                    .map(|max| Restart::OnFailure { max })
                    .map_err(|_| format!("on-failure:{n}: the most restarts, a whole number")),
                Some(n) => Err(format!(
                    "on-failure:{n}: the most restarts must be a whole number (on-failure:3)"
                )),
                None => Err(format!(
                    "no, always, on-failure, on-failure:<max> or unless-stopped (as Docker Compose's restart:), not {}",
                    crate::py::repr_str(s)
                )),
            },
        }
    }

    /// The policy in Compose's words (`on-failure:3`).
    pub fn name(&self) -> String {
        match self {
            Restart::No => "no".into(),
            Restart::Always => "always".into(),
            Restart::UnlessStopped => "unless-stopped".into(),
            Restart::OnFailure { max: 0 } => "on-failure".into(),
            Restart::OnFailure { max } => format!("on-failure:{max}"),
        }
    }

    /// Whether the policy ever starts the service again.
    pub fn restarts(&self) -> bool {
        *self != Restart::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compose_words() {
        for (v, want) in [
            (json!("no"), Restart::No),
            (json!(false), Restart::No),
            (json!("always"), Restart::Always),
            (json!("unless-stopped"), Restart::UnlessStopped),
            (json!("on-failure"), Restart::OnFailure { max: 0 }),
            (json!("on-failure:0"), Restart::OnFailure { max: 0 }),
            (json!("on-failure:3"), Restart::OnFailure { max: 3 }),
        ] {
            assert_eq!(Restart::parse(&v), Ok(want), "{v}");
        }
        assert_eq!(Restart::OnFailure { max: 3 }.name(), "on-failure:3");
        assert_eq!(Restart::OnFailure { max: 0 }.name(), "on-failure");
        for bad in [
            json!(true),
            json!(3),
            json!("Always"),
            json!("on-failure:"),
            json!("on-failure:-1"),
            json!("on-failure:x"),
            json!("on_failure"),
            json!(""),
        ] {
            assert!(Restart::parse(&bad).is_err(), "{bad} was accepted");
        }
    }
}
