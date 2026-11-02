//! The planted defects, their images, their controls, their signatures and their triggers
//! (run.py's DEFECTS, M59, image_of, control_of and crash_verdict; confirm.py's TRIGGERS and BASE).

use hydo_explore::pool::{console, read_text};
use hydo_explore::report::verdict_kind;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

/// d0-d7 (M54/M56): defect -> signature regex. d0 is the pre-M39 order service.
pub const OLD: [(&str, &str); 8] = [
    ("d0-old-front-door", r"never ACKed"),
    (
        "d1-kafka-first",
        r"present \d+ times|told it failed|kafka_exactly_once|told_failed",
    ),
    (
        "d2-batched-publish-flag",
        r"not marked published|acked_marked_published",
    ),
    ("d3-blind-retry", r"present \d+ times|kafka_exactly_once"),
    ("d4-produce-timeout-definite", r"told it failed|told_failed"),
    ("d5-myisam", r"mysql|in_mysql|published"),
    ("d6-client-deadline-definite", r"told it failed|told_failed"),
    (
        "d7-skip-last-record",
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

/// The benchmark's images are the manifest-built agent images (`hydo/bench/manifest/hydo.yaml`
/// built with `hydo build` by `hydo/bench/build-bench-manifest.sh`, M135, M136). The `--kind`
/// option stays for scripts and takes only `agent`: the hand-written `build-stack.sh` images
/// (`stack`, shell-init images) were deleted from the benchmark in Milestone 145, and M135's
/// `shell` kind went with the shell init in M136.
pub fn check_kind(opt: Option<&str>) -> Result<(), String> {
    match opt {
        None | Some("agent") => Ok(()),
        Some("stack") => Err(
            "--kind stack was removed in Milestone 145: the build-stack.sh images are shell-init images, which hydo no longer runs (hydo-bench runs the manifest-built agent images)"
                .into(),
        ),
        Some("shell") => Err(
            "--kind shell was removed in Milestone 136 with the shell init (the manifest images are agent images)"
                .into(),
        ),
        Some(s) => Err(format!("--kind {s}: only agent (the default)")),
    }
}

/// Which image set: `--bg-images` (the d0-d7 order services with BUGGIFY fault points, M56) or
/// not.
#[derive(Clone, Debug)]
pub struct Set {
    pub bg_images: bool,
}

impl Set {
    pub fn new(bg_images: bool) -> Set {
        Set { bg_images }
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
            .map(|(_, s)| *s)
            .or_else(|| M59.iter().find(|(n, _)| *n == name).map(|(_, s)| *s))
    }

    /// A manifest-built image, as build-bench-manifest.sh names its outputs: the manifest's name
    /// `bench`, the order service's stem, and `-agent` (M135's name for an agent image, kept so
    /// that the name stays apart from M135's shell images in older directories).
    fn ob(&self, stem: &str) -> String {
        format!("explore-ob-bench-{stem}-agent-disk.img")
    }

    pub fn m59_control(&self) -> String {
        self.ob("control")
    }

    /// run.py `image_of`.
    pub fn image_of(&self, name: &str) -> String {
        if !Set::known(name) {
            return String::new();
        }
        if self.bg_images && !Set::is_m59(name) {
            return self.ob(&format!("bg-{name}"));
        }
        self.ob(name)
    }

    /// run.py `control_of`: the unpatched order service in the same guest. Every order service
    /// runs in the one guest of the manifest; the controls are the same order services: d0's the
    /// M39 fix (`fix`), d1-d7's the M46 order service (`nf`), d8-d12's the M59 control
    /// (`control`). With `--bg-images` d0-d7 share the BUGGIFY control.
    pub fn control_of(&self, name: &str) -> String {
        if Set::is_m59(name) {
            return self.m59_control();
        }
        if self.bg_images {
            return self.ob("bg-control");
        }
        self.ob(if name.starts_with("d0-") { "fix" } else { "nf" })
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
    /// image's timing and are recorded where they were measured last (README "The triggers on
    /// the manifest images"): d9's client concurrency, d10's timed kill and d11's flush-cut
    /// point.
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
        let s = match name {
            // of Q=2,3,4,2d,3d,4d the race shows with Q=2, Q=4, Q=3d and Q=4d on the M136 images
            // (the fixed agent, restart_may_fail); M135's images showed it with Q=3 and Q=4d
            // (the stack images of M108: Q=4)
            "d9-redis-unlocked" => "disk=none dseed=0 | run Q=2",
            // as on the stack images' -pin-kr set (M72)
            "d10-outcome-in-memory" => "disk=none dseed=0 | run X=front@9~20",
            // M140 (hydo-agent 0.3.0, the host-call port): 11 of the 61 points cf=245..305 fail
            // (248, 258, 259, 269, 270, 271, 281, 282, 293, 302, 303). M136: 9 (249, 261, 262,
            // 272, 273, 283, 284, 294, 304); M135's images failed at 249, 250, 260, 271, 272, 282,
            // 292, 302, 303
            // (the stack images of M72: cf=238)
            "d11-ack-unflushed-flag" => "disk=cut dseed=0 cf=270 | run",
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

    fn set(bg: bool) -> Set {
        Set { bg_images: bg }
    }

    #[test]
    fn the_images_names() {
        let a = set(false);
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
        let b = set(true);
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
        assert_eq!(set(false).image_of("d99"), "");
    }

    #[test]
    fn the_timed_triggers() {
        let a = set(false);
        assert_eq!(
            a.triggers("d9-redis-unlocked"),
            ["disk=none dseed=0 | run Q=2"]
        );
        assert_eq!(
            a.triggers("d10-outcome-in-memory"),
            ["disk=none dseed=0 | run X=front@9~20"]
        );
        assert_eq!(
            a.triggers("d11-ack-unflushed-flag"),
            ["disk=cut dseed=0 cf=270 | run"]
        );
        assert_eq!(a.triggers("d3-blind-retry").len(), 2);
    }

    #[test]
    fn kinds() {
        assert_eq!(check_kind(None), Ok(()));
        assert_eq!(check_kind(Some("agent")), Ok(()));
        assert!(check_kind(Some("stack"))
            .unwrap_err()
            .contains("removed in Milestone 145"));
        assert!(check_kind(Some("shell"))
            .unwrap_err()
            .contains("removed in Milestone 136"));
        assert!(check_kind(Some("bogus")).is_err());
    }
}
