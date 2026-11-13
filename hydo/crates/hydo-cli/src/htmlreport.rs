//! `hydo report --html [DIR] [-o FILE]`: one self-contained web page for a run directory
//! (`hydo run`, `hydo explain`), an exploration directory (`hydo explore`) or a CI run's output
//! directory (`hydo ci run`), made to understand a failure and to share it.
//!
//! The page shows the summary (verdict, system, engine, host CPU, hydo and its fingerprint
//! epoch, schedule), each failure with its minimal schedule and the command that replays it, the
//! failing run's timeline (the events `hydo inspect events` lists), where it first differs from a
//! passing neighbour (`hydo diff`), the property table, a CI run's state of each finding against
//! its base commit, and the versions a finding is in when the results carry them.
//!
//! It is made from the directory's files only: no VM runs. The page holds its style sheet and its
//! one small script and loads nothing, so it works opened from disk and as an attachment. Its
//! size is bounded: the timeline keeps at most [`MAX_EVENTS`] events, a log at most
//! [`MAX_LOG`] bytes, and the page says what it left out and which command shows it all.

mod page;
#[cfg(test)]
mod tests;

use crate::ctx::{die, pjoin, take, OUT};
use crate::diff;
use crate::inspect::drive::{Recorded, RunFiles};
use crate::inspect::timeline;
use crate::inspect::work::quote;
use hydo_results::{
    CiFinding, CiRun, Diff, DiffLine, Divergence, Engine, Exploration, Failure, Image,
    PropertyTable, Run, Shrink, Stream, TimelineEvent,
};
use page::{badge, code, copy_block, cut, cut_bytes, esc, facts, section, table, verdict_words};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The most events one timeline keeps.
pub const MAX_EVENTS: usize = 400;
/// The longest text of one event.
const EVENT_TEXT: usize = 300;
/// The most bytes of one log or piece of evidence.
pub const MAX_LOG: usize = 4000;
/// Failures of an exploration or findings of a CI run that get a timeline and a diff.
const DETAILED: usize = 4;
/// Passing shrink candidates diffed against a minimal schedule, at most.
const CANDIDATES: usize = 8;

pub const USAGE: &str =
    "usage: hydo report --html [DIR] [-o FILE] [--against RUN_DIR] [--diff DIFF]
  writes one web page for a run directory, an exploration directory or a `hydo ci run` output
  directory: the verdict, each failure with its minimal schedule and the command that replays
  it, the failing run's timeline, where it first differs from a passing neighbour, and the
  property table. The page is one file that loads nothing, so it opens from disk and can be
  attached to an issue or kept as a CI artifact. DIR defaults to hydo-out/explore, FILE to
  DIR/report.html.
  --against RUN_DIR  the passing run to compare a run with (its first difference, as hydo diff)
  --diff DIFF        a diff.json, a hydo diff directory or a hydo explain directory to show
  An exploration compares each minimal schedule with its passing shrink candidates; a hydo
  explain directory given as DIR shows its closest passing neighbour.";

/// What a directory holds.
enum Input {
    Run {
        dir: PathBuf,
        run: Box<Run>,
        /// `hydo explain`'s directory when DIR was one.
        explain: Option<PathBuf>,
    },
    Exploration {
        dir: PathBuf,
        doc: Box<Exploration>,
        events: Option<Stream>,
    },
    Ci {
        dir: PathBuf,
        doc: Box<CiRun>,
    },
}

fn read_input(d: &Path) -> Result<Input, String> {
    let ci = d.join("ci-run.json");
    if ci.is_file() {
        let t = std::fs::read_to_string(&ci).map_err(|e| format!("{}: {e}", ci.display()))?;
        return match hydo_results::reader::document(&t)
            .map_err(|e| format!("{}: {e}", ci.display()))?
        {
            hydo_results::reader::Record::CiRun(r) => Ok(Input::Ci {
                dir: d.to_path_buf(),
                doc: r,
            }),
            _ => Err(format!("{}: not a CI run document", ci.display())),
        };
    }
    if d.join("explain.txt").is_file() && d.join("a").join(hydo_results::RUN_DOC).is_file() {
        let a = d.join("a");
        let run = hydo_results::reader::read_run(&a.join(hydo_results::RUN_DOC))
            .map_err(|e| e.to_string())?;
        return Ok(Input::Run {
            dir: a,
            run: Box::new(run),
            explain: Some(d.to_path_buf()),
        });
    }
    if d.join("explain.txt").is_file() {
        // `hydo explain <run dir>`: the run is where its diffs say
        if let Some(df) = best_explained(d) {
            for a in [PathBuf::from(&df.a.dir), d.join("..").join(&df.a.dir)] {
                if let Ok(run) = hydo_results::reader::read_run(&a.join(hydo_results::RUN_DOC)) {
                    return Ok(Input::Run {
                        dir: a,
                        run: Box::new(run),
                        explain: Some(d.to_path_buf()),
                    });
                }
            }
        }
        return Err(format!(
            "{}: a hydo explain directory whose failing run is not found; give the run directory and --diff {}",
            d.display(),
            d.display()
        ));
    }
    match hydo_results::read_dir(d).map_err(|e| e.to_string())? {
        hydo_results::Results::Run { run, .. } => Ok(Input::Run {
            dir: d.to_path_buf(),
            run,
            explain: None,
        }),
        hydo_results::Results::Exploration {
            doc: Some(doc),
            events,
            ..
        } => Ok(Input::Exploration {
            dir: d.to_path_buf(),
            doc,
            events,
        }),
        hydo_results::Results::Exploration { .. } => Err(format!(
            "{}: the exploration has not finished (no exploration.json yet); `hydo report {}` says how far it got",
            d.display(),
            d.display()
        )),
    }
}

/// `hydo report --html ...` (the `--html` already taken).
pub fn cmd(argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let out = take(&mut args, "-o").or_else(|| take(&mut args, "--out"));
    let against = take(&mut args, "--against").map(PathBuf::from);
    let diff_arg = take(&mut args, "--diff").map(PathBuf::from);
    if args.len() > 1 || args.iter().any(|a| a.starts_with('-')) {
        eprintln!("{USAGE}");
        return 2;
    }
    let d = args
        .first()
        .cloned()
        .unwrap_or_else(|| pjoin(OUT, "explore"));
    let input = read_input(Path::new(&d)).unwrap_or_else(|e| die(&format!("no report: {e}")));
    let extra = Extra {
        against,
        diff: diff_arg,
        shown_dir: d.clone(),
    };
    let html = render(&input, &extra).unwrap_or_else(|e| die(&e));
    let file = out.unwrap_or_else(|| pjoin(&d, "report.html"));
    if let Err(e) = std::fs::write(&file, &html) {
        die(&format!("{file}: {e}"));
    }
    println!(
        "hydo: wrote {file} ({} KiB): open it in a browser, or attach it to an issue or a CI run",
        html.len().div_ceil(1024)
    );
    0
}

/// What the command line adds to a directory.
#[derive(Clone, Debug, Default)]
struct Extra {
    against: Option<PathBuf>,
    diff: Option<PathBuf>,
    /// The directory as the user named it (the page shows it and the commands use it).
    shown_dir: String,
}

fn render(input: &Input, extra: &Extra) -> Result<String, String> {
    match input {
        Input::Run { dir, run, explain } => run_page(dir, run, explain.as_deref(), extra),
        Input::Exploration { dir, doc, events } => {
            Ok(exploration_page(dir, doc, events.as_ref(), extra))
        }
        Input::Ci { dir, doc } => Ok(ci_page(dir, doc, extra)),
    }
}

// ---------------------------------------------------------------- words shared by the pages

/// `1 run`, `2 runs`.
fn plural(n: u64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The system's name, from its image's file name (`explore-ob-redis-arm64.cpio.gz`: redis).
fn system_of(image: &str) -> Option<String> {
    let base = Path::new(image).file_name()?.to_str()?;
    let mut s = base.strip_prefix("explore-ob-")?.to_string();
    for suf in [".cpio.gz", "-disk.img", ".img", "-arm64", "-agent"] {
        if let Some(x) = s.strip_suffix(suf) {
            s = x.to_string();
        }
    }
    (!s.is_empty()).then_some(s)
}

fn image_words(i: &Image) -> String {
    let base = Path::new(&i.path)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| i.path.clone());
    let mut s = match system_of(&i.path) {
        Some(n) => format!("{} (image {})", esc(&n), code(&base)),
        None => format!("image {}", code(&base)),
    };
    if let Some(h) = &i.sha256 {
        s.push_str(&format!(
            " <span class=\"muted\">sha256 {}</span>",
            esc(&cut(h, 17))
        ));
    }
    s
}

