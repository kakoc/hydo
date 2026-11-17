//! `hydo ci page --store DIR [-o FILE]`: the history store as one web page (docs/design/ci.md
//! section 5.3, Milestone 286), in the plain words of `hydo report --html`.
//!
//! For each project of the store the page shows:
//!
//! - each finding across commits and branches: when it was first seen, when it was fixed,
//!   whether it came back, whether it is accepted, its minimal schedule, the line that replays
//!   it, the `hydo ci accept` line, and the report page of the run that last saw it when the
//!   store keeps one (`hydo ci record --report`);
//! - a timeline: one row per finding, one column per recorded run of a branch;
//! - the runs per commit, with their seeds, budget, engine and result.
//!
//! The page is made from the store's files only, with the style sheet and the one small script
//! of `hydo report --html`, and it loads nothing from the network. The runs of a pull request
//! may come from untrusted code (a fork), so every text is escaped and only `http` and `https`
//! links of a run are kept.

use super::page::{self, code, copy_block, esc, facts, pill, table};
use crate::ci::comment::{accept_line, replay_line};
use crate::ci::history::{self, Recorded};
use crate::ctx::{die, take};
use hydo_results::{ci_state, CiFinding, CiLedger, CiRun};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The most runs one timeline shows (the latest ones).
const MAX_COLUMNS: usize = 30;
/// The most runs the table of runs lists (the latest ones).
const MAX_RUNS: usize = 200;

pub const USAGE: &str = "usage: hydo ci page --store DIR [-o FILE]
  one web page of the history store (default DIR/index.html): each finding across commits and
  branches, the runs per commit with their seeds and budget, and each finding's replay line";

/// `hydo ci page`.
pub fn cmd_page(argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let store = take(&mut args, "--store").unwrap_or_else(|| die(USAGE));
    let out = take(&mut args, "-o").or_else(|| take(&mut args, "--out"));
    if !args.is_empty() {
        die(USAGE);
    }
    let store = PathBuf::from(store);
    if !store.join("v1").is_dir() {
        die(&format!(
            "hydo ci page: {} holds no history (no v1/ directory; hydo ci record writes one)",
            store.display()
        ));
    }
    let file = out
        .map(PathBuf::from)
        .unwrap_or_else(|| store.join("index.html"));
    let links = link_base(&store, &file);
    let html = page_of(&store, &store.display().to_string(), &links).unwrap_or_else(|e| die(&e));
    if let Err(e) = std::fs::write(&file, &html) {
        die(&format!("{}: {e}", file.display()));
    }
    println!(
        "hydo ci page: wrote {} ({} KiB): open it in a browser, or serve the history branch",
        file.display(),
        html.len().div_ceil(1024)
    );
    0
}

/// How the page links to the store's files: relative when the page is in the store's root.
fn link_base(store: &Path, file: &Path) -> String {
    let parent = file.parent().unwrap_or(Path::new("."));
    let same = match (parent.canonicalize(), store.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => parent == store,
    };
    if same {
        String::new()
    } else {
        let s = store.canonicalize().unwrap_or_else(|_| store.to_path_buf());
        format!("file://{}/", s.display())
    }
}

/// The branch the page leads with: `main` or `master` when the store has one, else the branch
/// with the most recorded runs.
pub fn default_branch(l: &CiLedger) -> Option<String> {
    for b in ["main", "master"] {
        if l.branches.contains_key(b) {
            return Some(b.to_string());
        }
    }
    l.branches
        .iter()
        .max_by_key(|(_, b)| b.runs)
        .map(|(n, _)| n.clone())
}

fn branch_rank(b: &str, default: Option<&str>) -> (u8, String) {
    let r = if Some(b) == default {
        0
    } else if b.starts_with("pull/") || b.starts_with("pr-") || b.starts_with("pr/") {
        2
    } else {
        1
    };
    (r, b.to_string())
}

/// A link of a run that the page may show: `http` and `https` only.
fn safe_url(u: &str) -> Option<&str> {
    (u.starts_with("https://") || u.starts_with("http://"))
        .then_some(u)
        .filter(|u| !u.chars().any(|c| c.is_whitespace() || c == '"'))
}

