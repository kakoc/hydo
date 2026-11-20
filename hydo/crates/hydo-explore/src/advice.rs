//! "Is this a bug?" for a failure, in plain words (Milestone 220): how much attention it needs,
//! a title that says what broke, one or two sentences why, and what to do. `hydo report --html`
//! shows it on every failure and every problem of an exploration; the mapping lives here, next to
//! the finding classes it reads ([`crate::gen::Explorer::finding_class`]), so that other outputs
//! say the same.
//!
//! The mapping, by the failure's class and what failed:
//!
//! | class | what failed | attention |
//! |---|---|---|
//! | `lying-disk` | anything | expected: no system keeps data its disk throws away |
//! | `unsafe-knob` | anything | expected for this setting: a setting that trades safety for speed |
//! | `refused` | anything | expected: hydo cannot run the faults on this machine |
//! | `io-errors` | anything | worth a look: decide whether the system must survive a failing disk |
//! | `damaged-disk` | anything | worth a look: decide whether the system must detect or survive a disk that tears or misplaces writes, has bad sectors, or stops answering (Milestone 313) |
//! | `corrupted-files` | anything | worth a look: decide whether the system must detect damaged files |
//! | `test-failed` | the test program | worth a look |
//! | `durable-defaults` and the rest | the user's own check | worth a look: hydo says that the user's rule failed and quotes it, and does not guess whether the rule must hold |
//! | `durable-defaults` and the rest | a value property (`// hydo: keep`, Milestone 291) | worth a look: the value's history names the first call where it changed |
//! | any | an error line new to the project's list | worth a look |
//!
//! The words name the user's system and its parts between backquotes (`` `redis` ``), as
//! [`hydo_schedule::describe`] does, and never hydo's own machinery.

use hydo_schedule::describe::{self, KnobInfo, Part};
use hydo_schedule::{Disk, GuestToken, ReactAction, Schedule};

/// How much attention a failure needs, most first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    /// The user has to decide: their own rule failed, or the system did not survive a fault it
    /// may have to survive.
    Look,
    /// The failure follows from a setting that trades safety for speed.
    ExpectedForSetting,
    /// No system survives this fault (a disk that throws data away), or hydo could not run it.
    Expected,
}

impl Attention {
    /// The label a reader sees.
    pub fn words(self) -> &'static str {
        match self {
            Attention::Look => "Worth a look",
            Attention::ExpectedForSetting => "Expected for this setting",
            Attention::Expected => "Expected",
        }
    }

    /// What the label means, in one sentence, and what it means for CI.
    pub fn meaning(self) -> &'static str {
        match self {
            Attention::Look => {
                "This can be a bug in your system, and only you can decide. In CI, a new \
                 finding of this kind fails the check until you fix it or accept it."
            }
            Attention::ExpectedForSetting => {
                "It follows from a setting that trades safety for speed. In CI, a new finding of \
                 this kind fails the check until you accept it or change the setting."
            }
            Attention::Expected => {
                "No system survives this fault. In CI, a new finding of this kind fails the \
                 check until you accept it."
            }
        }
    }

    /// A one-word name for a style or a machine (`look`, `setting`, `expected`).
    pub fn id(self) -> &'static str {
        match self {
            Attention::Look => "look",
            Attention::ExpectedForSetting => "setting",
            Attention::Expected => "expected",
        }
    }
}

/// What failed in a run, as the advice needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    /// A check the user wrote failed: an invariant, an assertion, a history check or a messaging
    /// claim. `id` is its id when the verdict names one, `description` what the check says it
    /// checks, `found` what it reported.
    Check {
        id: Option<String>,
        description: Option<String>,
        found: Option<String>,
    },
    /// A service did not start again when hydo restarted it.
    NoRestart { service: Option<String> },
    /// The run ended with an error before its checks: the verdict's words.
    Error { text: String },
    /// A service wrote an error line that the project's list of known errors does not cover.
    ErrorLine { service: String, line: String },
}

/// A failure in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub attention: Attention,
    /// One sentence without a full stop: what broke ("Redis broke your rule “...” after a power
    /// cut, with Redis appendfsync set to `everysec`").
    pub title: String,
    /// What failed, in one sentence (the check with its words and what it found).
    pub what: String,
    /// One or two sentences: why it failed, and whether it is a bug. It does not repeat
    /// [`Problem::what`].
    pub why: String,
    /// What to do.
    pub todo: String,
    /// What to do, for a page that shows the command that accepts the finding right under it:
    /// [`Problem::todo`] without its sentence about accepting.
    pub decide: String,
}

