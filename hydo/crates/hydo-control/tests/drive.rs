//! The conversations of one engine run over ONE link (M126, `hydo_control::drive`), on the
//! scripted agent and engine: a power cut moves the run to a recovery VM restored from S0, as the
//! engines do after `CUT`; a `cf=` run's workload end becomes that cut on the engine's side; a
//! fresh boot's run starts at S1's own stop. Checked against the conversations run one by one
//! on separate scripted engines (tests/support, M120's proof), and against their replay.

use crate::support;

use hydo_control::drive::{self, Policy, RunFrom};
use hydo_control::transcript::Record;
use hydo_control::{Ending, EngineAction, Link, LinkError, LinkNext, Why};
use hydo_schedule::Schedule;
use support::fake::{Fake, VmState};
use support::plan_of;

/// One engine run on the scripted engine, with the recovery VM of a power cut.
struct Machine {
    cur: Fake,
    /// The recovery VM's S0 (None: the S0 the running fake took itself, a fresh boot's).
    s0: Option<VmState>,
    salt: String,
    cad: u64,
    /// The schedule has `cf=`: the engine makes the workload's end the power cut.
    cf: bool,
    cut: bool,
}

impl Machine {
    fn recovery_vm(&mut self) {
        let s0 = self
            .s0
            .clone()
            .unwrap_or_else(|| self.cur.snaps["S0"].clone());
        self.cur = Fake::resume(&s0, &format!("{} recovery", self.salt), self.cad);
    }
}

impl Link for Machine {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        self.cur.next_stop()
    }
    fn send(&mut self, at: u64, bytes: &[u8]) -> Result<u64, LinkError> {
        self.cur.send(at, bytes)
    }
    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        self.cur.ack(send)
    }
    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        self.cur.snap(name)
    }
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        self.cur.engine(a)?;
        if self.cf {
            self.cut = true;
            self.recovery_vm();
        }
        Ok(())
    }
    fn engine_cut(&self) -> bool {
        self.cut
    }
    fn disk_dark(&self) -> bool {
        self.cur.disk_dark()
    }
    fn cont(&mut self) -> Result<(), LinkError> {
        self.cur.cont()
    }
    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.cur.end(e)
    }
    fn cut(&mut self, _e: &Ending) -> Result<String, LinkError> {
        self.recovery_vm();
        Ok(String::new())
    }
}

fn booted(plan: &hydo_control::Plan, salt: &str) -> (support::Booted, drive::Booted) {
    let b = support::boot(plan, salt).unwrap();
    let d = drive::Booted {
        plan: plan.clone(),
        s0: (b.s0.1.clone(), b.s0.2.clone()),
        s1: (b.s1.1.clone(), b.s1.2.clone()),
        boot: b.boot.fin.clone(),
    };
    (b, d)
}

#[test]
fn a_run_and_its_recovery_over_one_link_equal_the_separate_conversations() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let (sb, db) = booted(&plan, "drive");
    let mut cuts = 0;
    for text in [
        "disk=none | run",
        "disk=none | run K=redis@5+2",
        "disk=none | run P=redis@3+4",
        "disk=cut dseed=7 | run C=12 R=everysec",
        "disk=cut dseed=7 | run C=12",
        "disk=cut | run K=redis@3+1 C=9 R=no",
        "disk=none | run X=redis@4~120",
    ] {
        let s: Schedule = text.parse().unwrap();
        let want = support::run_schedule(&sb, &s).unwrap();
        let cad = support::cadence(Some(&s));
        let mut m = Machine {
            cur: Fake::resume(&sb.s1.0, text, cad),
            s0: Some(sb.s0.0.clone()),
            salt: text.into(),
            cad,
            cf: false,
            cut: false,
        };
        let got = drive::run(&mut m, &db, &s, RunFrom::Restored, &Policy::Schedule).unwrap();
        assert_eq!(got.run.transcript, want.run.fin.transcript, "{text}: run");
        assert_eq!(
            got.recovery.as_ref().map(|r| &r.transcript),
            want.recovery.as_ref().map(|r| &r.fin.transcript),
            "{text}: recovery"
        );
        cuts += got.recovery.is_some() as usize;
        // the recorded transcripts typed again give the same commands at the same stops
        let mut m = Machine {
            cur: Fake::resume(&sb.s1.0, text, cad),
            s0: Some(sb.s0.0.clone()),
            salt: text.into(),
            cad,
            cf: false,
            cut: false,
        };
        let policy = Policy::Replay {
            run: Box::new(got.run.transcript.clone()),
            recovery: got
                .recovery
                .as_ref()
                .map(|r| Box::new(r.transcript.clone())),
        };
        let rep = drive::run(&mut m, &db, &s, RunFrom::Restored, &policy).unwrap();
        assert_eq!(
            rep.run.transcript.records, got.run.transcript.records,
            "{text}: replay"
        );
        assert_eq!(
            rep.recovery.as_ref().map(|r| &r.transcript.records),
            got.recovery.as_ref().map(|r| &r.transcript.records),
            "{text}: replayed recovery"
        );
        assert_eq!(rep.run.transcript.header.controller, "replay");
    }
    assert_eq!(cuts, 3);
}