fn engine_words(e: &Engine) -> String {
    let mut s = match e.backend.as_deref() {
        Some("hvm") => "the Mac engine (Apple's Hypervisor.framework)".to_string(),
        Some("kvm") => "the KVM engine (Linux KVM, no kernel module)".to_string(),
        Some("hvx") => "the hvx engine (hydo's own kernel module)".to_string(),
        Some(b) => format!("the {b} engine"),
        None => "not recorded".to_string(),
    };
    let mut more = Vec::new();
    if let Some(v) = e.vcpus {
        more.push(format!("{v} vCPU{}", if v == 1 { "" } else { "s" }));
    }
    if let Some(m) = e.ram_mib {
        more.push(format!("{m} MiB of guest RAM"));
    }
    if let Some(c) = &e.cpu {
        more.push(format!("CPU level {c}"));
    }
    if let Some(m) = &e.module_srcversion {
        more.push(format!("module {m}"));
    }
    if !more.is_empty() {
        s.push_str(&format!(", {}", more.join(", ")));
    }
    esc(&s)
}

fn host_words(e: &Engine) -> Option<String> {
    e.host_cpu
        .as_ref()
        .map(|h| esc(&format!("{} ({})", h.model, h.vendor)))
}

fn hydo_words(version: Option<&str>, epoch: Option<u64>) -> String {
    let v = version.unwrap_or("not recorded");
    match epoch {
        Some(e) => format!(
            "{} <span class=\"muted\">(fingerprint epoch {e}: fingerprints compare only between runs of one epoch)</span>",
            esc(v)
        ),
        None => esc(v),
    }
}

/// The `hydo run` command line that runs `schedule` on `image` with the engine's options.
fn run_command(image: &str, engine: Option<&Engine>, schedule: &str) -> String {
    let mut w = vec!["hydo run".to_string()];
    if let Some(e) = engine {
        if e.backend.as_deref() == Some("kvm") {
            w.push("--backend kvm".into());
        }
        let o = crate::runs::RunOpts::of_eargs(e.args.as_deref().unwrap_or(&[]));
        if let Some(n) = o.vcpus {
            w.push(format!("--vcpus {n}"));
        }
        if let Some(m) = o.turn_margin {
            w.push(format!("--turn-margin {m}"));
        }
    }
    w.push(format!("--image {}", quote(image)));
    w.push(format!("\"{schedule}\""));
    w.join(" ")
}

/// A recorded command line without the backquotes the explorer puts around it.
fn unquote(c: &str) -> String {
    c.trim().trim_matches('`').to_string()
}

/// How schedule `b` was made from schedule `a` (`without R=everysec`, `with disk=none instead
/// of disk=lying:1`).
pub fn schedule_change(a: &str, b: &str) -> String {
    let toks = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .filter(|t| *t != "|")
            .map(str::to_string)
            .collect()
    };
    let (ta, tb) = (toks(a), toks(b));
    let only_a: Vec<&String> = ta.iter().filter(|t| !tb.contains(t)).collect();
    let only_b: Vec<&String> = tb.iter().filter(|t| !ta.contains(t)).collect();
    let key = |t: &str| t.split(['=', '@']).next().unwrap_or("").to_string();
    match (only_a.as_slice(), only_b.as_slice()) {
        ([], []) => "the same schedule".into(),
        ([x], [y]) if key(x) == key(y) => format!("with {y} instead of {x}"),
        (xs, []) => format!(
            "without {}",
            xs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")
        ),
        ([], ys) => format!(
            "with {} added",
            ys.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")
        ),
        (xs, ys) => format!(
            "without {}, with {}",
            xs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "),
            ys.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")
        ),
    }
}

fn header(title: &str, lead: &str, toc: &[(&str, &str)]) -> String {
    let mut s = format!(
        "<header>\n<p class=\"muted\">hydo report</p>\n<h1>{title}</h1>\n<p class=\"lead\">{lead}</p>\n"
    );
    if !toc.is_empty() {
        s.push_str("<nav class=\"toc\">");
        for (id, name) in toc {
            s.push_str(&format!("<a href=\"#{id}\">{}</a>", esc(name)));
        }
        s.push_str("</nav>\n");
    }
    s.push_str("</header>\n");
    s
}

fn footer(source: &str) -> String {
    format!(
        "<footer>Made by <code>hydo report --html</code> from {}. The page is one file and loads \
         nothing from the network. Every schedule on it replays bit for bit on the same image and \
         hydo version.</footer>\n",
        code(source)
    )
}

// ---------------------------------------------------------------- the timeline

/// A run's events: the timeline of `hydo inspect events` when the run has control transcripts,
/// else the events `hydo diff` reads from its consoles alone.
fn events_of(dir: &Path, run: &Run, files: &RunFiles) -> Vec<TimelineEvent> {
    if let Ok(rec) = Recorded::of(dir, run.clone(), files.clone()) {
        if !rec.transcripts.is_empty() {
            let mut evs = timeline::events(&rec);
            // the result's verdict repeats the last conversation's
            if evs
                .iter()
                .any(|e| e.kind == "verdict" && e.conversation != "result")
            {
                evs.retain(|e| !(e.kind == "verdict" && e.conversation == "result"));
            }
            return evs;
        }
    }
    let l = diff::load_run(dir, run.clone(), files);
    l.events
        .iter()
        .map(|e| TimelineEvent {
            point: e.stop.map(|s| format!("{}:{s}", e.conv)),
            conversation: e.conv.to_string(),
            stop: e.stop,
            at: e.at,
            kind: timeline::kind_of(e).to_string(),
            text: e.text.clone(),
            service: timeline::service_of(&e.text, e.json.as_ref().unwrap_or(&Value::Null)),
            details: None,
        })
        .collect()
}

/// An event's label, its colour, and whether it is a detail hidden until the reader asks.
fn kind_label(e: &TimelineEvent) -> (&'static str, &'static str, bool) {
    let t = e.text.as_str();
    match e.kind.as_str() {
        "fault" => ("fault", "bad", false),
        "exit" => ("process ended", "warn", false),
        "probe" => ("readiness check", "", false),
        "done" => ("command ended", "", false),
        "refused" => ("refused", "warn", false),
        "kernel" => ("kernel", "warn", false),
        "preempt" => ("thread switch", "note", false),
        "verdict" if t.contains("VIOLATION") || t.contains("ERROR") => ("verdict", "bad", false),
        "verdict" => ("verdict", "ok", false),
        "end" => ("phase ended", "", false),
        "assertion" if t.starts_with("HYDO-ASSERT-DECL") => ("assertion declared", "", true),
        "assertion" if t.contains("ok=false") || t.contains("\"ok\":false") => {
            ("assertion", "bad", false)
        }
        "assertion" => ("assertion", "ok", false),
        "report" => ("report", "", false),
        "note" if t.starts_with("hydo-ob: command:") => ("host note", "", true),
        "note" => ("host note", "", false),
        "command" if t.starts_with("spawn [") => ("service start", "note", false),
        "command" if t.starts_with("signal [") => ("signal", "warn", false),
        "command" => ("command", "", true),
        "snapshot" => ("snapshot", "", true),
        "engine" => ("engine", "", true),
        "guest" => ("guest output", "", true),
        _ => ("event", "", true),
    }
}

fn conversation_words(c: &str) -> &'static str {
    match c {
        "boot" => "Boot and setup, before the workload (the same in every run of this image)",
        "run" => "The workload, under the schedule's faults",
        "recovery" => {
            "The recovery boot: the machine starts again after the power cut, and the checks run"
        }
        "result" => "The result",
        _ => "Other events",
    }
}

