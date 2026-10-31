//! NET-FAULTS, FREEZE and NET-NAMESPACES (docs/HISTORY.md M124) against the shell model.
//!
//! The identity corpus of every example manifest runs in `corpus.rs`; this test adds what that
//! corpus does not reach, each run checked against the shell model (`tests/support/model.rs` and
//! its network part `model_net.rs`, written from hydo-init.sh, hydo-init-links.sh,
//! hydo-init-freeze.sh and hydo-init-netns.sh) and typed again from its transcript:
//!
//! 1. schedules written for every shell path of the three features, on the example manifests
//!    that have them (a port fault, links, network: bridge, the freeze), and on plan variants for
//!    the paths no example reaches (no tc, a pair without addresses or a port, a failing network
//!    setup);
//! 2. the freeze sweep's point lists (M76: a `#0` trace run per stream and step, then every
//!    packet of every step; M92's adaptive sweep: every 8th packet, then every packet in a
//!    window of 16 around each failing point), the counts read from the runs' own lines;
//! 3. solo mode and the boot with the service network;
//! 4. coverage: every path in [`PATHS`] is reached at least once;
//! 5. deliberate mistakes: recorded conversations changed in one place each (an order, a missing
//!    drain poll, a heal line, a namespace, the exported environment, a freeze field, a reply
//!    not waited for, a missing helper line, a trace line) must each be caught by the model.
//!
//! Goldens: `tests/golden/corpus-net.txt` (chunked SHA-256 of every transcript, per group) and a
//! few readable transcripts in `tests/golden/examples-net/`. `HYDO_BLESS=1` re-records them.

mod support;

use hydo_agent_proto::msg::{Command, Sig};
use hydo_control::{Ending, Plan, Seam, Transcript};
use hydo_explore::gen::Ex;
use hydo_explore::sched::{Fc, S};
use hydo_explore::space::Space;
use hydo_schedule::Schedule;
use std::collections::{BTreeMap, BTreeSet};
use support::model::{check, Conv as MConv};
use support::{boot, plan_of, run_schedule, top, Booted, Conv, Item};

/// Each shell path and a text that shows it was taken (in a note, a report or a verdict).
const PATHS: &[(&str, &[&str])] = &[
    ("port fault on", &["hydo-ob: NET FAULT on "]),
    ("port fault skipped", &["skipped (no port or no tc)"]),
    ("link fault on", &["hydo-ob: LINK FAULT "]),
    ("link fault skipped", &["link fault on ", "skipped (no tc)"]),
    ("bridge port fault (veth)", &["NET FAULT on", "on -:vh"]),
    (
        "bridge port fault (eth0 in the namespace)",
        &["NET FAULT on", "/ns/", ":eth0"],
    ),
    (
        "bridge link fault",
        &["LINK FAULT", "(both): netem", " on -:vh"],
    ),
    (
        "bridge fault skipped (no tc)",
        &["hydo-ob: network fault on ", "skipped (no tc)"],
    ),
    ("bridge fault skipped (no port)", &["skipped (no port)"]),
    ("heal", &["hydo-ob: network fault lifted: queued="]),
    ("heal drained", &["drained_ms=1"]),
    ("heal left a packet", &["left=1"]),
    ("heal stats", &["acted on: "]),
    ("healed before a step", &["network healed before step"]),
    ("healed at the end", &["network healed at the end"]),
    (
        "one-way healed before a step",
        &["one-way fault healed before step"],
    ),
    (
        "one-way healed at the end",
        &["one-way fault healed at the end"],
    ),
    (
        "one-way req record",
        &["\"id\":\"fault.oneway_req\",\"ok\":true"],
    ),
    (
        "one-way resp record",
        &["\"id\":\"fault.oneway_resp\",\"ok\":true"],
    ),
    ("unknown N=", &["hydo-ob: N: no service"]),
    ("unknown O=", &["hydo-ob: O: no service"]),
    ("tc setup failed", &["HYDO-ORACLE: ERROR tc setup failed"]),
    ("freeze parsed", &["hydo-ob: freeze at a call: "]),
    ("freeze ignored", &["hydo-ob: F: no services"]),
    ("freeze declared", &["\"id\":\"fault.freeze_call\",\"msg\""]),
    ("freeze armed", &["hydo-ob: FREEZE ARMED before step"]),
    ("froze", &["hydo-fc: FROZE "]),
    (
        "froze record",
        &["\"id\":\"fault.freeze_call\",\"ok\":true"],
    ),
    ("resumed", &["hydo-fc: RESUMED after"]),
    ("froze not all threads", &["NOT all threads stopped"]),
    (
        "freeze without its process",
        &["is not running; nothing frozen"],
    ),
    ("freeze ENOBUFS", &["hydo-fc: netlink ENOBUFS"]),
    ("freeze healed", &["hydo-ob: freeze healed"]),
    ("freeze ended by the init", &["ended by the init"]),
    ("freeze never fired", &["hydo-ob: freeze never fired: "]),
    ("freeze trace", &["hydo-fc-trace: "]),
    (
        "freeze skipped (no address, no port)",
        &["have neither addresses nor a port, skipped"],
    ),
    (
        "freeze helper did not start",
        &["HYDO-ORACLE: ERROR freeze helper did not start"],
    ),
    (
        "freeze rules failed",
        &["HYDO-ORACLE: ERROR freeze rules failed"],
    ),
    (
        "bridge set up",
        &["hydo-ob: service network (network: bridge): hydo0 10.89.0.1/16"],
    ),
    (
        "bridge set up failed",
        &["service network setup failed (network: bridge)"],
    ),
];

