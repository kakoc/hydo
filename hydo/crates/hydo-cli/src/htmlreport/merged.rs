//! The problems of several explorations of one image as one list (Milestone 345): `hydo report
//! A B C` and `hydo report --html A B C`.
//!
//! A person who explores in rounds, each with other options (a blind round, a round with
//! `--kill-now`, an aimed round), gets one report per round, and a problem that two rounds
//! found shows in both. `hydo explore --merge` makes one exploration of the shards of one
//! exploration (Milestone 304), which share one configuration; these rounds do not, so their
//! documents stay as they are and only their problems are merged. Each round's problems come from
//! its own story ([`super::exploration_story`]), the same words as its own report. Two failures
//! are one problem when their class group and signature agree, the key that each exploration
//! shrinks by. The merged problem is shown as the first round that found it shows it (its
//! minimal schedule, its command, and its own problem number for `hydo inspect diagnose`), with
//! every round and seed that showed it.

use super::page::{code, esc};
use super::{exploration_story, h2, item_card, top, ExStory, Item};
use hydo_results::{Exploration, Stream};
use std::path::{Path, PathBuf};

/// One exploration as the user named it, read.
pub struct Round {
    pub dir: PathBuf,
    pub shown: String,
    pub doc: Exploration,
    pub events: Option<Stream>,
}

/// Where a merged problem was seen: the round (from 0) and its failing seeds, in seed order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub round: usize,
    pub seeds: Vec<i64>,
}

/// One problem of the merged list.
#[derive(Clone, Debug)]
pub struct Merged {
    /// The round whose story shows it, and its item there (from 0).
    pub home: usize,
    pub item: usize,
    /// The (class group, signature) keys it covers.
    pub keys: Vec<(String, String)>,
    pub seen: Vec<Seen>,
}

impl Merged {
    fn runs(&self) -> usize {
        self.seen.iter().map(|s| s.seeds.len()).sum()
    }
}

