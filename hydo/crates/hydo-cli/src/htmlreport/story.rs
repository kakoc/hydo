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

/// "When it happens": the schedule's sentences, numbered, and the outcome.
pub fn steps_html(schedule: &str, pr: &Project, outcome: Option<String>) -> String {
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
    let mut s = String::from("<div class=\"card\"><ol class=\"steps\">\n");
    for i in items {
        s.push_str(&format!("<li>{}</li>\n", md(&i)));
    }
    s.push_str(
        "</ol>\n<p class=\"small\">A step is one numbered part of your workload \
         (<code>workload.steps</code> in <code>hydo.yaml</code>), counted from 1.</p></div>\n",
    );
    s
}

/// "In hydo's words: `|` separates ..., `disk=cut` = ...": every word of the schedule with its
/// gloss, in the order written.
pub fn glossary(schedule: &str, pr: &Project) -> String {
    let parsed = Schedule::parse(schedule).ok();
    let mut parts: Vec<String> = Vec::new();
    let mut add = |tok: String, gloss: String| {
        parts.push(format!("{} = {}", code(&tok), md(&gloss)));
    };
    match &parsed {
        Some(s) => {
            for t in &s.host {
                let d = describe::host(t, s);
                add(d.token, d.gloss);
            }
            for t in &s.guest {
                let d = describe::guest(t, &pr.knobs);
                add(d.token, d.gloss);
            }
        }
        None => {
            for d in describe::describe_text(schedule, &pr.knobs) {
                add(d.token, d.gloss);
            }
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(
        "<p class=\"small\">In hydo's words: the text in quotes names the faults of the run. \
         {} separates the machine's settings, before it, from what happens in the run, after \
         {}. {}.</p>\n",
        code("|"),
        code("run"),
        parts.join("; ")
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

/// What a command needs first, in one line.
pub fn needs_first(pr: &Project, image: Option<&str>) -> String {
    match (&pr.folder, image_arg(pr, image)) {
        (Some(f), _) => format!(
            "<p>Run this in your project's folder ({}), after <code>hydo build</code> has made \
             the image there. The run repeats exactly, every time:</p>\n",
            code(f)
        ),
        (None, Some(i)) => format!(
            "<p>Run this where the image file {} is{}. The run repeats exactly, every time:</p>\n",
            code(&i),
            if image.is_some_and(|x| Path::new(x).is_absolute()) {
                " (its full path is in the details below)"
            } else {
                ""
            }
        ),
        (None, None) => {
            "<p>Run this in your project's folder, after <code>hydo build</code>. The run \
             repeats exactly, every time:</p>\n"
                .to_string()
        }
    }
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

/// `hydo ci accept` with what it does.
pub fn accept_html(key: &str) -> String {
    format!(
        "{}. It writes the finding, under its name {}, to <code>hydo/known-findings.yaml</code> \
         with your reason. CI then stops failing on it, and lists it for removal if it ever \
         passes.",
        code(&format!("hydo ci accept {key} --reason \"…\"")),
        code(key)
    )
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

/// Step numbers (counted from 0) in words, counted from 1: `1 to 12`, `1, 3 and 6`.
pub fn steps_words(v: &[u64]) -> String {
    if v.is_empty() {
        return "none".into();
    }
    let v: Vec<u64> = v.iter().map(|x| x + 1).collect();
    let contiguous = v.windows(2).all(|w| w[1] == w[0] + 1);
    if contiguous && v.len() > 2 {
        return format!("{} to {}", v[0], v[v.len() - 1]);
    }
    if v.len() > 8 {
        return format!(
            "{}, … and {}",
            v[..4]
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            v[v.len() - 1]
        );
    }
    describe::and_list(&v.iter().map(u64::to_string).collect::<Vec<_>>())
}

/// "before step 12" in a note of the run (counted from 0) as the page says it: "after 12 steps".
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

/// "What hydo saw" for one run: only what the other parts do not say. The steps without an
/// answer, the restart after a power cut, the services that ended, and why the check names one
/// item when it stops at the first one. None when there is nothing to add.
pub fn evidence_html(
    run: &Run,
    evs: &[TimelineEvent],
    what: &What,
    pr: &Project,
) -> Option<String> {
    let mut s = String::new();
    if let Some((_, f)) = acked(evs) {
        if !f.is_empty() {
            s.push_str(&line(
                "?",
                "m-warn",
                &esc(&format!(
                    "Step{} {} got no answer, so the workload counts {} as unknown.",
                    if f.len() == 1 { "" } else { "s" },
                    steps_words(&f),
                    if f.len() == 1 { "it" } else { "them" }
                )),
            ));
        }
    }
    if evs.iter().any(|e| e.conversation == "recovery") {
        s.push_str(&line(
            "↻",
            "m-note",
            &esc(&format!(
                "The machine started again, and {} started from what was on its disk.",
                pr.system
            )),
        ));
    }
    let mut ended = 0;
    for e in evs.iter().filter(|e| e.kind == "exit") {
        let Some((who, st)) = e.text.split_once(" exited: status ") else {
            continue;
        };
        if who == "workload" || st.trim() == "0" {
            continue;
        }
        ended += 1;
        if ended <= 3 {
            s.push_str(&line(
                "✗",
                "m-bad",
                &md(&format!(
                    "`{who}` ended with the exit status {}.",
                    st.trim()
                )),
            ));
        }
    }
    // a check that stops at its first problem names one item only
    if let (What::Check { found: Some(f), .. }, Some(cmd)) = (what, check_command(run)) {
        if cmd.contains("break") {
            s.push_str(&line(
                "i",
                "m-note",
                &md(&format!(
                    "Your check stops at the first problem it finds, so it reports only `{f}`; \
                     later items can be missing too."
                )),
            ));
        }
    }
    (!s.is_empty()).then(|| format!("<div class=\"card evidence\">\n{s}</div>\n"))
}

/// The evidence of one failing run of an exploration: the notes of the run that a reader needs
/// (the faults, the restart, a service that stopped again) and the exit statuses.
pub fn run_evidence(evs: &[TimelineEvent]) -> Option<String> {
    let mut s = String::new();
    let mut n = 0;
    for e in evs {
        let w = match e.kind.as_str() {
            "exit" => {
                let Some((who, st)) = e.text.split_once(" exited: status ") else {
                    continue;
                };
                if who == "workload" || st.trim() == "0" {
                    continue;
                }
                format!("`{who}` ended with the exit status {}.", st.trim())
            }
            _ => match fault_words(&e.text) {
                Some(w) => w,
                None => continue,
            },
        };
        n += 1;
        if n > 8 {
            break;
        }
        s.push_str(&line("·", "m-note", &md(&w)));
    }
    (!s.is_empty()).then(|| format!("<div class=\"card evidence\">\n{s}</div>\n"))
}

/// The lead paragraph of a run: what the workload had confirmed, the first faults, and what the
/// check found.
pub fn lead_of(run: &Run, evs: &[TimelineEvent], what: &What, pr: &Project) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some((a, _)) = acked(evs) {
        parts.push(match a.len() {
            0 => "Your workload ran, and no step of it was confirmed.".into(),
            1 => format!("Your workload ran, and {} confirmed 1 step.", pr.system),
            n => format!("Your workload ran, and {} confirmed {n} steps.", pr.system),
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
    let recovered = evs.iter().any(|e| e.conversation == "recovery");
    let after = if recovered {
        "When the machine started again, "
    } else {
        "Then "
    };
    if run.verdict.kind == "ok" {
        parts.push(format!("{after}every check held."));
    } else {
        parts.push(match what {
            What::Check { found: Some(f), .. } => format!("{after}your check found `{f}`."),
            What::Check { .. } => format!("{after}your check failed."),
            What::NoRestart { service: Some(x) } => format!("{after}`{x}` did not start again."),
            What::NoRestart { service: None } => format!("{after}a service did not start again."),
            What::Error { text } => format!("{after}the run stopped with the error `{text}`."),
            What::ErrorLine { service, line } => format!("{after}`{service}` wrote `{line}`."),
        });
    }
    md(&parts.join(" "))
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

/// The outcome line at the end of "When it happens" (what it found is in the lead).
pub fn outcome_line(run: &Run, what: &What) -> String {
    if run.verdict.kind == "ok" {
        return "At the end, your checks run, and every check holds.".into();
    }
    match what {
        What::Check { .. } => "At the end, your check runs and fails.".into(),
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

/// The fault kinds every one of the schedules has.
pub fn common_kinds<'a>(
    mut schedules: impl Iterator<Item = &'a str>,
    pr: &Project,
) -> Vec<&'static str> {
    let Some(first) = schedules.next() else {
        return Vec::new();
    };
    let mut common = fault_kinds(std::iter::once(first), pr);
    for s in schedules {
        let k = fault_kinds(std::iter::once(s), pr);
        common.retain(|x| k.contains(x));
    }
    common
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

/// The line under a label: what it means, and what it means for CI.
pub fn attention_meaning(a: advice::Attention) -> String {
    format!("<p class=\"small\">{}</p>\n", esc(a.meaning()))
}
