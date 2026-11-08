//! `hydo explain <image> "<schedule>"` and `hydo explain <run dir>` (Milestone 165,
//! docs/design/explain.md): runs a failing schedule and its neighbours (the schedule with one
//! guest token removed, with one fault moved by one step, without its disk faults, the same
//! schedule on another image with `--with-image`, or schedules given with `--with`), diffs the
//! failing run against each, and shows the diff against the passing neighbour that stays the
//! same for longest.

use crate::ctx::{abspath, die, take, Ctx};
use crate::diff;
use crate::runs::{find_image, run_to_dir};
use hydo_engine::image::sha256_file;
use hydo_results::{Diff, Run};
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};

/// The guest tokens whose step a neighbour moves by one (the tokens with a step or a window).
const MOVABLE: [&str; 8] = ["K", "P", "X", "N", "O", "C", "T", "H"];

/// `tok` with its step moved by `by`: `K=redis@11+1` → `K=redis@12+1`, `C=9` → `C=10`,
/// `N=redis@3-6:delay` → `N=redis@4-7:delay`. None when the token has no step or the step would
/// go below its smallest value.
pub fn shifted(tok: &str, by: i64) -> Option<String> {
    let (k, v) = tok.split_once('=')?;
    if !MOVABLE.contains(&k) {
        return None;
    }
    let digits = |s: &str| s.chars().take_while(char::is_ascii_digit).count();
    let (head, rest) = match v.find('@') {
        Some(i) => (&v[..=i], &v[i + 1..]),
        None => ("", v),
    };
    let n = digits(rest);
    if n == 0 {
        return None;
    }
    let first: i64 = rest[..n].parse().ok()?;
    let low = if k == "C" { 1 } else { 0 };
    let a = first + by;
    if a < low {
        return None;
    }
    let mut out = format!("{k}={head}{a}");
    let after = &rest[n..];
    // a window `a-b`: both ends move
    if let Some(r) = after.strip_prefix('-') {
        let m = digits(r);
        if m > 0 {
            let second: i64 = r[..m].parse().ok()?;
            out.push_str(&format!("-{}{}", second + by, &r[m..]));
            return Some(out);
        }
    }
    out.push_str(after);
    Some(out)
}

/// The neighbours of a schedule, as (how it was made, the schedule): one guest token removed,
/// one fault moved one step earlier or later, the disk faults off. Only schedules that parse and
/// differ from the given one, each once.
pub fn neighbours(sched: &str) -> Vec<(String, String)> {
    let Some((host, guest)) = sched.split_once('|') else {
        return Vec::new();
    };
    let words: Vec<&str> = guest.split_whitespace().collect();
    let (verb, toks) = match words.split_first() {
        Some((v, t)) => (*v, t.to_vec()),
        None => return Vec::new(),
    };
    let host = host.trim();
    let make = |h: &str, t: &[String]| {
        let mut s = format!("{h} | {verb}");
        for x in t {
            s.push(' ');
            s.push_str(x);
        }
        s
    };
    let toks: Vec<String> = toks.iter().map(|s| s.to_string()).collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for i in 0..toks.len() {
        let mut t = toks.clone();
        t.remove(i);
        out.push((format!("without {}", toks[i]), make(host, &t)));
    }
    for i in 0..toks.len() {
        for by in [-1, 1] {
            if let Some(n) = shifted(&toks[i], by) {
                let mut t = toks.clone();
                t[i] = n.clone();
                out.push((format!("with {} moved to {n}", toks[i]), make(host, &t)));
            }
        }
    }
    for w in host.split_whitespace() {
        if w.starts_with("disk=") && w != "disk=none" {
            let h: Vec<&str> = host
                .split_whitespace()
                .map(|x| if x == w { "disk=none" } else { x })
                .collect();
            out.push((
                format!("with disk=none instead of {w}"),
                make(&h.join(" "), &toks),
            ));
        }
    }
    let mut seen = vec![sched.trim().to_string()];
    out.retain(|(_, s)| {
        if seen.contains(s) || Schedule::parse(s).is_err() {
            return false;
        }
        seen.push(s.clone());
        true
    });
    out
}

/// How far two runs go together: the conversation and the stop of the first difference (a
/// later first difference is a closer neighbour).
fn reach(d: &Diff) -> (usize, u64) {
    match &d.first {
        None => (usize::MAX, u64::MAX),
        Some(f) => (
            diff::CONVERSATIONS
                .iter()
                .position(|c| *c == f.conversation)
                .unwrap_or(0),
            f.a.as_ref().and_then(|p| p.stop).unwrap_or(0),
        ),
    }
}

