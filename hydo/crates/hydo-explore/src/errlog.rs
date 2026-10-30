//! Error logs as findings (M94): a run whose console carries `HYDO-ERRLOG` records (an image whose
//! manifest has `errors:`) has its error kinds compared with the reviewed list of known kinds. The
//! analysis, the aggregation and the report section are the hydo-errors crate (M104); this module
//! adds what only the explorer does with them: which new kinds it shrinks.

pub use hydo_errors::{Aggregate, Analysis, KnownList as Known};
use std::path::Path;

/// `--known-errors` (None: no list, every kind is new).
pub fn load(path: Option<&str>) -> Result<Known, String> {
    match path {
        None => Ok(Known::empty()),
        Some(p) => hydo_errors::load_known(Some(p)).map_err(|e| e.0),
    }
}

/// One run's analysis from its consoles `<prefix>.log` (+ `.recovery.log`); None without `errors:`.
pub fn analyse(prefix: &Path, schedule: &str, known: &Known) -> Option<Analysis> {
    hydo_errors::analyse(&hydo_errors::run_files(prefix), schedule, known)
}

/// The new kinds the explorer shrinks (explore.py `errlog_shrink`): not written before the
/// workload (`s`: boot, provisioning — in every schedule), in the order of (first seed, signature).
pub fn to_shrink(agg: &[Aggregate]) -> Vec<Aggregate> {
    let mut new: Vec<Aggregate> = agg
        .iter()
        .filter(|a| a.new_runs > 0)
        .filter(|a| {
            a.first_new
                .as_ref()
                .is_some_and(|f| !f.2.iter().any(|l| l == "s"))
        })
        .cloned()
        .collect();
    new.sort_by_key(|a| {
        (
            a.first_new.as_ref().and_then(|f| f.0.as_i64()).unwrap_or(0),
            a.sig.clone(),
        )
    });
    new
}
