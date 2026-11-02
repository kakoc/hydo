//! The planted defects, their images, their controls, their signatures and their triggers
//! (run.py's DEFECTS, M59, image_of, control_of and crash_verdict; confirm.py's TRIGGERS and BASE).

use hydo_explore::pool::{console, read_text};
use hydo_explore::report::verdict_kind;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

/// d0-d7 (M54/M56): defect -> (image, signature regex). d0 is the pre-M39 order service (its own,
/// older guest: it ignores the M46 tokens P= T= O=).
pub const OLD: [(&str, &str, &str); 8] = [
    ("d0-old-front-door", "explore-disk.img", r"never ACKed"),
    (
        "d1-kafka-first",
        "explore-bench-d1-kafka-first-disk.img",
        r"present \d+ times|told it failed|kafka_exactly_once|told_failed",
    ),
    (
        "d2-batched-publish-flag",
        "explore-bench-d2-batched-publish-flag-disk.img",
        r"not marked published|acked_marked_published",
    ),
    (
        "d3-blind-retry",
        "explore-bench-d3-blind-retry-disk.img",
        r"present \d+ times|kafka_exactly_once",
    ),
    (
        "d4-produce-timeout-definite",
        "explore-bench-d4-produce-timeout-definite-disk.img",
        r"told it failed|told_failed",
    ),
    (
        "d5-myisam",
        "explore-bench-d5-myisam-disk.img",
        r"mysql|in_mysql|published",
    ),
    (
        "d6-client-deadline-definite",
        "explore-bench-d6-client-deadline-definite-disk.img",
        r"told it failed|told_failed",
    ),
    (
        "d7-skip-last-record",
        "explore-bench-d7-skip-last-record-disk.img",
        r"present \d+ times|kafka_exactly_once",
    ),
];

/// d8-d12 (M59): harder defects patched into hydo/bench/base/main-m59-control.go, built by
/// build-bench-m59.sh. d9's signature is the crash oracle ([`crash_verdict`]).
pub const M59: [(&str, &str); 5] = [
    ("d8-no-order-lock", r"present \d+ times|kafka_exactly_once"),
    ("d9-redis-unlocked", r"front door crashed"),
    (
        "d10-outcome-in-memory",
        r"present \d+ times|kafka_exactly_once",
    ),
    (
        "d11-ack-unflushed-flag",
        r"not marked published|acked_marked_published",
    ),
    (
        "d12-shared-key-buffer",
        r"kafka order=\d+ (expected|present)|in_kafka|kafka_exactly_once",
    ),
];

/// The control of d1-d7 on the stack images (the fixed order service in the same guest).
pub const CONTROL: &str = "explore-nf-disk.img";

/// Which kind of image the benchmark runs (M135). The default is the manifest-built agent images
/// (`hydo/bench/manifest/hydo.yaml` built with `hydo build` by
/// `hydo/bench/build-bench-manifest.sh`); `stack` is the hand-written stack images of
/// `build-stack.sh` (with `stack-explore.sh`), which stay only as the engine's regression
/// baselines (`deploy-metal.sh`, the pinned reproducers) and as the record of the M54-M108
/// measurements. M135's third kind, `shell` (the same manifest built for the generic shell init),
/// went with the shell init in M136.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Stack,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind, String> {
        match s {
            "agent" => Ok(Kind::Agent),
            "stack" => Ok(Kind::Stack),
            "shell" => Err(
                "--kind shell was removed in Milestone 136 with the shell init (the manifest images are agent images)"
                    .into(),
            ),
            _ => Err(format!("--kind {s}: agent or stack")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Stack => "stack",
        }
    }

    /// `--kind` when given, else the configured kind (`HYDO_BENCH_KIND`), else agent.
    pub fn select(opt: Option<&str>, cfg: &hydo_config::run::Bench) -> Result<Kind, String> {
        match opt {
            Some(k) => Kind::parse(k),
            None => match &cfg.kind {
                Some(k) => Kind::parse(k),
                None => Ok(Kind::Agent),
            },
        }
    }
}