/// The verdict's words without hydo's prefix.
fn bare(verdict: &str) -> &str {
    verdict
        .trim()
        .strip_prefix("HYDO-ORACLE: ")
        .unwrap_or(verdict.trim())
}

/// What failed, from a run's verdict and its findings (`result.json`'s `findings`).
pub fn what_of(verdict: &str, findings: &[hydo_results::Finding]) -> What {
    let v = bare(verdict);
    if let Some(rest) = v.strip_prefix("ERROR ") {
        let rest = rest.trim();
        // `ERROR redis restart failed`, `ERROR redis restart failed (why)`,
        // `ERROR restart after the faults (redis)`, `ERROR restart after the faults`
        if let Some(svc) = rest.split_once(" restart failed").map(|(s, _)| s) {
            if !svc.contains(' ') {
                return What::NoRestart {
                    service: Some(svc.to_string()),
                };
            }
        }
        if let Some(r) = rest.strip_prefix("restart after the faults") {
            let svc = r
                .trim()
                .strip_prefix('(')
                .and_then(|x| x.strip_suffix(')'))
                .map(str::to_string);
            return What::NoRestart { service: svc };
        }
        return What::Error {
            text: rest.to_string(),
        };
    }
    if v.starts_with("TIMEOUT") {
        return What::Error {
            text: v.to_string(),
        };
    }
    let rest = v.strip_prefix("VIOLATION").unwrap_or(v).trim();
    if rest.starts_with("service cannot recover") {
        return What::NoRestart { service: None };
    }
    // `[id] what it found` or `what it found`, without the explorer's ` order=<n>`
    let (id, found) = match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((id, f)) => (Some(id.to_string()), f.trim()),
        None => (None, rest),
    };
    let found = found.split(" order=").next().unwrap_or("").trim();
    let description = id.as_ref().and_then(|id| {
        findings
            .iter()
            .filter(|f| &f.id == id && f.kind != hydo_results::kind::INVARIANT)
            .map(|f| f.message.trim())
            .find(|m| !m.is_empty() && !m.contains("VIOLATION"))
            .map(str::to_string)
    });
    What::Check {
        id,
        description,
        found: (!found.is_empty()).then(|| found.to_string()),
    }
}

/// The class of a schedule that ran once, without the explorer: its rule
/// ([`crate::gen::Explorer::finding_class`]) on the schedule and the project's settings. Without
/// the settings, `R=` other than `always` and `M=` other than `1` count as relaxed, as in the
/// explorer. `test` is the manifest's `workload.kind: test`.
pub fn class_of(s: &Schedule, knobs: &[KnobInfo], test: bool) -> String {
    match s.disk() {
        Disk::Lying(_) => return "lying-disk".into(),
        Disk::IoErr(_) => return "io-errors".into(),
        _ => {}
    }
    // Milestone 313: the block faults
    if !s.block_faults().is_empty() {
        return "damaged-disk".into();
    }
    if s.guest
        .iter()
        .any(|t| matches!(t, GuestToken::Corrupt { .. }))
    {
        return "corrupted-files".into();
    }
    // Milestone 238: a service switched to another version of itself during the run
    if s.guest.iter().any(|t| matches!(t, GuestToken::Upgrade(_))) {
        return "upgrade".into();
    }
    let cut = s.disk() == Disk::Cut
        || s.guest
            .iter()
            .any(|t| matches!(t, GuestToken::PowerLoss { .. }));
    if cut && relaxed(s, knobs).is_some() {
        return "unsafe-knob".into();
    }
    if test {
        return "test-failed".into();
    }
    defaults_class(s)
}

/// The fault families that name the class of a failure with the system's safe settings and
/// without a power cut (Milestone 328), in the order a class joins them: `process` (kills,
/// pauses, freezes, and the reactions that kill or stop), `network` (`N=`, `O=`), `clock` (`T=`,
/// a reaction's clock step), `protocol` (`H=`), `timing` (the host's tick and vCPU tokens) and
/// `buggify` (`B=`).
pub const FAMILIES: [&str; 6] = [
    "process", "network", "clock", "protocol", "timing", "buggify",
];

/// The class of a failure without a power cut, from the families it had (flags in
/// [`FAMILIES`]' order): the families joined by `+` (`process+clock`), or `no-fault`.
pub fn families_class(had: &[bool; 6]) -> String {
    let names: Vec<&str> = FAMILIES
        .iter()
        .zip(had)
        .filter(|(_, h)| **h)
        .map(|(n, _)| *n)
        .collect();
    if names.is_empty() {
        "no-fault".into()
    } else {
        names.join("+")
    }
}