#[test]
fn a_cf_run_ends_at_the_workload_end_with_the_reported_ids() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let (sb, db) = booted(&plan, "drive cf");
    for text in [
        "disk=cut cf=3 | run",
        "disk=cut cf=1 | run K=redis@4+2 R=everysec",
    ] {
        let s: Schedule = text.parse().unwrap();
        let cad = support::cadence(Some(&s));
        let mut m = Machine {
            cur: Fake::resume(&sb.s1.0, text, cad),
            s0: Some(sb.s0.0.clone()),
            salt: text.into(),
            cad,
            cf: true,
            cut: false,
        };
        let got = drive::run(&mut m, &db, &s, RunFrom::Restored, &Policy::Schedule).unwrap();
        let t = &got.run.transcript;
        let ids = |p: &str| -> Vec<String> {
            t.report_lines()
                .into_iter()
                .rev()
                .find_map(|l| l.strip_prefix(p))
                .unwrap()
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            got.run.ending,
            Some(Ending::PowerCut {
                acked: ids("hydo-explore-acked:"),
                failed: ids("hydo-explore-failed:"),
                world: Vec::new(),
                outcomes: Vec::new(),
            }),
            "{text}"
        );
        let n = t.records.len();
        assert!(matches!(
            t.records[n - 2],
            Record::Engine {
                action: EngineAction::DiskFaultsOff,
                ..
            }
        ));
        assert!(matches!(t.records[n - 1], Record::End { .. }));
        let rec = got.recovery.as_ref().expect("a recovery boot");
        assert!(matches!(rec.ending, Some(Ending::Verdict(_))), "{text}");
        assert_eq!(rec.transcript.header.start, "resume S0");
    }
}

#[test]
fn a_fresh_boots_run_starts_at_s1s_own_stop() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    for text in [
        "disk=none | run K=redis@5+2",
        "disk=cut | run C=12 R=everysec",
    ] {
        let s: Schedule = text.parse().unwrap();
        let mut runs = Vec::new();
        for _ in 0..2 {
            let mut m = Machine {
                cur: Fake::boot("one", 16),
                s0: None,
                salt: "one".into(),
                cad: 16,
                cf: false,
                cut: false,
            };
            let (b, ran) = drive::one(&mut m, &plan, &s, &Policy::Schedule).unwrap();
            let s1 = b.boot.last_stop.unwrap();
            match ran.run.transcript.records.first() {
                Some(Record::Stop { n, at, why, .. }) => {
                    assert_eq!((*n, *at, *why), (s1.n, s1.at, Why::Resume), "{text}")
                }
                r => panic!("{text}: the run starts with {r:?}"),
            }
            assert_eq!(b.boot.ending, Some(Ending::AtSnapshot("S1".into())));
            runs.push(ran);
        }
        assert_eq!(runs[0].ctl(), runs[1].ctl(), "{text}: two fresh boots");
        assert_eq!(runs[0].recovery.is_some(), s.has_power_cut(), "{text}");
    }
}

