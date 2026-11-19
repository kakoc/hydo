//! Why a harness does not build, in the user's terms (Milestone 326). A harness module is one
//! line that hydo writes into the copy, so the compiler's line and column mean nothing to the
//! user. This module names the error, the contract line or the part of the function that
//! caused it, and the compiler's own help, and it keeps the whole output of the failed build
//! apart for each function, where `--show-harness` prints it.

use super::cargo::{narrow, Diag};
use super::{Fn, OriginKind, Placed};
use crate::fuzz::rust::{Closure, FTy, Pass};
use std::path::{Path, PathBuf};

/// The character columns of the error's place inside the module `pl`, as indices into the
/// module's text: the primary place when it is inside, else the first place that is.
pub(crate) fn in_module(d: &Diag, pl: &Placed, copy: &Path) -> Option<(usize, usize)> {
    let inside = |file: &str, line: u32, col: u32| {
        let p = Path::new(file);
        let rel = p.strip_prefix(copy).unwrap_or(p);
        rel == pl.file && line == pl.line && (pl.cols.0..=pl.cols.1).contains(&col)
    };
    let at = |c: u32| (c - pl.cols.0) as usize;
    if let Some(s) = &d.primary {
        if inside(&s.file, s.line, s.cols.0) {
            let end = s.cols.1.clamp(s.cols.0, pl.cols.1);
            return Some((at(s.cols.0), at(end)));
        }
    }
    d.spans
        .iter()
        .find(|(f, l, c)| inside(f, *l, *c))
        .map(|(_, _, c)| (at(*c), at(*c) + 1))
}

/// The file that keeps the whole output of a function's failed harness, and its module's
/// text (`ext` is `log` or `rs`): `<out>/harness/<the function's name>.<ext>`.
pub(crate) fn harness_file(log: &Path, f: &Fn, ext: &str) -> PathBuf {
    let name: String = f
        .short_name()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    log.with_file_name("harness").join(format!("{name}.{ext}"))
}

