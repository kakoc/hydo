//! Reactions aimed at named events (Milestone 345): `hydo explore --reactive-at <event>[,...]
//! [--delays A..B[/STEP]] [--react-actions LIST]`.
//!
//! `--reactive` draws one reaction per schedule over every event and a few fixed delays, so a
//! window of one or two milliseconds after one event gets a handful of hits in a thousand runs
//! (Meilisearch's index swap, its example's finding 1). An aimed exploration tries every cell of
//! a grid instead: each named event (with its `#<k>` and `@<step>`, as in `A=`), each delay of
//! the range, and each action, one cell per seed in that order (the event, then the delay, then
//! the action), so every moment after the event meets every action before the next moment. A
//! seed's schedule is the plain workload with that one reaction (`disk=cut` for a power cut);
//! seeds past the grid run it again with the seed's own composition, walk and timing streams
//! ([`Ex::add_point_streams`]), so a longer exploration varies what runs beside the reaction.
//! Each schedule is an ordinary one: it is re-run, shrunk, replayed and saved as any other.
//!
//! The order of the cells is fixed by the options alone, so seed n names the same schedule in
//! every exploration of the same options (`hydo explore --schedule`, shards, `--time-limit`).

use crate::gen::Ex;
use crate::rng::SplitMix;
use crate::sched::{parse_a, Ra, S};

/// The default `--delays`: every millisecond from 0 to 50, the range of `--kill-now`'s draws.
pub const DELAYS_DEFAULT: &str = "0..50";
/// The most cells a grid may have (a mistyped range otherwise names millions of schedules).
pub const CELLS_MAX: u64 = 100_000;

/// One aimed event: its assertion id, which record of it acts (`#<k>`), and the step it is armed
/// at (`@<step>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AimEvent {
    pub event: String,
    pub nth: i64,
    pub at: i64,
}

/// The grid of an aimed exploration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aim {
    pub events: Vec<AimEvent>,
    /// The delays after the event, ms of guest time, in the order they are tried.
    pub delays: Vec<i64>,
    /// The actions, as `A=` writes them (`kill-now-<svc>`, `kill-<svc>`, `cut`, ...).
    pub actions: Vec<String>,
}

impl Aim {
    /// How many cells the grid has: one schedule each.
    pub fn cells(&self) -> u64 {
        (self.events.len() * self.delays.len() * self.actions.len()) as u64
    }

    /// The reaction of cell `j` (from 0; taken modulo the grid).
    pub fn cell(&self, j: u64) -> Ra {
        let (na, nd) = (self.actions.len() as u64, self.delays.len() as u64);
        let j = j % self.cells().max(1);
        let e = &self.events[(j / (na * nd)) as usize];
        let d = self.delays[((j / na) % nd) as usize];
        Ra {
            event: e.event.clone(),
            nth: e.nth,
            at: e.at,
            delay: d,
            action: self.actions[(j % na) as usize].clone(),
        }
    }

    /// The schedule of seed `seed` (seeds count from 1; seed n is cell n-1, and the cells repeat
    /// after the grid with the seed's own streams).
    pub fn schedule(&self, ex: &Ex, seed: u64) -> S {
        let n = self.cells().max(1);
        let j = seed.saturating_sub(1);
        let mut s = S::new("none", SplitMix::new(seed).next());
        s = if j < n {
            // the plain workload; a walk through the pages is the workload itself
            ex.add_walk(s, seed)
        } else {
            ex.add_point_streams(s, seed)
        };
        let r = self.cell(j);
        if r.action == "cut" {
            s.disk = "cut".into();
        }
        s.a.push(r);
        s
    }