fn lines_of(c: &Conv) -> Vec<String> {
    let mut v = Vec::new();
    for it in &c.items {
        match it {
            Item::Note { line, .. } | Item::Report { line, .. } => v.push(line.clone()),
            Item::End {
                end: Ending::Verdict(x),
                ..
            } => v.push(x.clone()),
            _ => {}
        }
    }
    v
}

fn stream_of(t: &Transcript) -> Vec<String> {
    t.to_jsonl()
        .lines()
        .skip(1)
        .filter(|l| !l.contains("\"t\":\"note\""))
        .map(str::to_string)
        .collect()
}

fn class(t: &Transcript) -> &'static str {
    match t.ending() {
        Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: OK") => "ok",
        Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: VIOLATION") => "violation",
        Some(Ending::Verdict(_)) => "error",
        Some(Ending::PowerCut { .. }) => "cut",
        Some(Ending::AtSnapshot(_)) => "snapshot",
        None => "none",
    }
}

/// The runs of one group (a manifest or a plan variant) and what they reached.
struct Group {
    name: String,
    stream: Vec<String>,
    counts: BTreeMap<String, usize>,
    seen: BTreeSet<String>,
}

struct Suite {
    groups: Vec<Group>,
    reached: BTreeMap<&'static str, usize>,
    runs: usize,
    /// Runs kept for the mistakes: (schedule, items, plan, guest part, the state at S1).
    kept: Vec<Kept>,
    readable: Vec<(String, String)>,
}

struct Kept {
    text: String,
    items: Vec<Item>,
    plan: Plan,
    cmd: String,
    s1: hydo_control::GuestState,
}

impl Suite {
    fn tally(&mut self, lines: &[String]) {
        for (k, pats) in PATHS {
            if lines.iter().any(|l| pats.iter().all(|p| l.contains(p))) {
                *self.reached.entry(k).or_default() += 1;
            }
        }
    }

    fn group(&mut self, name: &str) -> usize {
        self.groups.push(Group {
            name: name.into(),
            stream: Vec::new(),
            counts: BTreeMap::new(),
            seen: BTreeSet::new(),
        });
        self.groups.len() - 1
    }

    /// Runs one schedule from S1 (and its recovery), checks both against the model and the
    /// replay against the policy run; None when it is refused.
    fn run(&mut self, g: usize, b: &Booted, text: &str) -> Option<String> {
        let s = Schedule::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        if !self.groups[g].seen.insert(s.guest_text()) {
            return None;
        }
        let r = match run_schedule(b, &s) {
            Ok(r) => r,
            Err(e) => {
                let k = format!("refused:{}", e.seam.map(|x| x.name()).unwrap_or("other"));
                *self.groups[g].counts.entry(k).or_default() += 1;
                return None;
            }
        };
        let gname = self.groups[g].name.clone();
        let ctx = |w: &str| format!("{gname}: {text}: {w}");
        if let Err(e) = check(
            &b.plan,
            &r.run.items,
            MConv::Run,
            &s.guest_text(),
            (&[], &[]),
            Some(&b.s1.2),
        ) {
            panic!(
                "{}\n{e}\n--- transcript:\n{}",
                ctx("the run differs from the shell"),
                r.run.fin.transcript.to_jsonl()
            );
        }
        let mut reference = r.run.fin.transcript.clone();
        reference.header.controller = "replay".into();
        assert_eq!(
            r.replay.to_jsonl(),
            reference.to_jsonl(),
            "{}",
            ctx("the replay of the transcript differs from the policy run")
        );
        let mut lines = lines_of(&r.run);
        let cl = class(&r.run.fin.transcript);
        *self.groups[g]
            .counts
            .entry(format!("run:{cl}"))
            .or_default() += 1;
        self.groups[g].stream.push(format!("S\t{text}"));
        self.groups[g]
            .stream
            .extend(stream_of(&r.run.fin.transcript));
        if let Some(rc) = &r.recovery {
            let Some(Ending::PowerCut { acked, failed }) = r.run.fin.ending.clone() else {
                unreachable!()
            };
            let mut cmd = String::from("oracle");
            for i in &acked {
                cmd.push(' ');
                cmd.push_str(i);
            }
            if !failed.is_empty() {
                cmd.push_str(" failed");
                for i in &failed {
                    cmd.push(' ');
                    cmd.push_str(i);
                }
            }
            if let Err(e) = check(
                &b.plan,
                &rc.items,
                MConv::Recovery,
                &cmd,
                (&acked, &failed),
                Some(&b.s0.2),
            ) {
                panic!("{}\n{e}", ctx("the recovery differs from the shell"));
            }
            *self.groups[g]
                .counts
                .entry(format!("recovery:{}", class(&rc.fin.transcript)))
                .or_default() += 1;
            self.groups[g].stream.push("R".into());
            self.groups[g].stream.extend(stream_of(&rc.fin.transcript));
            lines.extend(lines_of(rc));
        }
        self.tally(&lines);
        self.runs += 1;
        let joined = lines.join("\n");
        if self.readable.len() < 8 && self.readable.iter().all(|(t, _)| t != text) {
            let want = [
                ("NET FAULT", "port"),
                ("LINK FAULT", "link"),
                ("on -:vh", "bridge"),
                ("hydo-fc: FROZE", "froze"),
                ("hydo-fc-trace", "trace"),
            ];
            for (pat, _) in want {
                if joined.contains(pat)
                    && !self.readable.iter().any(|(_, b)| b.contains(pat))
                    && r.recovery.is_none()
                {
                    self.readable
                        .push((text.to_string(), r.run.fin.transcript.to_jsonl()));
                    break;
                }
            }
        }
        if self.kept.len() < 400 {
            self.kept.push(Kept {
                text: text.to_string(),
                items: r.run.items.clone(),
                plan: b.plan.clone(),
                cmd: s.guest_text(),
                s1: b.s1.2.clone(),
            });
        }
        Some(joined)
    }
}

