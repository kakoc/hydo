//! The plain part of the page (Milestone 220): what broke, when it happens, whether it is a bug,
//! what hydo saw, and how to see it again, in words for a developer who knows their own system
//! and not hydo. The words come from the recorded files only: the schedule in words
//! (`hydo_schedule::describe`), the advice of the failure's class (`hydo_explore::advice`), the
//! run's events, and the project's `hydo.yaml` when the page can find it.

use super::page::{code, copy_block, esc, md};
use hydo_explore::advice::{self, What};
use hydo_results::{Engine, Run, TimelineEvent};
use hydo_schedule::describe::{self, KnobInfo, Part};
use hydo_schedule::Schedule;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What the page knows about the project the results belong to.
#[derive(Clone, Debug, Default)]
pub struct Project {
    /// The settings of `knobs:` (from the manifest, else from the runs' notes).
    pub knobs: Vec<KnobInfo>,
    /// The system's name for a sentence (`Redis`; `Your system` when no name is known).
    pub system: String,
    /// The project's name as written (`redis`; `your system` when no name is known).
    pub label: String,
    /// A name is known.
    pub named: bool,
    /// The project's folder name, when the image is the one `hydo build` wrote there.
    pub folder: Option<String>,
    /// The results folder as a path in the project's folder (`hydo-out/runA`), when it is in
    /// one.
    pub results_rel: Option<String>,
    /// The replay needs `hydo build` first (a CI run's image is in its cache, not in the project).
    pub needs_build: bool,
    /// `workload.kind: test`.
    pub test: bool,
    /// What each check of the manifest says it checks, by id.
    pub checks: BTreeMap<String, String>,
}

/// The folder that holds `hydo-out` (or `.hydo`) above `p`, at most four levels up.
fn project_dir_above(p: &Path) -> Option<PathBuf> {
    let mut cur = Some(p);
    for _ in 0..5 {
        let c = cur?;
        if matches!(
            c.file_name().and_then(|n| n.to_str()),
            Some("hydo-out") | Some(".hydo")
        ) {
            return c.parent().map(Path::to_path_buf);
        }
        cur = c.parent();
    }
    None
}

fn manifest_text(results: &Path, image: Option<&str>) -> Option<String> {
    if let Some(i) = image {
        if Path::new(i).is_file() {
            if let Ok(t) = hydo_engine::image::embedded_manifest(Path::new(i)) {
                return Some(t);
            }
        }
    }
    let abs = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = project_dir_above(&abs(results)) {
        dirs.push(d);
    }
    if let Some(d) = image.and_then(|i| project_dir_above(Path::new(i))) {
        dirs.push(d);
    }
    for d in dirs {
        for m in [d.join("hydo.yaml"), d.join("hydo").join("hydo.yaml")] {
            if let Ok(t) = std::fs::read_to_string(&m) {
                return Some(t);
            }
        }
    }
    None
}

