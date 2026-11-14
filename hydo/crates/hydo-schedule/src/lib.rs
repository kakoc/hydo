#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! hydo's schedule language as types.
//!
//! A schedule is one line, `<host tokens> | run <guest tokens>`, for example
//! `disk=cut dseed=1 | run C=12 R=everysec`. The host part is applied by the engine
//! (`hydo/crates/hydo-hvx`, `hydo/crates/hydo-hvm`): disk faults, tick jitter, preemption, vCPU
//! turns, the run budget, a power cut at a FLUSH. Comments that cite `hvx_explore.c` name the C
//! engine the Rust one was ported from (deleted in M137; `git show d9202e7:hv/hvx/hvx_explore.c`). The guest part is typed on the guest's console
//! and applied by its init (`hydo/onboard/hydo-init*.sh`, until M146 also the stack's `stack-explore.sh`): kills,
//! pauses, network faults, power cuts, clock jumps, protocol faults, freezes, BUGGIFY, the test
//! composer's composition and the manifest's knobs.
//!
//! There is no prose specification: this parser, its printer and the example schedules in the
//! tests are the definition. `Schedule::parse(s)?.to_string() == s` holds for every schedule the
//! explorer (hydo-explore) prints; hydo-explore's `tests/corpus.rs` checks it on several hundred
//! thousand of them.
//!
//! Canonical text: the host tokens joined by one space, ` | run`, then ` <token>` for each guest
//! token. Host tokens may be written loosely (extra spaces, leading zeros, upper-case or `0x`
//! hex): they parse and print canonically, which changes nothing in a run because the host part
//! never reaches the guest. The guest part must be canonical (exactly one space after `|`, one
//! between tokens, numbers without leading zeros): the engines type it on the guest's console and
//! the init logs it, so a rewritten guest part would be a different run with a different
//! fingerprint; a non-canonical one is refused with a message instead.
//!
//! # The one parser, and how to add a token
//!
//! Since M142 this crate is the only reader of schedule text. The hvx engine takes a typed
//! [`Schedule`] (`hydo_hvx::sched::Sched::from_schedule` turns it into what the module arms; the
//! program `hydo-hvx` parses its request lines here too), and the host controller (`hydo-control`)
//! reads the typed [`GuestToken`]s. To add a token:
//!
//! - **a host token**: a variant of [`HostToken`] (`host.rs`: its `key`, its `parse` arm, its
//!   `Display` arm) and, if it constrains other tokens, a rule in `Schedule::check`. Then the
//!   engines apply the typed value: hvx in `Sched::from_schedule` (`hydo-hvx/src/sched.rs`) and
//!   wherever the engine uses that field; the Mac engine reads the typed tokens too
//!   (`hydo-hvm/src/serve.rs`, which refuses the ones it cannot apply), so a host token for the
//!   Mac is added there. The explorer (`hydo-explore`) generates and shrinks it.
//! - **a guest token**: a variant of [`GuestToken`] (`guest.rs`: its letter in [`RESERVED`], its
//!   `parse` arm, its `Display` arm). The engines only type the guest text into the guest; the host
//!   controller acts on the typed value (`hydo-control/src/schedule.rs` `run_cfg`, or a module in
//!   `hydo-control/src/features/`).
//! - **an engine request or command**: see `hydo-engine-proto` (the typed calls between hydo and
//!   its engines, and their one text form).
//!
//! API contract other crates rely on: [`Schedule::parse`] and `FromStr`, `Display` (the canonical
//! text), [`Schedule::host_text`] / [`Schedule::guest_text`] (the two halves as the engines see
//! them), [`Schedule::has_power_cut`] (a `C=` token or a `cf=` host token: a recovery boot follows).

mod clock;
mod compose;
mod guest;
mod host;
mod num;
mod storage;
mod target;

pub use clock::{civil_from_days, days_from_civil, ClockPoint, Every};
pub use compose::{Compose, Omit, RangeItem, RangeList, ResolvedField, SeedField, WORKLOAD_KINDS};
pub use guest::{
    Buggify, Direction, Freeze, FreezeDir, GuestToken, NetTarget, Netem, Pause, ProtocolFault,
    React, ReactAction, ResetKind, Rule, Status, ThreadSel, RESERVED, THREAD_NAME_MAX,
    THREAD_TARGET,
};
pub use host::{Disk, HostToken, BRANCH_KEYS, MAX_EXPLICIT};
pub use storage::{mix, CorruptKind, Corruption, Edit, MAX_FLIPS, MAX_LEN};
pub use target::{binomial, component_links, subset, Selector, Shape, Target};

use num::err;
use std::fmt;
use std::str::FromStr;

