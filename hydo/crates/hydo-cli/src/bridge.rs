//! The seams to the product code that is still Python (hydo/onboard/hydo_bridge.py).
//!
//! Each function here is one seam a port replaces: when the module behind it is a Rust crate, the
//! function's body becomes a call into that crate and the bridge function in Python is deleted.
//! Owners of the next ports (one crate each): hydo_agents.py (guide/skill/init), hydo_compose.py
//! (compose), hydohist.py (run_checks), hydo-build (build_linux, build_mac). Ported: hydo_errors.py
//! (the hydo-errors crate) and hydo_composer.py's regression seams (the hydo-composer crate).

use crate::ctx::{die, python, Ctx};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// Run one bridge function. Its own output goes to our stdout/stderr; returns (exit status, data).
pub fn call(ctx: &Ctx, func: &str, args: Value, env: &[(String, String)]) -> (i32, Value) {
    let dir = std::env::temp_dir();
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let base = format!("hydo-bridge-{}-{}", std::process::id(), n);
    let req = dir.join(format!("{base}.req.json"));
    let res = dir.join(format!("{base}.res.json"));
    let body = json!({"args": args, "result": res.display().to_string()});
    if let Err(e) = std::fs::write(&req, body.to_string()) {
        die(&format!("cannot write {}: {e}", req.display()));
    }
    let script = ctx.onboard().join("hydo_bridge.py");
    let mut c = Command::new(python());
    c.arg(&script).arg(func).arg(&req);
    for (k, v) in env {
        c.env(k, v);
    }
    let status = c.status();
    let _ = std::fs::remove_file(&req);
    let status = match status {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&res);
            die(&format!(
                "cannot run {} {}: {e} (set HYDO_PYTHON to a Python 3.9+)",
                python(),
                script.display()
            ))
        }
    };
    let data = std::fs::read_to_string(&res)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let _ = std::fs::remove_file(&res);
    (status.code().unwrap_or(1), data)
}

/// A bridge function that only returns an exit status (its output is the command's output).
pub fn rc(ctx: &Ctx, func: &str, args: Value, env: &[(String, String)]) -> i32 {
    call(ctx, func, args, env).0
}

/// hydo_agents.run(cmd, argv, root): `hydo guide | skill | init`.
pub fn agents(ctx: &Ctx, cmd: &str, argv: &[String]) -> i32 {
    rc(
        ctx,
        "agents",
        json!({"cmd": cmd, "argv": argv, "root": ctx.root.display().to_string()}),
        &[],
    )
}

/// hydohist.run_checks(srcs, verdict) -> {records, report, verdict}.
pub fn run_checks(ctx: &Ctx, srcs: &[(String, String)], verdict: Option<&str>) -> Value {
    let (code, v) = call(
        ctx,
        "hist_run_checks",
        json!({"srcs": srcs, "verdict": verdict}),
        &[],
    );
    if code != 0 {
        std::process::exit(code);
    }
    v
}