/// Read the explorations; Err names a directory that is no finished exploration, or two that
/// explored different images.
pub fn read(dirs: &[String]) -> Result<Vec<Round>, String> {
    let mut out: Vec<Round> = Vec::new();
    for d in dirs {
        match hydo_results::read_dir(Path::new(d)).map_err(|e| format!("{d}: {e}"))? {
            hydo_results::Results::Exploration {
                doc: Some(doc),
                events,
                ..
            } => out.push(Round {
                dir: PathBuf::from(d),
                shown: d.clone(),
                doc: *doc,
                events,
            }),
            hydo_results::Results::Exploration { .. } => {
                return Err(format!(
                    "{d}: the exploration has not finished (no exploration.json yet)"
                ))
            }
            hydo_results::Results::Run { .. } => {
                return Err(format!(
                    "{d}: a run directory; several directories are explorations of one image"
                ))
            }
        }
    }
    let image = |r: &Round| {
        r.doc.image.sha256.clone().unwrap_or_else(|| {
            Path::new(&r.doc.image.path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    };
    for r in out.iter().skip(1) {
        if image(r) != image(&out[0]) {
            return Err(format!(
                "{} and {} explored different images ({} and {}): a problem of one image is not \
                 a problem of the other; report them one by one",
                out[0].shown,
                r.shown,
                short(&image(&out[0])),
                short(&image(r))
            ));
        }
    }
    Ok(out)
}

fn short(s: &str) -> String {
    if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("sha256 {}…", &s[..12])
    } else {
        s.to_string()
    }
}

/// The merged problems of the rounds' stories, the most attention first, then the most runs.
pub fn merge(stories: &[ExStory]) -> Vec<Merged> {
    let mut out: Vec<Merged> = Vec::new();
    for (r, st) in stories.iter().enumerate() {
        for (gi, (key, fs)) in st.groups.iter().enumerate() {
            let seeds: Vec<i64> = {
                let mut v: Vec<i64> = fs.iter().filter_map(|f| f.run.seed).collect();
                v.sort_unstable();
                v.dedup();
                v
            };
            let add = |m: &mut Merged| match m.seen.iter_mut().find(|s| s.round == r) {
                Some(s) => {
                    s.seeds.extend(seeds.iter().copied());
                    s.seeds.sort_unstable();
                    s.seeds.dedup();
                }
                None => m.seen.push(Seen {
                    round: r,
                    seeds: seeds.clone(),
                }),
            };
            // a key an earlier round showed joins that problem
            if let Some(m) = out.iter_mut().find(|m| m.keys.contains(key)) {
                add(m);
                continue;
            }
            let item = st.problem_of[gi].saturating_sub(1);
            // a key of a problem this round already shows joins it
            if let Some(m) = out.iter_mut().find(|m| m.home == r && m.item == item) {
                m.keys.push(key.clone());
                add(m);
                continue;
            }
            let mut m = Merged {
                home: r,
                item,
                keys: vec![key.clone()],
                seen: Vec::new(),
            };
            add(&mut m);
            out.push(m);
        }
    }
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by_key(|&i| {
        let m = &out[i];
        (
            stories[m.home].items[m.item].problem.attention,
            std::cmp::Reverse(m.runs()),
            i,
        )
    });
    order.into_iter().map(|i| out[i].clone()).collect()
}

/// A round's problem without the sentence under its accept lines, which names the round's own
/// problem numbers.
fn without_notes(it: &Item) -> Item {
    Item {
        accept_note: String::new(),
        ..it.clone()
    }
}

/// The error-line problem of each round that has one: the round and its item.
fn error_items<'a>(stories: &'a [ExStory]) -> Vec<(usize, &'a Item)> {
    stories
        .iter()
        .enumerate()
        .filter_map(|(r, st)| st.items.iter().find(|i| i.link == "errors").map(|i| (r, i)))
        .collect()
}

/// The most seeds a line names; the rest are counted.
const SEEDS_SHOWN: usize = 8;

/// "seeds 3, 7 and 12", "seed 4", or "seeds 1, 5, ... 18 and 104 more".
fn seeds_words(s: &[i64]) -> String {
    let mut v: Vec<String> = s.iter().take(SEEDS_SHOWN).map(|x| x.to_string()).collect();
    if s.len() > SEEDS_SHOWN {
        v.push(format!("{} more", s.len() - SEEDS_SHOWN));
    }
    match v.len() {
        0 => "a run without a seed".into(),
        1 => format!("seed {}", v[0]),
        _ => format!("seeds {}", hydo_schedule::describe::and_list(&v)),
    }
}

/// "in 2 of 3 explorations: 1 (seeds 3 and 7) and 3 (seed 12)".
fn seen_words(m: &Merged, rounds: usize) -> String {
    let parts: Vec<String> = m
        .seen
        .iter()
        .map(|s| format!("{} ({})", s.round + 1, seeds_words(&s.seeds)))
        .collect();
    format!(
        "in {} of {rounds} exploration{}: {}",
        m.seen.len(),
        if rounds == 1 { "" } else { "s" },
        hydo_schedule::describe::and_list(&parts)
    )
}

/// The options that tell a round from the others: its command line without the image, the
/// output folder, the seeds and the worker count.
fn options_of(doc: &Exploration) -> String {
    let skip_with_value = [
        "--image",
        "--out",
        "--seeds",
        "--workers",
        "--manifest",
        "--known-errors",
        "--backend",
        "--hvm-kernel",
        "--time-limit",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut it = doc.config.argv.iter().peekable();
    while let Some(a) = it.next() {
        let name = a.split('=').next().unwrap_or(a);
        if skip_with_value.contains(&name) {
            if !a.contains('=') {
                it.next();
            }
            continue;
        }
        if !a.starts_with("--") && out.is_empty() {
            continue;
        }
        out.push(a.clone());
    }
    out.join(" ")
}

/// One line per round: its number, folder, seeds, options and how many of its runs failed.
fn round_lines(rounds: &[Round]) -> Vec<String> {
    rounds
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let s = &r.doc.summary;
            let opts = options_of(&r.doc);
            format!(
                "{}. {}: {}{}, {} of {} run{} failed",
                i + 1,
                r.shown,
                r.doc
                    .config
                    .seeds
                    .as_ref()
                    .map(|x| format!("seeds {x}"))
                    .unwrap_or_else(|| r.doc.mode.clone()),
                if opts.is_empty() {
                    String::new()
                } else {
                    format!(", {opts}")
                },
                s.failures,
                s.schedules,
                if s.schedules == 1 { "" } else { "s" }
            )
        })
        .collect()
}

/// The headline: how many problems in how many explorations.
fn title(stories: &[ExStory], n: usize, rounds: usize) -> String {
    let pr = &stories[0].pr;
    let setup = if pr.named {
        format!("your {} setup", pr.system)
    } else {
        "your system".to_string()
    };
    match n {
        0 => format!("hydo found no problem in {rounds} explorations of {setup}"),
        1 => format!("hydo found 1 problem in {rounds} explorations of {setup}"),
        n => format!("hydo found {n} problems in {rounds} explorations of {setup}"),
    }
}