/// Whether a class is one of a failure with the system's safe settings and a disk that keeps
/// what it said it wrote: `durable-defaults` (a power cut), a class of [`families_class`], or
/// `test-failed`. Before Milestone 328 every such failure was `durable-defaults`.
pub fn safe_settings(class: &str) -> bool {
    class == "durable-defaults"
        || class == "test-failed"
        || class == "no-fault"
        || class.split('+').all(|f| FAMILIES.contains(&f))
}

/// The group of a class for choosing which failures an exploration shrinks, one per group and
/// signature: a class of [`families_class`] is in the group `durable-defaults`, the class every
/// such failure had before Milestone 328, so that the new names shrink the same failures as
/// before; every other class is its own group.
pub fn shrink_group(class: &str) -> &str {
    if class != "test-failed" && class != "durable-defaults" && safe_settings(class) {
        "durable-defaults"
    } else {
        class
    }
}

/// The class of a failure with the system's safe settings (Milestone 328): `durable-defaults`
/// when the schedule cuts the power (`disk=cut`, `C=`, `cf=`, `L=`, a reaction's cut), where
/// the system's durability with its defaults is under test; else its fault families
/// ([`families_class`]). Before, every such failure was `durable-defaults`, also a liveness
/// failure under a pause with no disk fault.
pub fn defaults_class(s: &Schedule) -> String {
    let cut = s.disk() == Disk::Cut
        || s.has_power_cut()
        || s.guest
            .iter()
            .any(|t| matches!(t, GuestToken::PowerLoss { .. }));
    if cut {
        return "durable-defaults".into();
    }
    let mut had = [false; 6];
    for t in &s.guest {
        match t {
            GuestToken::Kill { .. }
            | GuestToken::Pause(_)
            | GuestToken::TimedKill { .. }
            | GuestToken::Freeze(_) => had[0] = true,
            GuestToken::Net { .. } | GuestToken::OneWay { .. } => had[1] = true,
            GuestToken::Clock { .. } | GuestToken::ClockTo { .. } => had[2] = true,
            GuestToken::Protocol(_) => had[3] = true,
            GuestToken::Buggify(_) => had[5] = true,
            GuestToken::React(r) => match r.action {
                ReactAction::Clock(_) => had[2] = true,
                ReactAction::Cut => {}
                _ => had[0] = true,
            },
            _ => {}
        }
    }
    had[4] = s.main_host().iter().any(|t| TIMING_KEYS.contains(&t.key()));
    families_class(&had)
}

/// The host tokens of the `timing` family: tick jitter, extra ticks, a shorter tick period,
/// vCPU turns and forced preemption points.
pub const TIMING_KEYS: [&str; 8] = ["tj", "tjx", "xt", "xtx", "pp", "q", "qb", "pct"];

/// The first setting of the schedule that is not at its safe value: (its letter, its value).
pub fn relaxed(s: &Schedule, knobs: &[KnobInfo]) -> Option<(char, String)> {
    s.guest.iter().find_map(|t| match t {
        GuestToken::Knob { letter, value } => {
            let off = match knobs.iter().find(|k| k.letter == *letter) {
                Some(k) => k.safe_value().is_some_and(|safe| safe != value),
                None if knobs.is_empty() => match letter {
                    'R' => value != "always",
                    'M' => value != "1",
                    _ => false,
                },
                None => false,
            };
            off.then(|| (*letter, value.clone()))
        }
        _ => None,
    })
}

/// The circumstance a failure happened in, for its title: the faults of its class ("when the
/// disk dropped writes it had said were saved", "after a power cut, with Redis appendfsync set to
/// `no`"), or the run's faults (at most two). Empty when the run had none.
pub fn context(class: &str, s: &Schedule, knobs: &[KnobInfo]) -> String {
    let said = describe::describe(s, knobs);
    let of = |pred: &dyn Fn(&str) -> bool| -> Option<String> {
        said.iter()
            .find(|x| pred(&x.token))
            .and_then(|x| x.context.clone())
    };
    match class {
        "lying-disk" => of(&|t| t.starts_with("disk=lying")).unwrap_or_default(),
        "io-errors" => of(&|t| t.starts_with("disk=ioerr")).unwrap_or_default(),
        "corrupted-files" => of(&|t| t.starts_with("G=")).unwrap_or_default(),
        "damaged-disk" => {
            let blocks = s.block_faults();
            of(&|t| blocks.iter().any(|b| b == t)).unwrap_or_default()
        }
        "upgrade" => of(&|t| t.starts_with("U=")).unwrap_or_default(),
        "unsafe-knob" => {
            let setting = match relaxed(s, knobs) {
                Some((l, v)) => {
                    let name = knobs
                        .iter()
                        .find(|k| k.letter == l)
                        .and_then(|k| k.name.clone())
                        .unwrap_or_else(|| format!("your setting {l}"));
                    format!("with {name} set to `{v}`")
                }
                None => "with a setting that trades safety for speed".into(),
            };
            match of(&|t| t.starts_with("C=") || t.starts_with("cf=") || t.starts_with("L=")) {
                Some(p) => format!("{p}, {setting}"),
                None => setting,
            }
        }
        _ => {
            let mut cs: Vec<String> = Vec::new();
            for x in said.iter().filter(|x| x.part != Part::Quiet) {
                if let Some(c) = &x.context {
                    if !cs.contains(c) {
                        cs.push(c.clone());
                    }
                }
            }
            cs.truncate(2);
            cs.join(" and ")
        }
    }
}