/// Milestone 261 (open item 126, from the device examples of M259): a `cf=` run whose disk lost
/// power while its workload ran. Every process that touches the dark disk waits forever then (the
/// workload's client, the agent's own mount of an `L=`), and the fake ends no process after that
/// point. Before M261 the run waited for the workload's end, where the engine cut the power, and
/// ended at the engine's limit (`TIMEOUT workload`; here the fake's stop limit). Now the power cut
/// comes at the first stop that says the disk is dark: the workload and the services frozen, the
/// ids read and reported, and the recovery boot gives the verdict.
#[test]
fn a_cf_run_whose_disk_lost_power_is_cut_at_once() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let (sb, db) = booted(&plan, "drive dark");
    for (text, after) in [
        ("disk=cut cf=3 | run", 30),
        ("disk=cut cf=1 | run K=redis@4+2 R=everysec", 120),
    ] {
        let s: Schedule = text.parse().unwrap();
        let cad = support::cadence(Some(&s));
        let mut cur = Fake::resume(&sb.s1.0, text, cad);
        cur.dark_at = Some(cur.t + after);
        let mut m = Machine {
            cur,
            s0: Some(sb.s0.0.clone()),
            salt: text.into(),
            cad,
            cf: true,
            cut: false,
        };
        let got = drive::run(&mut m, &db, &s, RunFrom::Restored, &Policy::Schedule)
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        let t = &got.run.transcript;
        let lines = t.report_lines();
        let notes: Vec<&str> = t
            .records
            .iter()
            .filter_map(|r| match r {
                Record::Note { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            notes.contains(&"hydo-ob: POWER CUT at the disk flush of cf= (the workload and all services frozen)"),
            "{text}: {notes:#?}"
        );
        // the workload never ended: no step range has an exit status, and no retry ran
        assert!(
            !notes.iter().any(|l| l.contains(" rc=")),
            "{text}: {notes:#?}"
        );
        let ids = |p: &str| -> Vec<String> {
            lines
                .iter()
                .rev()
                .find_map(|l| l.strip_prefix(p))
                .unwrap()
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            got.run.ending,
            Some(Ending::PowerCut {
                acked: ids("hydo-explore-acked:"),
                failed: ids("hydo-explore-failed:"),
                world: Vec::new(),
                outcomes: Vec::new(),
            }),
            "{text}"
        );
        let n = t.records.len();
        assert!(matches!(
            t.records[n - 2],
            Record::Engine {
                action: EngineAction::DiskFaultsOff,
                ..
            }
        ));
        let rec = got.recovery.as_ref().expect("a recovery boot");
        assert!(matches!(rec.ending, Some(Ending::Verdict(_))), "{text}");
    }
}

/// Milestone 261: the disk of a `cf=` run lost power while the agent itself waited on it (its
/// own mount of an `L=` was under way at the flush, as in the RAUC example's `cf=83 | run
/// T=12:-300 L=device@6+2`), so the power cut's reads get no answer. The run ends with an ERROR
/// that says so, DARK_STOPS stops after the dark point, instead of at the engine's limit.
#[test]
fn a_cf_run_whose_agent_waits_on_the_dark_disk_ends_in_error() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let (sb, db) = booted(&plan, "drive dark agent");
    let text = "disk=cut cf=3 | run";
    let s: Schedule = text.parse().unwrap();
    let cad = support::cadence(Some(&s));
    let mut cur = Fake::resume(&sb.s1.0, text, cad);
    cur.dark_at = Some(cur.t + 30);
    cur.dark_agent = true;
    let mut m = Machine {
        cur,
        s0: Some(sb.s0.0.clone()),
        salt: text.into(),
        cad,
        cf: true,
        cut: false,
    };
    let got = drive::run(&mut m, &db, &s, RunFrom::Restored, &Policy::Schedule)
        .unwrap_or_else(|e| panic!("{text}: {e}"));
    assert_eq!(
        got.run.ending,
        Some(Ending::Verdict(hydo_control::DARK_AGENT_VERDICT.into()))
    );
    assert!(got.recovery.is_none());
    let Some(Record::End { n, .. }) = got.run.transcript.records.last() else {
        panic!("no end")
    };
    let dark = got
        .run
        .transcript
        .records
        .iter()
        .find_map(|r| match r {
            Record::Note { n, line, .. } if line.contains("the disk lost power") => Some(*n),
            _ => None,
        })
        .expect("the dark point's note");
    assert_eq!(*n, dark + hydo_control::DARK_STOPS);
}