/// The timeline as a table, the details hidden behind a check box (no script needed).
fn timeline_html(id: &str, evs: &[TimelineEvent], more: &str) -> String {
    let labelled: Vec<(&TimelineEvent, (&str, &str, bool))> =
        evs.iter().map(|e| (e, kind_label(e))).collect();
    let total = labelled.len();
    let minor_total = labelled.iter().filter(|(_, l)| l.2).count();
    // keep every event while they fit; else the main events first, in order
    let kept: Vec<&(&TimelineEvent, (&str, &str, bool))> = if total <= MAX_EVENTS {
        labelled.iter().collect()
    } else {
        let majors = total - minor_total;
        let mut room_minor = MAX_EVENTS.saturating_sub(majors);
        let mut room_major = MAX_EVENTS;
        labelled
            .iter()
            .filter(|(_, l)| {
                if l.2 {
                    if room_minor > 0 {
                        room_minor -= 1;
                        return true;
                    }
                    false
                } else if room_major > 0 {
                    room_major -= 1;
                    true
                } else {
                    false
                }
            })
            .collect()
    };
    let minor_kept = kept.iter().filter(|(_, l)| l.2).count();
    let mut s = String::from("<div class=\"tl\">\n");
    s.push_str(&format!(
        "<p class=\"muted\">Each row is one event, in order. <b>Stop</b> is the moment at which \
         hydo's host side looked at the guest (<code>hydo inspect start DIR --at run:STOP</code> \
         goes back there); <b>clock</b> is the engine's virtual clock, the same in every replay. \
         The main events are shown; {minor_kept} more (commands, snapshots, the guest's own \
         output) are behind the box.</p>\n"
    ));
    s.push_str(&format!(
        "<input type=\"checkbox\" class=\"all\" id=\"{id}\"> <label for=\"{id}\">Show every event</label>\n"
    ));
    s.push_str("<div class=\"scroll\"><table>\n<thead><tr><th>Stop</th><th class=\"c-clock\">Clock</th><th>What</th><th>Service</th><th>Event</th></tr></thead>\n<tbody>\n");
    let mut conv = "";
    for (e, (label, colour, minor)) in &kept {
        if e.conversation != conv {
            conv = e.conversation.as_str();
            s.push_str(&format!(
                "<tr class=\"conv\"><td colspan=\"5\">{}</td></tr>\n",
                esc(conversation_words(conv))
            ));
        }
        let class = if *minor { " class=\"minor\"" } else { "" };
        s.push_str(&format!(
            "<tr{class}><td class=\"n\">{}</td><td class=\"n c-clock\">{}</td><td><span class=\"kind {colour}\">{}</span></td><td>{}</td><td class=\"t\">{}</td></tr>\n",
            e.stop.map(|x| x.to_string()).unwrap_or_default(),
            e.at.map(|x| x.to_string()).unwrap_or_default(),
            esc(label),
            esc(e.service.as_deref().unwrap_or("")),
            esc(&cut(&e.text, EVENT_TEXT))
        ));
    }
    s.push_str("</tbody></table></div>\n");
    if kept.len() < total {
        s.push_str(&format!(
            "<p class=\"muted\">The run has {total} events; this page keeps {} of them. {more}</p>\n",
            kept.len()
        ));
    } else if !more.is_empty() {
        s.push_str(&format!("<p class=\"muted\">{more}</p>\n"));
    }
    s.push_str("</div>\n");
    s
}

// ---------------------------------------------------------------- the diff

fn point_words(p: Option<&hydo_results::DiffPoint>) -> String {
    match p {
        Some(p) => match p.stop {
            Some(s) => format!("stop {s}"),
            None => "the result".into(),
        },
        None => "nowhere".into(),
    }
}

fn divergence_html(d: &Divergence, a: &str, b: &str) -> String {
    let mut s = format!(
        "<p>In the {} part of the runs, at {} in {a} and {} in {b} (level {}).</p>\n",
        esc(match d.conversation.as_str() {
            "boot" => "boot",
            "run" => "workload",
            "recovery" => "recovery boot",
            "result" => "result",
            c => c,
        }),
        esc(&point_words(d.a.as_ref())),
        esc(&point_words(d.b.as_ref())),
        esc(&d.level)
    );
    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    let mut row = |class: &str, sign: &str, l: &DiffLine, p: Option<&hydo_results::DiffPoint>| {
        rows.push((
            class.to_string(),
            vec![
                esc(sign),
                format!("<span class=\"n\">{}</span>", esc(&point_words(p))),
                esc(&l.level),
                format!(
                    "<span class=\"t\">{}</span>",
                    esc(&cut(&l.text, EVENT_TEXT))
                ),
            ],
        ));
    };
    for l in &d.context {
        row("same", "both", l, l.a.as_ref());
    }
    let mut last_after: Option<&Option<String>> = None;
    for l in d.lines.iter().filter(|l| l.a.is_some()) {
        if last_after != Some(&l.after) {
            last_after = Some(&l.after);
        }
        row("del", &format!("only {a}"), l, l.a.as_ref());
    }
    for l in d.lines.iter().filter(|l| l.a.is_none()) {
        row("add", &format!("only {b}"), l, l.b.as_ref());
    }
    s.push_str(&table(&["", "Where", "Level", "Event"], &rows));
    if let Some(after) = d.lines.iter().find_map(|l| l.after.as_ref()) {
        s.push_str(&format!(
            "<p class=\"muted\">Where in the workload: after {}.</p>\n",
            code(after)
        ));
    }
    for x in &d.details {
        s.push_str(&format!("<p>What differs: {}</p>\n", code(x)));
    }
    s
}

/// The diff of a failing run (A) against a passing one (B), for people.
fn diff_html(d: &Diff, how: Option<&str>, a: &str, b: &str) -> String {
    let mut s = String::new();
    let how = how.or(d.neighbour.as_deref());
    s.push_str(&format!(
        "<p>This compares {a} with {b}, {}: {} ended with {}.</p>\n",
        match how {
            Some(h) => format!("which has the same schedule {}", esc(h)),
            None => "another run".into(),
        },
        code(&d.b.schedule),
        code(&d.b.verdict.text),
        a = esc(a),
        b = esc(b),
    ));
    if d.a.image_sha256 != d.b.image_sha256 {
        s.push_str("<p class=\"muted\">The two runs booted different images, so lines only one image's code writes are expected.</p>\n");
    }
    if d.identical {
        s.push_str("<p>The runs do not differ in anything hydo records here (the host's commands, the guest's lines, the results); the difference is in what these records leave out, such as the disk's contents.</p>\n");
        return s;
    }
    if let Some(f) = &d.first {
        s.push_str("<h4>Where they first part</h4>\n");
        s.push_str(&divergence_html(f, a, b));
    }
    match (&d.first_outcome, &d.first) {
        (Some(o), Some(f)) if o == f => s.push_str(
            "<p class=\"muted\">This is also the first difference in what the checks judge.</p>\n",
        ),
        (Some(o), _) => {
            s.push_str("<h4>The first difference in what the checks judge</h4>\n");
            s.push_str(&divergence_html(o, a, b));
        }
        (None, _) => {
            s.push_str("<p class=\"muted\">The runs do not differ in what the checks judge.</p>\n")
        }
    }
    if let Some(p) = &d.first_preempt {
        s.push_str("<h4>The first forced thread switch that differs</h4>\n");
        s.push_str(&divergence_html(p, a, b));
    }
    s.push_str(&format!(
        "<p class=\"muted\">The runs differ in {} place{} in all.</p>\n",
        d.hunks,
        if d.hunks == 1 { "" } else { "s" }
    ));
    s
}

/// The diff `--diff` names: a diff.json, a directory with one, or `hydo explain`'s directory
/// (its closest neighbour with another verdict).
fn read_diff_arg(p: &Path) -> Result<Diff, String> {
    if p.is_file() {
        return hydo_results::reader::read_diff(p).map_err(|e| e.to_string());
    }
    if p.join(hydo_results::DIFF_DOC).is_file() {
        return hydo_results::reader::read_diff(&p.join(hydo_results::DIFF_DOC))
            .map_err(|e| e.to_string());
    }
    best_explained(p).ok_or_else(|| {
        format!(
            "{}: no diff.json, and no hydo explain neighbour (n01/diff.json, ...) with another verdict",
            p.display()
        )
    })
}

/// `hydo explain`'s neighbour with another verdict whose first difference comes latest.
fn best_explained(dir: &Path) -> Option<Diff> {
    let mut ds: Vec<Diff> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(hydo_results::DIFF_DOC))
        .filter(|p| p.is_file())
        .filter_map(|p| hydo_results::reader::read_diff(&p).ok())
        .filter(|d| d.a.verdict.kind != d.b.verdict.kind)
        .collect();
    ds.sort_by(|x, y| {
        crate::explain::reach(x)
            .cmp(&crate::explain::reach(y))
            .then(y.b.dir.cmp(&x.b.dir))
    });
    ds.pop()
}

// ---------------------------------------------------------------- properties and checks

fn status_words(s: &str) -> (&'static str, &'static str) {
    match s {
        "ok" => ("ok", "held"),
        "violation" => ("bad", "failed"),
        "not-exercised" => ("warn", "never reached"),
        "not-evaluated" => ("warn", "never evaluated"),
        _ => ("warn", "unknown"),
    }
}