fn boot_ok(plan: &Plan, name: &str, suite: &mut Suite) -> Booted {
    for i in 0..20 {
        let salt = format!("{name}#{i}");
        // every boot, failed or not, is checked against the model
        let mut ctl = hydo_control::ScheduleController::boot(plan.clone()).unwrap();
        let c = support::converse(
            support::fake::Fake::boot(&salt, 16),
            &mut ctl,
            hydo_control::Start::Boot,
            hydo_control::Header {
                controller: String::new(),
                conversation: "boot".into(),
                schedule: None,
                cadence: "default".into(),
                start: "boot".into(),
            },
        );
        check(plan, &c.items, MConv::Boot, "provision", (&[], &[]), None)
            .unwrap_or_else(|e| panic!("{name}: boot {salt} differs from the shell:\n{e}"));
        suite.tally(&lines_of(&c));
        if let Ok(b) = boot(plan, &salt) {
            return b;
        }
    }
    panic!("{name}: no boot reached S1");
}

fn solo(plan: &Plan, name: &str, suite: &mut Suite) {
    for i in 0..6 {
        let salt = format!("{name} solo {i}");
        let mut ctl = hydo_control::ScheduleController::solo(plan.clone()).unwrap();
        let c = support::converse(
            support::fake::Fake::boot(&salt, 16),
            &mut ctl,
            hydo_control::Start::Boot,
            hydo_control::Header {
                controller: String::new(),
                conversation: "solo".into(),
                schedule: None,
                cadence: "default".into(),
                start: "boot".into(),
            },
        );
        check(plan, &c.items, MConv::Solo, "", (&[], &[]), None)
            .unwrap_or_else(|e| panic!("{name} ({salt}): solo differs from the shell:\n{e}"));
        suite.tally(&lines_of(&c));
    }
}

/// Port faults, N= and O= on one service, at once, healed before a step or at the end, with a cut.
fn port_schedules(svc: &str, steps: i64) -> Vec<String> {
    let m = (steps / 2).max(1);
    let e = steps;
    vec![
        format!("disk=none | run N={svc}@1-{m}:delay,200ms"),
        format!("disk=none | run N={svc}@2-{e}:loss,30%"),
        format!("disk=none | run N={svc}@0-3:delay,100ms,loss,5%"),
        format!("disk=none | run O={svc}@1-{m}:req:delay,300ms"),
        format!("disk=none | run O={svc}@1-{e}:resp:loss,40%"),
        format!("disk=none | run N={svc}@1-{m}:delay,50ms O={svc}@2-{e}:req:loss,10%"),
        format!("disk=none | run N={svc}@1-2:delay,50ms O={svc}@2-{e}:resp:delay,80ms"),
        format!("disk=cut | run N={svc}@1-{e}:delay,100ms C={m}"),
        format!("disk=cut | run O={svc}@0-{e}:req:loss,50% C={}", m + 1),
        format!("disk=none | run N={svc}@3-3:delay,10ms"),
        format!("disk=none | run N={svc}@4-2:delay,10ms"),
        format!("disk=none | run N=nosuch@1-3:delay,10ms O=nosuch@1-3:req:delay,10ms"),
        format!("disk=none | run K={svc}@1+2 N={svc}@1-{m}:duplicate,20%"),
        format!("disk=none | run P={svc}@1+1 O={svc}@1-{m}:resp:corrupt,10%"),
        format!("disk=none | run N={svc}@1-{m}:delay,9ms N={svc}@2-{e}:delay,20ms"),
        format!("rb=1 | run N={svc}@1-{m}:delay,400ms"),
        format!("disk=none | run N={svc}@1-{m}:reorder,20%"),
        // windows that end after the last step: healed at the end
        format!("disk=none | run N={svc}@{m}-{}:delay,250ms", e + 3),
        format!("disk=none | run O={svc}@1-{}:req:delay,120ms", e + 1),
        format!(
            "disk=none | run N={svc}@1-{}:loss,2% O={svc}@2-{}:resp:delay,1000ms",
            e + 1,
            e + 2
        ),
    ]
}

