//! The test composer's plan copied into the root (port of hydo_composer.py's `stage`): the hydo/
//! folder (whole, helpers included) under /hydo/, the command and check lists the guest composer
//! reads, the plan for the explorer (compose.json), the saved compositions of regressions, the
//! plan's own regressions as files, and hydo-composer.sh as /etc/hydo/compose.sh.

use crate::fsx;
use crate::{pyjson, Ctx, Fail};
use hydo_manifest::composer::{key_of, shell_of};
use hydo_manifest::py;
use serde_json::{Map, Value};
use std::path::Path;

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
pub fn compose_stage(ctx: &Ctx, m: &Value, root: &Path) -> Result<(), Fail> {
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
    let dst = root.join("etc/hydo/compose.sh");
    fsx::copy_file(&ctx.onboard.join("hydo-composer.sh"), &dst)?;
    fsx::chmod(&dst, 0o755)
}