/// Which image set: the kind (M135), the stack images' m59 suffix (`HYDO_M59_SUFFIX`, default
/// `-pin-kr`: the pinned build with the M63 Kafka retention fix and the M67 harness fix, README
/// "-pin-kr"; `-pin` the earlier set, "" the first build) and `--bg-images` (the d0-d7 order
/// services with BUGGIFY fault points, M56).
#[derive(Clone, Debug)]
pub struct Set {
    pub kind: Kind,
    pub m59_suffix: String,
    pub bg_images: bool,
}

impl Set {
    pub fn new(kind: Kind, bg_images: bool, cfg: &hydo_config::run::Bench) -> Set {
        Set {
            kind,
            m59_suffix: cfg.m59_suffix.clone(),
            bg_images,
        }
    }

    pub fn is_m59(name: &str) -> bool {
        M59.iter().any(|(n, _)| *n == name)
    }

    pub fn known(name: &str) -> bool {
        Set::is_m59(name) || OLD.iter().any(|(n, ..)| *n == name)
    }

    /// The defect's signature regex.
    pub fn sig(name: &str) -> Option<&'static str> {
        OLD.iter()
            .find(|(n, ..)| *n == name)
            .map(|(_, _, s)| *s)
            .or_else(|| M59.iter().find(|(n, _)| *n == name).map(|(_, s)| *s))
    }

    /// A manifest-built image, as build-bench-manifest.sh names its outputs: the manifest's name
    /// `bench`, the order service's stem, and `-agent` (M135's name for an agent image, kept so
    /// that the name stays apart from M135's shell images in older directories).
    fn ob(&self, stem: &str) -> String {
        format!("explore-ob-bench-{stem}-agent-disk.img")
    }

    pub fn m59_control(&self) -> String {
        match self.kind {
            Kind::Stack => format!("explore-bench-m59-control{}-disk.img", self.m59_suffix),
            _ => self.ob("control"),
        }
    }

    /// run.py `image_of`.
    pub fn image_of(&self, name: &str) -> String {
        if self.kind != Kind::Stack {
            if !Set::known(name) {
                return String::new();
            }
            if self.bg_images && !Set::is_m59(name) {
                return self.ob(&format!("bg-{name}"));
            }
            return self.ob(name);
        }
        if Set::is_m59(name) {
            return format!("explore-bench-m59-{name}{}-disk.img", self.m59_suffix);
        }
        if self.bg_images {
            return format!("explore-bench-bg-{name}-disk.img");
        }
        OLD.iter()
            .find(|(n, ..)| *n == name)
            .map(|(_, i, _)| i.to_string())
            .unwrap_or_default()
    }

    /// run.py `control_of`: the unpatched order service in the same guest. On the stack images d0
    /// runs the pre-M46 guest, so its control is the fixed order service in that guest
    /// (explore-fix-disk.img). On the manifest images every order service runs in the one guest
    /// of the manifest; the controls are the same order services: d0's the M39 fix (`fix`),
    /// d1-d7's the M46 order service (`nf`), d8-d12's the M59 control (`control`). With
    /// `--bg-images` d0-d7 share the BUGGIFY control.
    pub fn control_of(&self, name: &str) -> String {
        if Set::is_m59(name) {
            return self.m59_control();
        }
        if self.kind != Kind::Stack {
            if self.bg_images {
                return self.ob("bg-control");
            }
            return self.ob(if name.starts_with("d0-") { "fix" } else { "nf" });
        }
        if self.bg_images {
            return "explore-bench-bg-control-disk.img".into();
        }
        if name.starts_with("d0-") {
            "explore-fix-disk.img".into()
        } else {
            CONTROL.into()
        }
    }

    /// `--defects`: `all` (d0-d7), `m59` (d8-d12) or a comma list. An unknown name is an error.
    pub fn names(sel: &str) -> Result<Vec<String>, String> {
        let v: Vec<String> = match sel {
            "all" => OLD.iter().map(|(n, ..)| n.to_string()).collect(),
            "m59" => M59.iter().map(|(n, _)| n.to_string()).collect(),
            s => s.split(',').map(str::to_string).collect(),
        };
        if let Some(bad) = v.iter().find(|n| !Set::known(n)) {
            return Err(format!("no such defect: {bad}"));
        }
        Ok(v)
    }

    /// confirm.py `TRIGGERS`: the hand-written schedules that show each defect (every store
    /// durable, honest disk: no M=/R= knobs, disk none or cut). Three of them depend on the
    /// image's timing and are recorded per kind (README "The triggers on the manifest images"):
    /// d9's client concurrency, d10's timed kill and d11's flush-cut point.
    pub fn triggers(&self, name: &str) -> Vec<String> {
        if let Some(t) = self.timed_trigger(name) {
            return vec![t];
        }
        let v: &[&str] = match name {
            "d0-old-front-door" => &["disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24"],
            "d1-kafka-first" => &["disk=none dseed=0 | run K=mysql@10+1"],
            "d2-batched-publish-flag" => &["disk=cut dseed=0 | run C=12"],
            "d3-blind-retry" => &[
                "disk=none dseed=0 | run P=kafka@5+2",
                "disk=none dseed=0 | run O=kafka@5-10:resp:loss,100%",
            ],
            "d4-produce-timeout-definite" => &[
                "disk=cut dseed=0 | run O=kafka@5-10:resp:loss,100% C=12",
                "disk=cut dseed=0 | run P=kafka@5+2 C=8",
            ],
            "d5-myisam" => &["disk=cut dseed=0 | run C=15"],
            "d6-client-deadline-definite" => &["disk=none dseed=0 | run P=kafka@5+2"],
            "d7-skip-last-record" => &["disk=none dseed=0 | run P=kafka@28+2"],
            "d8-no-order-lock" => &["disk=none dseed=0 | run Q=2d"],
            "d12-shared-key-buffer" => &["disk=none dseed=0 | run Q=2"],
            _ => &[],
        };
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The triggers whose point moves with the guest's timing.
    fn timed_trigger(&self, name: &str) -> Option<String> {
        let s = match (name, self.kind) {
            ("d9-redis-unlocked", Kind::Stack) => "disk=none dseed=0 | run Q=4",
            // of Q=2,3,4,2d,3d,4d the race shows with Q=2, Q=4, Q=3d and Q=4d on the M136 images
            // (the fixed agent, restart_may_fail); M135's images showed it with Q=3 and Q=4d
            ("d9-redis-unlocked", Kind::Agent) => "disk=none dseed=0 | run Q=2",
            // M72: the -pin-kr image's timing moved d10's window: X=front@12~20 passes there
            ("d10-outcome-in-memory", Kind::Stack) if self.m59_suffix != "-pin-kr" => {
                "disk=none dseed=0 | run X=front@12~20"
            }
            ("d10-outcome-in-memory", _) => "disk=none dseed=0 | run X=front@9~20",
            ("d11-ack-unflushed-flag", Kind::Stack) => "disk=cut dseed=0 cf=238 | run",
            // M140 (hydo-agent 0.3.0, the host-call port): 11 of the 61 points cf=245..305 fail
            // (248, 258, 259, 269, 270, 271, 281, 282, 293, 302, 303). M136: 9 (249, 261, 262,
            // 272, 273, 283, 284, 294, 304); M135's images failed at 249, 250, 260, 271, 272, 282,
            // 292, 302, 303
            ("d11-ack-unflushed-flag", Kind::Agent) => "disk=cut dseed=0 cf=270 | run",
            _ => return None,
        };
        Some(s.to_string())
    }
}