fn short(c: &str) -> String {
    c.chars().take(10).collect()
}

/// What one run did to one finding, as the timeline's cell shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cell {
    /// First seen on the branch.
    New,
    /// Seen failing again.
    Failing,
    /// Proven fixed by replay.
    Fixed,
    /// Seen failing after it was fixed.
    Back,
    /// Not found by the run, and still open (absence proves nothing).
    NotFound,
    /// Not open on the branch at that run.
    Empty,
}

impl Cell {
    fn html(self) -> String {
        match self {
            Cell::New => pill("bad", "new"),
            Cell::Failing => pill("warn", "fails"),
            Cell::Fixed => pill("ok", "fixed"),
            Cell::Back => pill("bad", "back"),
            Cell::NotFound => {
                "<span class=\"muted\" title=\"not found by this run; still open\">·</span>".into()
            }
            Cell::Empty => String::new(),
        }
    }
}

/// One finding on one branch, from the fold of its runs.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub title: String,
    pub open: bool,
    pub accepted: Option<String>,
    pub first_seen: Option<String>,
    pub fixed_at: Vec<String>,
    pub back_at: Vec<String>,
    pub last_seen: Option<String>,
    pub seen: u64,
    pub minimal: String,
    /// The run that last saw it failing (its index in the branch's runs).
    pub last_run: Option<usize>,
    pub first_time: Option<String>,
}

/// One branch's line: its runs in order, the cells of each finding, and each finding's track.
#[derive(Default)]
pub struct Line<'a> {
    pub runs: Vec<&'a Recorded>,
    pub cells: BTreeMap<String, Vec<Cell>>,
    pub tracks: BTreeMap<String, Track>,
    pub order: Vec<String>,
}

/// The fold of a branch's runs, as `hydo ci record` folded them, with what each run did.
pub fn line<'a>(branch: &str, runs: &[&'a Recorded]) -> Line<'a> {
    let mut l = Line {
        runs: runs.to_vec(),
        ..Line::default()
    };
    let mut ledger = CiLedger::default();
    for (i, r) in runs.iter().enumerate() {
        let at = r.run.commit.as_deref().map(short);
        let events = history::fold(&mut ledger, &r.run, branch);
        let failing: BTreeMap<&str, &CiFinding> = r
            .run
            .findings
            .iter()
            .filter(|f| history::seen_failing(f))
            .map(|f| (f.key.as_str(), f))
            .collect();
        for e in &events {
            if !l.cells.contains_key(&e.key) {
                l.cells.insert(e.key.clone(), vec![Cell::Empty; runs.len()]);
                l.order.push(e.key.clone());
            }
            let t = l.tracks.entry(e.key.clone()).or_default();
            match e.event.as_str() {
                "new" => {
                    t.first_seen = at.clone();
                    t.first_time = r.run.started.clone().or_else(|| r.entry.recorded.clone());
                    // a pull request's first run of a finding that the target branch had fixed
                    let back = failing.get(e.key.as_str()).is_some_and(|f| {
                        f.state == ci_state::REAPPEARED
                            || f.accepted
                                .as_ref()
                                .is_some_and(|a| a.state == ci_state::REAPPEARED)
                    });
                    if back {
                        t.back_at.push(at.clone().unwrap_or_default());
                    }
                    l.cells.get_mut(&e.key).expect("a row")[i] =
                        if back { Cell::Back } else { Cell::New };
                }
                "reappeared" => {
                    t.back_at.push(at.clone().unwrap_or_default());
                    l.cells.get_mut(&e.key).expect("a row")[i] = Cell::Back;
                }
                "fixed" => {
                    t.fixed_at.push(at.clone().unwrap_or_default());
                    l.cells.get_mut(&e.key).expect("a row")[i] = Cell::Fixed;
                }
                _ => {}
            }
        }
        for (k, row) in l.cells.iter_mut() {
            if row[i] != Cell::Empty {
                continue;
            }
            let open = ledger
                .branches
                .get(branch)
                .and_then(|b| b.findings.get(k))
                .is_some_and(|f| f.state == "open");
            row[i] = if failing.contains_key(k.as_str()) {
                Cell::Failing
            } else if open {
                Cell::NotFound
            } else {
                Cell::Empty
            };
        }
        for (k, f) in &failing {
            if let Some(t) = l.tracks.get_mut(*k) {
                t.last_run = Some(i);
                t.minimal = f.minimal.clone();
                t.accepted = f.accepted.as_ref().map(|a| a.reason.clone());
            }
        }
    }
    if let Some(b) = ledger.branches.get(branch) {
        for (k, f) in &b.findings {
            if let Some(t) = l.tracks.get_mut(k) {
                t.title = f.title.clone();
                t.open = f.state == "open";
                if !f.accepted {
                    t.accepted = None;
                } else if t.accepted.is_none() {
                    t.accepted = Some(String::new());
                }
                t.last_seen = f.last_seen.clone();
                t.seen = f.seen;
                if t.minimal.is_empty() {
                    t.minimal = f.minimal.clone();
                }
            }
        }
    }
    l
}

