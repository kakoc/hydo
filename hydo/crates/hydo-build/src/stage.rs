//! The test composer's plan copied into the root (port of hydo_composer.py's `stage`): the hydo/
//! folder (whole, helpers included) under /hydo/, the command and check lists the guest composer
//! reads, the plan for the explorer (compose.json), the saved compositions of regressions, the
//! plan's own regressions as files. (hydo-composer.sh, the guest composer of the shell-init
//! images, was staged as /etc/hydo/compose.sh until M136; the host controller runs the rounds.)
//!
//! From the fingerprint epoch 4 (Milestone 269) the saved regressions stay out of the image:
//! [`take_regressions`] takes them out of the manifest before anything is staged, so the image of
//! a project with regressions is byte for byte the image of the same project without them, and
//! [`write_regressions`] writes them beside the image (`<image>.regressions.json`), where the host
//! reads them. Before, saving or editing one regression, even a comment, moved the timing of
//! every run of the image (docs/HISTORY.md Milestones 251 and 268).

use crate::fsx;
use crate::{outln, pyjson, Fail, Io};
use hydo_manifest::composer::{key_of, shell_of};
use hydo_manifest::py;
use serde_json::{Map, Value};
use std::path::Path;

/// Take the plan's saved regressions out of the manifest `m` that the build stages, and return
/// them (the plan meta's entries, `test.regressions` and the folder's `regressions/*.schedule`).
/// What stays is what the same project gives without any regression: the plan meta's
/// `regressions` is empty, the folder's `regressions/` is not copied, and a plan of only
/// regressions is no plan (no `/etc/hydo/compose.json`, no composer lines in manifest.sh).
/// `test.regressions` leaves the validated manifest too, so no image decision reads it.
pub fn take_regressions(m: &mut Value) -> Vec<Value> {
    if let Some(t) = m.get_mut("test").and_then(Value::as_object_mut) {
        t.remove("regressions");
        // merge_folder adds an empty `test:` for a folder of only regressions/
        if t.is_empty() {
            if let Some(o) = m.as_object_mut() {
                o.remove("test");
            }
        }
    }
    let Some(c) = m.get_mut("_compose").filter(|c| py::truthy(c)) else {
        return Vec::new();
    };
    let regs = match c.get_mut("regressions") {
        Some(r) => std::mem::replace(r, Value::Array(Vec::new())),
        None => Value::Array(Vec::new()),
    };
    if let Some(f) = c.get_mut("_folder").filter(|f| f.is_object()) {
        f["regressions"] = Value::Array(Vec::new());
        let empty = match f.get_mut("_dirs").and_then(Value::as_object_mut) {
            Some(dirs) => {
                dirs.remove("regressions");
                dirs.is_empty()
            }
            None => true,
        };
        // scan_folder: a folder with none of workload/, checks/, regressions/ is no folder
        if empty {
            *f = Value::Null;
        }
    }
    // expand: a plan without commands and checks is a plan only when it has regressions
    if list(c, "commands").is_empty() && list(c, "checks").is_empty() {
        if let Some(o) = m.as_object_mut() {
            o.remove("_compose");
        }
    }
    match regs {
        Value::Array(a) => a,
        _ => Vec::new(),
    }
}

/// Write the saved regressions beside the finished `image` (`<image>.regressions.json`), or
/// remove a stale file when the plan has none.
pub fn write_regressions(image: &Path, regs: &[Value], io: &mut Io) -> Result<(), Fail> {
    let p = hydo_image::regressions_path(image);
    if regs.is_empty() {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    }
    fsx::write_text(&p, &hydo_image::regressions_text(regs))?;
    outln(
        io,
        &format!(
            "hydo-build: {} ({} saved regression{}: the host replays {} with `hydo repro --regressions`; the image does not carry {})",
            p.display(),
            regs.len(),
            if regs.len() == 1 { "" } else { "s" },
            if regs.len() == 1 { "it" } else { "them" },
            if regs.len() == 1 { "it" } else { "them" },
        ),
    );
    Ok(())
}

/// `w_of(schedule)`: the W= value of a schedule's guest part.
fn w_of(schedule: &str) -> Option<String> {
    let g = schedule.split_once('|').map(|x| x.1).unwrap_or(schedule);
    py::split_ws(g)
        .into_iter()
        .find_map(|t| t.strip_prefix("W=").map(str::to_string))
}

fn list<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or(&[])
}

fn d(v: &Value, what: &str) -> Result<String, Fail> {
    crate::pyv::pct_d(v, what)
}