fn with_context(head: String, ctx: &str) -> String {
    if ctx.is_empty() {
        head
    } else {
        format!("{head} {ctx}")
    }
}

fn quoted_rule(what: &What) -> String {
    match what {
        What::Check {
            id, description, ..
        } => match (description, id) {
            (Some(d), Some(id)) => format!("“{d}” (`{id}`)"),
            (Some(d), None) => format!("“{d}”"),
            (None, Some(id)) => format!("`{id}`"),
            (None, None) => "one of your checks".into(),
        },
        _ => String::new(),
    }
}

/// What failed, in one sentence: the check with its words and what it found, the service that
/// did not start, or the error.
pub fn what_words(what: &What) -> String {
    match what {
        What::Check { found, .. } => {
            let rule = quoted_rule(what);
            let rule = if rule == "one of your checks" {
                "One of your checks".to_string()
            } else {
                format!("Your check {rule}")
            };
            match found {
                Some(f) => format!("{rule} found `{f}`."),
                None => format!("{rule} failed."),
            }
        }
        What::NoRestart { service: Some(x) } => {
            format!("`{x}` did not start again when hydo restarted it.")
        }
        What::NoRestart { service: None } => {
            "A service did not start again when hydo restarted it.".into()
        }
        What::Error { text } => {
            format!("The run stopped before your checks could run, with the error `{text}`.")
        }
        What::ErrorLine { service, line } => format!("`{service}` wrote `{line}`."),
    }
}