fn s_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// What the page knows about the project: its `hydo.yaml` (inside the image when the image file
/// is there, else next to the `hydo-out` folder that holds the results or the image), the image's
/// name, and the runs' notes (`knob R=everysec (Redis appendfsync)`).
pub fn project(results: &Path, image: Option<&str>, notes: &[&str]) -> Project {
    let mut p = Project::default();
    let mut name: Option<String> = None;
    if let Some(t) = manifest_text(results, image) {
        if let Ok(m) = hydo_manifest::parse_text(&t, "hydo.yaml", true) {
            name = m.get("name").and_then(s_of);
            p.test = m
                .get("workload")
                .and_then(|w| w.get("kind"))
                .and_then(Value::as_str)
                == Some("test");
            for k in m
                .get("knobs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(letter) = k
                    .get("token")
                    .and_then(Value::as_str)
                    .and_then(|t| t.chars().next())
                else {
                    continue;
                };
                p.knobs.push(KnobInfo {
                    letter,
                    name: k.get("name").and_then(s_of),
                    default: k.get("default").and_then(s_of),
                    safe: k.get("safe").and_then(s_of),
                    description: k.get("description").and_then(s_of),
                });
            }
            for key in ["invariants", "checks"] {
                for c in m.get(key).and_then(Value::as_array).into_iter().flatten() {
                    let (Some(id), Some(msg)) = (
                        c.get("id").and_then(s_of),
                        c.get("msg").or_else(|| c.get("description")).and_then(s_of),
                    ) else {
                        continue;
                    };
                    p.checks.insert(id, msg);
                }
            }
        }
    }
    // the runs' notes name a setting the manifest did not give
    for n in notes {
        let Some(rest) = n
            .strip_prefix("hydo-ob: knob ")
            .or_else(|| n.strip_prefix("knob "))
        else {
            continue;
        };
        let Some((tok, label)) = rest.split_once(" (") else {
            continue;
        };
        let Some(letter) = tok.chars().next() else {
            continue;
        };
        if p.knobs.iter().any(|k| k.letter == letter) {
            continue;
        }
        p.knobs.push(KnobInfo {
            letter,
            name: Some(label.trim_end_matches(')').to_string()),
            ..Default::default()
        });
    }
    let from_image = image.and_then(super::system_of);
    p.named = name.is_some() || from_image.is_some();
    p.label = name.or(from_image).unwrap_or_else(|| "your system".into());
    p.system = capitalize(&p.label);
    p.folder = match image {
        Some(i) => project_dir_above(Path::new(i)),
        None => project_dir_above(&std::fs::canonicalize(results).unwrap_or_default()),
    }
    .and_then(|d| d.file_name().map(|f| f.to_string_lossy().into_owned()));
    let abs = std::fs::canonicalize(results).unwrap_or_default();
    p.results_rel = project_dir_above(&abs).and_then(|d| {
        abs.strip_prefix(&d)
            .ok()
            .map(|r| r.to_string_lossy().into_owned())
    });
    p
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ------------------------------------------------------------------ what failed

/// What failed in a run, with the manifest's description of a check when the run's own files do
/// not have it.
pub fn what_of(run: &Run, pr: &Project) -> What {
    let mut w = advice::what_of(&run.verdict.text, &run.findings);
    if let What::Check {
        id: Some(id),
        description,
        ..
    } = &mut w
    {
        if description.is_none() {
            *description = pr.checks.get(id.as_str()).cloned().or_else(|| {
                run.properties.as_ref().and_then(|t| {
                    t.properties
                        .iter()
                        .find(|p| &p.id == id && !p.message.trim().is_empty())
                        .map(|p| p.message.clone())
                })
            });
        }
    }
    w
}

/// The class of a run: the explorer's when it recorded one, else the rule on its schedule.
pub fn class_of(run: &Run, pr: &Project) -> String {
    if let Some(c) = run.class.as_ref().filter(|c| !c.is_empty()) {
        return c.clone();
    }
    match Schedule::parse(&run.schedule) {
        Ok(s) => advice::class_of(&s, &pr.knobs, pr.test).to_string(),
        Err(_) => "durable-defaults".into(),
    }
}

/// The label of a verdict for the line under the title.
pub fn verdict_pill(run: &Run, what: &What) -> (&'static str, &'static str) {
    if run.verdict.kind == "ok" {
        return ("ok", "Every check held");
    }
    match what {
        What::Check { .. } => ("bad", "Your check failed"),
        What::NoRestart { .. } => ("bad", "A service did not start again"),
        What::Error { .. } => ("warn", "The run stopped with an error"),
        What::ErrorLine { .. } => ("warn", "A new error line"),
    }
}

// ------------------------------------------------------------------ the schedule in words

/// "When it happens": the schedule's sentences and the outcome, in order (plain words with
/// backquotes).
pub fn steps(schedule: &str, pr: &Project, outcome: Option<String>) -> Vec<String> {
    let mut items: Vec<String> = describe::describe_text(schedule, &pr.knobs)
        .into_iter()
        .filter(|s| s.part != Part::Quiet)
        .map(|s| s.sentence)
        .collect();
    if items.is_empty() {
        items.push("Your workload runs with no fault.".into());
    }
    if let Some(o) = outcome {
        items.push(o);
    }
    items
}

/// A sentence without its explanation in parentheses at the end ("The disk fails some reads and
/// writes with an error (rate 3: a larger number means more errors)." is "The disk fails some
/// reads and writes with an error."), for the short lists of an exploration's problems.
pub fn short(sentence: &str) -> String {
    let s = sentence.replace(", as in a crash,", "");
    match s.rfind(" (") {
        Some(i) if s.ends_with(").") && s[i..].contains(": ") => format!("{}.", &s[..i]),
        _ => s,
    }
}

/// The steps of a problem of an exploration: [`steps`] in [`short`] sentences, without the
/// sentence on how a disk with a cache behaves (`disk=cut`: the power cut is its own step).
pub fn short_steps(schedule: &str, pr: &Project, outcome: Option<String>) -> Vec<String> {
    let mut items: Vec<String> = describe::describe_text(schedule, &pr.knobs)
        .into_iter()
        .filter(|s| s.part != Part::Quiet && s.token != "disk=cut")
        .map(|s| short(&s.sentence))
        .collect();
    if items.is_empty() {
        items.push("Your workload runs with no fault.".into());
    }
    if let Some(o) = outcome {
        items.push(o);
    }
    items
}

/// How many of a schedule's words change the run (its faults and settings).
pub fn fault_count(schedule: &str, pr: &Project) -> usize {
    describe::describe_text(schedule, &pr.knobs)
        .iter()
        .filter(|s| s.part != Part::Quiet)
        .count()
}

/// The numbered steps as HTML (without a card around them).
pub fn steps_list(items: &[String]) -> String {
    let mut s = String::from("<ol class=\"steps\">\n");
    for i in items {
        s.push_str(&format!("<li>{}</li>\n", md(i)));
    }
    s.push_str("</ol>\n");
    s
}

/// "When it happens": the schedule's sentences, numbered, and the outcome.
pub fn steps_html(schedule: &str, pr: &Project, outcome: Option<String>) -> String {
    format!(
        "<div class=\"card\">{}</div>\n",
        steps_list(&steps(schedule, pr, outcome))
    )
}

/// "In hydo's words: `disk=cut` = ..., `C=12` = ...": the words of the schedule that change
/// something in this run, with their glosses, in the order written. A disk fault's choice of
/// requests (`dseed=`) is there only with a disk fault, and hydo's own machinery (`rb=`) never.
pub fn glossary(schedule: &str, pr: &Project) -> String {
    let Ok(s) = Schedule::parse(schedule) else {
        return String::new();
    };
    let faulty = matches!(
        s.disk(),
        hydo_schedule::Disk::IoErr(_) | hydo_schedule::Disk::Lying(_)
    );
    let mut parts: Vec<String> = Vec::new();
    for t in &s.host {
        let d = describe::host(t, &s);
        let keep = match t {
            hydo_schedule::HostToken::Dseed(_) => faulty,
            hydo_schedule::HostToken::Disk(hydo_schedule::Disk::None) => false,
            _ => d.part != Part::Quiet,
        };
        if keep {
            parts.push(format!("{} = {}", code(&d.token), md(&d.gloss)));
        }
    }
    for t in &s.guest {
        let d = describe::guest(t, &pr.knobs);
        parts.push(format!("{} = {}", code(&d.token), md(&d.gloss)));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(
        "<p class=\"small\">In hydo's words: {}.</p>\n",
        parts.join(", ")
    )
}

/// The image argument of a command: none when the image is the one `hydo build` wrote in the
/// project's folder, else the image's path when it is relative, else its file name (the full
/// path is machine-specific and stays in the details).
fn image_arg(pr: &Project, image: Option<&str>) -> Option<String> {
    if pr.folder.is_some() {
        return None;
    }
    let i = image?;
    let p = Path::new(i);
    Some(if p.is_absolute() {
        p.file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| i.to_string())
    } else {
        i.to_string()
    })
}

/// The command that runs `schedule` again: from the project's folder when the image is the one
/// `hydo build` wrote there, else with the image.
pub fn replay_command(
    pr: &Project,
    image: Option<&str>,
    engine: Option<&Engine>,
    schedule: &str,
) -> String {
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
    if let Some(i) = image_arg(pr, image) {
        w.push(format!("--image {}", crate::inspect::work::quote(&i)));
    }
    w.push(format!("\"{schedule}\""));
    w.join(" ")
}

/// The sentence before a replay command (plain words with backquotes).
pub fn needs_first_words(pr: &Project, image: Option<&str>) -> String {
    match (&pr.folder, image_arg(pr, image)) {
        (Some(f), _) if pr.needs_build => format!(
            "In your project folder (`{f}`), after `hydo build`, this failure repeats exactly, \
             every time you run it:"
        ),
        (Some(f), _) => format!(
            "In your project folder (`{f}`), this failure repeats exactly, every time you run it:"
        ),
        (None, Some(i)) => {
            format!("Next to the image `{i}`, this failure repeats exactly, every time you run it:")
        }
        (None, None) => "This failure repeats exactly, every time you run it:".to_string(),
    }
}

/// [`needs_first_words`] for a run that passed: the run repeats, and there is no failure to name
/// (0.4.0's docs review: a passing run read "this failure repeats exactly").
pub fn of_a_pass(words: &str) -> String {
    words
        .replace("this failure repeats", "this run repeats")
        .replace("This failure repeats", "This run repeats")
}

/// The line before a replay command.
pub fn needs_first(pr: &Project, image: Option<&str>) -> String {
    format!("<p>{}</p>\n", md(&needs_first_words(pr, image)))
}

/// The replay command with what it needs first, its copy button, and the words of its
/// schedule.
pub fn see_again(
    pr: &Project,
    image: Option<&str>,
    engine: Option<&Engine>,
    schedule: &str,
) -> String {
    let mut s = needs_first(pr, image);
    s.push_str(&copy_block(&replay_command(pr, image, engine, schedule)));
    s.push_str(&glossary(schedule, pr));
    s
}

// ------------------------------------------------------------------ what hydo saw

/// The steps the workload had confirmed and the ones without an answer, from the event that ends
/// the workload (`end: power cut, acknowledged [0 1 2], failed []`).
pub fn acked(evs: &[TimelineEvent]) -> Option<(Vec<u64>, Vec<u64>)> {
    let list = |t: &str, key: &str| -> Option<Vec<u64>> {
        let i = t.find(key)?;
        let rest = &t[i + key.len()..];
        let end = rest.find(']')?;
        Some(
            rest[..end]
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect(),
        )
    };
    evs.iter()
        .filter(|e| e.kind == "end" && e.conversation == "run")
        .find_map(|e| {
            let a = list(&e.text, "acknowledged [")?;
            let f = list(&e.text, "failed [").unwrap_or_default();
            Some((a, f))
        })
}

/// "before step 12" in a note of the run (counted from 0) as the page says it: "after 12 steps"
/// (a cut after the workload's last step has no next step to name; 0.3.4's docs review).
fn step_of(head: &str) -> String {
    let Some(i) = head.find("before step ") else {
        return String::new();
    };
    let n: Option<u64> = head[i + "before step ".len()..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|x| x.parse().ok());
    match n {
        Some(n) => format!(" {}", describe::after(n)),
        None => String::new(),
    }
}

/// A note of the run in words (`hydo-ob: POWER CUT before step 12 (all services frozen)`: "The
/// power was cut after 12 steps."). None for notes a reader does not need.
pub fn fault_words(text: &str) -> Option<String> {
    let t = text.strip_prefix("hydo-ob: ").unwrap_or(text).trim();
    // the part before the first parenthesis or colon: what happened, and when
    let head = t.split(" (").next().unwrap_or(t);
    let head = head.split(": ").next().unwrap_or(head).trim();
    let step = step_of(head);
    let word = |p: &str| head.strip_prefix(p).map(str::trim);
    let svc_of = |rest: &str| -> String {
        rest.split_whitespace()
            .next()
            .map(|s| s.trim_end_matches("'s").to_string())
            .unwrap_or_default()
    };
    Some(if word("POWER CUT at the reaction").is_some() {
        "The power was cut when your system reported the event hydo waited for.".into()
    } else if word("POWER CUT").is_some() {
        format!("The power was cut{step}.")
    } else if let Some(r) = word("POWER LOSS of") {
        format!("`{}` lost power{step}.", svc_of(r))
    } else if let Some(r) = word("KILLED") {
        format!("`{}` was killed{step}.", svc_of(r))
    } else if word("PAUSED nothing").is_some() {
        "The pause found no thread to stop.".into()
    } else if let Some(r) = word("PAUSED") {
        format!("`{}` was paused{step}.", svc_of(r))
    } else if let Some(r) = word("RESUMED") {
        let at_end = if head.ends_with("at the end") {
            " at the end".to_string()
        } else {
            step.clone()
        };
        format!("`{}` continued{at_end}.", svc_of(r))
    } else if let Some(r) = word("NET FAULT on") {
        format!("The network to `{}` was faulted{step}.", svc_of(r))
    } else if let Some(r) = word("LINK FAULT") {
        let mut it = r.split(" <-> ");
        let a = it.next().unwrap_or("").trim();
        let b = it
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("");
        format!("The link between `{a}` and `{b}` was faulted{step}.")
    } else if word("PARTITION").is_some() {
        format!("The network was split{step}.")
    } else if let Some(r) = word("CLOCK JUMP of") {
        let secs: i64 = r
            .split_whitespace()
            .next()
            .and_then(|x| x.trim_end_matches('s').parse().ok())
            .unwrap_or(0);
        if secs == 0 {
            format!("The system clock was set to a new time{step}.")
        } else {
            format!(
                "The system clock jumped {} by {}{step}.",
                if secs < 0 { "back" } else { "forward" },
                describe::duration(secs.unsigned_abs())
            )
        }
    } else if let Some(r) = word("CORRUPTED") {
        format!("hydo damaged a file of `{}`{step}.", svc_of(r))
    } else if word("PROTOCOL FAULT").is_some() {
        format!("An outside service started to misbehave{step}.")
    } else if word("BUGGIFY").is_some() {
        "The fault points in your code were switched on.".into()
    } else if let Some(r) = word("restarting") {
        format!("hydo started `{}` again{step}.", svc_of(r))
    } else {
        return None;
    })
}

fn line(mark: &str, class: &str, html: &str) -> String {
    format!("<div><span class=\"mark {class}\">{mark}</span>{html}</div>\n")
}

/// The failing check's command when the run recorded it (the diagnosis's exits).
fn check_command(run: &Run) -> Option<&str> {
    let d = run.diagnosis.as_ref()?;
    d.exits
        .iter()
        .rev()
        .find(|x| x.status != 0 && x.output.as_deref().is_some_and(|o| !o.trim().is_empty()))
        .and_then(|x| x.command.as_deref())
}

/// The items a check reports, named the way its output names them: when the output names an
/// item whose number is one of the confirmed steps (`key k:0 lost`, step 0 confirmed), the
/// confirmed steps by that name (`k:0 … k:11`).
pub fn items_of(found: &str, acked: &[u64]) -> Option<String> {
    let (first, last) = (*acked.first()?, *acked.last()?);
    for w in found.split_whitespace() {
        let digits = w.len() - w.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || digits == w.len() {
            continue;
        }
        let (prefix, num) = w.split_at(w.len() - digits);
        if num.parse::<u64>().is_ok_and(|n| acked.contains(&n)) {
            return Some(if first == last {
                format!("`{prefix}{first}`")
            } else {
                format!("`{prefix}{first}` … `{prefix}{last}`")
            });
        }
    }
    None
}

/// What a check found, as a short phrase: `key k:0 lost` is "`key k:0` was gone".
fn found_phrase(found: &str) -> String {
    for suf in [" lost", " missing"] {
        if let Some(x) = found.strip_suffix(suf) {
            return format!("`{x}` was gone");
        }
    }
    format!("your check found `{found}`")
}

/// One line of "What hydo saw": its mark, the mark's colour, the words (plain with backquotes),
/// a muted note after them, and lines quoted under it (a service's last log lines).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub mark: &'static str,
    pub class: &'static str,
    pub words: String,
    pub note: Option<String>,
    pub quote: Vec<String>,
}