fn properties_html(t: &PropertyTable, over: &str) -> String {
    let order = |s: &str| match s {
        "violation" => 0,
        "not-exercised" | "not-evaluated" => 1,
        _ => 2,
    };
    let mut ps: Vec<&hydo_results::Property> = t.properties.iter().collect();
    ps.sort_by_key(|p| order(&p.status));
    let count = |f: &dyn Fn(&str) -> bool| ps.iter().filter(|p| f(&p.status)).count();
    let mut s = format!(
        "<p>{} properties over {over}: {} held, {} failed, {} never reached. A property that is \
         never reached was not tested by these schedules: the fault or the state it needs did not \
         happen.</p>\n",
        ps.len(),
        count(&|s| s == "ok"),
        count(&|s| s == "violation"),
        count(&|s| s.starts_with("not-")),
    );
    let rows: Vec<(String, Vec<String>)> = ps
        .iter()
        .map(|p| {
            let (c, w) = status_words(&p.status);
            let mut msg = esc(&p.message);
            if let Some(f) = &p.first_failure {
                msg.push_str(&format!(
                    "<br><span class=\"muted\">first failure:</span> <span class=\"t\">{}</span>",
                    esc(&cut(f, EVENT_TEXT))
                ));
            }
            (
                String::new(),
                vec![
                    badge(c, w),
                    format!("<span class=\"t\">{}</span>", esc(&p.id)),
                    esc(&p.assertion),
                    format!("{} / {}", p.runs_passed, p.runs_failed),
                    msg,
                ],
            )
        })
        .collect();
    s.push_str(&table(
        &[
            "Result",
            "Property",
            "Kind",
            "Runs held / failed",
            "What it says",
        ],
        &rows,
    ));
    s
}

