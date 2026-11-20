//! `hydo stats [DIR]` (Milestone 336): how much an exploration explored, what it cost, which
//! faults it drew, and how many first runs fit in a time budget, from its `stats.json`
//! (`hydo_explore::stats`). A command of its own because it answers another question than
//! `hydo report`: not what broke, but whether to run more, and with how many workers or shards.
//! `hydo report` prints its short form, and `hydo report --html` draws its curve.

use crate::ctx::{die, pjoin, OUT};
use std::path::Path;

pub const HELP: &str = "usage: hydo stats [DIR] [--json] [--budget TIME] [--workers N] [--shards N]
  prints how much an exploration explored and what it cost, from DIR's stats.json (hydo explore
  writes it beside exploration.json, and hydo explore --merge for the whole range):
  - new behaviour: after each run in seed order, the distinct verdicts, property outcomes, error
    kinds and error paths so far, and whether the last quarter of the runs showed anything new;
  - the cost: the engine's time by the run's role, the share that shrinking took, the host CPU,
    the memory, and the workers' time busy, starting their engines, waiting and idle;
  - the faults drawn: each kind with its targets and workload steps, and the kinds and services
    that the project allows but no schedule drew;
  - how many runs fit in an hour or eight hours, on this exploration's workers, on the workers
    this machine fits, and on 4 or 20 shards.
  --budget TIME (90m, 2h, 3600) with --workers N and --shards N adds that estimate. --json prints
  the document instead, with the asked estimate among its fits. DIR defaults to
  hydo-out/explore. `hydo guide workflow` says how to read the curve.";

/// A time budget: `3600`, `90s`, `90m`, `2h`, `1h30m`.
pub fn parse_budget(s: &str) -> Option<f64> {
    if let Ok(x) = s.parse::<f64>() {
        return (x > 0.0).then_some(x);
    }
    let mut total = 0.0;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let n: f64 = num.parse().ok()?;
        num.clear();
        total += n * match c {
            's' => 1.0,
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86400.0,
            _ => return None,
        };
    }
    if !num.is_empty() {
        return None;
    }
    (total > 0.0).then_some(total)
}

pub fn cmd_stats(argv: &[String]) -> i32 {
    let mut dir = None;
    let mut json = false;
    let (mut budget, mut workers, mut shards) = (None, None, None);
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| -> String {
            it.next()
                .cloned()
                .unwrap_or_else(|| die(&format!("hydo stats: {name} needs a value")))
        };
        match a.as_str() {
            "--json" => json = true,
            "--budget" => {
                let v = val("--budget");
                budget = Some(parse_budget(&v).unwrap_or_else(|| {
                    die(&format!(
                        "hydo stats: --budget {v}: a time such as 3600, 90m, 2h or 1h30m"
                    ))
                }));
            }
            "--workers" | "--shards" => {
                let v = val(a);
                let n: u64 =
                    v.parse().ok().filter(|n| *n > 0).unwrap_or_else(|| {
                        die(&format!("hydo stats: {a} {v}: a whole number from 1"))
                    });
                if a == "--workers" {
                    workers = Some(n);
                } else {
                    shards = Some(n);
                }
            }
            x if x.starts_with('-') => die(&format!("hydo stats: unknown option {x}\n{HELP}")),
            x => dir = Some(x.to_string()),
        }
    }
    let d = dir.unwrap_or_else(|| pjoin(OUT, "explore"));
    let Some(mut doc) = hydo_results::stats_of(Path::new(&d)) else {
        let why = if Path::new(&d).join(hydo_results::EXPLORATION_DOC).is_file() {
            format!(
                "{d} has no {}: it is written by hydo explore since 0.5.0 (Milestone 336); \
                 explore again to have it",
                hydo_results::STATS_DOC
            )
        } else {
            format!(
                "{d} is no exploration's directory (no {})",
                hydo_results::STATS_DOC
            )
        };
        die(&format!("hydo stats: {why}"));
    };
    let asked: Vec<(f64, u64, u64)> = if budget.is_some() || workers.is_some() || shards.is_some() {
        vec![(
            budget.unwrap_or(3600.0),
            workers.unwrap_or(doc.estimate.workers),
            shards.unwrap_or(1),
        )]
    } else {
        Vec::new()
    };
    if json {
        for (b, w, s) in &asked {
            let runs = hydo_explore::stats::fit(*b, &doc.estimate, *w, *s);
            doc.estimate.fits.push(hydo_results::StatsFit {
                budget_s: *b,
                workers: *w,
                shards: *s,
                runs,
            });
        }
        print!("{}", hydo_results::to_document(&doc));
        return 0;
    }
    print!("{}", hydo_explore::stats::text(&doc, &asked));
    0
}

#[cfg(test)]
mod tests {
    use super::parse_budget;

    #[test]
    fn a_budget_reads_seconds_minutes_and_hours() {
        assert_eq!(parse_budget("3600"), Some(3600.0));
        assert_eq!(parse_budget("90m"), Some(5400.0));
        assert_eq!(parse_budget("1h30m"), Some(5400.0));
        assert_eq!(parse_budget("2h"), Some(7200.0));
        assert_eq!(parse_budget("2x"), None);
        assert_eq!(parse_budget("5m3"), None);
        assert_eq!(parse_budget("0"), None);
    }
}