impl Seen {
    fn new(mark: &'static str, class: &'static str, words: String) -> Seen {
        Seen {
            mark,
            class,
            words,
            ..Seen::default()
        }
    }
}

/// The most log lines a "What hydo saw" line quotes, and the longest one.
const LOG_LINES: usize = 2;
const LOG_CHARS: usize = 160;

/// The failing service's last log lines from a diagnosis whose replay read them: (the service,
/// the lines, at most [`LOG_LINES`], each cut to [`LOG_CHARS`] characters).
pub fn log_tail(d: &hydo_results::Diagnosis) -> Option<(String, Vec<String>)> {
    let svc = d.service.clone()?;
    let want = format!("the log of {svc}");
    let e = d.evidence.iter().find(|e| e.what == want)?;
    let lines: Vec<String> = e
        .text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(LOG_LINES)
        .rev()
        .map(|l| {
            super::page::cut(
                &l.split_whitespace().collect::<Vec<_>>().join(" "),
                LOG_CHARS,
            )
        })
        .collect();
    (!lines.is_empty()).then_some((svc, lines))
}

/// The line that quotes a service's last log lines; `how` says where they come from.
pub fn log_seen(svc: &str, lines: Vec<String>, how: &str) -> Seen {
    Seen {
        note: Some(how.to_string()),
        quote: lines,
        ..Seen::new("▤", "m-note", format!("`{svc}`'s log ends"))
    }
}