/// Link faults: a-b both ways, a-* for every other service, O= one way, O= and N= on one link.
fn link_schedules(a: &str, b: &str, steps: i64) -> Vec<String> {
    let m = (steps / 2).max(1);
    let e = steps;
    vec![
        format!("disk=none | run N={a}-{b}@1-{m}:delay,200ms"),
        format!("disk=none | run N={a}-*@1-{e}:loss,20%"),
        format!("disk=none | run O={a}-{b}@1-{m}:req:delay,300ms"),
        format!("disk=none | run O={b}-{a}@2-{e}:resp:loss,40%"),
        format!("disk=none | run O={a}-*@1-{m}:resp:delay,100ms"),
        format!("disk=none | run N={a}-{b}@1-{m}:delay,50ms O={a}-{b}@1-{m}:req:delay,50ms"),
        format!("disk=none | run N={a}-{b}@1-2:delay,50ms O={b}-*@3-{e}:req:delay,80ms"),
        format!("disk=none | run N={a}-zz@1-3:delay,10ms O={a}-{a}@1-3:req:delay,10ms"),
        format!("disk=cut | run N={a}-*@1-{e}:delay,100ms C={m}"),
        format!("disk=none | run N={a}@1-{m}:delay,30ms"),
        format!("disk=none | run K={b}@1+2 N={a}-{b}@1-{m}:delay,90ms"),
    ]
}

/// F= on a pair: fired, a trace, a pattern, the step past the end, ignored pairs, with K= and C=.
fn freeze_schedules(svc: &str, peer: &str, steps: i64) -> Vec<String> {
    let m = (steps / 2).max(1);
    vec![
        format!("disk=none | run F={svc}<{peer}@1#2:500"),
        format!("disk=none | run F={svc}>{peer}@{m}#1:150000"),
        format!("disk=none | run F={svc}<{peer}@2#0:1"),
        format!("disk=none | run F={svc}>{peer}@0#0:1"),
        format!("disk=none | run F={svc}<{peer}@1#3:SET:2000"),
        format!("disk=none | run F={svc}<{peer}@1#9999:100"),
        format!("disk=none | run F={svc}<{peer}@{steps}#1:100"),
        format!("disk=none | run F={svc}<{peer}@{}#1:100", steps + 5),
        format!("disk=none | run F=nosuch<{peer}@1#1:100"),
        format!("disk=none | run F={svc}<{svc}@1#1:100"),
        format!("disk=none | run F={svc}<{peer}@1#1:100 F=nosuch<{peer}@1#1:100"),
        format!("disk=none | run F=nosuch<{peer}@1#1:100 F={svc}<{peer}@2#1:100"),
        format!("disk=cut | run F={svc}<{peer}@1#1:150000 C={m}"),
        format!("disk=cut | run F={svc}<{peer}@1#2:1 C={}", m + 1),
        format!("disk=none | run K={svc}@1+5 F={svc}<{peer}@2#1:300"),
        format!("disk=none | run P={svc}@1+2 F={svc}>{peer}@1#1:300"),
        format!("disk=none | run F={svc}<{peer}@1#1:60000 X={svc}@3~40"),
        format!("rb=1 | run F={svc}<{peer}@1#1:1000"),
        format!("disk=none | run R=everysec F={svc}<{peer}@1#4:100"),
    ]
}