/// The longest guest part hvx_explore keeps (`char guest[512]`): a longer one would be cut short
/// there without a word, so it is refused here. hydo_composer.GUEST_MAX (500) keeps a margin.
pub const GUEST_MAX_BYTES: usize = 511;
/// The longest host part hvx_explore keeps (`char host[1024]`).
pub const HOST_MAX_BYTES: usize = 1023;

/// One schedule: the host tokens and the guest tokens, each in the order written.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Schedule {
    pub host: Vec<HostToken>,
    pub guest: Vec<GuestToken>,
}

/// Why a schedule string was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

impl Schedule {
    /// Parse a schedule string. See the crate documentation for what is accepted.
    pub fn parse(text: &str) -> Result<Schedule, ParseError> {
        // hvx_explore.c parse_sched: no '|' returns -1 ("bad schedule"); serve.c: "ERROR bad schedule"
        let (host, guest) = text.split_once('|').ok_or_else(|| {
            err("a schedule needs a '|' between its host part and its guest part")
        })?;
        if host.len() > HOST_MAX_BYTES {
            return Err(err(format!(
                "the host part is {} bytes; the engine keeps {HOST_MAX_BYTES}",
                host.len()
            )));
        }
        let mut hs: Vec<HostToken> = Vec::new();
        // the tokens of the current segment: the run's own, then each branch point's (br=)
        let mut seg = 0;
        for w in host.split_ascii_whitespace() {
            let t = HostToken::parse(w)?;
            if let HostToken::Br(at) = t {
                if let Some(HostToken::Br(last)) =
                    hs.iter().rev().find(|x| matches!(x, HostToken::Br(_)))
                {
                    if at <= *last {
                        return Err(err(format!(
                            "br={at} after br={last}: the branch points go up"
                        )));
                    }
                }
                hs.push(t);
                seg = hs.len();
                continue;
            }
            if seg > 0 && !host::BRANCH_KEYS.contains(&t.key()) {
                return Err(err(format!(
                    "{}= after br=: a branch point changes only the timing ({})",
                    t.key(),
                    host::BRANCH_KEYS.join("=, ")
                )));
            }
            if hs[seg..].iter().any(|x| x.key() == t.key()) {
                // the engines read the first occurrence (strstr) and ignore the rest
                return Err(err(format!("host token {}= appears twice", t.key())));
            }
            hs.push(t);
        }
        // The guest part. hvx strips the spaces after '|' and the trailing spaces/newline; hvm
        // types the text after '|' as it is (its leading space included). Exactly one space after
        // '|' is the form both explore.py and the composer write; anything else is refused, as are
        // runs of spaces between tokens (the init logs the command line as typed).
        let g = guest.trim_end_matches(['\n', '\r', ' ']);
        let g = g.strip_prefix(' ').ok_or_else(|| {
            err("the guest part starts with one space after '|' (\"... | run ...\")")
        })?;
        if g.len() > GUEST_MAX_BYTES {
            return Err(err(format!(
                "the guest part is {} bytes; hvx_explore keeps {GUEST_MAX_BYTES} (a saved \
                 composition, W=@<key>, keeps a long one short)",
                g.len()
            )));
        }
        let mut words = g.split(' ');
        if words.next() != Some("run") {
            return Err(err(format!(
                "the guest part starts with \"run\" (got {:?})",
                g.chars().take(20).collect::<String>()
            )));
        }
        let mut gs = Vec::new();
        for w in words {
            if w.is_empty() || w.contains(['\t', '\n', '\r']) {
                return Err(err(
                    "guest tokens are separated by exactly one space (the guest logs the text as typed)",
                ));
            }
            gs.push(GuestToken::parse(w)?);
        }
        let s = Schedule {
            host: hs,
            guest: gs,
        };
        s.check()?;
        Ok(s)
    }

    /// Rules that join several tokens.
    fn check(&self) -> Result<(), ParseError> {
        for (_, seg) in self.branch_points() {
            Schedule {
                host: seg,
                guest: Vec::new(),
            }
            .check_host()?;
        }
        self.check_host()
    }