/// "What hydo saw" for one run: what the workload had confirmed, the faults as they happened,
/// the restart, the services that ended with the last lines of the failing one's log, and what
/// the check found.
pub fn seen(run: &Run, evs: &[TimelineEvent], what: &What, pr: &Project) -> Vec<Seen> {
    let mut s: Vec<Seen> = Vec::new();
    let acked = acked(evs);
    let found = match what {
        What::Check { found, .. } => found.clone(),
        _ => None,
    };
    if let Some((a, f)) = &acked {
        let n = a.len();
        let items = found.as_deref().and_then(|x| items_of(x, a));
        s.push(Seen::new(
            "✔",
            "m-ok",
            match (n, items) {
                (0, _) => "No step of your workload was confirmed before the faults.".to_string(),
                (n, Some(i)) => format!(
                    "{} confirmed {i} ({n} step{})",
                    pr.system,
                    if n == 1 { "" } else { "s" }
                ),
                (n, None) => format!(
                    "{} confirmed {n} step{} of your workload",
                    pr.system,
                    if n == 1 { "" } else { "s" }
                ),
            },
        ));
        if !f.is_empty() {
            s.push(Seen::new(
                "?",
                "m-warn",
                format!(
                    "{} step{} got no answer",
                    f.len(),
                    if f.len() == 1 { "" } else { "s" }
                ),
            ));
        }
    }
    let mut n = 0;
    for e in evs.iter().filter(|e| e.kind == "fault") {
        if let Some(w) = fault_words(&e.text) {
            n += 1;
            if n <= 4 {
                s.push(Seen::new(
                    "⚡",
                    "m-warn",
                    w.trim_end_matches('.').to_string(),
                ));
            }
        }
    }
    if evs.iter().any(|e| e.conversation == "recovery") {
        s.push(Seen::new(
            "↻",
            "m-note",
            format!(
                "The machine started again, and {} started from what was on its disk",
                pr.system
            ),
        ));
    }
    for e in evs.iter().filter(|e| e.kind == "exit").take(3) {
        let Some((who, st)) = e.text.split_once(" exited: status ") else {
            continue;
        };
        if who == "workload" || st.trim() == "0" {
            continue;
        }
        s.push(Seen::new(
            "✘",
            "m-bad",
            format!("`{who}` exited with status {}", st.trim()),
        ));
    }
    if let Some((svc, lines)) = run.diagnosis.as_ref().and_then(log_tail) {
        s.push(log_seen(&svc, lines, "from a replay"));
    }
    // a broken value property (Milestone 291): the first key's history up to the break
    for f in run
        .findings
        .iter()
        .filter(|f| f.kind == hydo_results::kind::VALUES && f.status != "ok")
    {
        let property = f.id.strip_prefix("values ").unwrap_or(&f.id);
        let quote: Vec<String> = match f.details.get("output") {
            Some(serde_json::Value::Array(ls)) => ls
                .iter()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        let n = f
            .details
            .get("broken_keys")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        let of = f
            .details
            .get("keys")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(n);
        s.push(Seen {
            quote,
            ..Seen::new(
                "✘",
                "m-bad",
                format!(
                    "Your value property `{property}` broke for {n} of the {of} keys it judged. \
                     It says that {}",
                    f.message
                ),
            )
        });
    }
    if run.verdict.kind == "ok" {
        s.push(Seen::new("✔", "m-ok", "Every check held".into()));
    } else {
        let stops = matches!(what, What::Check { found: Some(_), .. })
            && check_command(run).is_some_and(|c| c.contains("break"));
        let words = match what {
            What::Check { found: Some(f), .. } => format!("`{f}`"),
            What::Check { .. } => "Your check failed".into(),
            What::NoRestart { service: Some(x) } => format!("`{x}` did not start again"),
            What::NoRestart { service: None } => "A service did not start again".into(),
            What::Error { text } => format!("The run stopped with `{text}`"),
            What::ErrorLine { service, line } => format!("`{service}` wrote `{line}`"),
        };
        s.push(Seen {
            note: stops.then(|| "the check stops at the first one".to_string()),
            ..Seen::new("✘", "m-bad", words)
        });
    }
    s
}

/// The lines of "What hydo saw" as HTML (without a card around them).
pub fn seen_lines(lines: &[Seen]) -> String {
    let mut s = String::new();
    for l in lines {
        let mut h = md(&l.words);
        if let Some(n) = &l.note {
            h.push_str(&format!(" <span class=\"muted\">({})</span>", esc(n)));
        }
        if !l.quote.is_empty() {
            h.push_str(&format!(
                "<pre class=\"log\">{}</pre>",
                esc(&l.quote.join("\n"))
            ));
        }
        s.push_str(&line(l.mark, l.class, &h));
    }
    s
}

/// "What hydo saw" for one run, as a card.
pub fn evidence_html(run: &Run, evs: &[TimelineEvent], what: &What, pr: &Project) -> String {
    format!(
        "<div class=\"card evidence\">\n{}</div>\n",
        seen_lines(&seen(run, evs, what, pr))
    )
}

/// The disk fault of a schedule as the first sentence of a lead, when the run had one: the
/// disk's faults are no event of the run, so the lead says them first.
fn disk_sentence(schedule: &str) -> Option<&'static str> {
    match Schedule::parse(schedule).ok()?.disk() {
        hydo_schedule::Disk::IoErr(_) => {
            Some("The disk failed some of the system's reads and writes with an error.")
        }
        hydo_schedule::Disk::Lying(_) => {
            Some("The disk threw away some writes that it had confirmed.")
        }
        _ => None,
    }
}