    /// The grid in words, for the exploration's first lines.
    pub fn words(&self) -> String {
        let events: Vec<String> = self
            .events
            .iter()
            .map(|e| {
                let mut t = e.event.clone();
                if e.nth > 1 {
                    t.push_str(&format!("#{}", e.nth));
                }
                if e.at > 0 {
                    t.push_str(&format!("@{}", e.at));
                }
                t
            })
            .collect();
        let delays = match self.delays.as_slice() {
            [d] => format!("{d} ms"),
            [a, b] => format!("{a} and {b} ms"),
            [first, .., last] => {
                let step = self.delays[1] - first;
                let even = self.delays.windows(2).all(|w| w[1] - w[0] == step);
                if even && step == 1 {
                    format!("every ms from {first} to {last}")
                } else if even {
                    format!("every {step} ms from {first} to {last}")
                } else {
                    let v: Vec<String> = self.delays.iter().map(|d| d.to_string()).collect();
                    format!("{} ms", and_list(&v))
                }
            }
            [] => "no delay".into(),
        };
        format!(
            "hydo explore: aimed reactions (--reactive-at): {} schedules, one per seed: after {}, \
             {delays}, with {}. Seeds after {} try the grid again with each seed's composition.",
            self.cells(),
            and_list(&events),
            and_list(&self.actions),
            self.cells()
        )
    }
}