const USAGE: &str = "usage: hydo explain [IMG] \"<schedule>\" [--image IMG] [--out DIR]
                    [--with \"<schedule>\"]... [--with-image IMG2] [--max N]
       hydo explain <run dir> [--out DIR] [...]
    runs a failing schedule (or takes a run directory's) and its neighbours: the schedule with one
    guest token removed, with one fault moved one step, without its disk faults (--max N runs the
    first N of them), the same schedule on IMG2 (--with-image) and any --with schedule; diffs the
    failing run against each (diff.json and diff.txt in each neighbour's directory) and shows the
    diff against the passing neighbour whose first difference comes latest (docs/design/explain.md).
    Every neighbour is a fresh boot of one VM, one after the other; a run already in DIR with the
    same schedule and image is reused (so an interrupted explain goes on where it stopped).";

/// `hydo explain`.
pub fn cmd_explain(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let out = take(&mut args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| diff::new_out_dir("explain"));
    let other = take(&mut args, "--with-image").map(|p| abspath(Path::new(&p)));
    let max: Option<usize> = take(&mut args, "--max").map(|m| {
        m.parse()
            .unwrap_or_else(|_| die(&format!("--max {m}: not a number")))
    });
    let mut with = Vec::new();
    while let Some(w) = take(&mut args, "--with") {
        with.push(w);
    }
    let _ = std::fs::create_dir_all(&out);
    // the failing run: a run directory's, or a new one
    let given_dir = args
        .first()
        .map(PathBuf::from)
        .filter(|p| p.join(hydo_results::RUN_DOC).is_file());
    let (image, sched, a_dir) = match given_dir {
        Some(d) => {
            args.remove(0);
            let run: Run = hydo_results::reader::read_run(&d.join(hydo_results::RUN_DOC))
                .unwrap_or_else(|e| die(&e.to_string()));
            let image = match run.image.as_ref() {
                _ if args.iter().any(|a| a == "--image") => find_image(ctx, &mut args),
                Some(i) => abspath(Path::new(i.resolved.as_deref().unwrap_or(&i.path))),
                None => die(&format!("{}: names no image; give --image", d.display())),
            };
            (image, run.schedule.clone(), d)
        }
        None => {
            let image = find_image(ctx, &mut args);
            if args.len() != 1 {
                eprintln!("{USAGE}");
                return 2;
            }
            (image, args[0].clone(), out.join("a"))
        }
    };
    if !image.is_file() {
        die(&format!("no such image: {}", image.display()));
    }
    let sha = sha256_file(&image).ok();
    let a_done = hydo_results::reader::read_run(&a_dir.join(hydo_results::RUN_DOC))
        .is_ok_and(|r| r.schedule == sched);
    if !a_done {
        println!("hydo explain: the schedule        {sched}");
        let Some(run) = run_to_dir(ctx, &image, sha.clone(), &sched, &a_dir) else {
            die(&format!(
                "the schedule produced no RESULT line (see {})",
                a_dir.join("engine.log").display()
            ));
        };
        crate::runs::print_warnings(&run);
    }
    let a = diff::load(&a_dir).unwrap_or_else(|e| die(&e));
    println!(
        "hydo explain: A = {}   {}",
        a_dir.display(),
        a.run.verdict.text
    );
    // the neighbours: (how it was made, schedule, image)
    let mut nbs: Vec<(String, String, PathBuf)> = neighbours(&sched)
        .into_iter()
        .map(|(h, s)| (h, s, image.clone()))
        .collect();
    if let Some(m) = max {
        nbs.truncate(m);
    }
    for w in with {
        nbs.push((format!("replaced by {w}"), w, image.clone()));
    }
    if let Some(o) = &other {
        nbs.push((
            format!("schedule on the image {}", o.display()),
            sched.clone(),
            o.clone(),
        ));
    }
    let mut rows: Vec<(String, String, Diff, PathBuf)> = Vec::new();
    for (i, (how, s, img)) in nbs.iter().enumerate() {
        let dir = out.join(format!("n{:02}", i + 1));
        println!("hydo explain: neighbour {:>2}      {s}   ({how})", i + 1);
        let isha = if img == &image {
            sha.clone()
        } else {
            sha256_file(img).ok()
        };
        // a neighbour run before in this directory (an interrupted or repeated explain) is kept
        let done = hydo_results::reader::read_run(&dir.join(hydo_results::RUN_DOC))
            .is_ok_and(|r| &r.schedule == s && r.image.and_then(|i| i.sha256) == isha);
        if !done && run_to_dir(ctx, img, isha, s, &dir).is_none() {
            println!(
                "hydo explain:   no RESULT line (see {})",
                dir.join("engine.log").display()
            );
            continue;
        }
        let b = match diff::load(&dir) {
            Ok(b) => b,
            Err(e) => {
                println!("hydo explain:   {e}");
                continue;
            }
        };
        let mut d = diff::diff(&a, &b);
        d.neighbour = Some(how.clone());
        if let Err(e) = diff::write(&dir, &d) {
            die(&format!("{}: {e}", dir.display()));
        }
        println!("hydo explain:   {}", b.run.verdict.text);
        rows.push((how.clone(), s.clone(), d, dir));
    }
    // the table, then the closest neighbour with another verdict
    let mut text = format!(
        "hydo explain: {} neighbours of\n  {sched}\n  ({})\n\n",
        rows.len(),
        a.run.verdict.text
    );
    for (how, _, d, dir) in &rows {
        let first = d
            .first
            .as_ref()
            .map(|f| {
                format!(
                    "first difference in {} at {} ({})",
                    f.conversation,
                    f.a.as_ref()
                        .and_then(|p| p.stop)
                        .map(|s| format!("stop {s}"))
                        .unwrap_or_else(|| "the end".into()),
                    f.level
                )
            })
            .unwrap_or_else(|| {
                if d.a.fingerprints.fp == d.b.fingerprints.fp {
                    "the same run (equal fingerprints)".into()
                } else {
                    "no difference in the records, other fingerprints".into()
                }
            });
        text.push_str(&format!(
            "  {:<9} {:<44} {}   ({})\n",
            d.b.verdict.kind,
            how,
            first,
            dir.display()
        ));
        // what the checks judge differently, in one line
        if let Some(o) = &d.first_outcome {
            let what = o.details.first().cloned().unwrap_or_else(|| {
                o.lines
                    .first()
                    .map(|l| {
                        format!(
                            "{} {}",
                            if l.a.is_some() {
                                "only in A:"
                            } else {
                                "only in B:"
                            },
                            l.text
                        )
                    })
                    .unwrap_or_default()
            });
            text.push_str(&format!("            judged: {what}\n"));
        }
    }
    let best = rows
        .iter()
        .filter(|(_, _, d, _)| d.b.verdict.kind != a.run.verdict.kind)
        .max_by_key(|(_, _, d, _)| (reach(d), std::cmp::Reverse(d.hunks)));
    match best {
        None => text.push_str(&format!(
            "\nNo neighbour has another verdict than {}: every neighbour fails the same way (or \
             none ran). Give a passing schedule with --with, or another image with --with-image.\n",
            a.run.verdict.kind
        )),
        Some((how, s, d, dir)) => {
            text.push_str(&format!(
                "\nThe closest neighbour with another verdict: {how}\n  {s}\n  ({})\n\n",
                dir.display()
            ));
            text.push_str(&diff::render(d));
        }
    }
    print!("\n{text}");
    let _ = std::fs::write(out.join("explain.txt"), &text);
    println!(
        "hydo explain: the runs and their diffs are in {}",
        out.display()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_moves_by_one() {
        assert_eq!(shifted("K=redis@11+1", 1).as_deref(), Some("K=redis@12+1"));
        assert_eq!(shifted("K=redis@11+1", -1).as_deref(), Some("K=redis@10+1"));
        assert_eq!(shifted("C=9", -1).as_deref(), Some("C=8"));
        assert_eq!(shifted("C=1", -1), None);
        assert_eq!(
            shifted("N=redis@3-6:delay,300ms", 1).as_deref(),
            Some("N=redis@4-7:delay,300ms")
        );
        assert_eq!(shifted("T=17:-30", 1).as_deref(), Some("T=18:-30"));
        assert_eq!(shifted("R=no", 1), None);
        assert_eq!(shifted("W=0000002a,p4", 1), None);
    }

    #[test]
    fn the_pinned_reproducers_neighbours() {
        let n = neighbours("disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24");
        let s: Vec<&str> = n.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            s,
            [
                "disk=none dseed=4d5cb825dfaab05b | run C=24",
                "disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1",
                "disk=none dseed=4d5cb825dfaab05b | run K=redis@10+1 C=24",
                "disk=none dseed=4d5cb825dfaab05b | run K=redis@12+1 C=24",
                "disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=23",
                "disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=25",
            ]
        );
        assert_eq!(n[0].0, "without K=redis@11+1");
        let r = neighbours("disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no");
        let s: Vec<&str> = r.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            s,
            [
                "disk=cut dseed=1f16055b6d41c6d9 | run R=no",
                "disk=cut dseed=1f16055b6d41c6d9 | run C=9",
                "disk=cut dseed=1f16055b6d41c6d9 | run C=8 R=no",
                "disk=cut dseed=1f16055b6d41c6d9 | run C=10 R=no",
                "disk=none dseed=1f16055b6d41c6d9 | run C=9 R=no",
            ]
        );
    }
}
