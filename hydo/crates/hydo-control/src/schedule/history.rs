//! History recording (seam `history`; docs/HISTORY.md M87, M123): what
//! `hydo/onboard/hydo-init-history.sh` adds to the init for a manifest with `checks:`.
//!
//! The workload records its operations itself (`hydo_hist`, and the SDKs' recorders, write
//! `HYDO-HIST` lines straight to /dev/console), and the checks run on the host after the run
//! (hydo-history). The init only
//! - prints one `HYDO-CHECK <spec>` line per check when the workload starts (start_periodic, once
//!   per boot), so a saved console describes its own checks: [`declare`];
//! - at the end of the boot that gives the verdict (the run, or the recovery boot after a power
//!   cut) runs each check's `final:` read-back, bounded by workload.timeout with its output in
//!   workload.log, and prints its exit status (`HYDO-CHECK-FINAL`), before the final invariants
//!   (finish): [`finals`].

use super::{env, log, spawn_wait, timeout_argv, Ctx, F};
use crate::flow::Step;
use hydo_manifest::py;
use serde_json::{Map, Value};

/// One history check, as manifest.sh carries it (HYDO_CHECK_c<i>_*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistCheck {
    pub id: String,
    /// The spec the HYDO-CHECK line prints (hydo-build's `check_spec`).
    pub json: String,
    /// The `final:` read-back ("" = none).
    pub final_: String,
}

/// The checks of a validated manifest, in its order.
pub fn plan_of(d: &Value) -> Vec<HistCheck> {
    d.get("checks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| HistCheck {
            id: py::str_of(&c["id"]),
            json: spec(c),
            final_: c
                .get("final")
                .filter(|f| py::truthy(f))
                .map(py::str_of)
                .unwrap_or_default(),
        })
        .collect()
}

/// check_spec: the check's id, type, history and msg, and its model and initial value
/// (linearizable) or its consistency level (list-append), as compact JSON with sorted keys.
fn spec(c: &Value) -> String {
    let mut sp = Map::new();
    sp.insert("id".into(), c["id"].clone());
    sp.insert("type".into(), c["type"].clone());
    sp.insert("history".into(), c["history"].clone());
    sp.insert(
        "msg".into(),
        c.get("msg").cloned().unwrap_or_else(|| c["id"].clone()),
    );
    if c["type"].as_str() == Some("linearizable") {
        sp.insert(
            "model".into(),
            c.get("model").cloned().unwrap_or("register".into()),
        );
        sp.insert(
            "initial".into(),
            c.get("initial").cloned().unwrap_or("nil".into()),
        );
    } else {
        sp.insert(
            "consistency".into(),
            c.get("consistency")
                .cloned()
                .unwrap_or("strict-serializable".into()),
        );
    }
    let mut out = String::new();
    compact(&Value::Object(sp), &mut out);
    out
}

/// `json.dumps(v, sort_keys=True, separators=(",", ":"))` (ASCII only).
fn compact(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match n.as_f64().filter(|_| n.is_f64()) {
            Some(f) => out.push_str(&py::repr_float(f)),
            None => out.push_str(&n.to_string()),
        },
        Value::String(s) => {
            out.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\x08' => out.push_str("\\b"),
                    '\x0c' => out.push_str("\\f"),
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                        let mut buf = [0u16; 2];
                        for u in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{u:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                compact(x, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            let mut items: Vec<(&String, &Value)> = o.iter().collect();
            items.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (k, x)) in items.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                compact(&Value::String(k.clone()), out);
                out.push(':');
                compact(x, out);
            }
            out.push('}');
        }
    }
}

/// The host's part of `$H/hist.declared` (one conversation is one boot).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub declared: bool,
}

/// hist_declare, at the end of start_periodic: once per boot, one HYDO-CHECK line per check.
pub(super) fn declare(fx: &mut F<'_>, ctx: &Ctx) {
    if ctx.plan.history.is_empty() {
        return;
    }
    if fx.effect_bool(|h, _| std::mem::replace(&mut h.hist.declared, true)) {
        return;
    }
    for c in &ctx.plan.history {
        fx.report(format!("HYDO-CHECK {}", c.json));
    }
}

/// hist_final, at the start of finish: each check's `final:` read-back and its exit status.
pub(super) fn finals(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    for (i, c) in ctx.plan.history.iter().enumerate() {
        if c.final_.is_empty() {
            continue;
        }
        let e = env(fx, ctx, &[]);
        // `$BB timeout $T $BB sh -c ". prelude; $f" >> $H/workload.log 2>&1`
        let rc = spawn_wait(
            fx,
            &format!("history:c{i}"),
            timeout_argv(ctx, &c.final_),
            e,
            &format!("{}/workload.log", ctx.plan.layout.dir),
            true,
        )?;
        // AMBIGUOUS (as M120's readiness lines): the shell also logs its uptime here.
        log(fx, format!("check {}: final read rc={rc}", c.id));
        fx.report(format!(
            "HYDO-CHECK-FINAL {{\"id\":\"{}\",\"rc\":{rc}}}",
            c.id
        ));
    }
    Ok(())
}