/// `hydo report A B ...` in the terminal.
pub fn text(rounds: &[Round]) -> String {
    let stories: Vec<ExStory> = rounds
        .iter()
        .map(|r| exploration_story(&r.dir, &r.doc, r.events.as_ref(), &r.shown))
        .collect();
    let merged = merge(&stories);
    let errors = error_items(&stories);
    let n = merged.len() + usize::from(!errors.is_empty());
    let failed: u64 = rounds.iter().map(|r| r.doc.summary.failures).sum();
    let total: u64 = rounds.iter().map(|r| r.doc.summary.schedules).sum();
    let mut o = title(&stories, n, rounds.len());
    if failed > 0 {
        o.push_str(&format!(" ({failed} of {total} runs failed)"));
    }
    o.push_str("\nThe explorations:\n");
    for l in round_lines(rounds) {
        o.push_str(&format!("   {l}\n"));
    }
    for (k, m) in merged.iter().enumerate() {
        let st = &stories[m.home];
        let it = &without_notes(&st.items[m.item]);
        let label = format!(
            "{}, {}",
            it.problem.attention.words(),
            seen_words(m, rounds.len())
        );
        o.push_str(&super::text::item(
            k + 1,
            it,
            &label,
            &rounds[m.home].shown,
            m.item + 1,
        ));
    }
    if let Some((r, it)) = errors.first() {
        let label = format!(
            "{}, in {} of {} explorations: {}",
            it.problem.attention.words(),
            errors.len(),
            rounds.len(),
            hydo_schedule::describe::and_list(
                &errors
                    .iter()
                    .map(|(r, _)| (r + 1).to_string())
                    .collect::<Vec<_>>()
            )
        );
        o.push_str(&super::text::item(
            n,
            it,
            &label,
            &rounds[*r].shown,
            stories[*r].items.len(),
        ));
    }
    o.push_str("\nNext:\n");
    o.push_str(
        "   Each exploration's own report: hydo report <dir>; its failures' timelines: hydo \
         report --html <dir>\n",
    );
    o
}

