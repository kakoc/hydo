//! The function contracts' plan next to an image (Milestone 241, docs/design/contracts.md):
//! `contracts:` names programs of `root.files` and the folders of their sources; the plan is
//! compiled from each program's debug information and its `// hydo:` comments and written to
//! `<image>.contracts.json`, which the engines follow. An image built without `contracts:` has
//! no plan (a stale one is removed), so nothing changes for it.

use crate::{outln, Fail, Io};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn expand(p: &str, dir: &Path) -> PathBuf {
    let p = if p == "~" || p.starts_with("~/") {
        match std::env::var("HOME") {
            Ok(h) => format!("{}{}", h.trim_end_matches('/'), &p[1..]),
            Err(_) => p.to_string(),
        }
    } else {
        p.to_string()
    };
    let pb = PathBuf::from(&p);
    if pb.is_absolute() {
        pb
    } else {
        dir.join(pb)
    }
}

/// Write (or remove) the plan of `image` built from manifest `m` at `manifest_path`.
pub fn write_plan(m: &Value, manifest_path: &str, image: &Path, io: &mut Io) -> Result<(), Fail> {
    let plan_path = hydo_contracts::plan::plan_path(image);
    let Some(list) = m.get("contracts").and_then(Value::as_array) else {
        let _ = std::fs::remove_file(&plan_path);
        return Ok(());
    };
    let mp = Path::new(manifest_path);
    let mdir = if mp.is_dir() {
        mp.to_path_buf()
    } else {
        mp.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let mdir = std::fs::canonicalize(&mdir).unwrap_or(mdir);
    let files: Vec<&Value> = m
        .get("root")
        .and_then(|r| r.get("files"))
        .and_then(Value::as_array)
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    let mut sources = Vec::new();
    for (i, c) in list.iter().enumerate() {
        let program = c.get("program").and_then(Value::as_str).unwrap_or("");
        let Some(src) = files
            .iter()
            .find(|f| f.get("dst").and_then(Value::as_str) == Some(program))
            .and_then(|f| f.get("src").and_then(Value::as_str))
        else {
            return Err(Fail::Manifest(format!(
                "contracts[{i}]: program {program} is not a dst of root.files (a contract reads the \
                 program's own file for its debug information)"
            )));
        };
        let dirs: Vec<PathBuf> = c
            .get("sources")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(|s| expand(s, &mdir))
                    .collect()
            })
            .unwrap_or_default();
        for d in &dirs {
            if !d.exists() {
                return Err(Fail::Manifest(format!(
                    "contracts[{i}]: sources: {} does not exist",
                    d.display()
                )));
            }
        }
        sources.push(hydo_contracts::compile::Source {
            program: program.to_string(),
            binary: expand(src, &mdir),
            sources: dirs,
            error_paths: c.get("error_paths").and_then(Value::as_bool) == Some(true),
            all_functions: c.get("all_functions").and_then(Value::as_bool) == Some(true),
        });
    }
    let plan = hydo_contracts::compile::compile(&sources)
        .map_err(|e| Fail::Manifest(format!("contracts: {e}")))?;
    std::fs::write(&plan_path, hydo_contracts::plan::plan_text(&plan))
        .map_err(|e| Fail::Crash(format!("{}: {e}", plan_path.display())))?;
    let n: usize = plan.programs.iter().map(|p| p.functions.len()).sum();
    let clauses: usize = plan
        .programs
        .iter()
        .flat_map(|p| &p.functions)
        .map(|f| f.clauses.len())
        .sum();
    // the values of `record` lines (Milestone 285); a plan without them prints as before
    let values: usize = plan
        .programs
        .iter()
        .flat_map(|p| &p.functions)
        .map(|f| f.values.iter().filter(|v| !v.key).count())
        .sum();
    let mut recorded = if values > 0 {
        format!(", {values} recorded values")
    } else {
        String::new()
    };
    // the `keep` lines (Milestone 291)
    let keeps: usize = plan.programs.iter().map(|p| p.keeps.len()).sum();
    if keeps > 0 {
        recorded.push_str(&format!(
            ", {keeps} value propert{}",
            if keeps == 1 { "y" } else { "ies" }
        ));
    }
    let paths_only = n == 0
        && plan
            .programs
            .iter()
            .any(|p| !p.paths.is_empty() || !p.fn_places.is_empty());
    outln(
        io,
        &if paths_only {
            let what = match (
                plan.programs.iter().any(|p| !p.paths.is_empty()),
                plan.programs.iter().any(|p| !p.fn_places.is_empty()),
            ) {
                (true, true) => "error paths and every function",
                (false, true) => "every function",
                _ => "error paths",
            };
            format!("hydo-build: {} ({what})", plan_path.display())
        } else {
            format!(
                "hydo-build: {} (function contracts: {n} functions, {clauses} clauses{recorded})",
                plan_path.display()
            )
        },
    );
    // the error paths (Milestone 306); a plan without them prints as before
    for p in plan.programs.iter().filter(|p| !p.paths.is_empty()) {
        let sites: usize = p.paths.iter().map(|x| x.sites.len()).sum();
        let mut line = format!(
            "hydo-build: error paths of {}: {} places at {sites} addresses",
            p.program,
            p.paths.len()
        );
        if p.paths_without_code > 0 {
            line.push_str(&format!(
                "; {} more places of the sources have no code in the program",
                p.paths_without_code
            ));
        }
        outln(io, &line);
    }
    // every function (Milestone 394, the live map); a plan without them prints as before
    for p in plan.programs.iter().filter(|p| !p.fn_places.is_empty()) {
        let own = p.fn_places.iter().filter(|x| x.own).count();
        let bytes: u64 = p.fn_places.iter().map(|x| x.size).sum();
        outln(
            io,
            &format!(
                "hydo-build: functions of {}: {} functions with code of their own ({own} in the \
                 sources, {} KiB of code), on {} pages of the program's file",
                p.program,
                p.fn_places.len(),
                bytes / 1024,
                p.path_pages.len()
            ),
        );
    }
    for p in &plan.programs {
        for u in &p.unchecked {
            outln(
                io,
                &format!(
                    "hydo-build: warning: the contract of {} ({}:{}) is not checked: {}",
                    u.function, u.file, u.line, u.reason
                ),
            );
        }
    }
    Ok(())
}