/// The lead paragraph of a run: what the workload had confirmed, the first faults, and what the
/// check found.
pub fn lead_of(run: &Run, evs: &[TimelineEvent], what: &What, pr: &Project) -> String {
    md(&lead_words(run, evs, what, pr))
}

/// [`lead_of`] in plain words with backquotes.
pub fn lead_words(run: &Run, evs: &[TimelineEvent], what: &What, pr: &Project) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some((a, f)) = acked(evs) {
        let ran = a.len() + f.len();
        parts.push(match (ran, a.len()) {
            (0, _) => "Your workload had not confirmed anything yet.".into(),
            (r, k) if r == k => format!(
                "Your workload ran {r} step{} and {} confirmed {}.",
                if r == 1 { "" } else { "s" },
                pr.system,
                if r == 1 {
                    "it".to_string()
                } else {
                    format!("all {r}")
                }
            ),
            (r, k) => format!(
                "Your workload ran {r} steps and {} confirmed {k}.",
                pr.system
            ),
        });
    }
    let faults: Vec<String> = evs
        .iter()
        .filter(|e| e.kind == "fault")
        .filter_map(|e| fault_words(&e.text))
        .take(2)
        .collect();
    for (i, f) in faults.iter().enumerate() {
        parts.push(if i == 0 && !parts.is_empty() {
            format!("Then {}", lower_first(f))
        } else {
            f.clone()
        });
    }
    // a disk fault is no event: say it first, so that the lead starts with what happened
    if let Some(d) = disk_sentence(&run.schedule) {
        parts.insert(0, d.to_string());
    }
    let recovered = evs.iter().any(|e| e.conversation == "recovery");
    let after = if recovered {
        format!("When {} started again, ", pr.system)
    } else if parts.is_empty() {
        String::new()
    } else {
        "Then ".to_string()
    };
    if run.verdict.kind == "ok" {
        parts.push(format!("{after}every check held."));
    } else {
        parts.push(match what {
            What::Check { id: Some(id), .. } if advice::is_values(what) => format!(
                "{after}a call saw another value than your value property `{}` allows.",
                id.strip_prefix("values ").unwrap_or(id)
            ),
            What::Check { found: Some(f), .. } => format!("{after}{}.", found_phrase(f)),
            What::Check { .. } => format!("{after}your check failed."),
            What::NoRestart { service: Some(x) } => format!("{after}`{x}` did not start again."),
            What::NoRestart { service: None } => format!("{after}a service did not start again."),
            What::Error { text } => format!("{after}the run stopped with the error `{text}`."),
            What::ErrorLine { service, line } => format!("{after}`{service}` wrote `{line}`."),
        });
    }
    let mut s = parts.join(" ");
    if !s.starts_with('`') {
        s = capitalize(&s);
    }
    s
}