/// A failure in plain words. `system` is the system's name as the reader knows it (`Redis`);
/// `s` the schedule that shows the failure (the minimal one when there is one), and `knobs` the
/// project's settings. The explanation (`why`) does not repeat what failed ([`what_words`]): a
/// page shows that once.
pub fn problem(
    class: &str,
    what: &What,
    system: &str,
    s: Option<&Schedule>,
    knobs: &[KnobInfo],
) -> Problem {
    let ctx = s.map(|s| context(class, s, knobs)).unwrap_or_default();
    // the title: what broke, short
    let title = match what {
        What::Check {
            id,
            description,
            found,
        } => {
            // a check that reports something lost or missing: say that it was lost
            let lost = found
                .as_deref()
                .is_some_and(|f| f.ends_with(" lost") || f.ends_with(" missing"));
            let confirmed = [id.as_deref(), description.as_deref()]
                .iter()
                .flatten()
                .any(|x| {
                    let x = x.to_lowercase();
                    x.contains("ack") || x.contains("confirm")
                });
            match (lost, confirmed) {
                _ if is_values(what) => with_context(
                    format!("{system} broke a value property you declared"),
                    &ctx,
                ),
                (true, true) if !ctx.is_empty() => {
                    format!("{system} lost data it had confirmed, {ctx}")
                }
                (true, true) => format!("{system} lost data it had confirmed"),
                (true, false) => with_context(format!("{system} lost data"), &ctx),
                _ => with_context(format!("{system} failed your check"), &ctx),
            }
        }
        What::NoRestart { service } => with_context(
            format!(
                "{} did not start again",
                match service {
                    Some(x) => format!("`{x}`"),
                    None => "A service".to_string(),
                }
            ),
            &ctx,
        ),
        What::Error { .. } if class == "test-failed" => {
            with_context("Your test program failed".into(), &ctx)
        }
        What::Error { .. } => with_context("The run stopped early".into(), &ctx),
        What::ErrorLine { service, .. } => {
            format!("`{service}` wrote an error line that your list of known errors does not have")
        }
    };
    let (attention, why, todo) = match (class, what) {
        (_, What::ErrorLine { .. }) => (
            Attention::Look,
            "Your project's list of known errors (`errors:` in `hydo.yaml`) does not have this \
             line."
                .to_string(),
            "If the line is harmless, add it to the list of known errors. If not, look at what \
             led to it in the run."
                .to_string(),
        ),
        ("lying-disk", _) => (
            Attention::Expected,
            "hydo made the disk faulty: it threw away writes that it had already confirmed, as \
             a broken or misconfigured disk can. No system keeps data that its disk throws away."
                .to_string(),
            "No, not in your system: it lost what the disk threw away. It matters only if your \
             production disks can throw away confirmed writes. If you do not expect such disks, \
             accept this finding."
                .to_string(),
        ),
        ("unsafe-knob", _) => {
            let (setting, safe) = match s.and_then(|s| relaxed(s, knobs)) {
                Some((l, v)) => match knobs.iter().find(|k| k.letter == l) {
                    Some(k) => (
                        format!(
                            "{} is set to `{v}`",
                            k.name
                                .clone()
                                .unwrap_or_else(|| format!("Your setting {l}"))
                        ),
                        k.safe_value().map(|x| {
                            (
                                k.name.clone().unwrap_or_else(|| format!("the setting {l}")),
                                x.to_string(),
                            )
                        }),
                    ),
                    None => (format!("Your setting {l} is set to `{v}`"), None),
                },
                None => (
                    "A setting of your project trades safety for speed".into(),
                    None,
                ),
            };
            let keep = match &safe {
                Some((name, x)) => format!("keep {name} at `{x}`"),
                None => "keep the setting at its safe value".to_string(),
            };
            let safe_note = match &safe {
                Some((_, x)) => format!(", and its safe value is `{x}`"),
                None => String::new(),
            };
            (
                Attention::ExpectedForSetting,
                format!(
                    "{setting}{safe_note}. With that setting, a power cut can lose data that the \
                     system had already confirmed."
                ),
                format!(
                    "If confirmed data must survive a power loss, {keep}. If losing it is fine \
                     for you, accept this finding."
                ),
            )
        }
        ("refused", _) => (
            Attention::Expected,
            "hydo cannot run this combination of faults on this machine, so nothing of your \
             system was tested."
                .to_string(),
            "Nothing.".to_string(),
        ),
        ("io-errors", _) => (
            Attention::Look,
            "hydo made the disk return read and write errors.".to_string(),
            "Decide whether your system must survive a failing disk. If it must, look at the \
             service's log in the run; if not, accept this finding."
                .to_string(),
        ),
        ("damaged-disk", _) => (
            Attention::Look,
            "hydo made the disk fail as real disks can: a write torn at a power cut, a write \
             that went to the wrong place, sectors that cannot be read, or a request that got no \
             answer."
                .to_string(),
            "Decide whether your system must detect or survive such a disk, for example with \
             checksums of its records and a careful commit. If it need not, accept this finding."
                .to_string(),
        ),
        ("corrupted-files", _) => (
            Attention::Look,
            "hydo damaged one of the system's files on purpose, and the system read the damaged \
             data."
                .to_string(),
            "Decide whether your system must detect damaged files, for example with checksums. \
             If it need not, accept this finding."
                .to_string(),
        ),
        ("upgrade", _) => (
            Attention::Look,
            "hydo switched a service to another version of itself during the run, as a rolling \
             upgrade or a rollback does, and the system failed while versions were mixed or \
             after the switch."
                .to_string(),
            "Run the minimal schedule below again and compare it with the same schedule without \
             its U= token. If only the upgrade fails, the versions cannot run side by side or the \
             new one cannot read what the old one wrote; check the project's upgrade notes for a \
             required order or an intermediate version."
                .to_string(),
        ),
        // Milestone 301: a test that failed under faults is not called a matter of timing
        // (0.4.0's trial: web-shop's test fails by design when its order cannot be read)
        ("test-failed", _) if !ctx.is_empty() => (
            Attention::Look,
            format!("Your test program failed {ctx}."),
            "Run it again with the command below and read the test's output. If the same run \
             without these faults passes, the faults made it fail: decide whether your system \
             must handle them, or whether the test expects too much of it."
                .to_string(),
        ),
        ("test-failed", _) => (
            Attention::Look,
            "Your test program failed, with no fault in its run.".to_string(),
            "Run it again with the command below and read the test's output. A test that fails \
             only under these conditions depends on timing."
                .to_string(),
        ),
        (_, What::Check { .. }) if is_values(what) => (
            Attention::Look,
            "A `// hydo: keep` line next to your code says how a recorded value must stay \
             along a key's history, and a call in this run saw another value. The disk and your \
             settings were not at fault. Only you can say whether the value must stay; a \
             business rule over the end state may not see it."
                .to_string(),
            "The run's value history names the first call where the value changed: `hydo \
             inspect values <run dir> --broken --text` shows it for every key that broke the \
             property. If the value must stay, this is a bug; if not, change the `keep` line or \
             accept this finding."
                .to_string(),
        ),
        (_, What::Check { .. }) => (
            Attention::Look,
            "This is your own rule, and the disk and your settings were not at fault. hydo does \
             not know what your system promises, so only you can say whether the rule must hold \
             here."
                .to_string(),
            "Replay the run and look at the moment it failed. If the rule must hold, this is a \
             bug; if not, change the check or accept this finding."
                .to_string(),
        ),
        (_, What::NoRestart { .. }) => (
            Attention::Look,
            "The service did not come back after the faults.".to_string(),
            "Look at the service's log in the run to see why it did not start.".to_string(),
        ),
        (_, What::Error { .. }) => (
            Attention::Look,
            "The run ended before your checks could tell anything.".to_string(),
            "Look at the run's log. If the error comes from hydo itself, attach the details of \
             this page to a bug report for hydo."
                .to_string(),
        ),
    };
    // the advice under which a page shows the command that accepts the finding: without the
    // sentence that says to accept it (Milestone 247: an exploration's problem carries its own
    // `hydo ci accept` line)
    let decide = match (class, what) {
        (_, What::ErrorLine { .. }) => todo.clone(),
        ("lying-disk", _) => "No, not in your system: it lost what the disk threw away.".into(),
        ("unsafe-knob", _) => todo
            .strip_suffix(" If losing it is fine for you, accept this finding.")
            .unwrap_or(&todo)
            .to_string(),
        ("io-errors", _) => "Decide whether your system must survive a failing disk.".into(),
        ("damaged-disk", _) => "Decide whether your system must detect or survive a disk that \
                                tears or misplaces writes, cannot read some sectors, or stops \
                                answering."
            .into(),
        ("corrupted-files", _) => {
            "Decide whether your system must detect damaged files, for example with checksums."
                .into()
        }
        ("refused" | "upgrade" | "test-failed", _) => todo.clone(),
        (_, What::Check { .. }) if is_values(what) => todo
            .strip_suffix(" or accept this finding.")
            .map(|t| format!("{t}."))
            .unwrap_or_else(|| todo.clone()),
        (_, What::Check { .. }) => "Replay the run and look at the moment it failed. If the rule \
                                    must hold, this is a bug; if not, change the check."
            .into(),
        _ => todo.clone(),
    };
    Problem {
        attention,
        title: capitalize(&title),
        what: what_words(what),
        why,
        todo,
        decide,
    }
}