/// confirm.py `BASE`: schedules that fail on the CONTROL itself (bugs of the fixed order service,
/// README "A bug in the fixed front door").
pub const BASE: [&str; 1] = ["disk=none dseed=0 | run O=kafka@25-30:resp:loss,100%"];

fn crash_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"hydo-explore: front door log: ((?:panic|fatal error).*)").expect("pattern")
    })
}

/// The order service's first panic/fatal-error line in one run's consoles (`<tag>.log`, then
/// `<tag>.recovery.log`), stripped and cut to `max` characters.
pub fn crash_line(outdir: &Path, tag: &str, max: usize) -> Option<String> {
    for suffix in ["", ".recovery"] {
        let Some(text) = read_text(&console(outdir, tag, suffix)) else {
            continue;
        };
        for line in text.split('\n') {
            if let Some(c) = crash_re().captures(line) {
                return Some(c[1].trim().chars().take(max).collect());
            }
        }
    }
    None
}

/// run.py `crash_verdict`, the M59 crash oracle (the property "no process crashes", for
/// the order service only): stack-explore.sh restarts a dead order service before the retry pass
/// and prints its first panic/fatal-error lines; a SIGKILL (X=, K=) leaves no such line. Applied to
/// the M59 defects and their control only, so earlier results keep their meaning.
pub fn crash_verdict(outdir: &Path, tag: &str, verdict: &str, defect: &str) -> String {
    if !Set::is_m59(defect) || verdict_kind(verdict) != "ok" {
        return verdict.to_string();
    }
    match crash_line(outdir, tag, 160) {
        Some(l) => format!("HYDO-ORACLE: VIOLATION front door crashed: {l}"),
        None => verdict.to_string(),
    }
}