/// `_compose_stage(m, R)`.
pub fn compose_stage(m: &Value, root: &Path) -> Result<(), Fail> {
    let Some(full) = m.get("_compose").filter(|c| py::truthy(c)) else {
        return Ok(());
    };
    let folder = full.get("_folder").filter(|f| py::truthy(f));
    let mut meta = Map::new();
    if let Some(o) = full.as_object() {
        for (k, v) in o {
            if k != "_folder" {
                meta.insert(k.clone(), v.clone());
            }
        }
    }
    let meta = Value::Object(meta);
    if let Some(f) = folder {
        let mut dirs: Vec<(String, String)> = f
            .get("_dirs")
            .and_then(Value::as_object)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), py::str_of(v))).collect())
            .unwrap_or_default();
        dirs.sort();
        for (sub, src) in dirs {
            let dst = root.join("hydo").join(&sub);
            if dst.is_dir() {
                std::fs::remove_dir_all(&dst)
                    .map_err(|e| fsx::io_fail("cannot remove", &dst, e))?;
            }
            fsx::copytree(Path::new(&src), &dst)?;
        }
    }
    let eh = root.join("etc/hydo");
    fsx::makedirs(&eh)?;
    let mut t = String::new();
    for (i, c) in list(&meta, "commands").iter().enumerate() {
        t.push_str(&format!(
            "{} {} {} {} {}\n",
            i,
            py::str_of(&c["kind"]),
            d(&c["timeout"], "test.commands.timeout")?,
            py::str_of(&c["id"]),
            shell_of(&c["run"]).replace('\n', " ")
        ));
    }
    fsx::write_text(&eh.join("compose.list"), &t)?;
    let ct = d(&meta["params"]["command_timeout"], "test.command_timeout")?;
    let mut t = String::new();
    for (i, c) in list(&meta, "checks").iter().enumerate() {
        t.push_str(&format!(
            "{} {} {} {} {}\n",
            i,
            py::str_of(&c["kind"]),
            ct,
            py::str_of(&c["name"]),
            shell_of(&c["run"]).replace('\n', " ")
        ));
    }
    fsx::write_text(&eh.join("compose.checks"), &t)?;
    fsx::write_text(
        &eh.join("compose.json"),
        &(pyjson::dumps_indent1_sorted(&meta) + "\n"),
    )?;
    let regs = list(&meta, "regressions");
    let mut saved: Vec<(String, String)> = Vec::new();
    for r in regs {
        let w = w_of(&py::str_of(&r["schedule"]));
        let comp = r.get("compose").filter(|c| py::truthy(c)).map(py::str_of);
        if let (Some(w), Some(c)) = (w, comp) {
            if let Some(k) = w.strip_prefix('@') {
                if key_of(&c) == k {
                    if let Some(e) = saved.iter_mut().find(|(x, _)| x == k) {
                        e.1 = c;
                    } else {
                        saved.push((k.to_string(), c));
                    }
                }
            }
        }
    }
    if !saved.is_empty() {
        let dd = root.join("etc/hydo/compose.saved");
        fsx::makedirs(&dd)?;
        saved.sort();
        for (k, c) in saved {
            fsx::write_text(&dd.join(k), &format!("{c}\n"))?;
        }
    }
    if !regs.is_empty() {
        let dd = root.join("hydo").join("regressions");
        fsx::makedirs(&dd)?;
        for r in regs {
            let p = dd.join(format!("{}.schedule", py::str_of(&r["name"])));
            if p.exists() {
                continue;
            }
            let mut t = String::new();
            if let Some(cmds) = r.get("commands").filter(|c| !c.is_null()) {
                t.push_str(&format!(
                    "# commands: {}\n",
                    crate::pyv::join_strs(cmds, " ", "regressions.commands")?
                ));
            }
            if let Some(c) = r.get("compose").filter(|c| py::truthy(c)) {
                t.push_str(&format!("# compose: {}\n", py::str_of(c)));
            }
            if let Some(Value::Object(o)) = r.get("contents").filter(|c| py::truthy(c)) {
                let mut kv: Vec<(&String, &Value)> = o.iter().collect();
                kv.sort_by(|a, b| a.0.cmp(b.0));
                for (k, v) in kv {
                    t.push_str(&format!("# command-sha: {} {}\n", k, py::str_of(v)));
                }
            }
            let sched = r
                .get("saved_schedule")
                .filter(|x| py::truthy(x))
                .unwrap_or(&r["schedule"]);
            t.push_str(&format!(
                "# expect: {}\n{}\n",
                py::str_of(&r["expect"]),
                py::str_of(sched)
            ));
            fsx::write_text(&p, &t)?;
        }
    }
    // The guest composer (hydo-composer.sh as /etc/hydo/compose.sh) was staged here until M136;
    // the host controller drives the rounds itself (hydo-control schedule/composer.rs).
    Ok(())
}
