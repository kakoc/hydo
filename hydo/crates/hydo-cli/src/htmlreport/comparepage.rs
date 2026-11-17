//! The page of a `hydo compare-versions` directory (Milestone 298): for each schedule, whether
//! the two versions agree, and where they first part, in plain words; the details of every
//! stream that differs are folded at the bottom.

use super::page::{badge, code, copy_block, cut, esc, facts, md, table};
use super::{footer, h2, h3, share, top};
use hydo_results::{CompareDoc, CompareSchedule};

fn verdict_cell(v: Option<&str>) -> String {
    match v {
        None => badge("warn", "no run"),
        Some(v) => {
            let k = hydo_engine::protocol::verdict_kind(v);
            let class = match k {
                "ok" => "ok",
                "violation" => "bad",
                _ => "warn",
            };
            format!("{} {}", badge(class, k), esc(&cut(v, 160)))
        }
    }
}

fn schedule_card(n: usize, s: &CompareSchedule, la: &str, lb: &str) -> String {
    let (class, word) = match s.identical {
        Some(true) => ("ok", "the same"),
        Some(false) => ("bad", "different"),
        None => ("warn", "not compared"),
    };
    let mut h = format!(
        "<div class=\"card\" id=\"s{n}\">\n<h3>{} {}</h3>\n<p class=\"small\">{}</p>\n",
        badge(class, word),
        code(&s.schedule),
        esc(&s.from)
    );
    if let Some(f) = &s.shrunk_from {
        h.push_str(&format!(
            "<p class=\"small\">Shrunk from {} with {} candidate runs on each version.</p>\n",
            code(f),
            s.shrink_runs.unwrap_or(0)
        ));
    }
    let summary = s
        .summary
        .replacen("the first difference: ", "The first difference: ", 1);
    h.push_str(&format!("<p>{}</p>\n", md(&summary)));
    h.push_str(&facts(&[
        (format!("A ({la})"), verdict_cell(s.verdict_a.as_deref())),
        (format!("B ({lb})"), verdict_cell(s.verdict_b.as_deref())),
    ]));
    for (l, r) in [("A", &s.replay_a), ("B", &s.replay_b)] {
        if let Some(r) = r.as_deref().filter(|_| s.identical != Some(true)) {
            h.push_str(&format!("<p class=\"small\">Run it again on {l}:</p>\n"));
            h.push_str(&copy_block(r));
        }
    }
    h.push_str("</div>\n");
    h
}

fn details(doc: &CompareDoc) -> String {
    let (la, lb) = (&doc.a.version, &doc.b.version);
    let mut s = h3("versions", "The two versions");
    let v = |x: &hydo_results::CompareVersion| {
        let mut t = esc(&x.version);
        if let Some(c) = &x.commit {
            t.push_str(&format!(" at {}", code(c)));
        }
        if let Some(sha) = &x.image_sha256 {
            t.push_str(&format!(", image {}", code(&sha[..sha.len().min(16)])));
        }
        if let Some(w) = &x.why {
            t.push_str(&format!(" ({})", esc(w)));
        }
        t
    };
    s.push_str(&facts(&[
        ("A".into(), v(&doc.a)),
        ("B".into(), v(&doc.b)),
        ("Made from".into(), esc(&doc.source)),
        (
            "Engine".into(),
            esc(doc.engine.backend.as_deref().unwrap_or("-")),
        ),
        ("Levels".into(), esc(&doc.levels.join(", "))),
        (
            "Normalized".into(),
            if doc.normalize.is_empty() {
                "nothing".into()
            } else {
                doc.normalize
                    .iter()
                    .map(|r| code(r))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ),
    ]));
    for (n, sc) in doc.schedules.iter().enumerate() {
        if sc.differences.is_empty() {
            continue;
        }
        s.push_str(&h3(&format!("d{}", n + 1), &format!("Schedule {}", n + 1)));
        s.push_str(&format!("<p>{}</p>\n", code(&sc.schedule)));
        let rows: Vec<(String, Vec<String>)> = sc
            .differences
            .iter()
            .map(|d| {
                (
                    String::new(),
                    vec![
                        esc(&d.level),
                        esc(&format!(
                            "{}{}",
                            d.stream,
                            d.conversation
                                .as_deref()
                                .map(|c| format!(" ({c})"))
                                .unwrap_or_default()
                        )),
                        esc(&format!("{}", d.index + 1)),
                        esc(d.step_a.as_deref().or(d.step_b.as_deref()).unwrap_or("")),
                        d.a.as_deref()
                            .map(|x| code(&cut(x, 200)))
                            .unwrap_or_default(),
                        d.b.as_deref()
                            .map(|x| code(&cut(x, 200)))
                            .unwrap_or_default(),
                        esc(&format!("{} / {}", d.only_a, d.only_b)),
                    ],
                )
            })
            .collect();
        s.push_str(&table(
            &[
                "Level",
                "Stream",
                "Record",
                "Steps",
                &format!("A ({la})"),
                &format!("B ({lb})"),
                "Only in A / B",
            ],
            &rows,
        ));
        for (l, dir) in [("A", &sc.dir_a), ("B", &sc.dir_b)] {
            if let Some(d) = dir {
                s.push_str(&format!(
                    "<p class=\"small\">Run directory of {l}: {}</p>\n",
                    code(d)
                ));
            }
        }
    }
    s
}

/// The whole page.
pub fn page(doc: &CompareDoc, shown: &str) -> String {
    let (la, lb) = (&doc.a.version, &doc.b.version);
    let differ = doc
        .schedules
        .iter()
        .filter(|s| s.identical == Some(false))
        .count();
    let title = if differ == 0 {
        format!("{la} and {lb} behave the same")
    } else {
        format!("{lb} behaves differently from {la}")
    };
    let mut lead = format!(
        "hydo ran {} schedule{} on version A ({la}) and on version B ({lb}) and compared what the \
         two runs of each answered and stored. hydo repeats a run exactly, so every difference \
         comes from the versions, not from chance.",
        doc.schedules.len(),
        if doc.schedules.len() == 1 { "" } else { "s" }
    );
    if let Some(sr) = &doc.search {
        lead.push_str(&format!(
            " The schedules were drawn from the seeds {} ({} drawn, {} differ), and each \
             difference was shrunk.",
            sr.seeds, sr.drawn, sr.differ
        ));
    }
    let mut body = top("hydo compare-versions", &esc(&title), &esc(&lead));
    body.push_str(&h2("schedules", "Each schedule"));
    let mut shown_any = false;
    for (n, s) in doc.schedules.iter().enumerate() {
        if doc.search.is_some() && s.identical == Some(true) {
            continue;
        }
        shown_any = true;
        body.push_str(&schedule_card(n + 1, s, la, lb));
    }
    if !shown_any {
        body.push_str("<p>No drawn schedule gave runs that differ.</p>\n");
    }
    body.push_str(&format!("<p>{}</p>\n", esc(&doc.summary)));
    body.push_str(&share(&details(doc)));
    body.push_str(&footer(shown));
    super::page::document(&format!("{title} (hydo report)"), &body)
}