/// M76's every-packet sweep and M92's stride-8 stage with its windows, for one manifest: the
/// trace runs first (their counts from the runs' own "freeze never fired" lines), then the points.
fn sweep(
    suite: &mut Suite,
    g: usize,
    b: &Booted,
    m: &str,
    max_points: usize,
) -> (usize, usize, usize) {
    let mut ex = Ex::default();
    let sp = Space::from_path(&top().join(m)).expect("the explorer reads the manifest");
    sp.apply(&mut ex);
    assert!(!ex.fc_pairs.is_empty(), "{m}: no freeze pairs");
    let norders = ex.norders;
    let mut streams: Vec<(String, String, char, Option<String>)> = Vec::new();
    for (svc, peer) in &ex.fc_pairs {
        for d in ['<', '>'] {
            streams.push((svc.clone(), peer.clone(), d, None));
        }
        for (mt, md) in &ex.fc_match {
            for d in ['<', '>'] {
                if md.is_none_or(|x| x == d) {
                    streams.push((svc.clone(), peer.clone(), d, Some(mt.clone())));
                }
            }
        }
    }
    let render = |svc: &str, peer: &str, d: char, mt: &Option<String>, st: i64, n: i64, dur| {
        let mut s = S::new("none", 0);
        s.f = Some(Fc {
            svc: svc.into(),
            dir: d,
            peer: peer.into(),
            step: st,
            n,
            matched: mt.clone(),
            dur,
        });
        ex.render(&s)
    };
    let mut traces = 0;
    let mut count: BTreeMap<(usize, i64), i64> = BTreeMap::new();
    for (i, (svc, peer, d, mt)) in streams.iter().enumerate() {
        for st in 0..norders {
            let text = render(svc, peer, *d, mt, st, 0, 1);
            let lines = suite.run(g, b, &text).unwrap_or_default();
            traces += 1;
            let pre = "freeze never fired: ";
            let suf = format!(" of 0 matching events after step {st}");
            let n = lines
                .lines()
                .filter_map(|l| l.split_once(pre))
                .filter_map(|(_, r)| r.strip_suffix(&suf))
                .filter_map(|x| x.parse().ok())
                .next_back()
                .unwrap_or(0);
            count.insert((i, st), n);
        }
    }
    // the every-packet points (freeze_sweep: n_here = max(0, c(st) - c(st+1)), every step swept)
    let mut every = Vec::new();
    for (i, (svc, peer, d, mt)) in streams.iter().enumerate() {
        for st in 0..norders {
            let c = |s: i64| count.get(&(i, s)).copied().unwrap_or(0);
            let mut n_here = c(st);
            if st + 1 < norders {
                n_here = 0.max(c(st) - c(st + 1)).min(c(st));
            }
            for n in 1..=n_here {
                every.push((i, st, n, render(svc, peer, *d, mt, st, n, 150000)));
            }
        }
    }
    let mut points = 0;
    let mut failing = Vec::new();
    for (i, st, n, text) in every.iter().take(max_points) {
        if let Some(l) = suite.run(g, b, text) {
            points += 1;
            if !l.contains("HYDO-ORACLE: OK") {
                failing.push((*i, *st, *n));
            }
        }
    }
    // M92: stride 8 of each step's packets, then a window of 16 around each failing point
    let mut windows = 0;
    for (i, (svc, peer, d, mt)) in streams.iter().enumerate() {
        for st in 0..norders {
            let mut n = 1;
            while n <= count.get(&(i, st)).copied().unwrap_or(0) {
                if suite
                    .run(g, b, &render(svc, peer, *d, mt, st, n, 150000))
                    .is_some()
                {
                    windows += 1;
                }
                n += 8;
            }
        }
    }
    for (i, st, c) in failing.iter().take(4) {
        let (svc, peer, d, mt) = &streams[*i];
        for n in (c - 16).max(1)..=c + 16 {
            if suite
                .run(g, b, &render(svc, peer, *d, mt, *st, n, 150000))
                .is_some()
            {
                windows += 1;
            }
        }
    }
    (traces, points, windows)
}

// ------------------------------------------------------------------ deliberate mistakes

type Mutator = fn(&[Item], &Plan) -> Option<Vec<Item>>;

fn main_sends(items: &[Item]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, Item::Send { by, .. } if by == "main"))
        .map(|(k, _)| k)
        .collect()
}

fn is_tc(c: &Command, what: &str) -> bool {
    matches!(c, Command::Run(r) if r.argv.first().is_some_and(|a| a.ends_with("/bin/tc"))
        && r.argv.join(" ").contains(what))
}

/// The netem qdisc put on before the prio root.
fn swap_tc_order(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let ms = main_sends(items);
    for w in ms.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (Item::Send { cmd: ca, .. }, Item::Send { cmd: cb, .. }) = (&items[a], &items[b])
        else {
            continue;
        };
        if is_tc(ca, "root handle 1: prio") && is_tc(cb, "netem") {
            let (ca, cb) = (ca.clone(), cb.clone());
            let mut v = items.to_vec();
            if let Item::Send { cmd, .. } = &mut v[a] {
                *cmd = cb;
            }
            if let Item::Send { cmd, .. } = &mut v[b] {
                *cmd = ca;
            }
            return Some(v);
        }
    }
    None
}

/// One drain poll left out (its qlen and the answer).
fn drop_drain_poll(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let k = items
        .iter()
        .position(|i| matches!(i, Item::Send { by, cmd: Command::Qlen(_), .. } if by == "main"))?;
    let Item::Send { id, .. } = items[k] else {
        unreachable!()
    };
    let mut v = items.to_vec();
    v.retain(|i| match i {
        Item::Send { id: x, .. } => *x != id,
        Item::Got { msg, .. } => !matches!(msg, support::fake::Msg::Reply(x, _) if *x == id),
        _ => true,
    });
    Some(v)
}

