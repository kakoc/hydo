//! The terminal's words for an exploration (Milestone 230): `hydo report` prints the problems as
//! the page shows them, and `hydo explore` in a terminal ends with two lines that count them. Both
//! come from the page's own story ([`super::exploration_story`]), so the terminal, the page and
//! the advice never disagree. The technical report stays in `report.md` and in
//! `hydo report --details`.

use super::page::plain;
use super::{story, ExStory};
use crate::ctx::{pjoin, OUT};
use crate::inspect::drive::RunFiles;
use crate::inspect::work::quote;
use hydo_explore::advice;
use hydo_results::{Exploration, Run, Stream};
use hydo_schedule::Schedule;
use std::path::Path;

/// The width the sentences are wrapped to (commands are never wrapped).
const WIDTH: usize = 80;
/// The indent of a problem's lines.
const INDENT: &str = "   ";
/// The indent of a problem's numbered steps, under its own number.
const STEPS: &str = "     ";

/// `text` without backquotes, wrapped to [`WIDTH`] with `indent` before every line.
fn wrap(text: &str, indent: &str) -> String {
    hang(indent, text, indent)
}

/// `text` without backquotes, wrapped to [`WIDTH`]: `first` before the first line and `indent`
/// before the others.
fn hang(first: &str, text: &str, indent: &str) -> String {
    let mut out = String::new();
    let mut line = String::new();
    let mut lead = first;
    for w in plain(text).split_whitespace() {
        if !line.is_empty()
            && lead.chars().count() + line.chars().count() + 1 + w.chars().count() > WIDTH
        {
            out.push_str(lead);
            out.push_str(&line);
            out.push('\n');
            line.clear();
            lead = indent;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        out.push_str(lead);
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// A `hydo` command on the exploration's directory: without it when it is the default one.
fn on_dir(cmd: &str, shown: &str) -> String {
    if shown == pjoin(OUT, "explore") {
        cmd.to_string()
    } else {
        format!("{cmd} {}", quote(shown))
    }
}

/// The sentence of how the failing runs replayed.
fn replayed(doc: &Exploration, shown: &str) -> Option<String> {
    let s = &doc.summary;
    if s.failures == 0 {
        return None;
    }
    let rep = s.reproduced.unwrap_or(0);
    Some(if rep == s.failures {
        if s.failures == 1 {
            "The failing run failed again the same way when hydo replayed it.".to_string()
        } else {
            "Each failing run failed again the same way when hydo replayed it.".to_string()
        }
    } else {
        format!(
            "{} of the failing runs did not fail the same way when hydo replayed them: {} lists \
             them.",
            s.failures - rep,
            on_dir("hydo report --details", shown)
        )
    })
}

/// `hydo report` of an exploration in the terminal.
pub fn exploration(dir: &Path, doc: &Exploration, events: Option<&Stream>, shown: &str) -> String {
    let st = super::exploration_story(dir, doc, events, shown);
    render(&st, doc, shown, hydo_results::stats_of(dir).as_ref())
}

fn render(
    st: &ExStory,
    doc: &Exploration,
    shown: &str,
    stats: Option<&hydo_results::StatsDoc>,
) -> String {
    let s = &doc.summary;
    let mut o = String::new();
    // the headline and what the runs had
    let mut head = st.title.clone();
    if s.failures > 0 {
        head.push_str(&format!(" ({} of {} runs failed)", s.failures, st.total));
    }
    o.push_str(&head);
    o.push('\n');
    let mut lead = String::new();
    if !st.kinds.is_empty() {
        lead.push_str(&format!(
            "Faults tried: {}.",
            hydo_schedule::describe::and_list(
                &st.kinds.iter().map(|k| k.to_string()).collect::<Vec<_>>()
            )
        ));
    }
    if let Some(r) = replayed(doc, shown) {
        if !lead.is_empty() {
            lead.push(' ');
        }
        lead.push_str(&r);
    }
    o.push_str(&wrap(&lead, ""));
    // the problems
    for (n, it) in st.items.iter().enumerate() {
        let mut label = it.problem.attention.words().to_string();
        if let Some((k, of)) = it.runs {
            label.push_str(&format!(
                ", {k} of {of} run{}",
                if of == 1 { "" } else { "s" }
            ));
        }
        o.push_str(&item(n + 1, it, &label, shown, n + 1));
    }
    // Milestone 355: runs whose reaction never fired tested nothing of it
    if let Some(n) = s.unfired_reactions.filter(|n| *n > 0) {
        o.push('\n');
        o.push_str(&wrap(
            &format!(
                "Reactions that never fired: in {n} of {} runs the schedule's reaction (A=) never \
                 fired, because its event did not come after it was armed. Those runs did not \
                 test the reaction; each run's record in events.jsonl names it.",
                st.total
            ),
            "",
        ));
    }
    tail(&mut o, st, shown, stats);
    o
}

/// One problem's block: its number `n`, title and `label`, what happens, the steps, what hydo
/// saw, the command, and the lines that name the exploration `shown` (its problem `own`: the
/// number `hydo inspect diagnose` takes there). Milestone 345: also the merged report's.
pub(super) fn item(n: usize, it: &super::Item, label: &str, shown: &str, own: usize) -> String {
    let mut o = String::new();
    {
        o.push('\n');
        let num = format!("{n}. ");
        o.push_str(&hang(&num, &it.problem.title, INDENT));
        if INDENT.len() + label.chars().count() < WIDTH {
            o.push_str(&format!("{INDENT}{label}.\n"));
        } else {
            o.push_str(&wrap(&format!("{label}."), INDENT));
        }
        if !it.happens.is_empty() {
            o.push_str(&wrap(&it.happens, INDENT));
        }
        if !it.steps.is_empty() {
            let head = match it.whole_run {
                Some(w) => format!("When it happens ({w}):"),
                None => "When it happens:".to_string(),
            };
            o.push_str(&wrap(&head, INDENT));
            o.push_str(&numbered(&it.steps, STEPS));
        }
        o.push_str(&seen(&it.seen, INDENT));
        // one new kind is already in the sentence
        let list: &[(String, String, u64)] = if it.lines.len() > 1 { &it.lines } else { &[] };
        for (svc, line, runs) in list {
            let l = super::page::cut(line, 100);
            o.push_str(&format!(
                "{INDENT}- {svc}: {l} ({runs} run{})\n",
                if *runs == 1 { "" } else { "s" }
            ));
        }
        if it.more_lines > 0 {
            o.push_str(&format!("{INDENT}- and {} more\n", it.more_lines));
        }
        o.push_str(&wrap(&format!("Is it a bug? {}", it.problem.todo), INDENT));
        let again = if it.link == "errors" {
            "See them"
        } else {
            "See it again"
        };
        o.push_str(&format!("{INDENT}{again}: {}\n", it.command));
        // Milestone 324: an exploration keeps no run directory of a problem; say how to get one
        if let Some(c) = &it.run_command {
            o.push_str(&format!(
                "{INDENT}Why it failed: hydo inspect diagnose {} --problem {own} --text\n",
                crate::inspect::work::quote(shown),
            ));
            o.push_str(&format!("{INDENT}A run directory of it: {c}\n"));
        }
        if let Some(r) = &it.recording {
            o.push_str(&wrap(&r.words(shown), INDENT));
        }
        for k in &it.accept {
            o.push_str(&format!(
                "{INDENT}If it is expected: hydo ci accept {k} --reason \"…\"\n"
            ));
        }
        if !it.accept_note.is_empty() {
            o.push_str(&wrap(&it.accept_note, INDENT));
        }
    }
    o
}

/// What held, how much was explored, and what to run next.
fn tail(o: &mut String, st: &ExStory, shown: &str, stats: Option<&hydo_results::StatsDoc>) {
    // what held
    if let Some(h) = &st.held {
        o.push('\n');
        o.push_str(&wrap(&format!("What held: {}", h.words()), ""));
    }
    // Milestone 336: how much was explored, and whether more runs would show more
    if let Some(s) = stats {
        o.push_str("\nHow much was explored:\n");
        for l in hydo_explore::stats::short_text(s) {
            o.push_str(&hang(INDENT, &l, INDENT));
        }
    }
    // next
    o.push_str("\nNext:\n");
    if !st.items.is_empty() {
        o.push_str(&format!(
            "{INDENT}Each failure's timeline, as one web page: {}\n",
            on_dir("hydo report --html", shown)
        ));
    }
    o.push_str(&format!("{INDENT}More runs: {}\n", st.explore_more));
    if stats.is_some() {
        o.push_str(&format!(
            "{INDENT}The cost, the faults drawn and how many runs fit: {}\n",
            on_dir("hydo stats", shown)
        ));
    }
}

/// Numbered steps, each wrapped under its number.
fn numbered(items: &[String], indent: &str) -> String {
    let mut o = String::new();
    for (i, s) in items.iter().enumerate() {
        let num = format!("{indent}{}. ", i + 1);
        let more = " ".repeat(num.chars().count());
        o.push_str(&hang(&num, s, &more));
    }
    o
}

/// The lines of "What hydo saw", each with its mark; a quoted log line under its line, as it
/// is (log lines are never wrapped).
fn seen(lines: &[story::Seen], indent: &str) -> String {
    let mut o = String::new();
    for l in lines {
        let mut w = l.words.clone();
        if let Some(n) = &l.note {
            w.push_str(&format!(" ({n})"));
        }
        if !l.quote.is_empty() {
            w.push(':');
        }
        let first = format!("{indent}{} ", l.mark);
        let more = format!("{indent}  ");
        o.push_str(&hang(&first, &w, &more));
        for q in &l.quote {
            o.push_str(&format!("{indent}    | {q}\n"));
        }
    }
    o
}

/// `hydo report <run dir>` in the terminal: the page's plain part in words. The title, the lead,
/// "When it happens", "Is it a bug?", "What hydo saw" and "See it again", from the page's own
/// story code; `hydo report --details` prints the technical report.
pub fn run(dir: &Path, run: &Run, shown: &str) -> String {
    let evs = super::events_of(dir, run, &RunFiles::default());
    let notes: Vec<&str> = evs.iter().map(|e| e.text.as_str()).collect();
    let image = run.image.as_ref().map(|i| i.path.as_str());
    let pr = story::project(dir, image, &notes);
    let what = story::what_of(run, &pr);
    let class = story::class_of(run, &pr);
    let sched = Schedule::parse(&run.schedule).ok();
    let failed = run.verdict.kind != "ok";
    let problem = advice::problem(&class, &what, &pr.system, sched.as_ref(), &pr.knobs);
    let mut o = String::new();
    o.push_str(&hang(
        "",
        &if failed {
            problem.title.clone()
        } else {
            format!("{} passed every check in this run", pr.system)
        },
        "",
    ));
    o.push_str(&wrap(&story::lead_words(run, &evs, &what, &pr), ""));
    let recovered = evs.iter().any(|e| e.conversation == "recovery");
    o.push_str("\nWhen it happens:\n");
    o.push_str(&numbered(
        &story::steps(
            &run.schedule,
            &pr,
            Some(story::outcome_line(run, &what, recovered)),
        ),
        INDENT,
    ));
    if failed {
        o.push('\n');
        o.push_str(&wrap(
            &format!(
                "Is it a bug? {}. {} {}",
                problem.attention.words(),
                problem.why,
                problem.todo
            ),
            "",
        ));
    }
    o.push_str("\nWhat hydo saw:\n");
    o.push_str(&seen(&story::seen(run, &evs, &what, &pr), INDENT));
    // Milestone 355: a reaction that never fired, which the run did not test
    for w in run
        .warnings
        .iter()
        .filter(|w| hydo_explore::results::is_unfired_warning(w))
    {
        o.push_str(&hang(&format!("{INDENT}! "), w, &format!("{INDENT}  ")));
    }
    o.push('\n');
    let words = story::needs_first_words(&pr, image);
    let words = if failed {
        words
    } else {
        story::of_a_pass(&words)
    };
    o.push_str(&wrap(&words, ""));
    o.push_str(&format!(
        "{INDENT}{}\n",
        story::replay_command(&pr, image, run.engine.as_ref(), &run.schedule)
    ));
    o.push_str("\nNext:\n");
    o.push_str(&format!(
        "{INDENT}As one web page: hydo report --html {}\n",
        quote(shown)
    ));
    if failed {
        let folder = pr.results_rel.clone().unwrap_or_else(|| shown.to_string());
        o.push_str(&format!(
            "{INDENT}Look inside at the moment it failed: hydo inspect start {} --at end\n",
            quote(&folder)
        ));
    }
    o.push_str(&format!(
        "{INDENT}The technical report: hydo report --details {}\n",
        quote(shown)
    ));
    o
}

/// The end of `hydo explore` in a terminal: how many runs failed and in how many ways, and the
/// command that shows them. None when the directory has no exploration.json.
pub fn explore_end(dir: &Path, shown: &str) -> Option<String> {
    let (doc, events) = match hydo_results::read_dir(dir).ok()? {
        hydo_results::Results::Exploration {
            doc: Some(doc),
            events,
            ..
        } => (doc, events),
        _ => return None,
    };
    let st = super::exploration_story(dir, &doc, events.as_ref(), shown);
    let s = &doc.summary;
    let runs = format!(
        "hydo ran your system {} time{}",
        st.total,
        if st.total == 1 { "" } else { "s" }
    );
    let mut first = if s.failures == 0 {
        format!("{runs}: every run passed your checks.")
    } else {
        format!(
            "{runs}: {} failed{}.",
            s.failures,
            match st.failure_problems {
                1 if s.failures > 1 => ", all in the same way".to_string(),
                0 | 1 => String::new(),
                n => format!(", in {n} different ways"),
            }
        )
    };
    if let Some(r) = replayed(&doc, shown) {
        first.push(' ');
        first.push_str(&r);
    }
    if st.items.len() > st.failure_problems {
        first.push_str(
            " Your services also wrote error lines that your list of known errors does not have.",
        );
    }
    let mut o = wrap(&first, "");
    let report = on_dir("hydo report", shown);
    o.push_str(&if st.items.is_empty() {
        format!("What held: {report}\n")
    } else {
        format!(
            "See them: {report}   (as one web page: {})\n",
            on_dir("hydo report --html", shown)
        )
    });
    Some(o)
}