/// The end (exclusive) of the bracketed group that opens at `open`.
fn close(t: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in t.iter().enumerate().skip(open) {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every place where `pat` stands in `t`.
fn find_all(t: &[char], pat: &str) -> Vec<usize> {
    let p: Vec<char> = pat.chars().collect();
    if p.is_empty() || p.len() > t.len() {
        return vec![];
    }
    (0..=t.len() - p.len())
        .filter(|&i| t[i..i + p.len()] == p[..])
        .collect()
}

/// How the harness passes a parameter, in words.
fn passed(p: &super::harness::Param) -> String {
    let shown = match &p.ty {
        FTy::Built { build: Some(c), .. } if Closure::of(&c.path).is_some() => {
            "a closure that hydo generates".to_string()
        }
        FTy::Built { build: Some(c), .. } => format!("a value built by `{}(...)`", c.path),
        t => format!("a generated `{}`", t.rust()),
    };
    let how = if p.leak {
        match p.pass {
            Pass::RefMut { .. } => " as a `&'static mut` reference to a leaked value",
            _ => " as a `&'static` reference to a leaked value",
        }
    } else {
        match p.pass {
            Pass::Value => "",
            Pass::Ref { .. } => " by reference (`&`)",
            Pass::RefMut { .. } => " by a mutable reference (`&mut`)",
        }
    };
    format!("{shown}{how}")
}

/// The part of the user's contract or function that the harness text at `[s, e)` comes from.
fn cause(f: &Fn, text: &str, (s, e): (usize, usize)) -> String {
    let t: Vec<char> = text.chars().collect();
    let snippet: String = t[s.min(t.len())..e.min(t.len())].iter().collect();
    let within = |open: usize| close(&t, open).is_some_and(|end| (open..end).contains(&s));
    // a `read` line: the method that reads the value
    for o in &f.origins {
        if let OriginKind::Read(m) = &o.kind {
            if m.is_empty() {
                continue;
            }
            let call = format!(".{m}(");
            let fish = format!(".{m}::<");
            let hit = snippet == *m
                || snippet.contains(&call)
                || snippet.contains(&fish)
                || find_all(&t, &call)
                    .into_iter()
                    .chain(find_all(&t, &fish))
                    .any(|i| (i..i + call.len() + 2).contains(&s));
            if hit {
                return format!(
                    "{}: the harness reads the value by calling `{m}()` on it",
                    o.show()
                );
            }
        }
    }
    // a `build` line: the constructor's call, its arguments included
    for o in &f.origins {
        if let OriginKind::Build(c) = &o.kind {
            if c.is_empty() {
                continue;
            }
            let hit = snippet.contains(c.as_str())
                || find_all(&t, &format!("{c}("))
                    .into_iter()
                    .any(|i| within(i + c.chars().count()) || (i..i + c.len()).contains(&s));
            if hit {
                return format!(
                    "{}: the harness builds the value by calling `{c}(...)` with the \
                     arguments that the line names",
                    o.show()
                );
            }
        }
    }
    // a generated closure
    for i in find_all(&t, "{ let s: usize = r.int(); let k:") {
        if within(i) {
            let p = f
                .params
                .iter()
                .find(|p| matches!(&p.ty, FTy::Built { build: Some(c), .. } if Closure::of(&c.path).is_some()));
            return format!(
                "the closure that hydo generates for the parameter `{}`: write the contract on a \
                 property function in the crate's test module that calls the function with a \
                 closure of your own",
                p.map_or("?", |p| p.name.as_str())
            );
        }
    }
    // the call: its path, or one of its arguments
    let call = format!("{}(", f.call);
    for i in find_all(&t, &call) {
        let open = i + call.chars().count() - 1;
        let Some(end) = close(&t, open) else { continue };
        if (i..open).contains(&s) {
            let with: Vec<String> = f
                .origins
                .iter()
                .filter(|o| matches!(o.kind, OriginKind::With(_)) && o.file == f.file)
                .map(|o| format!("`// hydo: {}` (line {})", o.text, o.line))
                .collect();
            return if with.is_empty() {
                format!("the call of `{}` ({}:{})", f.name, f.file.display(), f.line)
            } else {
                format!(
                    "the call of `{}` with the types of the lines {} ({}:{}): a type that a \
                     `with` line names must fit the function's bounds",
                    f.name,
                    with.join(", "),
                    f.file.display(),
                    f.line
                )
            };
        }
        if (open..end).contains(&s) {
            // which argument: the top-level commas before the place
            let mut depth = 0i32;
            let mut n = 0;
            for c in &t[open + 1..s] {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    ',' if depth == 0 => n += 1,
                    _ => {}
                }
            }
            if let Some(p) = f.params.get(n) {
                return format!(
                    "the parameter `{}` of `{}` ({}:{}), which the harness passes as {}",
                    p.name,
                    f.name,
                    f.file.display(),
                    f.line,
                    passed(p)
                );
            }
        }
    }
    // the generated value of a parameter
    for (i, p) in f.params.iter().enumerate() {
        for a in [format!("let mut a{i}: "), format!("let l{i}: ")] {
            if find_all(&t, &a).into_iter().any(|at| {
                let rest = &t[at..];
                let mut depth = 0i32;
                let end = rest
                    .iter()
                    .position(|c| {
                        match c {
                            '(' | '[' | '{' => depth += 1,
                            ')' | ']' | '}' => depth -= 1,
                            ';' if depth == 0 => return true,
                            _ => {}
                        }
                        false
                    })
                    .map_or(t.len(), |x| at + x);
                (at..end).contains(&s)
            }) {
                return format!(
                    "the value of the parameter `{}` of `{}` ({}:{}): the harness makes {}",
                    p.name,
                    f.name,
                    f.file.display(),
                    f.line,
                    passed(p)
                );
            }
        }
    }
    format!(
        "hydo's harness of `{}` ({}:{}), at `{}`: a mistake of hydo's, which its maintainers \
         want to hear about with the output that --show-harness prints",
        f.name,
        f.file.display(),
        f.line,
        snippet.trim()
    )
}

/// A place in the copy as the user's file names it: the copy's path made the package's, and
/// the line, which the harness does not move (each module is on one line).
fn user_place(file: &str, line: u32, copy: &Path) -> String {
    let p = Path::new(file);
    let rel = p.strip_prefix(copy).unwrap_or(p);
    format!("{}:{line}", rel.display())
}

/// One error as the user reads it: its first line, the cause, its labels and the compiler's
/// help.
fn describe(f: &Fn, d: &Diag, pl: Option<&Placed>, copy: &Path) -> Vec<String> {
    let mut out = vec![d.message.clone()];
    match pl.and_then(|pl| Some((pl, in_module(d, pl, copy)?))) {
        Some((pl, at)) => {
            out.push(format!("caused by {}", cause(f, &pl.text, at)));
            for (s, l) in &d.labels {
                let text: String = pl
                    .text
                    .chars()
                    .skip((s.cols.0.saturating_sub(pl.cols.0)) as usize)
                    .take((s.cols.1.saturating_sub(s.cols.0)) as usize)
                    .collect();
                let inside = s.line == pl.line && (pl.cols.0..=pl.cols.1).contains(&s.cols.0);
                if inside && !text.trim().is_empty() && text.chars().count() <= 80 {
                    out.push(format!("at `{}`: {l}", text.trim()));
                } else if !inside {
                    out.push(format!("at {}: {l}", user_place(&s.file, s.line, copy)));
                } else {
                    out.push(l.clone());
                }
            }
        }
        None => {
            if let Some(s) = &d.primary {
                out.push(format!(
                    "at {}, outside the harness",
                    user_place(&s.file, s.line, copy)
                ));
            }
            for (_, l) in &d.labels {
                out.push(l.clone());
            }
        }
    }
    out.extend(d.notes.iter().cloned());
    out
}

/// The text after `its harness did not build: ` for a function, and the files that keep the
/// failed build's whole output (`<out>/harness/<name>.log`) and the harness's text.
pub(crate) fn failed(
    f: &Fn,
    ds: &[Diag],
    fallback: &str,
    pl: Option<&Placed>,
    copy: &Path,
    log: &Path,
) -> String {
    let file = harness_file(log, f, "log");
    let mut body = String::new();
    // the errors inside the function's module first, then the others
    let mut order: Vec<&Diag> = ds
        .iter()
        .filter(|d| pl.is_some_and(|p| in_module(d, p, copy).is_some()))
        .collect();
    order.extend(
        ds.iter()
            .filter(|d| pl.is_none_or(|p| in_module(d, p, copy).is_none())),
    );
    // one error at several places of the harness (the values before and after the call) is
    // one error
    let mut seen: Vec<&str> = Vec::new();
    order.retain(|d| {
        let new = !seen.contains(&d.message.as_str());
        seen.push(&d.message);
        new
    });
    for d in &order {
        for (i, l) in describe(f, d, pl, copy).iter().enumerate() {
            body.push_str(if i == 0 { "" } else { "  " });
            body.push_str(l);
            body.push('\n');
        }
        body.push('\n');
    }
    if ds.is_empty() {
        body.push_str(fallback);
        body.push('\n');
    }
    body.push_str("The compiler's output:\n\n");
    for d in &order {
        body.push_str(&narrow(&d.rendered));
    }
    if let Some(pl) = pl {
        let rs = harness_file(log, f, "rs");
        body.push_str(&format!(
            "\nThe harness of `{}` (written after line {} of {} in the copy) is {}.\n",
            f.name,
            pl.line,
            pl.file.display(),
            rs.display()
        ));
        if let Some(d) = rs.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&rs, formatted(&pl.text));
    }
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&file, &body);
    let mut msg = match order.first() {
        Some(d) => describe(f, d, pl, copy).join("\n    "),
        None => fallback.lines().next().unwrap_or("").to_string(),
    };
    if order.len() > 1 {
        msg.push_str(&format!(
            "\n    and {} more error{}",
            order.len() - 1,
            if order.len() == 2 { "" } else { "s" }
        ));
    }
    msg.push_str(&format!(
        "\n    the compiler's whole output: {} (--show-harness prints it)",
        file.display()
    ));
    msg
}

/// A module's text formatted by rustfmt when it is on PATH, else as it is.
fn formatted(text: &str) -> String {
    let run = || -> Option<String> {
        use std::io::Write as _;
        let mut c = std::process::Command::new(super::cargo::rustfmt())
            .args(["--edition", "2021", "--emit", "stdout"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        c.stdin.take()?.write_all(text.trim().as_bytes()).ok()?;
        let out = c.wait_with_output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).to_string())
    };
    run().unwrap_or_else(|| format!("{}\n", text.trim()))
}
