//! Shared by the controller's tests: the example manifests, their plans, conversations on the
//! scripted agent and engine ([`fake`]), the merged log the shell model reads ([`model`]), and
//! golden files of chunked SHA-256 (the format of hydo-explore's `tests/support`).
#![allow(dead_code)]

pub mod fake;
pub mod model;

use fake::{Delivered, Fake, Msg, VmState};
use hydo_agent_proto::line::HostLine;
use hydo_agent_proto::msg::Command;
use hydo_control::transcript::{Header, Record};
use hydo_control::{
    run, AgentPoint, Ending, EngineAction, Finished, GuestState, Plan, ReplayController,
    ScheduleController, Start, Transcript, Unsupported,
};
use hydo_schedule::Schedule;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root")
}

pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

pub fn bless() -> bool {
    std::env::var_os("HYDO_BLESS").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn sorted_files(dir: &Path, pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !n.starts_with('.') && pred(n))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The example manifests the explorer's identity corpus uses (hydo-explore tests/support):
/// `examples/*/hydo*.yaml`, `examples/*/hydo/hydo.yaml`, `examples-mac/*/hydo*.yaml`, relative to
/// the repository.
pub fn manifests() -> Vec<String> {
    let top = top();
    let onb = top.join("hydo/onboard");
    let rel = |p: &Path| p.strip_prefix(&top).unwrap().to_string_lossy().into_owned();
    let mut out = Vec::new();
    let dirs = |d: &Path| -> Vec<PathBuf> {
        sorted_files(d, |_| true)
            .into_iter()
            .filter(|p| p.is_dir())
            .collect()
    };
    let mut a = Vec::new();
    for d in dirs(&onb.join("examples")) {
        for f in sorted_files(&d, |n| n.starts_with("hydo") && n.ends_with(".yaml")) {
            if f.is_file() {
                a.push(rel(&f));
            }
        }
    }
    a.sort();
    out.extend(a);
    let mut b = Vec::new();
    for d in dirs(&onb.join("examples")) {
        let f = d.join("hydo/hydo.yaml");
        if f.is_file() {
            b.push(rel(&f));
        }
    }
    b.sort();
    out.extend(b);
    let mut c = Vec::new();
    for d in dirs(&onb.join("examples-mac")) {
        for f in sorted_files(&d, |n| n.starts_with("hydo") && n.ends_with(".yaml")) {
            if f.is_file() {
                c.push(rel(&f));
            }
        }
    }
    c.sort();
    out.extend(c);
    out
}

/// The plan of a manifest as the explorer reads it (`hydo_manifest::for_explorer`).
pub fn plan_of(rel: &str) -> Result<Plan, Unsupported> {
    Plan::from_manifest(&hydo_manifest::Manifest::from_validated(data_of(rel)?))
}

/// The validated manifest as the explorer reads it, with a composer plan's meta as `_compose`
/// (where hydo_manifest::load puts it).
pub fn data_of(rel: &str) -> Result<serde_json::Value, Unsupported> {
    let (mut data, meta) =
        hydo_manifest::for_explorer(&top().join(rel)).map_err(|e| Unsupported::other(e.message))?;
    if let Some(meta) = meta {
        data["_compose"] = meta;
    }
    Ok(data)
}

/// One item of a conversation as the shell model reads it: what the agent delivered and what
/// the host did, in order (at each stop: the deliveries first, then the host's actions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Got {
        n: u64,
        msg: Msg,
    },
    Send {
        n: u64,
        id: u64,
        by: String,
        cmd: Command,
    },
    Report {
        n: u64,
        by: String,
        line: String,
    },
    Note {
        n: u64,
        by: String,
        line: String,
    },
    Snap {
        n: u64,
        name: String,
    },
    Engine {
        n: u64,
        action: EngineAction,
    },
    End {
        n: u64,
        end: Ending,
    },
}

impl Item {
    pub fn stop(&self) -> u64 {
        match self {
            Item::Got { n, .. }
            | Item::Send { n, .. }
            | Item::Report { n, .. }
            | Item::Note { n, .. }
            | Item::Snap { n, .. }
            | Item::Engine { n, .. }
            | Item::End { n, .. } => *n,
        }
    }