/// The heal line with a wrong queue length.
fn wrong_heal_line(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let mut v = items.to_vec();
    for i in v.iter_mut() {
        if let Item::Note { line, .. } = i {
            if line.contains("network fault lifted: queued=") {
                *line = line.replacen("queued=", "queued=1", 1);
                return Some(v);
            }
        }
    }
    None
}

/// A service started outside its namespace.
fn no_namespace(items: &[Item], plan: &Plan) -> Option<Vec<Item>> {
    let mut v = items.to_vec();
    for i in v.iter_mut() {
        if let Item::Send {
            cmd: Command::Spawn(s),
            ..
        } = i
        {
            if s.netns.is_some() && plan.service(&s.h).is_some() {
                s.netns = None;
                return Some(v);
            }
        }
    }
    None
}

/// XTABLES_LOCKFILE missing from the environment of a command after the arming.
fn lost_export(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let mut v = items.to_vec();
    let k = v.iter().position(|i| {
        matches!(
            i,
            Item::Send {
                cmd: Command::Freeze(_),
                ..
            }
        )
    })?;
    for i in v.iter_mut().skip(k + 1) {
        if let Item::Send {
            by,
            cmd: Command::Run(r),
            ..
        } = i
        {
            if by == "main" {
                let e = r.env.as_mut()?;
                let n = e.len();
                e.retain(|x| !x.starts_with("XTABLES_LOCKFILE="));
                if e.len() < n {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// The freeze armed for the next packet.
fn freeze_off_by_one(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let mut v = items.to_vec();
    for i in v.iter_mut() {
        if let Item::Send {
            cmd: Command::Freeze(f),
            ..
        } = i
        {
            f.n += 1;
            return Some(v);
        }
    }
    None
}

/// The freeze's reply arriving only after the controller went on (a call not waited for).
fn freeze_not_waited(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let k = items.iter().position(|i| {
        matches!(
            i,
            Item::Send {
                cmd: Command::Freeze(_),
                ..
            }
        )
    })?;
    let Item::Send { id, .. } = items[k] else {
        unreachable!()
    };
    let r = items.iter().position(
        |i| matches!(i, Item::Got { msg: support::fake::Msg::Reply(x, _), .. } if *x == id),
    )?;
    let next = items
        .iter()
        .enumerate()
        .skip(r + 1)
        .find(|(_, i)| matches!(i, Item::Send { by, .. } if by == "main"))?
        .0;
    let mut v = items.to_vec();
    let got = v.remove(r);
    v.insert(next, got);
    Some(v)
}

/// The helper's FROZE line missing.
fn froze_line_lost(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let k = items.iter().position(
        |i| matches!(i, Item::Report { by, line, .. } if by == "freeze" && line.starts_with("hydo-fc: FROZE")),
    )?;
    let mut v = items.to_vec();
    v.remove(k);
    Some(v)
}

/// A trace line cut short.
fn trace_line_cut(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let mut v = items.to_vec();
    for i in v.iter_mut().rev() {
        if let Item::Report { line, .. } = i {
            if line.starts_with("hydo-fc-trace: ") && line.len() > 20 {
                line.truncate(line.len() - 1);
                return Some(v);
            }
        }
    }
    None
}

/// The heal's resume skipped: a SIGCONT the shell sends, missing.
fn no_cont_after_freeze(items: &[Item], _: &Plan) -> Option<Vec<Item>> {
    let k = items
        .iter()
        .position(|i| matches!(i, Item::Note { line, .. } if line.contains("ended by the init")))?;
    let c = (0..k).rev().find(
        |j| matches!(&items[*j], Item::Send { cmd: Command::Signal(s), .. } if s.sig == Sig::Cont),
    )?;
    let mut v = items.to_vec();
    v.remove(c);
    Some(v)
}

const MISTAKES: &[(&str, Mutator)] = &[
    ("the netem qdisc before the prio root", swap_tc_order),
    ("a drain poll left out", drop_drain_poll),
    ("a wrong heal line", wrong_heal_line),
    ("a service outside its namespace", no_namespace),
    ("XTABLES_LOCKFILE not exported", lost_export),
    ("the freeze one packet late", freeze_off_by_one),
    ("the freeze's reply not waited for", freeze_not_waited),
    ("the FROZE line missing", froze_line_lost),
    ("a trace line cut short", trace_line_cut),
    (
        "the init's SIGCONT after a freeze missing",
        no_cont_after_freeze,
    ),
];

#[test]
fn network_faults_freeze_and_namespaces_do_what_the_shell_does() {
    let t0 = std::time::Instant::now();
    let mut suite = Suite {
        groups: Vec::new(),
        reached: BTreeMap::new(),
        runs: 0,
        kept: Vec::new(),
        readable: Vec::new(),
    };
    let mut inputs: Vec<String> = Vec::new();
    let mut golden: Vec<String> = Vec::new();
    let grp = |suite: &mut Suite, name: &str, plan: &Plan, texts: Vec<String>| {
        let g = suite.group(name);
        let b = boot_ok(plan, name, suite);
        solo(plan, name, suite);
        for t in texts {
            suite.run(g, &b, &t);
        }
        (g, b)
    };

    // port faults: Redis (default fault kinds) and the netem example
    for m in [
        "hydo/onboard/examples/redis/hydo.yaml",
        "hydo/onboard/examples/redis/hydo-netem.yaml",
    ] {
        inputs.push(m.into());
        let p = plan_of(m).unwrap();
        let svc = p.services[0].name.clone();
        grp(&mut suite, m, &p, port_schedules(&svc, p.steps));
    }
    // links: three-node etcd (addr: on every member)
    let m = "hydo/onboard/examples/etcd/hydo.yaml";
    inputs.push(m.into());
    let etcd = plan_of(m).unwrap();
    let (ea, eb) = (etcd.services[0].name.clone(), etcd.services[1].name.clone());
    let mut texts = link_schedules(&ea, &eb, etcd.steps);
    texts.extend(port_schedules(&ea, etcd.steps).into_iter().take(6));
    grp(&mut suite, m, &etcd, texts);
    // network: bridge (Docker's voting app as hydo-compose writes it)
    let m = "hydo/onboard/examples/compose-voting/hydo.generated.yaml";
    inputs.push(m.into());
    let bridge = plan_of(m).unwrap();
    assert!(bridge.bridge);
    let names: Vec<String> = bridge.services.iter().map(|s| s.name.clone()).collect();
    let ported = bridge
        .services
        .iter()
        .find(|s| !s.port.is_empty())
        .unwrap()
        .name
        .clone();
    let portless = bridge
        .services
        .iter()
        .find(|s| s.port.is_empty())
        .unwrap()
        .name
        .clone();
    let mut texts = port_schedules(&ported, bridge.steps);
    texts.extend(link_schedules(&names[0], &names[1], bridge.steps));
    texts.push(format!(
        "disk=none | run N={portless}@1-2:delay,10ms O={ported}@1-2:resp:delay,10ms"
    ));
    texts.push(format!(
        "disk=none | run N={}@1-3:delay,30ms O={}@1-3:resp:loss,5%",
        names[0], names[1]
    ));
    texts.push(format!(
        "disk=none | run F={}<{}@1#1:100",
        names[0], names[1]
    ));
    grp(&mut suite, m, &bridge, texts);
    // the freeze: Redis with faults: [freeze] (its pair redis<workload), and its sweep
    let m = "hydo/onboard/examples/redis/hydo-freeze.yaml";
    inputs.push(m.into());
    let fz = plan_of(m).unwrap();
    assert!(fz.freeze);
    let svc = fz.services[0].name.clone();
    let mut texts = freeze_schedules(&svc, "workload", fz.steps);
    texts.extend(port_schedules(&svc, fz.steps).into_iter().take(2));
    let (g, b) = grp(&mut suite, m, &fz, texts);
    let (traces, points, windows) = sweep(&mut suite, g, &b, m, 400);

    // ---------------------------------------------------------- plan variants
    // no tc in the image: the port fault and the link fault are skipped
    for (name, base, texts) in [
        (
            "variant: redis without tc",
            plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap(),
            port_schedules("redis", 20).into_iter().take(3).collect(),
        ),
        (
            "variant: etcd without tc",
            etcd.clone(),
            link_schedules(&ea, &eb, etcd.steps)
                .into_iter()
                .take(3)
                .collect::<Vec<_>>(),
        ),
        (
            "variant: bridge without tc",
            bridge.clone(),
            port_schedules(&ported, bridge.steps)
                .into_iter()
                .take(2)
                .collect(),
        ),
    ] {
        let mut p = base;
        p.layout.tc = String::new();
        grp(&mut suite, name, &p, texts);
    }
    // a freeze pair with neither an address nor a port: the workload peer of a service without a
    // port (the rules give 3), and two services with neither (the freeze is not sent at all)
    let mut p = fz.clone();
    p.services[0].port = String::new();
    grp(
        &mut suite,
        "variant: freeze without a port",
        &p,
        vec![
            format!("disk=none | run F={svc}<workload@1#1:100"),
            format!("disk=none | run F={svc}<workload@2#0:1"),
        ],
    );
    let mut p = fz.clone();
    let mut aux = p.services[0].clone();
    aux.name = "aux".into();
    aux.port = String::new();
    p.services.push(aux);
    p.services[0].port = String::new();
    grp(
        &mut suite,
        "variant: freeze pair without addresses or ports",
        &p,
        vec![
            format!("disk=none | run F={svc}>aux@1#1:100"),
            format!("disk=none | run F=aux<{svc}@2#0:1"),
        ],
    );
    // a freeze between two services with addresses
    let mut p = fz.clone();
    let mut peer = p.services[0].clone();
    peer.name = "peer".into();
    peer.addr = "10.0.0.2".into();
    p.services[0].addr = "10.0.0.1".into();
    p.services.push(peer);
    grp(
        &mut suite,
        "variant: freeze between two addresses",
        &p,
        vec![
            format!("disk=none | run F={svc}>peer@1#1:100"),
            format!("disk=none | run F=peer<{svc}@2#2:150000"),
            format!("disk=none | run F={svc}<workload@1#1:100"),
            format!("disk=none | run N={svc}-peer@1-3:delay,10ms F={svc}>peer@1#1:100"),
        ],
    );
    // the freeze with network: bridge is refused (the agent's freeze has no namespace)
    let mut p = bridge.clone();
    p.freeze = true;
    let s1 = hydo_control::GuestState::default();
    let e = hydo_control::ScheduleController::run(
        p.clone(),
        &Schedule::parse(&format!(
            "disk=none | run F={}<{}@1#1:100",
            names[0], names[1]
        ))
        .unwrap(),
        &s1,
    )
    .unwrap_err();
    assert_eq!(e.seam, Some(Seam::Freeze), "{e}");
    golden.push(format!("refused\tF= with network: bridge\t{e}"));
    // a network setup that fails (an address the scripted ip refuses)
    let mut p = bridge.clone();
    p.net_extra = vec!["10.89.255.254".into()];
    let mut ctl = hydo_control::ScheduleController::boot(p.clone()).unwrap();
    let c = support::converse(
        support::fake::Fake::boot("bridge fails", 16),
        &mut ctl,
        hydo_control::Start::Boot,
        hydo_control::Header {
            controller: String::new(),
            conversation: "boot".into(),
            schedule: None,
            cadence: "default".into(),
            start: "boot".into(),
        },
    );
    check(&p, &c.items, MConv::Boot, "provision", (&[], &[]), None)
        .unwrap_or_else(|e| panic!("the failed network setup differs from the shell:\n{e}"));
    suite.tally(&lines_of(&c));
    golden.push(format!(
        "boot\tbridge setup fails\t{}\t{}",
        class(&c.fin.transcript),
        c.fin.transcript.hash()
    ));

    // ---------------------------------------------------------- coverage
    let missing: Vec<&str> = PATHS
        .iter()
        .map(|x| x.0)
        .filter(|k| !suite.reached.contains_key(k))
        .collect();
    assert!(
        missing.is_empty(),
        "shell paths no run reached: {missing:?} (reached: {:?}; {} runs)",
        suite.reached,
        suite.runs
    );

    // ---------------------------------------------------------- deliberate mistakes
    let mut caught = Vec::new();
    for (what, f) in MISTAKES {
        let (mut applied, mut found) = (0, 0);
        for k in &suite.kept {
            let Some(items) = f(&k.items, &k.plan) else {
                continue;
            };
            applied += 1;
            if check(&k.plan, &items, MConv::Run, &k.cmd, (&[], &[]), Some(&k.s1)).is_err() {
                found += 1;
            } else {
                panic!("the model did not catch \"{what}\" in {}", k.text);
            }
            if applied >= 25 {
                break;
            }
        }
        assert!(
            applied > 0,
            "the mistake \"{what}\" applies to no recorded run"
        );
        caught.push(format!("mistake\t{what}\tcaught {found} of {applied}"));
    }

    // ---------------------------------------------------------- goldens
    for g in &suite.groups {
        golden.push(format!(
            "counts\t{}\t{}",
            g.name,
            g.counts
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        golden.extend(support::chunked(&g.name, &g.stream, 500));
    }
    golden.push(format!(
        "sweep\thydo/onboard/examples/redis/hydo-freeze.yaml\ttraces={traces} points={points} stride-and-window={windows}"
    ));
    for (k, n) in &suite.reached {
        golden.push(format!("reached\t{k}\t{n}"));
    }
    golden.extend(caught);
    eprintln!(
        "corpus_net: {} runs in {:.1} s (sweep: {traces} traces, {points} points, {windows} stride/window points)",
        suite.runs,
        t0.elapsed().as_secs_f64()
    );
    support::check_golden(
        "corpus-net.txt",
        &[
            "NET-FAULTS, FREEZE and NET-NAMESPACES against the shell model (hydo/crates/hydo-control/tests/corpus_net.rs).",
            "Re-record: HYDO_BLESS=1 cargo test -p hydo-control --test corpus_net",
        ],
        &inputs,
        &golden,
    );
    let dir = support::golden_dir().join("examples-net");
    let mut index = String::new();
    for (i, (text, body)) in suite.readable.iter().enumerate() {
        let f = format!("net-{i}.jsonl");
        index.push_str(&format!("{f}\t{text}\n"));
        let p = dir.join(&f);
        if support::bless() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&p, body).unwrap();
        } else {
            let want =
                std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert_eq!(
                &want,
                body,
                "{} differs (HYDO_BLESS=1 re-records it)",
                p.display()
            );
        }
    }
    let ip = dir.join("INDEX");
    if support::bless() {
        std::fs::write(&ip, &index).unwrap();
    } else {
        assert_eq!(std::fs::read_to_string(&ip).unwrap_or_default(), index);
    }
}
