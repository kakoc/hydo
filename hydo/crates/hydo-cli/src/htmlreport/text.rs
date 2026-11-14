//! The terminal's words for an exploration (Milestone 230): `hydo report` prints the problems as
//! the page shows them, and `hydo explore` in a terminal ends with two lines that count them. Both
//! come from the page's own story ([`super::exploration_story`]), so the terminal, the page and
//! the advice never disagree. The technical report stays in `report.md` and in
//! `hydo report --details`.

use super::page::plain;
use super::ExStory;
use crate::ctx::{pjoin, OUT};
use crate::inspect::work::quote;
use hydo_results::{Exploration, Stream};
use std::path::Path;

/// The width the sentences are wrapped to (commands are never wrapped).
const WIDTH: usize = 80;
/// The indent of a problem's lines.
const INDENT: &str = "   ";

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
    render(&st, doc, shown)
}

fn render(st: &ExStory, doc: &Exploration, shown: &str) -> String {
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
        o.push('\n');
        let num = format!("{}. ", n + 1);
        o.push_str(&hang(&num, &it.problem.title, INDENT));
        let mut label = it.problem.attention.words().to_string();
        if let Some((k, of)) = it.runs {
            label.push_str(&format!(
                ", {k} of {of} run{}",
                if of == 1 { "" } else { "s" }
            ));
        }
        o.push_str(&format!("{INDENT}{label}.\n"));
        o.push_str(&wrap(&it.happens, INDENT));
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
    }
    // what held
    if let Some(h) = &st.held {
        o.push('\n');
        o.push_str(&wrap(&format!("What held: {}", h.words()), ""));
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
    if let Some(k) = &st.accept {
        o.push_str(&format!(
            "{INDENT}Accept an expected problem, so that it no longer fails your CI: hydo ci accept \
             {k} --reason \"…\"\n"
        ));
    }
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