/// Whether a check is a value property (`// hydo: keep`, Milestone 291): its finding's id, and
/// so the verdict's brackets, start with `values `.
pub fn is_values(what: &What) -> bool {
    matches!(what, What::Check { id: Some(id), .. } if id.starts_with("values "))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knobs() -> Vec<KnobInfo> {
        vec![KnobInfo {
            letter: 'R',
            name: Some("Redis appendfsync".into()),
            default: Some("always".into()),
            safe: None,
            description: None,
        }]
    }

    fn finding(kind: &str, id: &str, message: &str) -> hydo_results::Finding {
        hydo_results::Finding {
            kind: kind.into(),
            id: id.into(),
            status: "violation".into(),
            message: message.into(),
            ..Default::default()
        }
    }

    fn redis_check() -> What {
        let v = "HYDO-ORACLE: VIOLATION [redis.acked_writes_survive] key k:0 lost";
        what_of(
            v,
            &[
                finding("invariant", "redis.acked_writes_survive", v),
                finding(
                    "assertion",
                    "redis.acked_writes_survive",
                    "every SET the client saw acknowledged reads back with its value",
                ),
            ],
        )
    }

    fn s(t: &str) -> Schedule {
        Schedule::parse(t).unwrap()
    }

    #[test]
    fn what_failed_from_a_verdict() {
        assert_eq!(
            redis_check(),
            What::Check {
                id: Some("redis.acked_writes_survive".into()),
                description: Some(
                    "every SET the client saw acknowledged reads back with its value".into()
                ),
                found: Some("key k:0 lost".into()),
            }
        );
        assert_eq!(
            what_of(
                "HYDO-ORACLE: VIOLATION lost acknowledged write order=11",
                &[]
            ),
            What::Check {
                id: None,
                description: None,
                found: Some("lost acknowledged write".into()),
            }
        );
        for v in [
            "HYDO-ORACLE: ERROR redis restart failed",
            "HYDO-ORACLE: ERROR redis restart failed (exit status 1)",
            "HYDO-ORACLE: ERROR restart after the faults (redis)",
        ] {
            assert_eq!(
                what_of(v, &[]),
                What::NoRestart {
                    service: Some("redis".into())
                },
                "{v}"
            );
        }
        assert_eq!(
            what_of(
                "HYDO-ORACLE: VIOLATION service cannot recover after the faults",
                &[]
            ),
            What::NoRestart { service: None }
        );
        assert_eq!(
            what_of("ERROR harness: guest did not finish (I/O error)", &[]),
            What::Error {
                text: "harness: guest did not finish (I/O error)".into()
            }
        );
    }

    #[test]
    fn the_class_of_one_run() {
        let k = knobs();
        assert_eq!(
            class_of(&s("disk=lying:3 | run C=2"), &k, false),
            "lying-disk"
        );
        assert_eq!(class_of(&s("disk=ioerr:3 | run"), &k, false), "io-errors");
        assert_eq!(
            class_of(&s("disk=none | run G=redis@3:flip"), &k, false),
            "corrupted-files"
        );
        assert_eq!(
            class_of(&s("disk=cut | run C=12 R=everysec"), &k, false),
            "unsafe-knob"
        );
        assert_eq!(
            class_of(&s("disk=cut | run C=12 R=always"), &k, false),
            "durable-defaults"
        );
        // a relaxed setting without a power cut is not the setting's fault; since Milestone 328
        // such a failure is named by its fault families
        assert_eq!(
            class_of(&s("disk=none | run K=redis@3+1 R=no"), &k, false),
            "process"
        );
        assert_eq!(
            class_of(&s("disk=none | run L=redis@3+1"), &k, false),
            "durable-defaults"
        );
        assert_eq!(
            class_of(
                &s("disk=none | run P=redis@8+5 N=redis@1-4:loss,61% T=19:-86400"),
                &k,
                false
            ),
            "process+network+clock"
        );
        assert_eq!(
            class_of(&s("disk=none tj=7 | run A=x@2+5:clock+30"), &k, false),
            "clock+timing"
        );
        assert_eq!(
            class_of(&s("disk=none | run A=x@2+5:kill-redis"), &k, false),
            "process"
        );
        assert_eq!(class_of(&s("disk=none | run"), &k, false), "no-fault");
        assert_eq!(class_of(&s("disk=none | run"), &k, true), "test-failed");
        // without the manifest: the explorer's rule for R= and M=
        assert_eq!(
            class_of(&s("disk=cut | run C=1 R=no"), &[], false),
            "unsafe-knob"
        );
        assert_eq!(
            class_of(&s("disk=cut | run C=1 M=2"), &[], false),
            "unsafe-knob"
        );
    }

    /// Every class with what can fail in it: a label, a title, and words without hydo's own
    /// machinery.
    /// Milestone 328: which classes are a failure with the safe settings, and the group a class
    /// is shrunk in (the new family classes with `durable-defaults`, as before).
    #[test]
    fn family_classes_are_safe_settings_and_shrink_as_before() {
        for c in [
            "durable-defaults",
            "test-failed",
            "no-fault",
            "process",
            "process+network+clock+protocol+timing+buggify",
        ] {
            assert!(safe_settings(c), "{c}");
        }
        for c in [
            "unsafe-knob",
            "lying-disk",
            "io-errors",
            "refused",
            "process+disk",
        ] {
            assert!(!safe_settings(c), "{c}");
        }
        assert_eq!(shrink_group("process+clock"), "durable-defaults");
        assert_eq!(shrink_group("no-fault"), "durable-defaults");
        assert_eq!(shrink_group("durable-defaults"), "durable-defaults");
        assert_eq!(shrink_group("test-failed"), "test-failed");
        assert_eq!(shrink_group("unsafe-knob"), "unsafe-knob");
        assert_eq!(families_class(&[false; 6]), "no-fault");
    }

    #[test]
    fn every_class_has_advice() {
        let k = knobs();
        let cases: Vec<(&str, &str, Attention)> = vec![
            ("lying-disk", "disk=lying:3 | run C=20", Attention::Expected),
            (
                "unsafe-knob",
                "disk=cut | run C=9 R=no",
                Attention::ExpectedForSetting,
            ),
            ("io-errors", "disk=ioerr:5 | run", Attention::Look),
            ("damaged-disk", "disk=none mis=7 | run", Attention::Look),
            (
                "damaged-disk",
                "disk=none | run G=redis@3:bad",
                Attention::Look,
            ),
            (
                "corrupted-files",
                "disk=none | run G=redis@3:flip",
                Attention::Look,
            ),
            ("test-failed", "disk=none | run", Attention::Look),
            (
                "durable-defaults",
                "disk=none | run K=redis@3+2",
                Attention::Look,
            ),
            ("process", "disk=none | run K=redis@3+2", Attention::Look),
            (
                "process+network",
                "disk=none | run P=redis@3+2 N=redis@1-4:loss,50%",
                Attention::Look,
            ),
            ("refused", "disk=none | run", Attention::Expected),
            ("something-new", "disk=none | run", Attention::Look),
        ];
        let whats = [
            redis_check(),
            What::NoRestart {
                service: Some("redis".into()),
            },
            What::Error {
                text: "mount data".into(),
            },
            What::ErrorLine {
                service: "redis".into(),
                line: "connection lost <n>".into(),
            },
        ];
        for (class, sched, want) in cases {
            for w in &whats {
                let p = problem(class, w, "Redis", Some(&s(sched)), &k);
                if !matches!(w, What::ErrorLine { .. }) {
                    assert_eq!(p.attention, want, "{class} {w:?}");
                }
                for text in [&p.title, &p.why, &p.todo, &p.what, &p.decide] {
                    assert!(!text.is_empty());
                    let low = text.to_lowercase();
                    for j in [
                        "fingerprint",
                        "epoch",
                        "engine",
                        "schedule",
                        "token",
                        "invariant",
                        "oracle",
                        "signature",
                        "class",
                        "seed",
                        "knob",
                    ] {
                        assert!(
                            !low.split(|c: char| !c.is_ascii_alphanumeric())
                                .any(|x| x == j),
                            "{class} {w:?}: {text:?} uses {j:?}"
                        );
                    }
                }
                assert!(
                    p.why.ends_with('.')
                        && p.todo.ends_with('.')
                        && p.what.ends_with('.')
                        && p.decide.ends_with('.'),
                    "{p:?}"
                );
                // the advice under an accept command does not say to accept
                if !matches!(w, What::ErrorLine { .. })
                    && !matches!(class, "upgrade" | "test-failed")
                {
                    assert!(!p.decide.contains("accept"), "{class} {w:?}: {}", p.decide);
                }
            }
        }
    }

    #[test]
    fn titles_and_words() {
        let k = knobs();
        let p = problem(
            "unsafe-knob",
            &redis_check(),
            "Redis",
            Some(&s("disk=cut | run C=12 R=everysec")),
            &k,
        );
        assert_eq!(
            p.title,
            "Redis lost data it had confirmed, after a power cut, with Redis appendfsync set to \
             `everysec`"
        );
        assert_eq!(p.attention.words(), "Expected for this setting");
        assert_eq!(
            p.what,
            "Your check “every SET the client saw acknowledged reads back with its value” \
             (`redis.acked_writes_survive`) found `key k:0 lost`."
        );
        assert_eq!(
            p.why,
            "Redis appendfsync is set to `everysec`, and its safe value is `always`. With that \
             setting, a power cut can lose data that the system had already confirmed."
        );
        assert_eq!(
            p.todo,
            "If confirmed data must survive a power loss, keep Redis appendfsync at `always`. If \
             losing it is fine for you, accept this finding."
        );
        // the explanation never repeats the check
        for class in ["unsafe-knob", "lying-disk", "durable-defaults", "io-errors"] {
            let p = problem(class, &redis_check(), "Redis", None, &k);
            assert!(!p.why.contains("acked_writes"), "{class}: {}", p.why);
            assert!(!p.why.contains("k:0"), "{class}: {}", p.why);
        }
        // a user's own rule under faults any system meets: no guess
        let p = problem(
            "durable-defaults",
            &redis_check(),
            "Redis",
            Some(&s("disk=none | run K=redis@3+2 T=5:-300")),
            &k,
        );
        assert_eq!(
            p.title,
            "Redis lost data it had confirmed, after `redis` was killed and after a clock jump"
        );
        assert!(p
            .why
            .contains("only you can say whether the rule must hold"));
        // two verdicts that mean the same for a user get the same title
        let a = problem(
            "io-errors",
            &what_of("ERROR redis restart failed", &[]),
            "Redis",
            Some(&s("disk=ioerr:1 | run K=redis@12+2")),
            &k,
        );
        let b = problem(
            "io-errors",
            &what_of("ERROR restart after the faults (redis)", &[]),
            "Redis",
            Some(&s("disk=ioerr:2 | run")),
            &k,
        );
        assert_eq!(a.title, "`redis` did not start again after disk errors");
        assert_eq!(a.title, b.title);
        assert_eq!(
            problem(
                "lying-disk",
                &redis_check(),
                "Redis",
                Some(&s("disk=lying:3 | run C=20")),
                &k
            )
            .title,
            "Redis lost data it had confirmed, with a faulty disk"
        );
        for a in [
            Attention::Look,
            Attention::ExpectedForSetting,
            Attention::Expected,
        ] {
            assert!(a.meaning().contains("In CI"));
        }
    }
}