    pub fn by(&self) -> Option<&str> {
        match self {
            Item::Send { by, .. } | Item::Report { by, .. } | Item::Note { by, .. } => Some(by),
            Item::Snap { .. } | Item::Engine { .. } | Item::End { .. } => Some("main"),
            Item::Got { .. } => None,
        }
    }
}

pub fn decode_send(bytes: &str) -> Command {
    let b = bytes.strip_prefix('\u{18}').unwrap_or(bytes);
    let b = b.strip_suffix('\n').unwrap_or(b);
    HostLine::decode(b.as_bytes(), &[1])
        .unwrap_or_else(|e| panic!("a recorded send does not decode ({}): {b}", e.msg))
        .cmd
}

/// The deliveries and the transcript, merged by stop.
pub fn merge(delivered: &[Delivered], t: &Transcript) -> Vec<Item> {
    let mut out = Vec::new();
    let mut di = 0;
    let mut recs = t.records.iter().peekable();
    let last = delivered
        .last()
        .map(|d| d.n)
        .max(t.records.last().map(|r| r.stop()))
        .unwrap_or(0);
    for n in 1..=last {
        while di < delivered.len() && delivered[di].n == n {
            if !matches!(delivered[di].msg, Msg::Kernel(_)) {
                out.push(Item::Got {
                    n,
                    msg: delivered[di].msg.clone(),
                });
            }
            di += 1;
        }
        while let Some(r) = recs.peek() {
            if r.stop() != n {
                break;
            }
            let r = recs.next().unwrap();
            match r {
                Record::Send { id, by, bytes, .. } => out.push(Item::Send {
                    n,
                    id: *id,
                    by: by.clone(),
                    cmd: decode_send(bytes),
                }),
                Record::Report { by, line, .. } => out.push(Item::Report {
                    n,
                    by: by.clone(),
                    line: line.clone(),
                }),
                Record::Note { by, line, .. } => out.push(Item::Note {
                    n,
                    by: by.clone(),
                    line: line.clone(),
                }),
                Record::Snap { name, .. } => out.push(Item::Snap {
                    n,
                    name: name.clone(),
                }),
                Record::Engine { action, .. } => out.push(Item::Engine {
                    n,
                    action: action.clone(),
                }),
                Record::End { end, .. } => out.push(Item::End {
                    n,
                    end: end.clone(),
                }),
                Record::Stop { .. } | Record::Ack { .. } => {}
            }
        }
    }
    out
}

/// One conversation's results.
pub struct Conv {
    pub fin: Finished,
    pub items: Vec<Item>,
    pub fake: Fake,
}

fn header(conv: &str, schedule: Option<&Schedule>, start: &str) -> Header {
    Header {
        controller: String::new(),
        conversation: conv.into(),
        schedule: schedule.map(|s| s.to_string()),
        cadence: hydo_control::runner::cadence_of(schedule),
        start: start.into(),
    }
}

