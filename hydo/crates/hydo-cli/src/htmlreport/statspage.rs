//! The section "How much was explored" of an exploration's page (Milestone 336): the short words
//! of `hydo stats`, one small chart per judged measure of the curve of new behaviour, drawn as
//! inline SVG (the page loads nothing and runs no script for it; each point's numbers are in its
//! `<title>`, which a browser shows on hover), the numbers as a table, and the faults drawn.

use super::page::{code, esc, md, table};
use hydo_explore::stats::{measure_words, short_text, FINGERPRINTS};
use hydo_results::StatsDoc;

/// The width and height of one chart, in SVG units, and its margins.
const W: f64 = 300.0;
const H: f64 = 84.0;
const LEFT: f64 = 30.0;
const RIGHT: f64 = 8.0;
const TOP: f64 = 10.0;
const BOTTOM: f64 = 20.0;

/// One measure's chart: its count after each point, as a step line, with the window of the
/// judgement shaded.
fn chart(s: &StatsDoc, m: &str) -> String {
    let pts: Vec<(u64, u64)> = s
        .curve
        .points
        .iter()
        .map(|p| (p.runs, p.counts.get(m).copied().unwrap_or(0)))
        .collect();
    let total = pts.last().map(|p| p.0).unwrap_or(0).max(1);
    let ymax = pts.iter().map(|p| p.1).max().unwrap_or(0).max(1);
    let x = |n: u64| LEFT + (W - LEFT - RIGHT) * n as f64 / total as f64;
    let y = |c: u64| TOP + (H - TOP - BOTTOM) * (1.0 - c as f64 / ymax as f64);
    let mut path = format!("M{:.1},{:.1}", x(0), y(0));
    for (n, c) in &pts {
        // a step: the count holds until the run that raised it
        path.push_str(&format!(" H{:.1} V{:.1}", x(*n), y(*c)));
    }
    let win = s.curve.window.min(total);
    let wx = x(total - win);
    let mut o = format!(
        "<figure class=\"card\" style=\"margin:8px 0;padding:10px 14px\">\n<figcaption><b>{}</b> \
         <span class=\"muted\">{} after {} runs</span></figcaption>\n<svg viewBox=\"0 0 {W} {H}\" \
         width=\"100%\" style=\"max-width:{W}px;display:block\" role=\"img\" aria-label=\"{}: {} \
         after {} runs\">\n",
        esc(&cap(measure_words(m))),
        pts.last().map(|p| p.1).unwrap_or(0),
        total,
        esc(measure_words(m)),
        pts.last().map(|p| p.1).unwrap_or(0),
        total
    );
    // the window of the judgement, then the axis, then the line and its points
    o.push_str(&format!(
        "<rect x=\"{wx:.1}\" y=\"{TOP}\" width=\"{:.1}\" height=\"{:.1}\" style=\"fill:var(--code)\"><title>the last {win} runs, which the judgement looks at</title></rect>\n",
        x(total) - wx,
        H - TOP - BOTTOM
    ));
    o.push_str(&format!(
        "<line x1=\"{LEFT}\" y1=\"{b:.1}\" x2=\"{:.1}\" y2=\"{b:.1}\" style=\"stroke:var(--line);stroke-width:1\"/>\n",
        W - RIGHT,
        b = H - BOTTOM
    ));
    for (v, ty) in [(0, y(0)), (ymax, y(ymax))] {
        o.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\" style=\"fill:var(--muted);font-size:10px\">{v}</text>\n",
            LEFT - 4.0,
            ty + 3.0
        ));
    }
    for (n, anchor) in [(0, "start"), (total, "end")] {
        o.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"{anchor}\" style=\"fill:var(--muted);font-size:10px\">{}</text>\n",
            x(n),
            H - 6.0,
            if n == 0 { "0 runs".to_string() } else { n.to_string() }
        ));
    }
    o.push_str(&format!(
        "<path d=\"{path}\" style=\"fill:none;stroke:var(--note);stroke-width:2;stroke-linejoin:round\"/>\n"
    ));
    for g in s.curve.grew_at.get(m).into_iter().flatten() {
        let c = pts.iter().find(|p| p.0 >= *g).map(|p| p.1).unwrap_or(0);
        o.push_str(&format!(
            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"4\" style=\"fill:var(--note);stroke:var(--card);stroke-width:2\"><title>new at run {g}</title></circle>\n",
            x(*g),
            y(c)
        ));
    }
    o.push_str("</svg>\n</figure>\n");
    o
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The section's HTML (inside its card).
pub fn html(s: &StatsDoc, shown: &str) -> String {
    let mut o = String::new();
    for l in short_text(s) {
        o.push_str(&format!("<p>{}</p>\n", md(&l)));
    }
    let judged: Vec<&String> = s
        .curve
        .measures
        .iter()
        .filter(|m| *m != FINGERPRINTS)
        .collect();
    o.push_str("<div style=\"display:flex;flex-wrap:wrap;gap:12px\">\n");
    for m in &judged {
        o.push_str(&chart(s, m));
    }
    o.push_str("</div>\n");
    o.push_str(
        "<p class=\"small\">Each dot is a run that showed something no earlier run showed; the \
         shaded part is the last runs the judgement looks at. A flat line says that more runs \
         drawn the same way mostly repeat these runs. It does not say that the \
         system has no more bugs: other fault kinds, other options or other checks can still \
         find more.</p>\n",
    );
    // the numbers as a table
    // the plain part of the page leaves the fingerprints out (they are in `hydo stats`)
    let judged_names: Vec<&String> = judged.clone();
    let mut head = vec!["Runs"];
    head.extend(judged_names.iter().map(|m| measure_words(m)));
    let rows: Vec<(String, Vec<String>)> = s
        .curve
        .points
        .iter()
        .filter(|p| {
            p.runs.is_power_of_two() || Some(p.runs) == s.curve.points.last().map(|x| x.runs)
        })
        .map(|p| {
            let mut cells = vec![p.runs.to_string()];
            cells.extend(
                judged_names
                    .iter()
                    .map(|m| p.counts.get(*m).copied().unwrap_or(0).to_string()),
            );
            (String::new(), cells)
        })
        .collect();
    o.push_str(&format!(
        "<details><summary class=\"small\">The curve as numbers</summary>\n{}</details>\n",
        table(&head, &rows)
    ));
    // the faults drawn
    let cv = &s.coverage;
    if !cv.kinds.is_empty() {
        let rows: Vec<(String, Vec<String>)> = cv
            .kinds
            .iter()
            .map(|k| {
                let ts: Vec<String> = k
                    .targets
                    .iter()
                    .take(6)
                    .map(|(t, n)| format!("{} {n}", code(t)))
                    .collect();
                (
                    String::new(),
                    vec![
                        esc(&k.words),
                        format!("{} of {}", k.runs, s.schedules),
                        ts.join(", "),
                        match (k.steps.len(), cv.steps) {
                            (0, _) => String::new(),
                            (n, Some(t)) => format!("{n} of {t}"),
                            (n, None) => n.to_string(),
                        },
                    ],
                )
            })
            .collect();
        o.push_str(&format!(
            "<details><summary class=\"small\">The faults drawn</summary>\n{}</details>\n",
            table(&["Kind", "Runs", "On", "Steps"], &rows)
        ));
    }
    o.push_str(&format!(
        "<p class=\"small\">{} prints the cost, the faults drawn and how many runs fit in a \
         budget.</p>\n",
        code(&if shown.is_empty() {
            "hydo stats".to_string()
        } else {
            format!("hydo stats {shown}")
        })
    ));
    o
}