fn state_pill(t: &Track) -> String {
    let mut s = if !t.open {
        pill("ok", "Fixed")
    } else if !t.back_at.is_empty() {
        pill("bad", "Back again")
    } else {
        pill("bad", "Open")
    };
    if t.open && t.accepted.is_some() {
        s.push_str(&pill("note", "Accepted"));
    }
    s
}

fn commits(v: &[String]) -> String {
    v.iter().map(|c| code(c)).collect::<Vec<_>>().join(", ")
}

fn report_link(links: &str, project: &str, r: &Recorded) -> Option<String> {
    let rep = r.entry.report.as_deref()?;
    // a path the store wrote: reports/<safe name>.html
    if rep.contains("..") || !rep.starts_with("reports/") {
        return None;
    }
    Some(format!(
        "<a href=\"{}\">the run's report</a>",
        esc(&format!("{links}v1/{}/{rep}", history::safe_name(project)))
    ))
}

/// `2026-11-17T16:55:53Z` as `2026-11-17 16:55:53 UTC`.
fn utc_words(t: &str) -> String {
    t.replace('T', " ").trim_end_matches('Z').to_string() + " UTC"
}

/// When a run started: its own start (`started`, Milestone 301), else the time `hydo ci record`
/// recorded it, said as such (a run made before Milestone 301 has no start).
fn when(r: &Recorded) -> String {
    match (r.run.started.as_deref(), r.entry.recorded.as_deref()) {
        (Some(t), _) => utc_words(t),
        (None, Some(t)) => format!("recorded {}", utc_words(t)),
        (None, None) => "-".into(),
    }
}

/// The time a run is ordered by: its start, else its record.
fn order_time(r: &Recorded) -> Option<&str> {
    r.run.started.as_deref().or(r.entry.recorded.as_deref())
}

fn budget_words(run: &CiRun) -> String {
    let b = &run.budget;
    let mut s = match b.time_limit_s {
        Some(t) => crate::ci::report::wall(t),
        None => "none".into(),
    };
    match b.seeds_from.as_deref() {
        Some("history") => s.push_str(&format!(
            ", seeds from history ({:.1} s per seed)",
            b.seconds_per_seed.unwrap_or(0.0)
        )),
        Some(_) => s.push_str(", seeds from the range (nothing measured yet)"),
        None => {}
    }
    s
}

fn seeds_words(run: &CiRun) -> String {
    let b = &run.budget;
    let done = b
        .completed_seeds
        .as_deref()
        .filter(|c| *c != b.seeds)
        .map(|c| {
            if c.is_empty() {
                "none".to_string()
            } else {
                c.to_string()
            }
        });
    let mut s = match done {
        Some(d) => format!("{d} of {}", b.seeds),
        None => b.seeds.clone(),
    };
    if let Some(r) = b.range.as_deref().filter(|r| *r != b.seeds) {
        s.push_str(&format!(" (range {r})"));
    }
    s.push_str(&format!(", {}", b.strategy));
    s
}

fn result_words(run: &CiRun) -> String {
    match run.outcome.exit {
        0 => pill("ok", "passed"),
        1 => pill("bad", "failed"),
        _ => pill("warn", "not completed"),
    }
}

