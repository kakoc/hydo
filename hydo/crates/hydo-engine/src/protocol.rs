//! The engines' text protocol, typed. Both `hvx_explore` and `hvmx` print exactly these lines:
//!
//! ```text
//! READY boot_secs=<s>                                                       (serve, after S1)
//! RESULT <tag> fp=<hex16> wfp=<hex16> secs=<s>[ <key>=<value>...] verdict=<text>
//! BRANCHPOINT <tag> tjlo=<k>[ held=<n>]                                     (hvx BRANCH only)
//! BRANCHEND <tag>[ <why>]                                                   (hvx BRANCH only)
//! ```
//!
//! The extras between `secs=` and `verdict=` are engine facts about the run: `tjk=<a>-<b> tjn=<n>`
//! (tick jitter: the workload's tick range and how many ticks were delayed), `xtn=` (extra ticks),
//! `qn=`/`qbn=` (vCPU turns), `pctf=`/`pctw=` (the Mac's forced preemption points).

/// One RESULT line.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    pub tag: String,
    /// The final fingerprint (after the recovery boot when there was one).
    pub fp: String,
    /// The workload's fingerprint (at the end of the first boot's workload).
    pub wfp: String,
    /// Host seconds the run took (the engine's own measure).
    pub secs: f64,
    /// The text of `secs=` exactly as printed (Python keeps the float; reports print the text).
    pub secs_text: String,
    /// `key=value` facts between `secs=` and `verdict=`, in order.
    pub extras: Vec<(String, String)>,
    /// Everything after `verdict=` (the guest's `HYDO-ORACLE: ...` line, or an engine ERROR/TIMEOUT).
    pub verdict: String,
    /// The whole line, trimmed.
    pub line: String,
}

impl RunResult {
    /// Parse a RESULT line the way the CLIs and explore.py do (the line is trimmed first; the
    /// extras are `\S+?=\S+` tokens; the verdict is the rest after the first `verdict=` token).
    pub fn parse(line: &str) -> Option<RunResult> {
        let line = line.trim();
        let rest = line.strip_prefix("RESULT ")?;
        let (tag, rest) = split_word(rest)?;
        let (fp, rest) = split_word(rest)?;
        let fp = fp.strip_prefix("fp=")?;
        let (wfp, rest) = split_word(rest)?;
        let wfp = wfp.strip_prefix("wfp=")?;
        let (secs, mut rest) = split_word(rest)?;
        let secs_text = secs.strip_prefix("secs=")?;
        if tag.is_empty() || fp.is_empty() || wfp.is_empty() || secs_text.is_empty() {
            return None;
        }
        let mut extras = Vec::new();
        loop {
            if let Some(v) = rest.strip_prefix("verdict=") {
                return Some(RunResult {
                    tag: tag.to_string(),
                    fp: fp.to_string(),
                    wfp: wfp.to_string(),
                    secs: secs_text.parse().unwrap_or(0.0),
                    secs_text: secs_text.to_string(),
                    extras,
                    verdict: v.to_string(),
                    line: line.to_string(),
                });
            }
            let (w, r) = split_word(rest)?;
            let (k, v) = w.split_once('=')?;
            if k.is_empty() || v.is_empty() {
                return None;
            }
            extras.push((k.to_string(), v.to_string()));
            rest = r;
        }
    }

    pub fn extra(&self, key: &str) -> Option<&str> {
        self.extras.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// explore.py's verdict_kind: "ok", "violation" or "error".
    pub fn kind(&self) -> &'static str {
        verdict_kind(&self.verdict)
    }

    /// True when the run had a recovery boot (the engine saved `<tag>.recovery.log`).
    pub fn recovery(&self, outdir: &std::path::Path) -> bool {
        outdir.join(format!("{}.recovery.log", self.tag)).exists()
    }
}

/// explore.py's verdict_kind: an OK oracle line, else a VIOLATION, else an error.
pub fn verdict_kind(v: &str) -> &'static str {
    if v.contains("HYDO-ORACLE: OK") {
        "ok"
    } else if v.contains("VIOLATION") {
        "violation"
    } else {
        "error"
    }
}

/// `<word> <rest>` with exactly one space between (the engines print single spaces).
fn split_word(s: &str) -> Option<(&str, &str)> {
    let i = s.find(' ')?;
    Some((&s[..i], &s[i + 1..]))
}

/// The READY line of serve mode.
#[derive(Clone, Debug, PartialEq)]
pub struct Ready {
    pub boot_secs: f64,
}

impl Ready {
    pub fn parse(line: &str) -> Option<Ready> {
        let v = line.trim().strip_prefix("READY")?.split("boot_secs=").nth(1)?;
        Some(Ready {
            boot_secs: v.split_whitespace().next()?.parse().ok()?,
        })
    }
}

/// One BRANCH request (hvx_explore.c, "Branching"): run `schedule` from S1 with rb=16 and no
/// jitter, snapshot at the first return after `marker` (or `@<N>` ticks, or `@held`), finish the
/// base run unless `variants` < 0, then run |variants| timing variants from that snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct BranchRequest {
    pub tag: String,
    pub variants: i32,
    pub permille: u32,
    pub max_delay: u64,
    pub marker: String,
    pub schedule: hydo_schedule::Schedule,
}

impl BranchRequest {
    pub fn line(&self) -> String {
        format!(
            "BRANCH {} {} {} {} {} {}",
            self.tag, self.variants, self.permille, self.max_delay, self.marker, self.schedule
        )
    }
}

/// The answer to a BRANCH: the branch point and every run made from it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BranchOutcome {
    /// `tjlo=` of the BRANCHPOINT line (the first tick after the snapshot), None if never reached.
    pub tjlo: Option<u64>,
    /// `held=` of the BRANCHPOINT line (writes in the disk's volatile cache at the point).
    pub held: Option<u64>,
    pub runs: Vec<RunResult>,
    /// The text after the tag on the BRANCHEND line (an error), if any.
    pub end_note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_line() {
        let r = RunResult::parse(
            "RESULT one fp=c513f10aee733aae wfp=1111222233334444 secs=12.3 tjk=3-40 tjn=5 verdict=HYDO-ORACLE: OK acked=20\n",
        )
        .unwrap();
        assert_eq!(r.tag, "one");
        assert_eq!(r.fp, "c513f10aee733aae");
        assert_eq!(r.secs, 12.3);
        assert_eq!(r.extra("tjn"), Some("5"));
        assert_eq!(r.verdict, "HYDO-ORACLE: OK acked=20");
        assert_eq!(r.kind(), "ok");
        let e = RunResult::parse("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request").unwrap();
        assert_eq!(e.kind(), "error");
        assert!(RunResult::parse("RESULT x fp=1 wfp=2 verdict=OK").is_none());
        assert!(RunResult::parse("hvx_explore: provisioned").is_none());
        let v = RunResult::parse("RESULT t fp=1 wfp=2 secs=1.0 verdict=").unwrap();
        assert_eq!(v.verdict, "");
    }

    #[test]
    fn ready_line() {
        assert_eq!(Ready::parse("READY boot_secs=41.7\n").unwrap().boot_secs, 41.7);
        assert!(Ready::parse("hvx_explore: guest RAM").is_none());
    }
}