/// A run's own checks (invariants, history checks, assertions, messaging claims).
fn checks_html(run: &Run) -> String {
    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    for f in &run.findings {
        let (c, w) = status_words(&f.status);
        rows.push((
            String::new(),
            vec![
                badge(c, w),
                format!("<span class=\"t\">{}</span>", esc(&f.id)),
                esc(&f.kind),
                format!(
                    "<span class=\"t\">{}</span>",
                    esc(&cut(&f.message, EVENT_TEXT))
                ),
            ],
        ));
    }
    for m in &run.messages {
        for c in &m.claims {
            let (cl, w) = status_words(&c.status);
            rows.push((
                String::new(),
                vec![
                    badge(cl, w),
                    format!("<span class=\"t\">{}</span>", esc(&c.id)),
                    "messages".into(),
                    esc(&c.summary),
                ],
            ));
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    table(&["Result", "Check", "Kind", "What it found"], &rows)
}

// ---------------------------------------------------------------- versions

/// `hydo versions`' answer for this directory (Milestone 221): `versions.json` next to the
/// results document, or in its `versions/` folder, where `hydo versions <dir>` writes it
/// (`hydo_results::versions_of`).
fn versions_raw(dir: &Path, _doc_file: &str) -> Option<hydo_results::VersionsDoc> {
    hydo_results::versions_of(dir).map(|(_, d)| d)
}

fn versions_html(doc: &hydo_results::VersionsDoc) -> String {
    let mut s = format!(
        "<p>The finding's minimal schedule replayed on versions of the system, oldest first \
         (<code>hydo versions</code>, on the {} engine). One replay per version is enough: hydo \
         replays exactly, so each verdict is exact for that engine and image.</p>\n",
        esc(doc.engine.backend.as_deref().unwrap_or("recorded"))
    );
    for v in &doc.findings {
        s.push_str("<div class=\"card\">\n");
        s.push_str(&format!(
            "<h3>{}</h3>\n",
            code(v.title.as_deref().unwrap_or(&v.schedule))
        ));
        s.push_str(&format!("<p>{}</p>\n", esc(&v.summary)));
        let mut rows: Vec<(String, String)> = Vec::new();
        let mut fact = |k: &str, x: &Option<String>| {
            if let Some(x) = x {
                rows.push((k.to_string(), esc(x)));
            }
        };
        fact("Introduced in", &v.first_bad);
        fact("The last version without it", &v.last_good);
        fact("Fixed in", &v.fixed_in);
        let newest = v.versions.last().map(|r| r.version.clone());
        fact(
            "Still present in the newest version",
            &v.still_present_in_newest.map(|p| {
                format!(
                    "{} ({})",
                    if p { "yes" } else { "no" },
                    newest.clone().unwrap_or_default()
                )
            }),
        );
        fact("Contradiction", &v.contradiction);
        rows.push(("Schedule".into(), code(&v.schedule)));
        if let Some(k) = &v.key {
            rows.push(("Finding key".into(), code(k)));
        }
        rows.push(("Versions from".into(), esc(&v.source)));
        rows.push((
            "Replays".into(),
            esc(&format!("{} for {} versions", v.replays, v.versions.len())),
        ));
        s.push_str(&facts(&rows));
        let rows: Vec<(String, Vec<String>)> = v
            .versions
            .iter()
            .map(|r| {
                let word = match r.outcome.as_str() {
                    hydo_results::version_outcome::REPRODUCED => badge("bad", "reproduced"),
                    hydo_results::version_outcome::PASSES => badge("ok", "passes"),
                    hydo_results::version_outcome::FAILS_OTHERWISE => {
                        badge("warn", "fails otherwise")
                    }
                    hydo_results::version_outcome::CANNOT_RUN => badge("warn", "cannot run"),
                    other => esc(&other.replace('-', " ")),
                };
                let how = match (r.recorded, r.replay) {
                    (Some(true), _) => "the finding's own run".to_string(),
                    (_, Some(n)) => format!("replay {n}"),
                    _ => String::new(),
                };
                (
                    String::new(),
                    vec![
                        esc(&r.version),
                        word,
                        r.fingerprint.as_deref().map(code).unwrap_or_default(),
                        esc(&how),
                        esc(&cut(
                            r.verdict.as_deref().or(r.why.as_deref()).unwrap_or(""),
                            200,
                        )),
                    ],
                )
            })
            .collect();
        s.push_str(&table(
            &["Version", "Outcome", "Fingerprint", "From", "Verdict"],
            &rows,
        ));
        s.push_str("</div>\n");
    }
    s
}

// ---------------------------------------------------------------- a run

fn diagnosis_html(run: &Run) -> String {
    let Some(d) = &run.diagnosis else {
        return String::new();
    };
    let mut s = format!("<p><b>Why it failed:</b> {}</p>\n", esc(&d.summary));
    if !d.likely.is_empty() {
        s.push_str("<ul>\n");
        for l in &d.likely {
            s.push_str(&format!("<li>{}</li>\n", esc(l)));
        }
        s.push_str("</ul>\n");
    }
    for ev in d.evidence.iter().take(6) {
        let (t, was_cut) = cut_bytes(&ev.text, MAX_LOG);
        s.push_str(&format!(
            "<details><summary>{} <span class=\"muted\">({})</span></summary>\n<pre>{}</pre>\n{}</details>\n",
            esc(&ev.what),
            esc(&ev.source),
            esc(&t),
            if was_cut || ev.truncated {
                format!(
                    "<p class=\"muted\">Cut to {MAX_LOG} bytes here; <code>hydo inspect diagnose DIR --evidence</code> shows it whole.</p>\n"
                )
            } else {
                String::new()
            }
        ));
    }
    if !d.next.is_empty() {
        s.push_str("<p>What to do next:</p>\n<ul>\n");
        for n in &d.next {
            s.push_str(&format!("<li>{}</li>\n", code(n)));
        }
        s.push_str("</ul>\n");
    }
    s
}

fn run_page(
    dir: &Path,
    run: &Run,
    explain: Option<&Path>,
    extra: &Extra,
) -> Result<String, String> {
    let shown = if explain.is_some() {
        dir.display().to_string()
    } else {
        extra.shown_dir.clone()
    };
    let system = run
        .image
        .as_ref()
        .and_then(|i| system_of(&i.path))
        .unwrap_or_else(|| "the system".into());
    let (vc, vw, vs) = verdict_words(&run.verdict.kind);
    // the diff: --against, --diff, an explain directory, or none
    let mut how: Option<String> = None;
    let d: Option<Diff> = if let Some(b) = &extra.against {
        let a = diff::load(dir)?;
        let bl = diff::load(b)?;
        how = Some(schedule_change(&run.schedule, &bl.run.schedule));
        Some(diff::diff(&a, &bl))
    } else if let Some(p) = &extra.diff {
        Some(read_diff_arg(p)?)
    } else {
        explain.and_then(best_explained)
    };
    let failed = run.verdict.kind != "ok";
    let mut toc = vec![("summary", "Summary")];
    if failed {
        toc.push(("failure", "The failure"));
    }
    toc.push(("replay", "Run it again"));
    toc.push(("timeline", "Timeline"));
    if d.is_some() {
        toc.push(("diff", "Against a passing run"));
    }
    toc.push(("checks", "Checks"));
    let versions = versions_raw(dir, hydo_results::RUN_DOC);
    if versions.is_some() {
        toc.push(("versions", "Versions"));
    }
    let mut body = header(
        &format!("{}: {}", esc(&system), esc(vs)),
        &format!(
            "One run of the schedule {} ended with {}.",
            code(&run.schedule),
            code(&run.verdict.text)
        ),
        &toc,
    );
    // summary
    let mut f: Vec<(String, String)> = vec![
        (
            "Result".into(),
            format!("{} {}", badge(vc, vw), code(&run.verdict.text)),
        ),
        ("Schedule".into(), copy_block(&run.schedule)),
    ];
    if let Some(i) = &run.image {
        f.push(("System".into(), image_words(i)));
    }
    if let Some(e) = &run.engine {
        f.push(("Engine".into(), engine_words(e)));
        if let Some(h) = host_words(e) {
            f.push(("Host CPU".into(), h));
        }
    }
    f.push((
        "hydo".into(),
        hydo_words(run.hydo_version.as_deref(), run.epoch),
    ));
    f.push((
        "Fingerprint".into(),
        format!(
            "{} <span class=\"muted\">(a hash of the machine's whole final state: a replay gives the same one)</span>",
            code(&run.fingerprints.fp)
        ),
    ));
    if let Some(s) = run.secs {
        f.push(("Wall time".into(), esc(&format!("{s} s"))));
    }
    f.push(("Run directory".into(), code(&shown)));
    body.push_str(&section("summary", "Summary", &facts(&f)));
    // the failure
    if failed {
        let mut s = String::new();
        let bad: Vec<&hydo_results::Finding> =
            run.findings.iter().filter(|x| x.status != "ok").collect();
        if bad.is_empty() {
            s.push_str(&format!("<p>{}</p>\n", code(&run.verdict.text)));
        } else {
            s.push_str("<ul>\n");
            for x in &bad {
                s.push_str(&format!(
                    "<li>{} {} <span class=\"muted\">({})</span>: {}</li>\n",
                    badge("bad", status_words(&x.status).1),
                    code(&x.id),
                    esc(&x.kind),
                    esc(&cut(&x.message, EVENT_TEXT))
                ));
            }
            s.push_str("</ul>\n");
        }
        s.push_str(&diagnosis_html(run));
        if let Some(w) = run.warnings.first() {
            s.push_str(&format!("<p class=\"muted\">Warning: {}</p>\n", esc(w)));
        }
        body.push_str(&section("failure", "The failure", &s));
    }
    // replay
    let mut s = String::from(
        "<p>The first command runs the recorded run again from a fresh boot and checks that it \
         gives the same fingerprint. The second runs the same schedule as a new run.</p>\n",
    );
    s.push_str(&copy_block(&format!("hydo repro {}", quote(&shown))));
    if let Some(i) = &run.image {
        s.push_str(&copy_block(&run_command(
            &i.path,
            run.engine.as_ref(),
            &run.schedule,
        )));
    }
    body.push_str(&section("replay", "Run it again", &s));
    // timeline
    let evs = events_of(dir, run, &RunFiles::default());
    let more = format!(
        "<code>hydo inspect events {}</code> lists every event with its details.",
        esc(&quote(&shown))
    );
    body.push_str(&section(
        "timeline",
        "Timeline of the run",
        &timeline_html("tl-run", &evs, &more),
    ));
    if let Some(d) = &d {
        body.push_str(&section(
            "diff",
            "Against a passing run",
            &diff_html(d, how.as_deref(), "the failing run", "the passing run"),
        ));
    }
    // checks
    let mut s = checks_html(run);
    if let Some(t) = &run.properties {
        s.push_str(&properties_html(t, "this run"));
    }
    if s.is_empty() {
        s = "<p>The run reported no checks.</p>\n".into();
    }
    body.push_str(&section("checks", "Checks and properties", &s));
    if let Some(v) = &versions {
        body.push_str(&section("versions", "Versions", &versions_html(v)));
    }
    body.push_str(&footer(&pjoin(&shown, hydo_results::RUN_DOC)));
    Ok(page::document(
        &format!("hydo report: {system}, {vs}"),
        &body,
    ))
}

// ---------------------------------------------------------------- an exploration

/// The runs of an exploration's stream by tag.
fn runs_by_tag(events: Option<&Stream>) -> BTreeMap<String, &Run> {
    let mut m = BTreeMap::new();
    if let Some(ev) = events {
        for r in ev.runs() {
            if let Some(t) = &r.tag {
                m.entry(t.clone()).or_insert(r);
            }
        }
    }
    m
}

/// The minimal schedule's confirmation run against its passing shrink candidates: the one whose
/// first difference comes latest, with how its schedule was made.
fn shrink_neighbour(
    dir: &Path,
    sh: &Shrink,
    events: Option<&Stream>,
    by_tag: &BTreeMap<String, &Run>,
) -> Option<(Diff, String, String, String)> {
    let min_tag = sh.confirmations.first()?.tag.clone()?;
    let min_run = by_tag.get(&min_tag)?;
    let mut cands: Vec<&Run> = events?
        .runs()
        .filter(|r| r.role.as_deref() == Some(hydo_results::role::SHRINK))
        .filter(|r| r.shrink.as_ref().is_some_and(|s| s.of == sh.signature))
        .filter(|r| r.verdict.kind == "ok" && r.schedule != sh.minimal)
        .filter(|r| {
            r.artifacts
                .as_ref()
                .and_then(|a| a.consoles.first())
                .is_some_and(|c| dir.join(c).is_file())
        })
        .collect();
    cands.sort_by_key(|r| std::cmp::Reverse(r.shrink.as_ref().map(|s| s.round).unwrap_or(0)));
    let mut seen: Vec<&str> = Vec::new();
    cands.retain(|r| {
        if seen.contains(&r.schedule.as_str()) {
            return false;
        }
        seen.push(&r.schedule);
        true
    });
    let a = diff::load_run(dir, (*min_run).clone(), &RunFiles::explorer(&min_tag));
    let mut best: Option<(Diff, String, String, String)> = None;
    for c in cands.into_iter().take(CANDIDATES) {
        let tag = c.tag.clone().unwrap_or_default();
        let b = diff::load_run(dir, c.clone(), &RunFiles::explorer(&tag));
        let d = diff::diff(&a, &b);
        let better = match &best {
            None => true,
            Some((bd, ..)) => crate::explain::reach(&d) > crate::explain::reach(bd),
        };
        if better {
            let how = schedule_change(&sh.minimal, &c.schedule);
            best = Some((d, how, min_tag.clone(), tag));
        }
    }
    best
}

/// What a finding class says, in words (hydo-explore's `finding_class`).
fn class_words(c: &str) -> Option<&'static str> {
    Some(match c {
        "lying-disk" => "The disk lost writes it had reported as written (disk=lying). A system is only as safe as its disk, so this class is often expected.",
        "io-errors" => "The disk returned errors (disk=ioerr).",
        "corrupted-files" => "A service's files were corrupted on purpose (G=): the system's own checks of its data are under test.",
        "unsafe-knob" => "A setting that trades safety for speed (one of the project's knobs, such as R=everysec) met a power cut or lost writes.",
        "test-failed" => "The test program failed, with no fault that explains it.",
        "durable-defaults" => "The system failed with its safe settings and a working disk: the most serious class.",
        "refused" => "The engine cannot run this schedule, so it is not a failure of the system.",
        _ => return None,
    })
}

fn class_of(f: &Failure) -> String {
    f.run.class.clone().unwrap_or_default()
}

fn exploration_page(
    dir: &Path,
    doc: &Exploration,
    events: Option<&Stream>,
    extra: &Extra,
) -> String {
    let shown = extra.shown_dir.clone();
    let system = system_of(&doc.image.path).unwrap_or_else(|| "the system".into());
    let by_tag = runs_by_tag(events);
    let s_ = &doc.summary;
    // failures grouped by class and signature, in the order of their first seed
    let mut groups: Vec<((String, String), Vec<&Failure>)> = Vec::new();
    for f in &doc.failures {
        let k = (class_of(f), f.signature.clone());
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(f),
            None => groups.push((k, vec![f])),
        }
    }
    // a minimal schedule's group first
    groups.sort_by_key(|((c, sig), _)| {
        !doc.shrinks
            .iter()
            .any(|x| &x.class == c && &x.signature == sig)
    });
    let title = if doc.failures.is_empty() {
        format!(
            "{}: no failures in {} schedules",
            esc(&system),
            s_.schedules
        )
    } else {
        format!(
            "{}: {} failing schedule{} of {}, in {} kind{}",
            esc(&system),
            s_.failures,
            if s_.failures == 1 { "" } else { "s" },
            s_.schedules,
            groups.len(),
            if groups.len() == 1 { "" } else { "s" }
        )
    };
    let mut toc = vec![("summary", "Summary")];
    if !groups.is_empty() {
        toc.push(("failures", "Failures"));
    }
    toc.push(("properties", "Properties"));
    let new_kinds: Vec<&hydo_results::ErrorKind> = doc
        .error_kinds
        .as_ref()
        .map(|k| k.kinds.iter().filter(|x| x.new).collect())
        .unwrap_or_default();
    if !new_kinds.is_empty() {
        toc.push(("errors", "New error lines"));
    }
    let versions = versions_raw(dir, hydo_results::EXPLORATION_DOC);
    if versions.is_some() {
        toc.push(("versions", "Versions"));
    }
    let lead = if doc.failures.is_empty() {
        "hydo ran the system under schedules of faults drawn from seeds, and every run passed its checks.".to_string()
    } else {
        "hydo ran the system under schedules of faults drawn from seeds. Each failure below was run again to check that it reproduces, and shrunk to the smallest schedule that still fails.".to_string()
    };
    let mut body = header(&title, &esc(&lead), &toc);
    // summary
    let (rc, rw) = if doc.failures.is_empty() {
        ("ok", "no failures".to_string())
    } else {
        ("bad", format!("{} failures", s_.failures))
    };
    let mut f: Vec<(String, String)> = vec![("Result".into(), badge(rc, &rw))];
    f.push(("System".into(), image_words(&doc.image)));
    f.push(("Engine".into(), engine_words(&doc.engine)));
    if let Some(h) = host_words(&doc.engine) {
        f.push(("Host CPU".into(), h));
    }
    f.push((
        "hydo".into(),
        hydo_words(doc.hydo_version.as_deref(), doc.epoch),
    ));
    let mut how = format!("{} schedules", s_.schedules);
    if let Some(seeds) = &doc.config.seeds {
        how.push_str(&format!(" from the seeds {seeds}"));
    }
    if let Some(sw) = &doc.config.sweep {
        how.push_str(&format!(", the sweep {sw}"));
    }
    how.push_str(&format!(", strategy {}", doc.config.strategy));
    f.push(("Explored".into(), esc(&how)));
    if let Some(sc) = &doc.config.schedule {
        f.push(("Schedule".into(), copy_block(sc)));
    }
    if !doc.failures.is_empty() {
        let rep = s_.reproduced.unwrap_or(0);
        f.push((
            "Reproduced".into(),
            esc(&format!(
                "{rep} of {} failures gave the same fingerprint when run again",
                s_.failures
            )),
        ));
    }
    if let Some(n) = s_.vacuous_passes.filter(|n| *n > 0) {
        f.push((
            "Passes that checked nothing".into(),
            esc(&format!(
                "{n} runs passed without acknowledging anything to check"
            )),
        ));
    }
    if let Some(n) = s_.refused.filter(|n| *n > 0) {
        f.push((
            "Refused".into(),
            esc(&format!(
                "{n} schedules the engine cannot run (not failures)"
            )),
        ));
    }
    if let Some(w) = s_.total_wall_s {
        f.push(("Wall time".into(), esc(&format!("{w} s"))));
    }
    f.push(("Exploration directory".into(), code(&shown)));
    body.push_str(&section("summary", "Summary", &facts(&f)));
    // failures
    if !groups.is_empty() {
        let mut s = String::new();
        let mut rows: Vec<(String, Vec<String>)> = Vec::new();
        for (i, ((c, sig), fs)) in groups.iter().enumerate() {
            let minimal = doc
                .shrinks
                .iter()
                .find(|x| &x.class == c && &x.signature == sig)
                .map(|x| x.minimal.clone())
                .unwrap_or_else(|| "not shrunk".into());
            rows.push((
                String::new(),
                vec![
                    format!("<a href=\"#f{}\">{}</a>", i + 1, i + 1),
                    esc(c),
                    format!("<span class=\"t\">{}</span>", esc(sig)),
                    fs.len().to_string(),
                    format!(
                        "{}/{}",
                        fs.iter().filter(|x| x.reproduced).count(),
                        fs.len()
                    ),
                    format!("<span class=\"t\">{}</span>", esc(&minimal)),
                ],
            ));
        }
        s.push_str(&table(
            &[
                "",
                "Class",
                "Signature",
                "Runs",
                "Reproduced",
                "Minimal schedule",
            ],
            &rows,
        ));
        s.push_str("<p class=\"muted\">The class names the kind of fault that led to it; the signature is the verdict with its numbers left out, so failures with the same cause fall together.</p>\n");
        for (i, ((c, sig), fs)) in groups.iter().enumerate() {
            let sh = doc
                .shrinks
                .iter()
                .find(|x| &x.class == c && &x.signature == sig);
            s.push_str(&failure_card(
                i + 1,
                dir,
                doc,
                c,
                sig,
                fs,
                sh,
                events,
                &by_tag,
                &shown,
            ));
        }
        body.push_str(&section("failures", "Failures", &s));
    }
    // properties
    let s = match &doc.properties {
        Some(t) => properties_html(t, &format!("{} schedules", t.runs)),
        None => "<p>The guest wrote no property records (the image has no assertion SDK).</p>\n"
            .to_string(),
    };
    body.push_str(&section("properties", "Properties over every schedule", &s));
    if !new_kinds.is_empty() {
        let rows: Vec<(String, Vec<String>)> = new_kinds
            .iter()
            .take(20)
            .map(|k| {
                (
                    String::new(),
                    vec![
                        esc(&k.service),
                        format!(
                            "<span class=\"t\">{}</span>",
                            esc(&cut(&k.template, EVENT_TEXT))
                        ),
                        k.new_runs.to_string(),
                        format!(
                            "<span class=\"t\">{}</span>",
                            esc(k
                                .shrink
                                .as_ref()
                                .map(|x| x.minimal.as_str())
                                .unwrap_or(k.first_schedule.as_str()))
                        ),
                    ],
                )
            })
            .collect();
        let mut s = format!(
            "<p>{} kinds of error line that the project's list of known errors does not cover; \
             each with the smallest schedule that still writes it.</p>\n",
            new_kinds.len()
        );
        s.push_str(&table(
            &[
                "Service",
                "Line (numbers and ids left out)",
                "Runs",
                "Smallest schedule",
            ],
            &rows,
        ));
        if new_kinds.len() > 20 {
            s.push_str(&format!(
                "<p class=\"muted\">The first 20 of {}; <code>hydo errors {}</code> lists them all.</p>\n",
                new_kinds.len(),
                esc(&quote(&shown))
            ));
        }
        body.push_str(&section("errors", "New error lines", &s));
    }
    if let Some(v) = &versions {
        body.push_str(&section("versions", "Versions", &versions_html(v)));
    }
    body.push_str(&footer(&pjoin(&shown, hydo_results::EXPLORATION_DOC)));
    page::document(
        &format!("hydo report: {system}, {}", strip_tags(&title)),
        &body,
    )
}