fn and_list(xs: &[String]) -> String {
    match xs {
        [] => String::new(),
        [x] => x.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `--reactive-at`'s events: `<event>[#<k>][@<step>]`, several with commas, each checked as the
/// head of an `A=` token.
pub fn parse_events(v: &str) -> Result<Vec<AimEvent>, String> {
    let mut out: Vec<AimEvent> = Vec::new();
    for item in v.split(',').map(str::trim).filter(|x| !x.is_empty()) {
        if item.contains(['+', ':']) {
            return Err(format!(
                "--reactive-at {item:?}: an event is <event>[#<k>][@<step>]; the delays come \
                 from --delays and the actions from --react-actions"
            ));
        }
        let probe = format!("disk=none | run A={item}:cut");
        hydo_schedule::Schedule::parse(&probe)
            .map_err(|e| format!("--reactive-at {item:?}: {}", e.message))?;
        let r = parse_a(&format!("{item}:cut")).map_err(|e| format!("--reactive-at: {e}"))?;
        let e = AimEvent {
            event: r.event,
            nth: r.nth,
            at: r.at,
        };
        if !out.contains(&e) {
            out.push(e);
        }
    }
    if out.is_empty() {
        return Err(
            "--reactive-at: name at least one event (an assertion id of the system's SDK records)"
                .into(),
        );
    }
    Ok(out)
}

/// `--delays`: `A..B` (every ms), `A..B/STEP`, or a list `A,B,C`, ms of guest time.
pub fn parse_delays(v: &str) -> Result<Vec<i64>, String> {
    let bad = || {
        format!(
            "--delays {v:?}: A..B (every ms from A to B), A..B/STEP, or a list A,B,C, in ms of \
             guest time from 0"
        )
    };
    let num = |x: &str| -> Result<i64, String> {
        let x = x.trim();
        if x.is_empty() || !x.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        x.parse::<i64>().map_err(|_| bad())
    };
    let mut out: Vec<i64> = Vec::new();
    if let Some((a, b)) = v.split_once("..") {
        let (b, step) = match b.split_once('/') {
            Some((b, s)) => (b, num(s)?),
            None => (b, 1),
        };
        let (a, b) = (num(a)?, num(b)?);
        if step < 1 || b < a {
            return Err(bad());
        }
        let mut d = a;
        while d <= b {
            out.push(d);
            d += step;
            if out.len() as u64 > CELLS_MAX {
                return Err(format!("--delays {v:?}: more than {CELLS_MAX} delays"));
            }
        }
    } else {
        for x in v.split(',') {
            let d = num(x)?;
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    if out.is_empty() {
        return Err(bad());
    }
    Ok(out)
}

/// The actions of `--react-actions`: `kill-now` and `kill` name one action per service, `cut` a
/// power cut, and any other item is one action as `A=` writes it (`kill-now-db`,
/// `stop-db-500`). `None`: the default, `kill-now`, `kill` and `cut`, each only where the
/// manifest's faults and the engine keep it (`keep`). An action given by name that they drop is
/// an error, so an aimed exploration never runs schedules whose reaction was left out.
pub fn actions(
    list: Option<&str>,
    svcs: &[String],
    keep: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, String> {
    let given = list.is_some();
    let items: Vec<String> = match list {
        Some(l) => l
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect(),
        None => vec!["kill-now".into(), "kill".into(), "cut".into()],
    };
    let mut out: Vec<String> = Vec::new();
    for it in &items {
        let expanded: Vec<String> = match it.as_str() {
            "kill-now" | "kill" => svcs.iter().map(|v| format!("{it}-{v}")).collect(),
            _ => vec![it.clone()],
        };
        for a in expanded {
            let probe = format!("disk=cut | run A=x:{a}");
            if let Err(e) = hydo_schedule::Schedule::parse(&probe) {
                return Err(format!("--react-actions {a:?}: {}", e.message));
            }
            if !keep(&a) {
                if given {
                    return Err(format!(
                        "--react-actions {a:?}: this image's manifest (its faults: and its \
                         services) or the engine leaves that reaction out of every schedule"
                    ));
                }
                continue;
            }
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    if out.is_empty() {
        return Err("--reactive-at: no reaction is left: the manifest's faults: allows no kill and no power cut on this engine; name the actions with --react-actions".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aim() -> Aim {
        Aim {
            events: parse_events("a.x,b.y#2@20").unwrap(),
            delays: parse_delays("0..2").unwrap(),
            actions: vec!["kill-now-db".into(), "cut".into()],
        }
    }

    #[test]
    fn the_cells_go_event_then_delay_then_action() {
        let a = aim();
        assert_eq!(a.cells(), 12);
        let t: Vec<String> = (0..13)
            .map(|j| crate::sched::render_a(&a.cell(j)))
            .collect();
        assert_eq!(
            t,
            [
                "A=a.x:kill-now-db",
                "A=a.x:cut",
                "A=a.x+1:kill-now-db",
                "A=a.x+1:cut",
                "A=a.x+2:kill-now-db",
                "A=a.x+2:cut",
                "A=b.y#2@20:kill-now-db",
                "A=b.y#2@20:cut",
                "A=b.y#2@20+1:kill-now-db",
                "A=b.y#2@20+1:cut",
                "A=b.y#2@20+2:kill-now-db",
                "A=b.y#2@20+2:cut",
                "A=a.x:kill-now-db",
            ]
        );
    }

    #[test]
    fn the_options_are_read_or_refused() {
        assert_eq!(parse_delays("0..50").unwrap().len(), 51);
        assert_eq!(parse_delays("10..20/5").unwrap(), [10, 15, 20]);
        assert_eq!(parse_delays("3,1,3").unwrap(), [3, 1]);
        for bad in ["", "5..1", "0..10/0", "-1..3", "a..b", "1,,2"] {
            assert!(parse_delays(bad).is_err(), "{bad:?}");
        }
        assert!(parse_events("x+5").unwrap_err().contains("--delays"));
        assert!(parse_events("x#1").unwrap_err().contains("#1"));
        assert!(parse_events(" , ").is_err());
        let svcs = vec!["db".to_string(), "api".to_string()];
        let all = |_: &str| true;
        assert_eq!(
            actions(None, &svcs, &all).unwrap(),
            ["kill-now-db", "kill-now-api", "kill-db", "kill-api", "cut"]
        );
        let no_cut = |a: &str| a != "cut";
        assert_eq!(actions(None, &svcs, &no_cut).unwrap().len(), 4);
        assert!(actions(Some("cut"), &svcs, &no_cut)
            .unwrap_err()
            .contains("leaves that reaction out"));
        assert!(actions(Some("boom"), &svcs, &all).is_err());
        assert_eq!(
            actions(Some("kill-now,stop-db-500"), &svcs, &all).unwrap(),
            ["kill-now-db", "kill-now-api", "stop-db-500"]
        );
    }

    #[test]
    fn the_words_name_the_grid() {
        let w = aim().words();
        assert!(w.contains("12 schedules"), "{w}");
        assert!(
            w.contains("after a.x and b.y#2@20, every ms from 0 to 2"),
            "{w}"
        );
        assert!(w.contains("with kill-now-db and cut"), "{w}");
    }
}