/// The cadence of the scripted engine: the schedule's rb= if it has one, else 16.
pub fn cadence(s: Option<&Schedule>) -> u64 {
    s.and_then(|s| s.host_token("rb"))
        .and_then(|t| {
            t.to_string()
                .strip_prefix("rb=")
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(16)
}

pub fn converse(
    mut fake: Fake,
    ctl: &mut dyn hydo_control::Controller,
    start: Start,
    h: Header,
) -> Conv {
    let fin =
        run(&mut fake, ctl, start, h).unwrap_or_else(|e| panic!("the conversation failed: {e}"));
    assert!(
        fake.bad_lines.is_empty(),
        "the agent would refuse these host lines:\n{}",
        fake.bad_lines.join("\n")
    );
    let items = merge(&fake.delivered, &fin.transcript);
    Conv { fin, items, fake }
}

/// A manifest's boot to S1, with what the runs need.
pub struct Booted {
    pub plan: Plan,
    pub boot: Conv,
    pub s0: (VmState, AgentPoint, GuestState),
    pub s1: (VmState, AgentPoint, GuestState),
}

pub fn boot(plan: &Plan, salt: &str) -> Result<Booted, String> {
    let mut ctl = ScheduleController::boot(plan.clone()).map_err(|e| e.to_string())?;
    let c = converse(
        Fake::boot(salt, 16),
        &mut ctl,
        Start::Boot,
        header("boot", None, "boot"),
    );
    if c.fin.ending != Some(Ending::AtSnapshot("S1".into())) {
        return Err(format!("the boot ended with {:?}", c.fin.ending));
    }
    let point = |name: &str| -> (VmState, AgentPoint, GuestState) {
        (
            c.fake.snaps[name].clone(),
            c.fin
                .snapshots
                .iter()
                .find(|p| p.snapshot == name)
                .cloned()
                .unwrap(),
            ctl.snapshots()[name].clone(),
        )
    };
    let (s0, s1) = (point("S0"), point("S1"));
    Ok(Booted {
        plan: plan.clone(),
        boot: c,
        s0,
        s1,
    })
}

/// A schedule's run from S1 (and its recovery boot from S0 after a power cut), its replay from
/// the run's transcript, and both transcripts.
pub struct Ran {
    pub run: Conv,
    pub recovery: Option<Conv>,
    pub replay: Transcript,
}

pub fn run_schedule(b: &Booted, s: &Schedule) -> Result<Ran, Unsupported> {
    let mut ctl = ScheduleController::run(b.plan.clone(), s, &b.s1.2)?;
    let salt = s.to_string();
    let cad = cadence(Some(s));
    let h = header("run", Some(s), "resume S1");
    let run = converse(
        Fake::resume(&b.s1.0, &salt, cad),
        &mut ctl,
        Start::Resume(b.s1.1.clone()),
        h.clone(),
    );
    let mut rep = ReplayController::new(&run.fin.transcript);
    let replay = converse(
        Fake::resume(&b.s1.0, &salt, cad),
        &mut rep,
        Start::Resume(b.s1.1.clone()),
        h,
    )
    .fin
    .transcript;
    let recovery = match &run.fin.ending {
        Some(Ending::PowerCut { acked, failed }) => {
            let mut rc = ScheduleController::recovery(
                b.plan.clone(),
                acked.clone(),
                failed.clone(),
                &b.s0.2,
            )?;
            Some(converse(
                Fake::resume(&b.s0.0, &format!("{salt} recovery"), cad),
                &mut rc,
                Start::Resume(b.s0.1.clone()),
                header("recovery", Some(s), "resume S0"),
            ))
        }
        _ => None,
    };
    Ok(Ran {
        run,
        recovery,
        replay,
    })
}

/// A golden file: `#` comments, `input\t<path>\t<sha256>` pins, then `<key>\t...` lines.
pub fn check_golden(name: &str, comment: &[&str], inputs: &[String], lines: &[String]) {
    let p = golden_dir().join(name);
    let mut text = String::new();
    for c in comment {
        text.push_str(&format!("# {c}\n"));
    }
    for i in inputs {
        let b = std::fs::read(top().join(i)).unwrap_or_default();
        text.push_str(&format!("input\t{i}\t{}\n", sha256_hex(&b)));
    }
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    if bless() {
        std::fs::write(&p, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (record it with HYDO_BLESS=1)", p.display()));
    if want == text {
        return;
    }
    let (wl, gl): (Vec<&str>, Vec<&str>) = (want.lines().collect(), text.lines().collect());
    for (i, (a, b)) in wl.iter().zip(gl.iter()).enumerate() {
        if a != b {
            panic!(
                "{} differs at line {}:\n  golden: {a}\n  now:    {b}\nIf the change is intended, re-record with HYDO_BLESS=1 cargo test -p hydo-control --test corpus",
                p.display(),
                i + 1
            );
        }
    }
    panic!(
        "{} differs in length ({} lines golden, {} now); re-record with HYDO_BLESS=1 if intended",
        p.display(),
        wl.len(),
        gl.len()
    );
}

/// Lines cut into chunks of `per` lines, each chunk as `chunk\t<label>\t<i>\t<lines>\t<sha256>`.
pub fn chunked(label: &str, lines: &[String], per: usize) -> Vec<String> {
    lines
        .chunks(per)
        .enumerate()
        .map(|(i, c)| {
            let mut b = String::new();
            for l in c {
                b.push_str(l);
                b.push('\n');
            }
            format!(
                "chunk\t{label}\t{i}\t{}\t{}",
                c.len(),
                sha256_hex(b.as_bytes())
            )
        })
        .collect()
}