/// `hydo report --html A B ...`: the same list as one page.
pub fn page(rounds: &[Round]) -> String {
    let stories: Vec<ExStory> = rounds
        .iter()
        .map(|r| exploration_story(&r.dir, &r.doc, r.events.as_ref(), &r.shown))
        .collect();
    let merged = merge(&stories);
    let errors = error_items(&stories);
    let n = merged.len() + usize::from(!errors.is_empty());
    let pr = &stories[0].pr;
    let t = title(&stories, n, rounds.len());
    let lead = format!(
        "These are the problems of {} explorations of one image, each listed once with every \
         exploration and seed that showed it.",
        rounds.len()
    );
    let mut body = top(
        &format!(
            "hydo report · {}{} explorations",
            if pr.named {
                format!("project <b>{}</b> · ", esc(&pr.label))
            } else {
                String::new()
            },
            rounds.len()
        ),
        &esc(&t),
        &esc(&lead),
    );
    body.push_str(&h2("rounds", "The explorations"));
    body.push_str("<div class=\"card\"><ol>\n");
    for l in round_lines(rounds) {
        let l = l.split_once(". ").map(|x| x.1).unwrap_or(&l);
        body.push_str(&format!("<li>{}</li>\n", esc(l)));
    }
    body.push_str("</ol></div>\n");
    if n > 0 {
        body.push_str(&h2("start", "Start here"));
    }
    for (k, m) in merged.iter().enumerate() {
        let it = &without_notes(&stories[m.home].items[m.item]);
        let first = format!(
            "<p class=\"small\">Seen {}. Its own report: {}</p>\n",
            esc(&seen_words(m, rounds.len())),
            code(&format!("hydo report --html {}", rounds[m.home].shown))
        );
        body.push_str(&item_card(k + 1, it, None, &first, None));
    }
    if let Some((r, it)) = errors.first() {
        let first = format!(
            "<p class=\"small\">In {} of {} explorations. The first: {}</p>\n",
            errors.len(),
            rounds.len(),
            code(&format!("hydo errors {}", rounds[*r].shown))
        );
        body.push_str(&item_card(n, it, None, &first, None));
    }
    super::page::document(&format!("{t} (hydo report)"), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydo_results::{Failure, Image, Run, Shrink, Summary, Verdict};

    fn failure(seed: i64, schedule: &str, verdict: &str) -> Failure {
        let mut run = Run::new(
            schedule,
            Verdict {
                kind: "violation".into(),
                text: format!("HYDO-ORACLE: {verdict}"),
                ..Default::default()
            },
            "0123456789abcdef",
        );
        run.seed = Some(seed);
        run.tag = Some(format!("seed-{seed}"));
        run.class = Some("durable-defaults".into());
        Failure {
            signature: verdict.into(),
            reproduced: true,
            run,
            ..Default::default()
        }
    }

    fn doc(seeds: &str, argv: &[&str], failures: Vec<Failure>) -> Exploration {
        let shrinks = failures
            .first()
            .map(|f| Shrink {
                class: "durable-defaults".into(),
                signature: f.signature.clone(),
                minimal: f.run.schedule.clone(),
                from_seed: f.run.seed.unwrap_or(0),
                ..Default::default()
            })
            .into_iter()
            .collect();
        let mut d = Exploration {
            image: Image {
                path: "/x/explore-ob-meilisearch-arm64.cpio.gz".into(),
                sha256: Some("ab".repeat(32)),
                ..Default::default()
            },
            summary: Summary {
                schedules: 20,
                failures: failures.len() as u64,
                reproduced: Some(failures.len() as u64),
                ..Default::default()
            },
            failures,
            shrinks,
            ..Default::default()
        };
        d.config.seeds = Some(seeds.into());
        d.config.argv = argv.iter().map(|s| s.to_string()).collect();
        d
    }

    struct Rm(PathBuf);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Two rounds with other options found one swap problem at other seeds, and the first round
    /// also a lost addition: the merged list has two problems, the swap's with both rounds.
    #[test]
    fn a_problem_two_rounds_found_is_listed_once() {
        let d = std::env::temp_dir().join(format!("hydo-merged-{}", std::process::id()));
        let _rm = Rm(d.clone());
        let swap = "VIOLATION [swap] the swap ran twice";
        let lost = "VIOLATION [docs] an acknowledged document is lost";
        let a = doc(
            "1-20",
            &[
                "--image",
                "i",
                "--seeds",
                "1-20",
                "--out",
                "a",
                "--kill-now",
            ],
            vec![
                failure(
                    4,
                    "disk=none | run A=meili.edit_acked:kill-now-meilisearch",
                    swap,
                ),
                failure(9, "disk=cut | run C=12", lost),
                failure(
                    11,
                    "disk=none | run A=meili.swap_acked+3:kill-now-meilisearch",
                    swap,
                ),
            ],
        );
        let b = doc(
            "1-153",
            &[
                "--image",
                "i",
                "--out",
                "b",
                "--reactive-at",
                "meili.swap_acked@20",
            ],
            vec![failure(
                40,
                "disk=none | run A=meili.swap_acked@20+13:kill-now-meilisearch",
                swap,
            )],
        );
        let mut dirs = Vec::new();
        for (name, x) in [("a", &a), ("b", &b)] {
            let p = d.join(name);
            std::fs::create_dir_all(&p).unwrap();
            hydo_results::write_document(&p.join(hydo_results::EXPLORATION_DOC), x).unwrap();
            dirs.push(p.display().to_string());
        }
        let rounds = read(&dirs).unwrap();
        let stories: Vec<ExStory> = rounds
            .iter()
            .map(|r| exploration_story(&r.dir, &r.doc, r.events.as_ref(), &r.shown))
            .collect();
        let m = merge(&stories);
        assert_eq!(m.len(), 2, "{m:?}");
        let sw = m.iter().find(|x| x.keys[0].1 == swap).unwrap();
        assert_eq!(
            sw.seen,
            [
                Seen {
                    round: 0,
                    seeds: vec![4, 11]
                },
                Seen {
                    round: 1,
                    seeds: vec![40]
                }
            ]
        );
        assert_eq!(sw.home, 0);
        let t = text(&rounds);
        assert!(
            t.starts_with("hydo found 2 problems in 2 explorations"),
            "{t}"
        );
        assert!(
            t.contains("seeds 1-20, --kill-now, 3 of 20 runs failed"),
            "{t}"
        );
        assert!(
            t.contains("seeds 1-153, --reactive-at meili.swap_acked@20, 1 of 20 runs failed"),
            "{t}"
        );
        assert!(
            t.contains("in 2 of 2 explorations: 1 (seeds 4 and 11) and 2 (seed 40)"),
            "{t}"
        );
        let html = page(&rounds);
        assert!(html.contains("Seen in 2 of 2 explorations"), "{html}");
        assert_eq!(html.matches("<div class=\"card problem\"").count(), 2);
        // another image is refused
        let mut c = b.clone();
        c.image.sha256 = Some("cd".repeat(32));
        let p = d.join("c");
        std::fs::create_dir_all(&p).unwrap();
        hydo_results::write_document(&p.join(hydo_results::EXPLORATION_DOC), &c).unwrap();
        let e = read(&[dirs[0].clone(), p.display().to_string()])
            .err()
            .unwrap();
        assert!(e.contains("explored different images"), "{e}");
    }
}