fn strip_tags(s: &str) -> String {
    let mut o = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => o.push(c),
            _ => {}
        }
    }
    o.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[allow(clippy::too_many_arguments)]
fn failure_card(
    n: usize,
    dir: &Path,
    doc: &Exploration,
    class: &str,
    sig: &str,
    fs: &[&Failure],
    sh: Option<&Shrink>,
    events: Option<&Stream>,
    by_tag: &BTreeMap<String, &Run>,
    shown: &str,
) -> String {
    let first = fs[0];
    let kind = &first.run.verdict.kind;
    let (vc, _, _) = verdict_words(kind);
    let mut s = format!(
        "<div class=\"card\" id=\"f{n}\">\n<h3>{n}. {} {}</h3>\n<p class=\"t mono\">{}</p>\n",
        badge(vc, if kind == "error" { "error" } else { "failure" }),
        esc(class),
        esc(sig)
    );
    if let Some(w) = class_words(class) {
        s.push_str(&format!("<p class=\"muted\">{}</p>\n", esc(w)));
    }
    let seeds: Vec<String> = fs
        .iter()
        .filter_map(|f| f.run.seed.map(|x| x.to_string()))
        .collect();
    let rep = fs.iter().filter(|f| f.reproduced).count();
    let mut f: Vec<(String, String)> = vec![
        (
            "Found in".into(),
            esc(&format!(
                "{} schedule{}{}",
                fs.len(),
                if fs.len() == 1 { "" } else { "s" },
                if seeds.is_empty() {
                    String::new()
                } else {
                    format!(
                        " (seed{} {})",
                        if seeds.len() == 1 { "" } else { "s" },
                        seeds
                            .iter()
                            .take(12)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            )),
        ),
        (
            "Reproduced".into(),
            if rep == fs.len() {
                badge("ok", "yes").to_string()
                    + " <span class=\"muted\">every re-run gave the same fingerprint</span>"
            } else {
                badge("warn", &format!("{rep} of {}", fs.len()))
                    + " <span class=\"muted\">a re-run gave another fingerprint: a hydo or host problem, not the system's</span>"
            },
        ),
    ];
    if first.timing_only == Some(true) {
        f.push((
            "Timing".into(),
            esc("appears only with the tick-timing change of its branch"),
        ));
    }
    let timeline_tag: Option<String>;
    if let Some(sh) = sh {
        f.push(("Minimal schedule".into(), copy_block(&sh.minimal)));
        if let Some(v) = &sh.min_verdict {
            f.push(("Its verdict".into(), code(v)));
        }
        if let Some(mc) = sh.min_class.as_ref().filter(|m| *m != class) {
            f.push((
                "Needs only".into(),
                esc(&format!(
                    "{mc} (found as {class}; the minimal schedule needs no more)"
                )),
            ));
        }
        f.push((
            "Two runs of it".into(),
            if sh.identical {
                format!(
                    "{} {}",
                    badge("ok", "same fingerprint"),
                    code(
                        sh.confirmations
                            .first()
                            .and_then(|c| c.fp.as_deref())
                            .unwrap_or("")
                    )
                )
            } else {
                badge("warn", "different fingerprints")
            },
        ));
        let cmd = sh
            .repro_command
            .as_deref()
            .map(unquote)
            .unwrap_or_else(|| run_command(&doc.image.path, Some(&doc.engine), &sh.minimal));
        f.push(("Replay it".into(), copy_block(&cmd)));
        f.push((
            "Shrinking".into(),
            esc(&format!(
                "from seed {}, {} shrink runs",
                sh.from_seed, sh.tries
            )),
        ));
        if let Some(c) = sh.composition.as_ref().filter(|c| !c.is_empty()) {
            f.push(("Composition".into(), esc(c)));
        }
        timeline_tag = sh.confirmations.first().and_then(|c| c.tag.clone());
    } else {
        f.push(("Schedule".into(), copy_block(&first.run.schedule)));
        f.push(("Verdict".into(), code(&first.run.verdict.text)));
        f.push((
            "Replay it".into(),
            copy_block(&run_command(
                &doc.image.path,
                Some(&doc.engine),
                &first.run.schedule,
            )),
        ));
        f.push((
            "Not shrunk".into(),
            esc(if kind == "error" {
                "hydo shrinks failed checks, not runs that ended with an error"
            } else {
                "the exploration did not shrink this one"
            }),
        ));
        timeline_tag = first.run.tag.clone();
    }
    s.push_str(&facts(&f));
    if let Some(d) = &first.run.diagnosis {
        s.push_str(&format!(
            "<p><b>Why it failed:</b> {}</p>\n",
            esc(&d.summary)
        ));
    }
    if n > DETAILED {
        s.push_str(&format!(
            "<p class=\"muted\">This page shows the timelines of the first {DETAILED} failures. \
             For this one, run the command above with <code>--out DIR</code>, then \
             <code>hydo report --html DIR</code>.</p>\n"
        ));
        s.push_str("</div>\n");
        return s;
    }
    if let Some(tag) = timeline_tag {
        if let Some(run) = by_tag.get(&tag) {
            let evs = events_of(dir, run, &RunFiles::explorer(&tag));
            if !evs.is_empty() {
                s.push_str(&format!(
                    "<details{}><summary>What happened in the run {}</summary>\n",
                    if n == 1 { " open" } else { "" },
                    code(&tag)
                ));
                s.push_str(&timeline_html(
                    &format!("tl-{n}"),
                    &evs,
                    &format!(
                        "Its files are <code>consoles/{}.*</code> in {}.",
                        esc(&tag),
                        code(shown)
                    ),
                ));
                s.push_str("</details>\n");
            }
        }
    }
    if let Some(sh) = sh {
        match shrink_neighbour(dir, sh, events, by_tag) {
            Some((d, how, a, b)) => {
                s.push_str("<h4>Against its closest passing neighbour</h4>\n");
                s.push_str(&format!(
                    "<p class=\"muted\">While shrinking, hydo also ran schedules one step simpler than the minimal one, and they passed. This compares the minimal schedule's run {} with the passing run {}, the one that stays the same for longest.</p>\n",
                    code(&a),
                    code(&b)
                ));
                s.push_str(&diff_html(&d, Some(&how), "the failing run", "the passing run"));
            }
            None => s.push_str(
                "<p class=\"muted\">No passing neighbour of the minimal schedule is recorded; <code>hydo explain IMAGE \"SCHEDULE\"</code> runs them.</p>\n",
            ),
        }
    }
    s.push_str("</div>\n");
    s
}

// ---------------------------------------------------------------- a CI run

fn state_words(s: &str) -> (&'static str, &'static str, &'static str) {
    match s {
        "new" => (
            "bad",
            "new",
            "Found on this commit; the base commit passes the same schedule.",
        ),
        "pre-existing" => (
            "warn",
            "pre-existing",
            "The base commit fails the same way: this change did not cause it.",
        ),
        "present" => (
            "warn",
            "still present",
            "A finding of the base commit that still fails here.",
        ),
        "changed" => (
            "warn",
            "changed",
            "A finding of the base commit whose minimal schedule now fails in another way.",
        ),
        "fixed" => (
            "ok",
            "fixed",
            "A finding of the base commit whose minimal schedule passes here.",
        ),
        "unverified" => (
            "note",
            "unverified",
            "hydo could not replay its schedule to decide.",
        ),
        _ => ("note", "unknown", ""),
    }
}

fn proof_words(p: &str) -> &'static str {
    match p {
        "base-replay-ok" => "its minimal schedule passes on the base commit's image",
        "base-replay-fails" => "its minimal schedule fails the same way on the base commit's image",
        "base-replay-other" => {
            "its minimal schedule fails in another way on the base commit's image"
        }
        "base-refused" => "the base commit's image cannot run its schedule",
        "base-run" => "the base commit's own CI run had it",
        "head-replay-ok" => "its minimal schedule passes on this commit's image",
        "head-replay-fails" => "its minimal schedule still fails on this commit's image",
        "head-replay-other" => "its minimal schedule fails in another way on this commit's image",
        "no-base" => "there is no base commit to compare with",
        "no-replay" => "it was not replayed",
        _ => "",
    }
}

fn ci_finding_card(n: usize, dir: &Path, f: &CiFinding) -> String {
    let (c, w, what) = state_words(&f.state);
    let mut s = format!(
        "<div class=\"card\" id=\"f{n}\">\n<h3>{n}. {} {}</h3>\n<p>{}</p>\n",
        badge(c, w),
        esc(&f.kind),
        esc(what)
    );
    let mut rows: Vec<(String, String)> = vec![(
        "Finding".into(),
        format!("<span class=\"t mono\">{}</span>", esc(&f.title)),
    )];
    if let Some(p) = &f.proof {
        let words = proof_words(p);
        rows.push((
            "How hydo knows".into(),
            if words.is_empty() {
                code(p)
            } else {
                format!("{} {}", esc(words), code(p))
            },
        ));
    }
    rows.push(("Minimal schedule".into(), copy_block(&f.minimal)));
    if let Some(r) = &f.repro {
        rows.push(("Replay it".into(), copy_block(&unquote(r))));
    }
    if !f.classes.is_empty() {
        rows.push(("Classes".into(), esc(&f.classes.join(", "))));
    }
    if let Some(r) = &f.renamed_from {
        rows.push((
            "Probably renamed".into(),
            format!("the same minimal schedule as {}", code(r)),
        ));
    }
    rows.push(("Key".into(), code(&f.key)));
    s.push_str(&facts(&rows));
    if !f.replays.is_empty() {
        let rr: Vec<(String, Vec<String>)> = f
            .replays
            .iter()
            .map(|r| {
                (
                    String::new(),
                    vec![
                        esc(if r.on == "base" {
                            "the base commit"
                        } else {
                            "this commit"
                        }),
                        format!(
                            "<span class=\"t\">{}</span>",
                            esc(r
                                .verdict
                                .as_deref()
                                .or(r.refused.as_deref())
                                .unwrap_or("no result"))
                        ),
                        format!(
                            "<code class=\"fp\">{}</code>",
                            esc(r.fp.as_deref().unwrap_or(""))
                        ),
                    ],
                )
            })
            .collect();
        s.push_str("<h4>The replays that decided it</h4>\n");
        s.push_str(&table(&["On", "Verdict", "Fingerprint"], &rr));
    }
    if let Some(d) = &f.diff {
        let (t, _) = cut_bytes(d, MAX_LOG);
        s.push_str("<h4>Where the base and this commit first differ on it</h4>\n");
        s.push_str(&format!("<pre>{}</pre>\n", esc(&t)));
    }
    if n <= DETAILED {
        let head = f
            .replays
            .iter()
            .filter(|r| r.on == "head")
            .find_map(|r| r.dir.as_ref())
            .map(|d| dir.join(d))
            .filter(|d| d.join(hydo_results::RUN_DOC).is_file());
        if let Some(hd) = head {
            if let Ok(run) = hydo_results::reader::read_run(&hd.join(hydo_results::RUN_DOC)) {
                let evs = events_of(&hd, &run, &RunFiles::default());
                if !evs.is_empty() {
                    s.push_str(&format!(
                        "<details{}><summary>What happened in this commit's replay</summary>\n",
                        if n == 1 { " open" } else { "" }
                    ));
                    s.push_str(&timeline_html(&format!("tl-{n}"), &evs, ""));
                    s.push_str("</details>\n");
                }
            }
        }
    }
    s.push_str("</div>\n");
    s
}

fn ci_page(dir: &Path, doc: &CiRun, extra: &Extra) -> String {
    let shown = extra.shown_dir.clone();
    let o = &doc.outcome;
    let (oc, ow) = match o.exit {
        0 => ("ok", "passed"),
        1 => ("bad", "failed"),
        _ => ("warn", "could not be completed"),
    };
    let new = doc.summary.states.get("new").copied().unwrap_or(0);
    let title = format!(
        "{}: the CI run {}{}",
        esc(&doc.project.name),
        esc(ow),
        if new > 0 {
            format!(", {new} new finding{}", if new == 1 { "" } else { "s" })
        } else {
            String::new()
        }
    );
    let mut toc = vec![("summary", "Summary"), ("findings", "Findings")];
    if !doc.regressions.is_empty() {
        toc.push(("regressions", "Regressions"));
    }
    toc.push(("properties", "Properties"));
    let versions = versions_raw(dir, "ci-run.json");
    if versions.is_some() {
        toc.push(("versions", "Versions"));
    }
    let mut body = header(
        &title,
        "hydo explored this commit with the budget below and compared every finding with the base commit by replaying its minimal schedule there.",
        &toc,
    );
    let mut f: Vec<(String, String)> = vec![(
        "Result".into(),
        format!(
            "{}{}",
            badge(oc, ow),
            if o.failed_by.is_empty() {
                String::new()
            } else {
                format!(
                    "<ul>{}</ul>",
                    o.failed_by
                        .iter()
                        .map(|x| format!("<li>{}</li>", esc(x)))
                        .collect::<String>()
                )
            }
        ),
    )];
    if !o.fail_on.is_empty() {
        f.push((
            "Fails on".into(),
            esc(&format!("findings that are {}", o.fail_on.join(", "))),
        ));
    }
    f.push((
        "Project".into(),
        format!(
            "{} {}",
            esc(&doc.project.name),
            code(&format!("{}/{}", doc.project.dir, doc.project.manifest))
        ),
    ));
    let mut commit = String::new();
    if let Some(c) = &doc.commit {
        commit.push_str(&code(&cut(c, 13)));
    }
    if let Some(b) = &doc.branch {
        commit.push_str(&format!(" on {}", code(b)));
    }
    if let Some(ch) = &doc.change {
        commit.push_str(&format!(", change {}", esc(ch)));
    }
    f.push((
        "Commit".into(),
        format!(
            "{commit} <span class=\"muted\">({})</span>",
            esc(&doc.trigger)
        ),
    ));
    match &doc.base {
        Some(b) => {
            let mut t = String::new();
            if let Some(c) = &b.commit {
                t.push_str(&code(&cut(c, 13)));
            }
            if let Some(br) = &b.branch {
                t.push_str(&format!(" on {}", code(br)));
            }
            t.push_str(&format!(
                " <span class=\"muted\">({})</span>",
                esc(match b.findings_from.as_str() {
                    "base-run" => "its findings from its own CI run",
                    "explored" => "explored here with the same budget",
                    "none" => "no findings of its own",
                    other => other,
                })
            ));
            if let Some(m) = &b.missing {
                t.push_str(&format!(" {}", esc(m)));
            }
            f.push(("Base commit".into(), t));
        }
        None => f.push(("Base commit".into(), esc("none: no comparison"))),
    }
    f.push(("CI system".into(), {
        let mut t = esc(&doc.ci.name);
        if let Some(id) = &doc.ci.run_id {
            t.push_str(&format!(" job {}", code(id)));
        }
        if let Some(u) = &doc.ci.run_url {
            t.push_str(&format!(" {}", code(u)));
        }
        t
    }));
    f.push(("Engine".into(), engine_words(&doc.engine)));
    if let Some(h) = host_words(&doc.engine) {
        f.push(("Host CPU".into(), h));
    }
    f.push((
        "hydo".into(),
        hydo_words(Some(&doc.hydo.version), Some(doc.hydo.epoch)),
    ));
    f.push((
        "Budget".into(),
        esc(&format!(
            "the {} mode: seeds {}, strategy {}{}",
            doc.budget.mode,
            doc.budget.seeds,
            doc.budget.strategy,
            if doc.budget.options.is_empty() {
                String::new()
            } else {
                format!(", {}", doc.budget.options.join(" "))
            }
        )),
    ));
    let sm = &doc.summary;
    f.push((
        "Explored".into(),
        esc(&format!(
            "{}, {}, {}, {} to compare with the base, {} s",
            plural(sm.runs, "run"),
            plural(sm.failures, "failing first run"),
            plural(sm.findings, "finding"),
            plural(sm.replays, "replay"),
            sm.wall_s
        )),
    ));
    f.push(("Output directory".into(), code(&shown)));
    body.push_str(&section("summary", "Summary", &facts(&f)));
    // findings
    let mut s = String::new();
    if doc.findings.is_empty() {
        s.push_str("<p>No findings: every schedule passed its checks.</p>\n");
    } else {
        let rows: Vec<(String, Vec<String>)> = doc
            .findings
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let (c, w, _) = state_words(&x.state);
                (
                    String::new(),
                    vec![
                        format!("<a href=\"#f{}\">{}</a>", i + 1, i + 1),
                        badge(c, w),
                        esc(&x.kind),
                        format!(
                            "<span class=\"t\">{}</span>",
                            esc(&cut(&x.title, EVENT_TEXT))
                        ),
                    ],
                )
            })
            .collect();
        s.push_str(&table(&["", "Against the base", "Kind", "Finding"], &rows));
        for (i, x) in doc.findings.iter().enumerate() {
            s.push_str(&ci_finding_card(i + 1, dir, x));
        }
    }
    if !doc.nondeterminism.is_empty() {
        s.push_str("<h3>Runs that did not reproduce</h3>\n<p>These failures gave another fingerprint when run again. That is a problem of hydo or of the host, never a finding of the project.</p>\n<ul>\n");
        for x in &doc.nondeterminism {
            s.push_str(&format!(
                "<li>{} {} then {}</li>\n",
                code(&x.schedule),
                code(&x.fp),
                esc(&x.rerun_fps.join(", "))
            ));
        }
        s.push_str("</ul>\n");
    }
    body.push_str(&section("findings", "Findings against the base commit", &s));
    if !doc.regressions.is_empty() {
        let rows: Vec<(String, Vec<String>)> = doc
            .regressions
            .iter()
            .map(|r| {
                (
                    String::new(),
                    vec![
                        if r.as_expected {
                            badge("ok", "as expected")
                        } else {
                            badge("bad", "not as expected")
                        },
                        esc(&r.name),
                        format!("<span class=\"t\">{}</span>", esc(&r.schedule)),
                        esc(&r.expect),
                        format!(
                            "<span class=\"t\">{}</span>",
                            esc(r.verdict.as_deref().or(r.error.as_deref()).unwrap_or(""))
                        ),
                    ],
                )
            })
            .collect();
        body.push_str(&section(
            "regressions",
            "Saved regressions",
            &table(&["Result", "Name", "Schedule", "Expected", "Got"], &rows),
        ));
    }
    // properties: the exploration's table, else what the CI run recorded
    let mut s = String::new();
    let expl = doc
        .artifacts
        .exploration
        .as_ref()
        .map(|e| dir.join(e).join(hydo_results::EXPLORATION_DOC))
        .and_then(|p| hydo_results::reader::read_exploration(&p).ok());
    match expl.as_ref().and_then(|e| e.properties.as_ref()) {
        Some(t) => s.push_str(&properties_html(t, &format!("{} schedules", t.runs))),
        None => match &doc.coverage.properties {
            Some(p) => {
                s.push_str(&format!(
                    "<p>{} properties declared, {} reached.</p>\n",
                    p.declared, p.reached
                ));
                if !p.not_exercised.is_empty() {
                    s.push_str(&format!(
                        "<p>Never reached: {}</p>\n",
                        p.not_exercised
                            .iter()
                            .map(|x| code(x))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            None => s.push_str("<p>No property records.</p>\n"),
        },
    }
    if !doc.coverage.faults.is_empty() {
        s.push_str(&format!(
            "<p class=\"muted\">Faults the schedules drew, by kind: {}.</p>\n",
            esc(&doc
                .coverage
                .faults
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", "))
        ));
    }
    body.push_str(&section("properties", "Properties", &s));
    if let Some(v) = &versions {
        body.push_str(&section("versions", "Versions", &versions_html(v)));
    }
    body.push_str(&footer(&pjoin(&shown, "ci-run.json")));
    page::document(
        &format!("hydo report: {}, CI run {ow}", doc.project.name),
        &body,
    )
}