fn states_words(run: &CiRun) -> String {
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &run.findings {
        *by.entry(f.state.as_str()).or_default() += 1;
    }
    if by.is_empty() {
        return "none".into();
    }
    by.iter()
        .map(|(s, n)| format!("{n} {s}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn engine_words(run: &CiRun) -> String {
    let b = run.engine.backend.clone().unwrap_or_else(|| "?".into());
    match &run.engine.host_cpu {
        Some(c) if !c.model.is_empty() => format!("{b} on {}", c.model),
        Some(c) => format!("{b} on {}", c.vendor),
        None => b,
    }
}

/// The page of one store (`shown` names it on the page; `links` starts the store's paths).
pub fn page_of(store: &Path, shown: &str, links: &str) -> Result<String, String> {
    let projects = history::projects(store);
    let mut body = String::new();
    let mut sections = String::new();
    let mut open_total = 0usize;
    let mut runs_total = 0usize;
    let mut names = Vec::new();
    // the lines with open findings, and how many each has
    let mut per_line: Vec<(String, usize)> = Vec::new();
    for p in &projects {
        let ledger = history::read_ledger(store, p)?;
        let runs = history::recorded_runs(store, p)?;
        runs_total += runs.len();
        let name = ledger.project.clone();
        let name = if name.is_empty() { p.clone() } else { name };
        names.push(name.clone());
        let default = default_branch(&ledger);
        let mut branches: Vec<String> = ledger.branches.keys().cloned().collect();
        for r in &runs {
            if !branches.contains(&r.branch) {
                branches.push(r.branch.clone());
            }
        }
        branches.sort_by_key(|b| branch_rank(b, default.as_deref()));
        let lines: Vec<(String, Line)> = branches
            .iter()
            .map(|b| {
                let rs: Vec<&Recorded> = runs.iter().filter(|r| &r.branch == b).collect();
                (b.clone(), line(b, &rs))
            })
            .collect();
        // Milestone 301: every line's open findings, not only the first line's (0.4.0's trial:
        // "No open problem in redis" above two open findings of pull/1); a finding open on two
        // lines is one problem
        let mut keys: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (b, l) in &lines {
            let open: Vec<&str> = l
                .tracks
                .iter()
                .filter(|(_, t)| t.open && t.accepted.is_none())
                .map(|(k, _)| k.as_str())
                .collect();
            if !open.is_empty() {
                per_line.push((b.clone(), open.len()));
            }
            keys.extend(open);
        }
        open_total += keys.len();
        sections.push_str(&project_section(
            &name,
            p,
            &lines,
            default.as_deref(),
            links,
        ));
    }
    let title = match (names.len(), open_total) {
        (0, _) => "The history holds no project yet".to_string(),
        (1, 0) => format!("No open problem in {}", names[0]),
        (1, n) => format!(
            "{n} open problem{} in {}",
            if n == 1 { "" } else { "s" },
            names[0]
        ),
        (_, 0) => "No open problem in any project".to_string(),
        (_, n) => format!(
            "{n} open problem{} across the projects",
            if n == 1 { "" } else { "s" }
        ),
    };
    let open_on = if per_line.is_empty() {
        String::new()
    } else {
        let each: Vec<String> = per_line
            .iter()
            .map(|(b, n)| format!("{n} on {b}"))
            .collect();
        format!(
            "Open and not accepted: {}. ",
            hydo_schedule::describe::and_list(&each)
        )
    };
    let lead = format!(
        "{open_on}hydo recorded {} CI run{} here. A finding is open from the run that first saw it until a \
         run proves it fixed by replaying its schedule: a run that does not find it changes \
         nothing, because a bounded search that finds nothing proves nothing. Accepted findings \
         are open but expected, and they never fail a check.",
        runs_total,
        if runs_total == 1 { "" } else { "s" }
    );
    body.push_str(&format!(
        "<header>\n<p class=\"eyebrow\">hydo ci page · history {}</p>\n<h1>{}</h1>\n<p class=\"lead\">{}</p>\n</header>\n",
        code(shown),
        esc(&title),
        esc(&lead)
    ));
    body.push_str(&sections);
    body.push_str(&format!(
        "<footer>Made by <code>hydo ci page</code> from {}. The page is one file and loads \
         nothing from the network. Every schedule on it replays bit for bit on the image of its \
         commit with the same hydo version.</footer>\n",
        code(shown)
    ));
    Ok(page::document(&format!("{title} (hydo ci page)"), &body))
}

fn project_section(
    name: &str,
    dir: &str,
    lines: &[(String, Line)],
    default: Option<&str>,
    links: &str,
) -> String {
    let mut s = format!(
        "<h2 id=\"p-{}\">{}</h2>\n",
        esc(dir),
        esc(&format!("Project {name}"))
    );
    let nruns: usize = lines.iter().map(|(_, l)| l.runs.len()).sum();
    let ncommits = {
        let mut c: Vec<&str> = lines
            .iter()
            .flat_map(|(_, l)| l.runs.iter().filter_map(|r| r.run.commit.as_deref()))
            .collect();
        c.sort();
        c.dedup();
        c.len()
    };
    s.push_str(&format!(
        "<p>{} recorded run{} of {} commit{} on {} branch{}{}.</p>\n",
        nruns,
        if nruns == 1 { "" } else { "s" },
        ncommits,
        if ncommits == 1 { "" } else { "s" },
        lines.len(),
        if lines.len() == 1 { "" } else { "es" },
        default
            .map(|d| format!("; the main line is {}", code(d)))
            .unwrap_or_default()
    ));
    // the findings: one card per key, with its state on each branch
    let mut keys: Vec<String> = Vec::new();
    for (_, l) in lines {
        for k in &l.order {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
    }
    let main = lines.first();
    let rank = |k: &String| -> u8 {
        match main.and_then(|(_, l)| l.tracks.get(k)) {
            Some(t) if t.open && t.accepted.is_none() => 0,
            Some(t) if t.open => 1,
            Some(_) => 2,
            None => 3,
        }
    };
    keys.sort_by_key(rank);
    s.push_str(&format!("<h3 id=\"findings-{}\">Findings</h3>\n", esc(dir)));
    if keys.is_empty() {
        s.push_str("<div class=\"card\"><p>No recorded run found a problem.</p></div>\n");
    }
    for (n, k) in keys.iter().enumerate() {
        s.push_str(&finding_card(n + 1, name, dir, k, lines, links));
    }
    // the timelines
    for (b, l) in lines {
        if l.runs.is_empty() {
            continue;
        }
        s.push_str(&format!(
            "<h3 id=\"tl-{}-{}\">Timeline of {}</h3>\n",
            esc(dir),
            esc(&history::safe_name(b)),
            esc(b)
        ));
        let from = l.runs.len().saturating_sub(MAX_COLUMNS);
        if l.order.is_empty() {
            s.push_str(&format!(
                "<p>{} run{}, none of which found a problem.</p>\n",
                l.runs.len(),
                if l.runs.len() == 1 { "" } else { "s" }
            ));
            continue;
        }
        let mut head: Vec<String> = vec!["Finding".into()];
        for r in &l.runs[from..] {
            head.push(
                r.run
                    .commit
                    .as_deref()
                    .map(|c| short(c).chars().take(7).collect())
                    .unwrap_or_else(|| "-".into()),
            );
        }
        let head_refs: Vec<&str> = head.iter().map(String::as_str).collect();
        let rows: Vec<(String, Vec<String>)> = l
            .order
            .iter()
            .map(|k| {
                let t = &l.tracks[k];
                let mut cells = vec![format!(
                    "<span class=\"t\">{}</span>",
                    esc(&page::cut(&t.title, 90))
                )];
                for c in &l.cells[k][from..] {
                    cells.push(c.html());
                }
                (String::new(), cells)
            })
            .collect();
        if from > 0 {
            s.push_str(&format!(
                "<p class=\"small\">The latest {MAX_COLUMNS} of {} runs.</p>\n",
                l.runs.len()
            ));
        }
        s.push_str(&table(&head_refs, &rows));
        s.push_str(
            "<p class=\"small\">Each column is one recorded run, oldest first, named by its commit. \
             <b>new</b>: first seen; <b>fails</b>: seen failing again; <b>fixed</b>: its schedule \
             passed on replay; <b>back</b>: failing again after it was fixed; <b>·</b>: not found \
             by that run, and still open.</p>\n",
        );
    }
    // the runs, the latest first
    s.push_str(&format!("<h3 id=\"runs-{}\">Runs</h3>\n", esc(dir)));
    let mut all: Vec<(&str, &Recorded)> = lines
        .iter()
        .flat_map(|(b, l)| l.runs.iter().map(move |r| (b.as_str(), *r)))
        .collect();
    all.sort_by(|a, b| order_time(b.1).cmp(&order_time(a.1)));
    let rows: Vec<(String, Vec<String>)> = all
        .iter()
        .take(MAX_RUNS)
        .map(|(b, r)| {
            let run = &r.run;
            let mut what = vec![];
            if let Some(l) = report_link(links, dir, r) {
                what.push(l);
            }
            if let Some(u) = run.ci.run_url.as_deref().and_then(safe_url) {
                what.push(format!("<a href=\"{}\">the job</a>", esc(u)));
            }
            (
                String::new(),
                vec![
                    format!("<span class=\"n\">{}</span>", esc(&when(r))),
                    code(b),
                    run.commit
                        .as_deref()
                        .map(|c| code(&short(c)))
                        .unwrap_or_else(|| "-".into()),
                    esc(&match &run.change {
                        Some(c) => format!("{} #{c}", run.trigger),
                        None => run.trigger.clone(),
                    }),
                    esc(&seeds_words(run)),
                    esc(&budget_words(run)),
                    esc(&engine_words(run)),
                    run.summary.runs.to_string(),
                    esc(&states_words(run)),
                    result_words(run),
                    what.join(" · "),
                ],
            )
        })
        .collect();
    s.push_str(&table(
        &[
            "Started", "Branch", "Commit", "Trigger", "Seeds", "Budget", "Engine", "Runs",
            "Findings", "Result", "Links",
        ],
        &rows,
    ));
    if all.len() > MAX_RUNS {
        s.push_str(&format!(
            "<p class=\"small\">The latest {MAX_RUNS} of {} runs; every run is in <code>v1/{}/runs/</code>.</p>\n",
            all.len(),
            esc(dir)
        ));
    }
    s
}

fn finding_card(
    n: usize,
    project: &str,
    dir: &str,
    key: &str,
    lines: &[(String, Line)],
    links: &str,
) -> String {
    let here: Vec<(&str, &Line, &Track)> = lines
        .iter()
        .filter_map(|(b, l)| l.tracks.get(key).map(|t| (b.as_str(), l, t)))
        .collect();
    let (b0, l0, t0) = here[0];
    let mut s = format!(
        "<div class=\"card\" id=\"f-{}-{}\">\n<h3>{n}. <span class=\"mono\">{}</span></h3>\n<p>{}</p>\n",
        esc(dir),
        esc(&history::safe_name(key)),
        esc(&page::cut(&t0.title, 300)),
        state_pill(t0)
    );
    // in words, on its first branch
    let mut said = format!(
        "On {}, hydo first saw it at {}",
        code(b0),
        t0.first_seen
            .as_deref()
            .map(code)
            .unwrap_or_else(|| "an unknown commit".into())
    );
    if let Some(t) = &t0.first_time {
        said.push_str(&format!(" ({})", esc(&utc_words(t))));
    }
    if !t0.fixed_at.is_empty() {
        said.push_str(&format!(
            ", a replay proved it fixed at {}",
            commits(&t0.fixed_at)
        ));
    }
    if !t0.back_at.is_empty() {
        said.push_str(&format!(", and it came back at {}", commits(&t0.back_at)));
    }
    said.push_str(&format!(
        ". {} recorded run{} saw it failing.",
        t0.seen,
        if t0.seen == 1 { "" } else { "s" }
    ));
    s.push_str(&format!("<p>{said}</p>\n"));
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(reason) = &t0.accepted {
        rows.push((
            if t0.open {
                "Accepted".into()
            } else {
                "Accepted before the fix".into()
            },
            if reason.is_empty() {
                "yes, in <code>hydo/known-findings.yaml</code>".into()
            } else {
                format!("yes: {}", esc(reason))
            },
        ));
    }
    if !t0.minimal.is_empty() {
        rows.push(("Minimal schedule".into(), copy_block(&t0.minimal)));
        // the replay line of the run that last saw it (its engine)
        let run = t0.last_run.map(|i| &l0.runs[i].run);
        if let Some(r) = run {
            rows.push((
                "Replay it".into(),
                format!(
                    "<p class=\"small\">In the project folder at {}, after <code>hydo build</code>:</p>{}",
                    t0.last_seen.as_deref().map(code).unwrap_or_else(|| "its commit".into()),
                    copy_block(&replay_line(r, &t0.minimal))
                ),
            ));
        }
    }
    if t0.open && t0.accepted.is_none() {
        rows.push(("If it is expected".into(), copy_block(&accept_line(key))));
    }
    if let Some(link) = t0
        .last_run
        .and_then(|i| report_link(links, dir, l0.runs[i]))
    {
        rows.push(("The run that last saw it".into(), link));
    }
    rows.push(("Key".into(), code(key)));
    s.push_str(&facts(&rows));
    if here.len() > 1 {
        let br: Vec<(String, Vec<String>)> = here
            .iter()
            .map(|(b, _, t)| {
                (
                    String::new(),
                    vec![
                        code(b),
                        state_pill(t),
                        t.first_seen.as_deref().map(code).unwrap_or_default(),
                        commits(&t.fixed_at),
                        commits(&t.back_at),
                    ],
                )
            })
            .collect();
        s.push_str("<h4>On each branch</h4>\n");
        s.push_str(&table(
            &["Branch", "State", "First seen", "Fixed at", "Back at"],
            &br,
        ));
    }
    let _ = project;
    s.push_str("</div>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydo_results::{CiAccepted, CiBudget, CiFinding};

    fn f(key: &str, state: &str, source: &str) -> CiFinding {
        CiFinding {
            key: key.into(),
            kind: "invariant".into(),
            title: format!("VIOLATION [{key}] <lost> & gone"),
            state: state.into(),
            minimal: format!("disk=cut | run C={}", key.len()),
            schedules: vec![format!("disk=cut | run C={}", key.len())],
            source: Some(source.into()),
            ..CiFinding::default()
        }
    }

    fn run(id: &str, commit: &str, branch: &str, fs: Vec<CiFinding>) -> CiRun {
        CiRun {
            id: id.into(),
            commit: Some(commit.into()),
            branch: Some(branch.into()),
            trigger: "push".into(),
            project: hydo_results::CiProject {
                name: "redis".into(),
                ..Default::default()
            },
            budget: CiBudget {
                mode: "push".into(),
                seeds: "1-8".into(),
                strategy: "swarm".into(),
                ..Default::default()
            },
            findings: fs,
            ..CiRun::default()
        }
    }

    /// The states across four commits and a pull request, as the page shows them.
    #[test]
    fn the_page_shows_each_finding_across_commits_and_branches() {
        let d = std::env::temp_dir().join(format!("hydo-ci-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut acc = f("k1", "accepted", "explored");
        acc.accepted = Some(CiAccepted {
            reason: "documented window".into(),
            state: "present".into(),
            ..Default::default()
        });
        let mut fixed = f("k1", "fixed", "history");
        fixed.proof = Some("head-replay-ok".into());
        let mut evil = run(
            "local:2",
            "c2c2c2c2c2c2",
            "main",
            vec![f("k1", "new", "explored")],
        );
        evil.ci.run_url = Some("javascript:alert(1)".into());
        for (r, b) in [
            (run("local:1", "c1c1c1c1c1c1", "main", vec![]), "main"),
            (evil, "main"),
            (run("local:3", "c3c3c3c3c3c3", "main", vec![acc]), "main"),
            (run("local:4", "c4c4c4c4c4c4", "main", vec![fixed]), "main"),
            (
                run(
                    "github:9",
                    "c5c5c5c5c5c5",
                    "pull/7",
                    vec![f("k1", "reappeared", "explored")],
                ),
                "pull/7",
            ),
        ] {
            history::record(&d, &r, b, Some("<html>report</html>")).unwrap();
        }
        let html = page_of(&d, "hydo-history", "").unwrap();
        // Milestone 301: the headline counts the open findings of every line, and names them
        // (0.4.0's trial: "No open problem in redis" above a finding open on a pull request)
        assert!(html.contains("<h1>1 open problem in redis</h1>"), "{html}");
        assert!(
            html.contains("Open and not accepted: 1 on pull/7."),
            "{html}"
        );
        assert!(!html.contains("No open problem"));
        assert!(html.contains("VIOLATION [k1] &lt;lost&gt; &amp; gone"));
        assert!(!html.contains("<lost>"));
        assert!(
            html.contains("first saw it at <code>c2c2c2c2c2</code>"),
            "{html}"
        );
        assert!(html.contains("a replay proved it fixed at <code>c4c4c4c4c4</code>"));
        assert!(html.contains("<span class=\"pill ok\">Fixed</span>"));
        // the pull request's line: back again there
        assert!(html.contains("<h4>On each branch</h4>"));
        assert!(html.contains("<span class=\"pill bad\">back</span>"));
        assert!(html.contains("Timeline of main"));
        assert!(html.contains("Timeline of pull/7"));
        // the timeline of main: nothing, new, fails, fixed
        let tl = html.split("Timeline of main").nth(1).unwrap();
        let row = tl
            .split("<tbody>")
            .nth(1)
            .unwrap()
            .split("</tr>")
            .next()
            .unwrap();
        let cells: Vec<&str> = row.split("<td>").skip(2).collect();
        assert_eq!(cells.len(), 4, "{row}");
        assert!(cells[0].starts_with("</td>"));
        assert!(cells[1].contains(">new<"));
        assert!(cells[2].contains(">fails<"));
        assert!(cells[3].contains(">fixed<"));
        // the replay line and the report links; the bad job link is dropped
        assert!(html.contains("hydo run &quot;disk=cut | run C=2&quot;"));
        assert!(html.contains("href=\"v1/redis/reports/github_9.html\""));
        assert!(!html.contains("javascript:"));
        assert!(html.contains("Accepted"));
        assert!(!html.contains("http-equiv") && !html.contains("<script src"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Milestone 301: each run shows its own start, not the time it was recorded; a run without
    /// a start says that its time is the record's.
    #[test]
    fn the_runs_show_when_they_started() {
        let d = std::env::temp_dir().join(format!("hydo-ci-page-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut a = run("local:1", "c1c1c1c1c1c1", "main", vec![]);
        a.started = Some("2026-11-17T16:52:40Z".into());
        let mut b = run(
            "local:2",
            "c2c2c2c2c2c2",
            "pull/1",
            vec![f("k1", "new", "explored")],
        );
        b.started = Some("2026-11-17T16:49:02Z".into());
        let old = run("local:3", "c3c3c3c3c3c3", "main", vec![]);
        for (r, br) in [(a, "main"), (b, "pull/1"), (old, "main")] {
            history::record(&d, &r, br, None).unwrap();
        }
        let html = page_of(&d, "hydo-history", "").unwrap();
        assert!(html.contains(">2026-11-17 16:52:40 UTC<"), "{html}");
        assert!(html.contains(">2026-11-17 16:49:02 UTC<"), "{html}");
        // the finding's first sight is its run's start
        assert!(html.contains("(2026-11-17 16:49:02 UTC)"), "{html}");
        assert!(html.contains(">recorded 20"), "{html}");
        assert!(html.contains("<th>Started</th>"), "{html}");
        assert!(html.contains("<h1>1 open problem in redis</h1>"), "{html}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn utc_is_the_civil_date() {
        assert_eq!(history::utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(history::utc(1_794_909_600), "2026-11-17T10:00:00Z");
        assert_eq!(history::utc(951_782_400), "2000-02-29T00:00:00Z");
    }
}