fn lower_first(s: &str) -> String {
    if s.starts_with('`') || s.starts_with("hydo") {
        return s.to_string();
    }
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The outcome line at the end of "When it happens".
pub fn outcome_line(run: &Run, what: &What, recovered: bool) -> String {
    if run.verdict.kind == "ok" {
        return "At the end, every check holds.".into();
    }
    let when = if recovered {
        "After the restart"
    } else {
        "At the end"
    };
    match what {
        What::Check { .. } if advice::is_values(what) => {
            "While it runs, a recorded value breaks your value property.".into()
        }
        // a check's sentence that ends with its own period ends the line (0.4.0's trial: "it
        // showed no error..")
        What::Check { found: Some(f), .. } if f.ends_with('.') => {
            format!("{when}, your check finds `{f}`")
        }
        What::Check { found: Some(f), .. } => format!("{when}, your check finds `{f}`."),
        What::Check { .. } => format!("{when}, your check fails."),
        What::NoRestart { service: Some(x) } => {
            format!("After the faults, `{x}` does not start again.")
        }
        What::NoRestart { service: None } => {
            "After the faults, a service does not start again.".into()
        }
        What::Error { .. } => "The run stops with an error before your checks run.".into(),
        What::ErrorLine { service, .. } => format!("`{service}` writes an error line."),
    }
}

/// One sentence of what a service that did not start again did in a run: "`redis` exited with
/// status 1 during the disk errors, and again after hydo restarted it."
pub fn exit_story(evs: &[TimelineEvent], svc: &str, disk_errors: bool) -> Option<String> {
    let restart = evs
        .iter()
        .position(|e| e.text.contains(&format!("restarting {svc}")));
    let exits: Vec<(usize, String)> = evs
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == "exit")
        .filter_map(|(i, e)| {
            let (who, st) = e.text.split_once(" exited: status ")?;
            (who == svc && st.trim() != "0").then(|| (i, st.trim().to_string()))
        })
        .collect();
    let (_, st) = exits.first()?;
    let before = restart.is_some_and(|r| exits.iter().any(|(i, _)| *i < r));
    let after = restart.is_some_and(|r| exits.iter().any(|(i, _)| *i > r));
    let during = if disk_errors {
        " during the disk errors"
    } else {
        ""
    };
    Some(match (before, after) {
        (true, true) => {
            format!("`{svc}` exited with status {st}{during}, and again after hydo restarted it.")
        }
        (false, true) => format!("`{svc}` exited with status {st} after hydo restarted it."),
        _ => format!("`{svc}` exited with status {st}{during}."),
    })
}