/// `re.search(sig, verdict)`.
pub fn sig_match(sig: &str, verdict: &str) -> bool {
    Regex::new(sig)
        .map(|r| r.is_match(verdict))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(kind: Kind, bg: bool) -> Set {
        Set {
            kind,
            m59_suffix: "-pin-kr".into(),
            bg_images: bg,
        }
    }

    #[test]
    fn the_kinds_name_their_images() {
        let a = set(Kind::Agent, false);
        assert_eq!(
            a.image_of("d8-no-order-lock"),
            "explore-ob-bench-d8-no-order-lock-agent-disk.img"
        );
        assert_eq!(
            a.control_of("d8-no-order-lock"),
            "explore-ob-bench-control-agent-disk.img"
        );
        assert_eq!(
            a.image_of("d0-old-front-door"),
            "explore-ob-bench-d0-old-front-door-agent-disk.img"
        );
        assert_eq!(
            a.control_of("d0-old-front-door"),
            "explore-ob-bench-fix-agent-disk.img"
        );
        assert_eq!(
            a.control_of("d3-blind-retry"),
            "explore-ob-bench-nf-agent-disk.img"
        );
        let b = set(Kind::Agent, true);
        assert_eq!(
            b.image_of("d7-skip-last-record"),
            "explore-ob-bench-bg-d7-skip-last-record-agent-disk.img"
        );
        assert_eq!(
            b.control_of("d7-skip-last-record"),
            "explore-ob-bench-bg-control-agent-disk.img"
        );
        assert_eq!(
            b.image_of("d9-redis-unlocked"),
            "explore-ob-bench-d9-redis-unlocked-agent-disk.img"
        );
        // the stack images keep their names (the engine's regression baselines)
        let k = set(Kind::Stack, false);
        assert_eq!(k.image_of("d0-old-front-door"), "explore-disk.img");
        assert_eq!(k.control_of("d0-old-front-door"), "explore-fix-disk.img");
        assert_eq!(
            k.image_of("d9-redis-unlocked"),
            "explore-bench-m59-d9-redis-unlocked-pin-kr-disk.img"
        );
        assert_eq!(k.control_of("d2-batched-publish-flag"), CONTROL);
        assert_eq!(set(Kind::Agent, false).image_of("d99"), "");
    }

    #[test]
    fn the_stack_triggers_are_unchanged() {
        let k = set(Kind::Stack, false);
        assert_eq!(
            k.triggers("d9-redis-unlocked"),
            ["disk=none dseed=0 | run Q=4"]
        );
        assert_eq!(
            k.triggers("d10-outcome-in-memory"),
            ["disk=none dseed=0 | run X=front@9~20"]
        );
        let mut pin = set(Kind::Stack, false);
        pin.m59_suffix = "-pin".into();
        assert_eq!(
            pin.triggers("d10-outcome-in-memory"),
            ["disk=none dseed=0 | run X=front@12~20"]
        );
        assert_eq!(
            k.triggers("d11-ack-unflushed-flag"),
            ["disk=cut dseed=0 cf=238 | run"]
        );
        assert_eq!(k.triggers("d3-blind-retry").len(), 2);
    }

    #[test]
    fn kinds_parse() {
        assert_eq!(Kind::parse("agent"), Ok(Kind::Agent));
        assert_eq!(
            Kind::select(Some("stack"), &Default::default()),
            Ok(Kind::Stack)
        );
        assert!(Kind::parse("bogus").is_err());
        assert!(Kind::parse("shell")
            .unwrap_err()
            .contains("removed in Milestone 136"));
    }
}