    /// The rules of [`Schedule::check`] on the run's own host tokens (before any `br=`).
    fn check_host(&self) -> Result<(), ParseError> {
        let main = self.main_host();
        let has = |k: &str| main.iter().any(|t| t.key() == k);
        if let Some(HostToken::Cf(_)) = self.host_token("cf") {
            // hvx_explore.c parse_sched: cf= needs the volatile write cache (disk=cut) and no C=
            if self.disk() != Disk::Cut {
                return Err(err(
                    "cf= needs disk=cut (the power cut at a FLUSH loses the volatile write cache)",
                ));
            }
            if self.guest.iter().any(|t| matches!(t, GuestToken::Cut(_))) {
                return Err(err(
                    "cf= and a guest C= are two power cuts; a schedule has one",
                ));
            }
        }
        // parse_turns: qbs= without qb= returns -1
        if has("qbs") && !has("qb") {
            return Err(err("qbs= (the burst seed) needs qb="));
        }
        // The modifiers below are read by the C only with their main token present and are
        // ignored otherwise; a modifier alone is refused here, because it does nothing.
        for (m, main) in [
            ("tjpm", "tj"),
            ("tjmax", "tj"),
            ("xtr", "xt"),
            ("xtpm", "xt"),
            ("pctn", "pct"),
            ("pctk", "pct"),
        ] {
            if has(m) && !has(main) {
                return Err(err(format!("{m}= without {main}= does nothing")));
            }
        }
        for m in ["tjlo", "tjhi"] {
            if has(m) && !has("tj") {
                return Err(err(format!("{m}= without tj= does nothing")));
            }
        }
        // parse_tj / parse_preempt: an explicit list wins and the seeded form is ignored
        if has("tjx") && has("tj") {
            return Err(err(
                "tjx= and tj= together: the engine uses tjx= and ignores tj=",
            ));
        }
        if has("xtx") && has("xt") {
            return Err(err(
                "xtx= and xt= together: the engine uses xtx= and ignores xt=",
            ));
        }
        Ok(())
    }

    /// The first host token with this key, among the run's own tokens (before any `br=`).
    pub fn host_token(&self, key: &str) -> Option<&HostToken> {
        self.main_host().iter().find(|t| t.key() == key)
    }

    /// The run's own host tokens: the ones before the first branch point (`br=`).
    pub fn main_host(&self) -> &[HostToken] {
        let end = self
            .host
            .iter()
            .position(|t| matches!(t, HostToken::Br(_)))
            .unwrap_or(self.host.len());
        &self.host[..end]
    }

    /// The branch points (`br=<clock>`), each with the timing tokens that follow it
    /// (docs/design/state-search.md).
    pub fn branch_points(&self) -> Vec<(u64, Vec<HostToken>)> {
        let mut out: Vec<(u64, Vec<HostToken>)> = Vec::new();
        for t in &self.host {
            match t {
                HostToken::Br(at) => out.push((*at, Vec::new())),
                t => {
                    if let Some(last) = out.last_mut() {
                        last.1.push(t.clone());
                    }
                }
            }
        }
        out
    }

    /// The schedule without its branch points: what the engine arms at the run's start.
    pub fn without_branch_points(&self) -> Schedule {
        Schedule {
            host: self.main_host().to_vec(),
            guest: self.guest.clone(),
        }
    }

    /// The disk fault (`none` when there is no `disk=` token, as in the engines).
    pub fn disk(&self) -> Disk {
        match self.host_token("disk") {
            Some(HostToken::Disk(d)) => *d,
            _ => Disk::None,
        }
    }

    /// The disk fault seed (0 when there is no `dseed=` token).
    pub fn dseed(&self) -> u64 {
        match self.host_token("dseed") {
            Some(HostToken::Dseed(d)) => *d,
            _ => 0,
        }
    }

    /// The guest tokens with this letter, in order.
    pub fn guest_tokens(&self, letter: char) -> impl Iterator<Item = &GuestToken> {
        self.guest.iter().filter(move |t| t.letter() == letter)
    }

    /// The host part as the engine's text (canonical, no trailing space).
    pub fn host_text(&self) -> String {
        self.host
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The guest part as typed on the guest's console: `run` and the tokens.
    pub fn guest_text(&self) -> String {
        let mut s = String::from("run");
        for t in &self.guest {
            s.push(' ');
            s.push_str(&t.to_string());
        }
        s
    }

    /// A power cut can happen in this run (a guest `C=`, a host `cf=`, or a reaction `A=...:cut`
    /// whose event may come, M139): a recovery boot may follow.
    pub fn has_power_cut(&self) -> bool {
        self.guest.iter().any(|t| {
            matches!(t, GuestToken::Cut(_))
                || matches!(t, GuestToken::React(r) if r.action == ReactAction::Cut)
        }) || self.host_token("cf").is_some()
    }

    /// True when `text` is a schedule in its canonical form (`parse(text)` prints it back).
    pub fn is_canonical(text: &str) -> bool {
        Schedule::parse(text).is_ok_and(|s| s.to_string() == text)
    }
}

impl FromStr for Schedule {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Schedule::parse(s)
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = self.host_text();
        if h.is_empty() {
            write!(f, "| {}", self.guest_text())
        } else {
            write!(f, "{h} | {}", self.guest_text())
        }
    }
}