/// The fault kinds of a set of schedules, in words ("power cuts, crashes and clock jumps").
pub fn fault_kinds<'a>(
    schedules: impl Iterator<Item = &'a str>,
    pr: &Project,
) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = Vec::new();
    for s in schedules {
        for d in describe::describe_text(s, &pr.knobs) {
            if let Some(k) = d.kind {
                if !kinds.contains(&k) {
                    kinds.push(k);
                }
            }
        }
    }
    kinds
}

/// The faults that hydo declares as reachability records (`fault.*`) in plain words, with the
/// explorer option that draws them.
pub fn untried_words(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "fault.paused" => ("pauses of a service", "--new-faults"),
        "fault.pause_mid_request" => ("a pause in the middle of a request", "--new-faults"),
        "fault.clock_back" => ("the clock jumping back", "--new-faults"),
        "fault.clock_fwd" => ("the clock jumping forward", "--new-faults"),
        "fault.oneway_req" => ("faults on the requests only", "--new-faults"),
        "fault.oneway_resp" => ("faults on the answers only", "--new-faults"),
        _ => return None,
    })
}

/// The label and colour of an attention level, and the line that says what it means.
pub fn attention_pill(a: advice::Attention) -> String {
    let class = match a {
        advice::Attention::Look => "bad",
        advice::Attention::ExpectedForSetting => "warn",
        advice::Attention::Expected => "ok",
    };
    super::page::pill(class, a.words())
}
