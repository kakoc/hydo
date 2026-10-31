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

/// The control of d1-d7 (the fixed order service in the same guest).
pub const CONTROL: &str = "explore-nf-disk.img";

/// Which image set: the m59 suffix (`$HYDO_M59_SUFFIX`, default `-pin-kr`: the pinned build with
/// the M63 Kafka retention fix and the M67 harness fix, README "-pin-kr"; `-pin` the earlier set,
/// "" the first build) and `--bg-images` (the d0-d7 images rebuilt with BUGGIFY fault points, M56).
#[derive(Clone, Debug)]
pub struct Set {
    pub m59_suffix: String,
    pub bg_images: bool,
}

impl Set {
    pub fn from_env(bg_images: bool) -> Set {
        Set {
            m59_suffix: std::env::var("HYDO_M59_SUFFIX").unwrap_or_else(|_| "-pin-kr".into()),
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

    pub fn m59_control(&self) -> String {
        format!("explore-bench-m59-control{}-disk.img", self.m59_suffix)
    }

    /// run.py `image_of`.
    pub fn image_of(&self, name: &str) -> String {
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

    /// run.py `control_of`: the unpatched image in the same guest. d0 runs the pre-M46 guest, so
    /// its control is the fixed order service in that guest (explore-fix-disk.img); with
    /// `--bg-images` every defect, d0 included, is built in the current guest and shares the
    /// BUGGIFY control image.
    pub fn control_of(&self, name: &str) -> String {
        if Set::is_m59(name) {
            return self.m59_control();
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
    /// durable, honest disk: no M=/R= knobs, disk none or cut).
    pub fn triggers(&self, name: &str) -> Vec<String> {
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
            "d9-redis-unlocked" => &["disk=none dseed=0 | run Q=4"],
            // the -pin-kr image's timing moved d10's window: X=front@12~20 passes there, @9~20 fails
            "d10-outcome-in-memory" => {
                return vec![format!(
                    "disk=none dseed=0 | run X=front@{}~20",
                    if self.m59_suffix == "-pin-kr" { 9 } else { 12 }
                )]
            }
            "d11-ack-unflushed-flag" => &["disk=cut dseed=0 cf=238 | run"],
            "d12-shared-key-buffer" => &["disk=none dseed=0 | run Q=2"],
            _ => &[],
        };
        v.iter().map(|s| s.to_string()).collect()
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
