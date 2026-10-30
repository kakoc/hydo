//! The `hydo-check` command line (hydo/history/hydo-check): run the host-side history checks of
//! one run from its saved consoles. The same arguments, output and exit status; available as
//! `hydo check-history` and as the `hydo-hist-check` binary of this crate.

use crate::{py, read_console, run_checks, PyVal, DEFAULT_BUDGET};
use std::io::Write;
use std::path::Path;

/// The usage text (the Python script's docstring, printed by `-h`).
pub const USAGE: &str = r#"hydo-check -- run the host-side history checks of one run (hydo/HISTORIES.md).

    hydo-check <console> [<recovery console>]         saved consoles, in run order
    hydo-check <run dir>                              a `hydo run` / `hvx_explore one` output dir:
                                                      one.log (+ one.recovery.log)
    hydo-check --tag T <consoles dir>                 explore.py's consoles/<T>.log (+ .recovery.log)
  options:
    --check '<json>'      a check spec, instead of the HYDO-CHECK lines in the console (repeatable),
                          e.g. '{"id":"kv","type":"linearizable","model":"register","history":"kv"}'
    --verdict '<line>'    the guest's verdict (default: the last HYDO-ORACLE line of the consoles)
    --report <file>       also write the full report (explanations) there
    --budget <n>          linearizability search steps per key (default 2000000)

Prints the HYDO-ASSERT records, the report, and the combined verdict as the last line.
Exit status: 0 OK, 1 VIOLATION, 2 ERROR (undecided check, bad history, guest error), 3 no checks.
"#;

/// What stopped the command before a verdict: the message and the exit status (1, as the
/// Python script's exceptions and SystemExit gave).
fn fail(msg: String) -> i32 {
    eprintln!("{msg}");
    1
}

fn sources_for(files: &[String], tag: Option<&str>) -> Result<Vec<(String, String)>, String> {
    if files.len() == 1 && Path::new(&files[0]).is_dir() {
        let d = Path::new(&files[0]);
        let t = tag.unwrap_or("one");
        let mut out = Vec::new();
        for suffix in ["", ".recovery"] {
            let p = d.join(format!("{t}{suffix}.log"));
            if p.exists() {
                let text = read_console(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                out.push((format!("{t}{suffix}"), text));
            }
        }
        if out.is_empty() {
            return Err(format!("hydo-check: no {t}.log in {}", files[0]));
        }
        return Ok(out);
    }
    files
        .iter()
        .map(|p| {
            // os.path.basename: the part after the last '/'
            let base = p.rsplit('/').next().unwrap_or_default().to_string();
            let label = match base.strip_suffix(".log") {
                Some(b) => b.to_string(),
                None => base,
            };
            let text = read_console(Path::new(p)).map_err(|e| format!("{p}: {e}"))?;
            Ok((label, text))
        })
        .collect()
}

/// The last HYDO-ORACLE line of the consoles (from the marker on, stripped).
pub fn guest_verdict<S: AsRef<str>, T: AsRef<str>>(srcs: &[(S, T)]) -> Option<String> {
    let mut v = None;
    for (_, text) in srcs {
        for line in py::splitlines(text.as_ref()) {
            if let Some(k) = line.find("HYDO-ORACLE:") {
                v = Some(py::strip(&line[k..]).to_string());
            }
        }
    }
    v
}

/// Run the command; returns the exit status (0 OK, 1 VIOLATION, 2 ERROR, 3 no checks).
pub fn main(argv: &[String]) -> i32 {
    if argv.is_empty() || matches!(argv[0].as_str(), "-h" | "--help") {
        println!("{USAGE}");
        return 0;
    }
    let mut specs: Vec<PyVal> = Vec::new();
    let mut verdict: Option<String> = None;
    let mut report: Option<String> = None;
    let mut tag: Option<String> = None;
    let mut budget = DEFAULT_BUDGET;
    let mut files = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        let val = || argv.get(i + 1).cloned();
        let need =
            |v: Option<String>| v.ok_or_else(|| "IndexError: list index out of range".to_string());
        match a {
            "--check" | "--verdict" | "--report" | "--tag" | "--budget" => {
                let v = match need(val()) {
                    Ok(v) => v,
                    Err(e) => return fail(e),
                };
                match a {
                    "--check" => match py::json_loads(&v) {
                        Ok(s) => specs.push(s),
                        Err(py::JsonError) => {
                            return fail(format!("json.decoder.JSONDecodeError: {v}"))
                        }
                    },
                    "--verdict" => verdict = Some(v),
                    "--report" => report = Some(v),
                    "--tag" => tag = Some(v),
                    _ => match py::parse_int(&v) {
                        Some(b) => budget = b.saturating_i64(),
                        None => {
                            return fail(format!(
                                "ValueError: invalid literal for int() with base 10: {}",
                                py::repr_str(&v)
                            ))
                        }
                    },
                }
                i += 2;
            }
            _ => {
                files.push(argv[i].clone());
                i += 1;
            }
        }
    }
    let srcs = match sources_for(&files, tag.as_deref()) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    if verdict.is_none() {
        verdict = guest_verdict(&srcs);
    }
    let r = match run_checks(
        &srcs,
        verdict.as_deref(),
        budget,
        (!specs.is_empty()).then_some(specs.as_slice()),
    ) {
        Ok(r) => r,
        Err(c) => return fail(format!("hydo-check: {c}")),
    };
    let Some(v) = r.verdict.clone() else {
        println!("hydo-check: no HYDO-CHECK declarations in the console (and no --check)");
        return 3;
    };
    let mut out = String::new();
    for rec in &r.records {
        out.push_str(rec);
        out.push('\n');
    }
    out.push_str(&r.report);
    out.push('\n');
    for w in r.warnings.iter().take(20) {
        out.push_str("warning: ");
        out.push_str(w);
        out.push('\n');
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(out.as_bytes());
    let _ = lock.flush();
    if let Some(p) = report {
        if let Err(e) = std::fs::write(&p, format!("{}\n{v}\n", r.report)) {
            return fail(format!("{p}: {e}"));
        }
    }
    let _ = writeln!(lock, "{v}");
    if v.contains("HYDO-ORACLE: OK") {
        0
    } else if v.contains("VIOLATION") {
        1
    } else {
        2
    }
}
