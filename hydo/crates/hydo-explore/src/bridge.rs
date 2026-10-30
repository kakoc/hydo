//! The seams to product code that is still Python while other ports land
//! (hydo/onboard/hydo_bridge.py): the history checkers (`hist_run_checks`, hydohist.py) and the
//! error-log analysis (`errors_load_known`, `errors_analyse`, hydo_errors.py). Each is called only
//! for a run whose console asks for it (`HYDO-CHECK` / `HYDO-ERRLOG-SUM` lines), so explorations
//! of images without history checks or error logs never start Python. When hydo-history and
//! hydo-errors are crates, these functions call them instead.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// `$HYDO_PYTHON`, else python3.
pub fn python() -> String {
    std::env::var("HYDO_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// Run one bridge function (`<onboard>/hydo_bridge.py <func> <request>`); its answer.
pub fn call(onboard: &Path, func: &str, args: Value) -> Result<Value, String> {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir();
    let base = format!("hydo-explore-bridge-{}-{}", std::process::id(), n);
    let req = dir.join(format!("{base}.req.json"));
    let res = dir.join(format!("{base}.res.json"));
    std::fs::write(
        &req,
        json!({"args": args, "result": res.display().to_string()}).to_string(),
    )
    .map_err(|e| format!("cannot write {}: {e}", req.display()))?;
    let script: PathBuf = onboard.join("hydo_bridge.py");
    let st = Command::new(python())
        .arg(&script)
        .arg(func)
        .arg(&req)
        .status();
    let _ = std::fs::remove_file(&req);
    let st = st.map_err(|e| format!("cannot run {} {}: {e}", python(), script.display()))?;
    let data = std::fs::read_to_string(&res)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let _ = std::fs::remove_file(&res);
    if !st.success() {
        return Err(format!("{} {func} failed ({st})", script.display()));
    }
    Ok(data.unwrap_or(Value::Null))
}