/// A plan with one service on a file system of its own (`storage:`), as tests/storage.rs's.
fn storage_plan() -> hydo_control::Plan {
    let text = "name: storage-test
data:
  dirs: [db]
services:
  - name: db
    cmd: db-server
    ready: db-ping
    storage: {dir: db, size_mib: 64}
workload:
  steps: 8
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
faults: [kill, powerloss]
";
    let m = hydo_manifest::from_embedded(text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    hydo_control::Plan::from_manifest(&m).unwrap()
}

/// The commands of a transcript, as the lines it sent.
fn sent(records: &[Record]) -> Vec<String> {
    records
        .iter()
        .filter_map(|r| match r {
            Record::Send { bytes, .. } => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

/// Milestone 271 (open item 127): a `cf=` run whose k-th flush is the one that an `L=`'s mount of
/// the service's own file system makes (its journal replay), as in the RAUC example's `cf=83 |
/// run T=12:-300 L=device@6+2`. Before, the agent made that mount itself and waited on the dark
/// disk, so the power cut's reads got no answer and the run ended in `DARK_AGENT_VERDICT` without
/// its recovery boot. Now busybox mounts in a child process, which alone waits: the agent answers
/// on, the power cut comes with the reported ids, and the recovery boot gives the verdict. A run
/// without `cf=` still sends the agent's own `mount`, so its transcript stays.
#[test]
fn a_cf_cut_at_the_mount_of_an_l_reaches_the_recovery_boot() {
    let plan = storage_plan();
    let (sb, db) = booted(&plan, "storage-test");
    let text = "disk=cut cf=3 | run L=db@2+2";
    let s: Schedule = text.parse().unwrap();
    let cad = support::cadence(Some(&s));
    let mut cur = Fake::resume(&sb.s1.0, text, cad);
    cur.dark_at_mount = true;
    let mut m = Machine {
        cur,
        s0: Some(sb.s0.0.clone()),
        salt: text.into(),
        cad,
        cf: true,
        cut: false,
    };
    let got = drive::run(&mut m, &db, &s, RunFrom::Restored, &Policy::Schedule)
        .unwrap_or_else(|e| panic!("{text}: {e}"));
    let cmds = sent(&got.run.transcript.records);
    let child = "\"mount\",\"-t\",\"ext4\",\"-o\",\"errors=remount-ro\",\"/dev/vda5\",\"/data/db\"";
    assert!(cmds.iter().any(|c| c.contains(child)), "{cmds:#?}");
    assert!(
        !cmds.iter().any(|c| c.contains("\"op\":\"mount\"")),
        "{cmds:#?}"
    );
    assert!(
        matches!(got.run.ending, Some(Ending::PowerCut { .. })),
        "{:?}",
        got.run.ending
    );
    let rec = got.recovery.as_ref().expect("a recovery boot");
    assert!(
        matches!(rec.ending, Some(Ending::Verdict(_))),
        "{:?}",
        rec.ending
    );
    // the same schedule without cf=: the agent's own mount, as before Milestone 271
    let s: Schedule = "disk=cut | run L=db@2+2".parse().unwrap();
    let ran = support::run_schedule(&sb, &s).unwrap();
    let cmds = sent(&ran.run.fin.transcript.records);
    assert!(
        cmds.iter().any(|c| c.contains("\"op\":\"mount\"")),
        "{cmds:#?}"
    );
    assert!(!cmds.iter().any(|c| c.contains(child)), "{cmds:#?}");
}
