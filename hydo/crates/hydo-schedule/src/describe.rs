//! A schedule in plain words (Milestone 220): one sentence and a short gloss for every token, for
//! a reader who knows their own system but not hydo. `hydo report --html` shows the sentences as
//! the numbered list "When it happens" and the glosses as the line that explains the replay
//! command; other outputs can use them the same way.
//!
//! The words never name hydo's own machinery (no "schedule", "token", "seed", "engine",
//! "fingerprint"): they say what happens to the user's system. A name of the user's system (a
//! service, a group, a file, an assertion id, a setting's value) is written between backquotes,
//! so a web page can show it as code and a terminal as it is. A word that is not a token of the
//! language gets a neutral sentence, never an error.
//!
//! A step is one numbered part of the workload (`workload.steps`); a fault "before step n"
//! happens before the workload runs step n, and the steps count from 0.

use crate::clock::{ClockPoint, Every};
use crate::compose::{Compose, ResolvedField, SeedField};
use crate::guest::{
    Direction, FreezeDir, GuestToken, NetTarget, Netem, Pause, ReactAction, ResetKind, Rule,
    Status, ThreadSel, THREAD_TARGET,
};
use crate::host::{Disk, HostToken};
use crate::storage::{CorruptKind, Corruption};
use crate::target::{Selector, Shape, Target};
use crate::Schedule;

/// What the project's manifest says about one of its settings (`knobs:` in `hydo.yaml`). The
/// caller reads the manifest; this crate reads no files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnobInfo {
    /// The setting's letter in the schedule (`R`).
    pub letter: char,
    /// Its name for people (`Redis appendfsync`).
    pub name: Option<String>,
    /// The value the system starts with.
    pub default: Option<String>,
    /// The safe value: `safe:`, else `default:`.
    pub safe: Option<String>,
    /// The manifest's description of the setting.
    pub description: Option<String>,
}

impl KnobInfo {
    /// The safe value, from `safe:` or `default:`.
    pub fn safe_value(&self) -> Option<&str> {
        self.safe.as_deref().or(self.default.as_deref())
    }
}

/// Where a sentence belongs in the story of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// How the system and the machine are set up for the whole run (a setting, the disk).
    Setup,
    /// Something that happens at a moment of the run (a crash, a power cut).
    Fault,
    /// A detail that changes nothing a reader needs to follow (which requests a disk fault picks,
    /// how often hydo looks at the machine). It has a gloss, and no line of its own in a list.
    Quiet,
}

/// One token in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    /// The token as written.
    pub token: String,
    /// One or two complete sentences.
    pub sentence: String,
    /// A few words: what the token means, for a line such as "`C=12` = cut the power before
    /// step 12".
    pub gloss: String,
    pub part: Part,
    /// The workload step the fault happens at (before it), when it has one.
    pub step: Option<u64>,
    /// The token as a noun phrase, for a sentence that compares two runs ("a power cut before
    /// step 12", "Redis appendfsync at `always`").
    pub noun: String,
    /// When the token is a fault: the circumstance it gives a failure, for a title ("after a
    /// power cut", "after disk errors").
    pub context: Option<String>,
    /// When the token is a fault: its kind in a few words, for a list of the faults a set of
    /// runs had ("power cuts", "crashes").
    pub kind: Option<&'static str>,
}

impl Said {
    fn new(token: String, part: Part, sentence: String, gloss: String) -> Said {
        Said {
            token,
            noun: gloss.clone(),
            sentence,
            gloss,
            part,
            step: None,
            context: None,
            kind: None,
        }
    }

    fn noun(mut self, n: String) -> Said {
        self.noun = n;
        self
    }

    fn at(mut self, step: u64) -> Said {
        self.step = Some(step);
        self
    }

    fn fault(mut self, kind: &'static str, context: String) -> Said {
        self.kind = Some(kind);
        self.context = Some(context);
        self
    }
}

/// Every token of a schedule in words, in the order a reader follows them: the setup first (in
/// the order written), then the faults by their step (ties in the order written), then the
/// faults without a step. Quiet tokens come last.
pub fn describe(s: &Schedule, knobs: &[KnobInfo]) -> Vec<Said> {
    let mut all: Vec<Said> = s.host.iter().map(|t| host(t, s)).collect();
    all.extend(s.guest.iter().map(|t| guest(t, knobs)));
    order(all)
}

/// [`describe`] for schedule text. Text that does not parse as a whole is read word by word: the
/// host words before `|`, the guest words after `run`; a word that is not a token gets
/// [`unknown`]'s neutral sentence.
pub fn describe_text(text: &str, knobs: &[KnobInfo]) -> Vec<Said> {
    if let Ok(s) = Schedule::parse(text) {
        return describe(&s, knobs);
    }
    let (h, g) = text.split_once('|').unwrap_or(("", text));
    let empty = Schedule {
        host: Vec::new(),
        guest: Vec::new(),
    };
    let mut all = Vec::new();
    for w in h.split_whitespace() {
        all.push(match HostToken::parse(w) {
            Ok(t) => host(&t, &empty),
            Err(_) => unknown(w),
        });
    }
    for w in g.split_whitespace().filter(|w| *w != "run") {
        all.push(match GuestToken::parse(w) {
            Ok(t) => guest(&t, knobs),
            Err(_) => unknown(w),
        });
    }
    order(all)
}

fn order(all: Vec<Said>) -> Vec<Said> {
    let rank = |x: &Said| match (x.part, x.step) {
        (Part::Setup, _) => (0, 0),
        (Part::Fault, Some(n)) => (1, n),
        (Part::Fault, None) => (2, 0),
        (Part::Quiet, _) => (3, 0),
    };
    let mut v: Vec<(usize, Said)> = all.into_iter().enumerate().collect();
    v.sort_by_key(|(i, x)| (rank(x), *i));
    v.into_iter().map(|(_, x)| x).collect()
}

/// The neutral sentence for a word that is not a token of the schedule language.
pub fn unknown(word: &str) -> Said {
    Said::new(
        word.to_string(),
        Part::Setup,
        format!(
            "The run also has the setting `{word}`. `hydo guide faults` lists what each setting \
             does."
        ),
        "a setting of the run".into(),
    )
}

/// How schedule `b` differs from schedule `a`, in words ("without a power cut before step 9",
/// "with Redis appendfsync at `always` instead of `everysec`"); None when they are the same or one
/// does not parse.
pub fn change(a: &str, b: &str, knobs: &[KnobInfo]) -> Option<String> {
    let (sa, sb) = (Schedule::parse(a).ok()?, Schedule::parse(b).ok()?);
    let ta: Vec<String> = words(&sa);
    let tb: Vec<String> = words(&sb);
    let only_a: Vec<&String> = ta.iter().filter(|t| !tb.contains(t)).collect();
    let only_b: Vec<&String> = tb.iter().filter(|t| !ta.contains(t)).collect();
    let noun = |w: &str| -> String {
        if let Ok(t) = HostToken::parse(w) {
            return host(&t, &sb).noun;
        }
        match GuestToken::parse(w) {
            Ok(t) => guest(&t, knobs).noun,
            Err(_) => code(w),
        }
    };
    let key = |t: &str| t.split(['=', '@']).next().unwrap_or("").to_string();
    // a token of one key on both sides is a change of it; the others are removed or added
    let mut parts: Vec<String> = Vec::new();
    let mut added: Vec<&String> = only_b.clone();
    let mut removed: Vec<&String> = Vec::new();
    for x in &only_a {
        match added.iter().position(|y| key(y) == key(x)) {
            Some(i) => {
                let y = added.remove(i);
                parts.push(swap(x, y, &noun));
            }
            None => removed.push(x),
        }
    }
    let joined =
        |xs: &[&String]| -> String { and_list(&xs.iter().map(|x| noun(x)).collect::<Vec<_>>()) };
    if !removed.is_empty() {
        parts.push(format!("without {}", joined(&removed)));
    }
    if !added.is_empty() {
        parts.push(format!("with {} added", joined(&added)));
    }
    if parts.is_empty() {
        return None;
    }
    Some(match parts.len() {
        1 => parts.remove(0),
        _ => parts.join(", and "),
    })
}

/// One token changed: "with Redis appendfsync at `always` instead of `everysec`", "with fewer
/// dropped writes (rate 1 instead of 3)", "with a working disk instead of a disk with a write cache".
fn swap(x: &str, y: &str, noun: &dyn Fn(&str) -> String) -> String {
    let value = |t: &str| {
        t.split_once('=')
            .map(|(_, v)| v.to_string())
            .unwrap_or_default()
    };
    if let (Ok(GuestToken::Knob { .. }), Ok(GuestToken::Knob { .. })) =
        (GuestToken::parse(x), GuestToken::parse(y))
    {
        return format!("with {} instead of {}", noun(y), code(&value(x)));
    }
    let rate = |t: &str| match HostToken::parse(t) {
        Ok(HostToken::Disk(Disk::IoErr(p))) => Some(("errors", p)),
        Ok(HostToken::Disk(Disk::Lying(p))) => Some(("thrown away", p)),
        _ => None,
    };
    if let (Some((kx, px)), Some((ky, py))) = (rate(x), rate(y)) {
        if kx == ky {
            let more = if py < px { "fewer" } else { "more" };
            return if kx == "errors" {
                format!("with the disk returning {more} errors")
            } else {
                format!("with the disk throwing away {more} writes")
            };
        }
    }
    if x.starts_with("dseed=") {
        return "with other disk requests hit by the fault".into();
    }
    format!("with {} instead of {}", noun(y), noun(x))
}

fn words(s: &Schedule) -> Vec<String> {
    s.host
        .iter()
        .map(ToString::to_string)
        .chain(s.guest.iter().map(ToString::to_string))
        .collect()
}

// ------------------------------------------------------------------ small words

/// `a`, `a and b`, `a, b and c`.
pub fn and_list(xs: &[String]) -> String {
    match xs.len() {
        0 => String::new(),
        1 => xs[0].clone(),
        n => format!("{} and {}", xs[..n - 1].join(", "), xs[n - 1]),
    }
}

/// A length of time in seconds, in words (`30 seconds`, `5 minutes`, `1 day`).
pub fn duration(secs: u64) -> String {
    let unit = |n: u64, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    if secs > 0 && secs.is_multiple_of(86_400) {
        unit(secs / 86_400, "day")
    } else if secs > 0 && secs.is_multiple_of(3_600) {
        unit(secs / 3_600, "hour")
    } else if secs > 0 && secs.is_multiple_of(60) {
        unit(secs / 60, "minute")
    } else {
        unit(secs, "second")
    }
}

fn ms(n: u64) -> String {
    format!("{n} ms")
}

fn ordinal(n: u64) -> String {
    let suf = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suf}")
}

/// When a fault at step `n` (counted from 0, before step n runs) happens, counted for people
/// from 1: "before the first step", "after the first step", "after 12 steps".
pub fn after(n: u64) -> String {
    match n {
        0 => "before the first step".into(),
        1 => "after the first step".into(),
        n => format!("after {n} steps"),
    }
}

/// A window of steps [from, to) counted from 1: "during steps 1 to 8", "during step 3".
pub fn during(from: u64, to: u64) -> String {
    if to <= from + 1 {
        format!("during step {}", from + 1)
    } else {
        format!("during steps {} to {to}", from + 1)
    }
}

/// Step `n` (counted from 0) counted from 1: "the 13th step".
pub fn nth_step(n: u64) -> String {
    match n {
        0 => "the first step".into(),
        n => format!("the {} step", ordinal(n + 1)),
    }
}

fn code(s: &str) -> String {
    format!("`{s}`")
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// What a fault acts on, and whether it is several things.
fn target_words(t: &Target) -> (String, bool) {
    match t {
        Target::Service(s) if s == THREAD_TARGET => ("the processes of your workload".into(), true),
        Target::Service(s) => (code(s), false),
        Target::List(l) => (
            and_list(
                &l.iter()
                    .map(|s| {
                        if s == THREAD_TARGET {
                            "the processes of your workload".to_string()
                        } else {
                            code(s)
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
            true,
        ),
        Target::Group { group, sel } => match sel {
            Selector::One(_) => (format!("one member of the group {}", code(group)), false),
            Selector::Minority(_) => (format!("a minority of the group {}", code(group)), true),
            Selector::Majority(_) => (format!("a majority of the group {}", code(group)), true),
            Selector::All => (format!("every member of the group {}", code(group)), true),
            Selector::Primary => (
                format!(
                    "the leader of the group {} (the member its `primary:` command names)",
                    code(group)
                ),
                false,
            ),
        },
    }
}

fn it_them(plural: bool) -> &'static str {
    if plural {
        "them"
    } else {
        "it"
    }
}

fn netem_words(l: &[Netem]) -> String {
    let parts: Vec<String> = l
        .iter()
        .map(|n| match n {
            Netem::Delay(d) => format!("delays every packet by {}", ms(*d)),
            Netem::Loss(100) => "drops every packet".into(),
            Netem::Loss(p) => format!("drops {p}% of the packets"),
            Netem::Duplicate(p) => format!("sends {p}% of the packets twice"),
            Netem::Reorder(p) => format!("delivers {p}% of the packets out of order"),
            Netem::Corrupt(p) => format!("damages {p}% of the packets"),
        })
        .collect();
    and_list(&parts)
}

fn net_kind(l: &[Netem]) -> &'static str {
    if l.iter().any(|n| matches!(n, Netem::Loss(100))) {
        "network cuts"
    } else {
        "network faults"
    }
}

/// The network a fault acts on, as the subject of a sentence ("the traffic to `redis`").
fn net_target_words(t: &NetTarget) -> String {
    match t {
        NetTarget::Service(s) => format!("the network on the way to {}", code(s)),
        NetTarget::Link(a, b) => format!("the link between {} and {}", code(a), code(b)),
        NetTarget::LinkAll(a) => format!("every link between {} and the other services", code(a)),
        NetTarget::Partition(c) => format!(
            "every link between {}",
            and_list(
                &c.iter()
                    .map(|x| {
                        if x.len() == 1 {
                            code(&x[0])
                        } else {
                            format!(
                                "the side {}",
                                and_list(&x.iter().map(|s| code(s)).collect::<Vec<_>>())
                            )
                        }
                    })
                    .collect::<Vec<_>>()
            )
        ),
        NetTarget::Shape { group, shape } => {
            let g = code(group);
            match shape {
                Shape::Halves(_) => format!("every link between two halves of the group {g}"),
                Shape::Isolate(_) => {
                    format!("every link between one member of the group {g} and the others")
                }
                Shape::Ring => format!(
                    "the links of the group {g} so that each member hears only a majority of \
                     them"
                ),
                Shape::Bridge(_) => format!(
                    "the links between two halves of the group {g}, which reach each other only \
                     through one member"
                ),
                Shape::Primary => {
                    format!("every link between the leader of the group {g} and the others")
                }
            }
        }
    }
}

fn clock_point_words(p: &ClockPoint) -> String {
    let base = match p.every {
        Every::Day { h, m, s } => {
            if s == 0 {
                format!("the next {h:02}:{m:02} UTC")
            } else {
                format!("the next {h:02}:{m:02}:{s:02} UTC")
            }
        }
        Every::Hour { m, s } => {
            if s == 0 {
                format!("the next minute {m} of an hour")
            } else {
                format!("the next minute {m}, second {s} of an hour")
            }
        }
        Every::HourStart => "the start of the next hour".into(),
        Every::DayStart => "the next midnight UTC".into(),
        Every::MonthStart => "the first day of the next month, 00:00 UTC".into(),
        Every::YearStart => "the next 1 January, 00:00 UTC".into(),
    };
    match p.offset {
        0 => base,
        o if o < 0 => format!("{} before {base}", duration(o.unsigned_abs())),
        o => format!("{} after {base}", duration(o as u64)),
    }
}

fn thread_words(th: &ThreadSel) -> String {
    match th.nth {
        Some(k) => format!(
            "the {} thread named {} of your workload",
            ordinal(k),
            code(&th.name)
        ),
        None => format!("the threads named {} of your workload", code(&th.name)),
    }
}

fn corruption_words(c: &Corruption) -> String {
    let mut s = match c.kind {
        CorruptKind::Flip => {
            let n = c.flips();
            format!("flips {n} bit{} of", if n == 1 { "" } else { "s" })
        }
        CorruptKind::Zero => format!("overwrites {} bytes with zeros in", c.len.unwrap_or(4096)),
        CorruptKind::Trunc => match c.len {
            Some(n) => format!("cuts {n} bytes off the end of"),
            None => "cuts a piece off the end of".into(),
        },
    };
    s.push_str(" one of its files");
    if let Some(f) = &c.file {
        s.push_str(&format!(" whose name contains {}", code(f)));
    }
    if let Some((a, b)) = c.range {
        s.push_str(&format!(", within its bytes {a} to {b}"));
    }
    s
}

fn knob_named(letter: char, knobs: &[KnobInfo]) -> Option<&KnobInfo> {
    knobs.iter().find(|k| k.letter == letter)
}

/// A rule list of `H=` in words: what the outside service does, and when.
fn rules_words(rules: &[Rule]) -> (String, String) {
    let mut acts: Vec<String> = Vec::new();
    let mut when: Vec<String> = Vec::new();
    let mut retry: Option<String> = None;
    let mut effect: Option<bool> = None;
    let mut dir: Option<Direction> = None;
    for r in rules {
        match r {
            Rule::Status(Status::Code(n)) => acts.push(format!("answers with the status {n}")),
            Rule::Status(Status::Name(n)) => {
                acts.push(format!("answers with the gRPC status {}", code(n)))
            }
            Rule::RetryAfter(v) => retry = Some(v.clone()),
            Rule::Delay(n) => acts.push(format!("answers {} late", ms(*n))),
            Rule::Late(n) => acts.push(format!("sends its webhook {} late", ms(*n))),
            Rule::P(n) => when.push(format!("for {n}% of the requests")),
            Rule::Drop => acts.push("does not answer".into()),
            Rule::Reset(Some(ResetKind::Stream)) => acts.push("resets the stream".into()),
            Rule::Reset(_) => acts.push("resets the connection".into()),
            Rule::Dup => acts.push("sends its webhook twice".into()),
            Rule::Reorder => acts.push("sends its webhooks out of order".into()),
            Rule::Lost => acts.push("never sends its webhook".into()),
            Rule::Effect(e) => effect = Some(*e),
            Rule::Rpc(m) => when.push(format!("on calls of {}", code(m))),
            Rule::Call(a, None) => when.push(format!("on the {} call", ordinal(*a))),
            Rule::Call(a, Some(b)) => when.push(format!("on calls {a} to {b}")),
            Rule::Cut(n) => acts.push(format!("cuts the stream after {n} messages")),
            Rule::Hold(n) => acts.push(format!("holds the stream after {n} messages")),
            Rule::Dir(d) => dir = Some(*d),
            Rule::Seed(_) => {}
        }
    }
    if acts.is_empty() {
        acts.push("misbehaves".into());
    }
    let mut a = and_list(&acts);
    if let Some(v) = retry {
        a.push_str(&format!(" with a Retry-After of {}", code(&v)));
    }
    match dir {
        Some(Direction::Req) => a.push_str(" on the request side"),
        Some(Direction::Resp) => a.push_str(" on the answer side"),
        None => {}
    }
    match effect {
        Some(true) => a.push_str(", but still does the work"),
        Some(false) => a.push_str(", and does not do the work"),
        None => {}
    }
    (a, when.join(", "))
}

// ------------------------------------------------------------------ the host part

/// One host token in words. `s` is the schedule it is in (a disk fault's choice of requests
/// matters only when the disk has a fault).
pub fn host(t: &HostToken, s: &Schedule) -> Said {
    let w = t.to_string();
    let q = |sentence: &str, gloss: &str| {
        Said::new(w.clone(), Part::Quiet, sentence.into(), gloss.into())
    };
    let setup =
        |sentence: String, gloss: &str| Said::new(w.clone(), Part::Setup, sentence, gloss.into());
    match t {
        HostToken::Disk(Disk::None) => q(
            "The disk keeps every write at once, so a power cut loses only what the system \
             still held in memory.",
            "the disk keeps every write at once",
        )
        .noun("a disk that keeps every write at once".into()),
        HostToken::Disk(Disk::Cut) => setup(
            "The disk keeps recent writes in a cache that a power cut loses, as real disks do."
                .into(),
            "a power cut loses unflushed writes",
        )
        .noun("a disk whose cache a power cut loses".into()),
        HostToken::Disk(Disk::IoErr(p)) => setup(
            format!(
                "The disk fails some reads and writes with an error (rate {p}: a larger number \
                 means more errors)."
            ),
            "the disk returns errors",
        )
        .noun("a disk that returns errors".into())
        .fault("disk errors", "after disk errors".into()),
        HostToken::Disk(Disk::Lying(p)) => setup(
            format!(
                "The disk is faulty: it throws away some writes that it had already confirmed, \
                 even ones flushed to disk (rate {p}: a larger number means more lost writes)."
            ),
            "a faulty disk throws away writes it already confirmed",
        )
        .noun("a faulty disk that throws away writes it already confirmed".into())
        .fault("a faulty disk", "with a faulty disk".into()),
        HostToken::Dseed(_) => match s.disk() {
            Disk::IoErr(_) | Disk::Lying(_) => q(
                "This number picks which disk requests the fault hits; the same number picks \
                     the same requests.",
                "which disk requests the fault hits",
            ),
            _ => q(
                "This number would pick which disk requests a disk fault hits. This run has no \
                 disk fault; the number is kept so that the run is exactly the same.",
                "kept so that the run is exactly the same",
            ),
        },
        HostToken::Tj(_) => setup(
            "Some timer interrupts come late, which changes the order in which the threads of \
             the system run."
                .into(),
            "late timer interrupts",
        )
        .fault("late timer interrupts", "with late timer interrupts".into()),
        HostToken::Tjpm(n) => q(
            &format!("About {n} in 1,000 timer interrupts come late."),
            &format!("{n} in 1,000 timer interrupts late"),
        ),
        HostToken::Tjmax(n) => q(
            &format!("A late timer interrupt is at most {n} units of the machine's clock late."),
            "how late an interrupt can be",
        ),
        HostToken::Tjlo(k) => q(
            &format!(
                "Timer interrupts come late only from the {} interrupt on.",
                ordinal(*k)
            ),
            "from which interrupt on they come late",
        ),
        HostToken::Tjhi(0) => q(
            "Timer interrupts can come late until the end of the run.",
            "late interrupts until the end",
        ),
        HostToken::Tjhi(k) => q(
            &format!(
                "Timer interrupts come late only before the {} interrupt.",
                ordinal(*k)
            ),
            "until which interrupt they come late",
        ),
        HostToken::Tjx(l) => setup(
            format!(
                "{} chosen timer interrupt{} come{} late, which changes the order in which the \
                 threads of the system run.",
                l.len(),
                if l.len() == 1 { "" } else { "s" },
                if l.len() == 1 { "s" } else { "" }
            ),
            "chosen timer interrupts come late",
        )
        .fault("late timer interrupts", "with late timer interrupts".into()),
        HostToken::Pp(_) => setup(
            "Timer interrupts come more often while the workload runs, so the threads of the \
             system switch more often."
                .into(),
            "more frequent timer interrupts",
        ),
        HostToken::Pptsc(_) => q(
            "The machine's clock advances by a fixed amount at each of those interrupts.",
            "how far the clock moves at an interrupt",
        ),
        HostToken::Xt(_) => setup(
            "hydo adds timer interrupts between the usual ones, so the threads of the system \
             switch at other moments."
                .into(),
            "extra timer interrupts",
        )
        .fault(
            "extra timer interrupts",
            "with extra timer interrupts".into(),
        ),
        HostToken::Xtr(_) | HostToken::Xtpm(_) | HostToken::Xtlo(_) | HostToken::Xthi(_) => q(
            "This number sets how many extra timer interrupts hydo adds, and where.",
            "where the extra interrupts go",
        ),
        HostToken::Xtsc(_) => q(
            "The machine's clock advances by a fixed amount at each extra interrupt.",
            "how far the clock moves at an extra interrupt",
        ),
        HostToken::Xtx(l) => setup(
            format!(
                "hydo adds {} timer interrupt{} at chosen moments, so the threads of the system \
                 switch there.",
                l.len(),
                if l.len() == 1 { "" } else { "s" }
            ),
            "timer interrupts at chosen moments",
        )
        .fault(
            "extra timer interrupts",
            "with extra timer interrupts".into(),
        ),
        HostToken::Cf(k) => Said::new(
            w.clone(),
            Part::Fault,
            format!(
                "The power goes out when the system asks the disk to save its data (fsync) for \
                 the {} time. Then the machine starts again.",
                ordinal(*k)
            ),
            format!("cut the power at the {} fsync", ordinal(*k)),
        )
        .noun(format!("a power cut at the {} fsync", ordinal(*k)))
        .fault("power cuts", "after a power cut".into()),
        HostToken::Cfn => q(
            "hydo counts how often the system asks the disk to save its data.",
            "count the fsync calls",
        ),
        HostToken::Rb(_) => q(
            "This number sets how often hydo looks at the machine; it changes nothing in the \
             system.",
            "how often hydo looks at the machine",
        ),
        HostToken::Q(n) => setup(
            format!(
                "The machine's processors take turns of a fixed length ({n}), which decides how \
                 threads on different processors interleave."
            ),
            "the length of each processor's turn",
        ),
        HostToken::Qs(_) => q(
            "The length of each processor's turn varies, in a way that repeats in every replay.",
            "turns of varying length",
        ),
        HostToken::Qb { len, q: s, .. } => setup(
            format!("Now and then the processors take bursts of short turns ({len} turns of {s})."),
            "bursts of short turns",
        ),
        HostToken::Qbs(_) => q(
            "This number picks where the bursts of short turns come.",
            "where the bursts come",
        ),
        HostToken::Qm(_) => q(
            "This number sets how precisely a processor's turn ends.",
            "how precisely a turn ends",
        ),
        HostToken::Pct(_) => setup(
            "hydo makes the system switch threads at chosen moments.".into(),
            "thread switches at chosen moments",
        )
        .fault(
            "forced thread switches",
            "with thread switches at chosen moments".into(),
        ),
        HostToken::Pctn(n) => q(
            &format!("hydo forces {n} thread switches."),
            &format!("{n} forced thread switches"),
        ),
        HostToken::Pctk(_) => q(
            "This number sets the part of the run in which the thread switches come.",
            "where the thread switches come",
        ),
        HostToken::SpinRewind => q(
            "hydo ends busy waits of the system the same way in every replay.",
            "busy waits end the same way every time",
        ),
        HostToken::Br(_) => q(
            "From a moment of the run on, the timing changes as the next settings say.",
            "from here on, other timing",
        ),
    }
}

// ------------------------------------------------------------------ the guest part

/// One guest token in words.
pub fn guest(t: &GuestToken, knobs: &[KnobInfo]) -> Said {
    let s = guest_said(t, knobs);
    match guest_noun(t, knobs) {
        Some(n) => s.noun(n),
        None => s,
    }
}

/// A guest token as a noun phrase, where the gloss is not one.
fn guest_noun(t: &GuestToken, knobs: &[KnobInfo]) -> Option<String> {
    Some(match t {
        GuestToken::Kill { target, at, .. } => {
            format!("a crash of {} {}", target_words(target).0, after(*at))
        }
        GuestToken::TimedKill { target, at, off_ms } => format!(
            "a crash of {} {} into {}",
            target_words(target).0,
            ms(*off_ms),
            nth_step(*at)
        ),
        GuestToken::Pause(Pause::Steps { target, at, .. }) => {
            format!("a pause of {} {}", target_words(target).0, after(*at))
        }
        GuestToken::Pause(Pause::Timer {
            target,
            at,
            off_ms,
            thread,
            ..
        }) => format!(
            "a pause of {} {} into {}",
            match thread {
                Some(th) => thread_words(th),
                None => target_words(target).0,
            },
            ms(*off_ms),
            nth_step(*at)
        ),
        GuestToken::Net { from, to, .. } => {
            format!("a network fault {}", during(*from, *to))
        }
        GuestToken::OneWay { from, to, .. } => {
            format!("a one-way network fault {}", during(*from, *to))
        }
        GuestToken::Cut(at) => format!("a power cut {}", after(*at)),
        GuestToken::Clock { at, delta } => format!(
            "a clock jump {} by {} {}",
            if *delta < 0 { "back" } else { "forward" },
            duration(delta.unsigned_abs()),
            after(*at)
        ),
        GuestToken::ClockTo { at, to } => {
            format!("a clock jump to {} {}", clock_point_words(to), after(*at))
        }
        GuestToken::Protocol(h) => format!(
            "a fault of the outside service {}",
            code(h.target.split('.').next().unwrap_or(&h.target))
        ),
        GuestToken::Freeze(f) => format!("a freeze of {}", code(&f.svc)),
        GuestToken::React(r) => format!("a reaction to {}", code(&r.event)),
        GuestToken::Corrupt { svc, at, .. } => {
            format!("a damaged file of {} {}", code(svc), after(*at))
        }
        GuestToken::PowerLoss { svc, at, .. } => {
            format!("a power loss of {} {}", code(svc), after(*at))
        }
        GuestToken::Knob { letter, value } => match knob_named(*letter, knobs) {
            Some(k) => format!(
                "{} at {}",
                k.name
                    .clone()
                    .unwrap_or_else(|| format!("the setting {letter}")),
                code(value)
            ),
            None => format!("your setting {letter} at {}", code(value)),
        },
        _ => return None,
    })
}

fn guest_said(t: &GuestToken, knobs: &[KnobInfo]) -> Said {
    let w = t.to_string();
    let fault =
        |sentence: String, gloss: String| Said::new(w.clone(), Part::Fault, sentence, gloss);
    let setup =
        |sentence: String, gloss: String| Said::new(w.clone(), Part::Setup, sentence, gloss);
    match t {
        GuestToken::Kill { target, at, down } => {
            let (who, pl) = target_words(target);
            fault(
                format!(
                    "hydo kills {who} {}, as in a crash, and starts {} again {}.",
                    after(*at),
                    it_them(pl),
                    after(at + down)
                ),
                format!("kill {who} {}", after(*at)),
            )
            .at(*at)
            .fault(
                "crashes",
                format!("after {who} {} killed", if pl { "were" } else { "was" }),
            )
        }
        GuestToken::Pause(Pause::Steps { target, at, steps }) => {
            let (who, pl) = target_words(target);
            fault(
                format!(
                    "hydo pauses {who} {} and lets {} continue {}.",
                    after(*at),
                    it_them(pl),
                    after(at + steps)
                ),
                format!("pause {who} {}", after(*at)),
            )
            .at(*at)
            .fault(
                "pauses",
                format!("while {who} {} paused", if pl { "were" } else { "was" }),
            )
        }
        GuestToken::Pause(Pause::Timer {
            target,
            at,
            off_ms,
            dur_ms,
            thread,
        }) => {
            let (who, pl) = match thread {
                Some(th) => (thread_words(th), th.nth.is_none()),
                None => target_words(target),
            };
            fault(
                format!(
                    "{} after {} starts, hydo pauses {who} for {}.",
                    cap(&ms(*off_ms)),
                    nth_step(*at),
                    ms(*dur_ms)
                ),
                format!("pause {who} {} into {}", ms(*off_ms), nth_step(*at)),
            )
            .at(*at)
            .fault(
                "pauses",
                format!("while {who} {} paused", if pl { "were" } else { "was" }),
            )
        }
        GuestToken::TimedKill { target, at, off_ms } => {
            let (who, pl) = target_words(target);
            fault(
                format!(
                    "{} after {} starts, hydo kills {who}, as in a crash.",
                    cap(&ms(*off_ms)),
                    nth_step(*at)
                ),
                format!("kill {who} {} into {}", ms(*off_ms), nth_step(*at)),
            )
            .at(*at)
            .fault(
                "crashes",
                format!("after {who} {} killed", if pl { "were" } else { "was" }),
            )
        }
        GuestToken::Net {
            target,
            from,
            to,
            netem,
        } => {
            let partition = target.is_partition();
            let subject = net_target_words(target);
            let sentence = if partition && netem.iter().any(|n| matches!(n, Netem::Loss(100))) {
                format!(
                    "{}, hydo cuts {subject}: the two sides \
                     cannot reach each other.",
                    cap(&during(*from, *to))
                )
            } else {
                format!(
                    "{}, {subject} {}.",
                    cap(&during(*from, *to)),
                    netem_words(netem)
                )
            };
            fault(sentence, format!("a network fault {}", during(*from, *to)))
                .at(*from)
                .fault(net_kind(netem), "during a network fault".into())
        }
        GuestToken::OneWay {
            target,
            from,
            to,
            dir,
            netem,
        } => {
            let who = match target {
                NetTarget::Service(s) => code(s),
                other => net_target_words(other),
            };
            let (way, other) = match dir {
                Direction::Req => (format!("the requests to {who}"), "the answers"),
                Direction::Resp => (format!("the answers from {who}"), "the requests"),
            };
            fault(
                format!(
                    "{}, the network {} on {way}; {other} pass \
                     normally.",
                    cap(&during(*from, *to)),
                    netem_words(netem)
                ),
                format!("a one-way network fault {}", during(*from, *to)),
            )
            .at(*from)
            .fault(net_kind(netem), "during a network fault".into())
        }
        GuestToken::Cut(at) => {
            let sentence = match at {
                0 => "The power goes out before the first step.".to_string(),
                n => format!(
                    "The power goes out after {}, before {}.",
                    if *n == 1 {
                        "the first step".to_string()
                    } else {
                        format!("{n} steps")
                    },
                    nth_step(*n)
                ),
            };
            fault(sentence, format!("power cut before {}", nth_step(*at)))
                .at(*at)
                .fault("power cuts", "after a power cut".into())
        }
        GuestToken::Clock { at, delta } => {
            let way = if *delta < 0 { "back" } else { "forward" };
            fault(
                format!(
                    "{}, the system clock jumps {way} by {}.",
                    cap(&after(*at)),
                    duration(delta.unsigned_abs())
                ),
                format!(
                    "move the clock {way} by {} {}",
                    duration(delta.unsigned_abs()),
                    after(*at)
                ),
            )
            .at(*at)
            .fault("clock jumps", "after a clock jump".into())
        }
        GuestToken::ClockTo { at, to } => fault(
            format!(
                "{}, the system clock jumps forward to {}.",
                cap(&after(*at)),
                clock_point_words(to)
            ),
            format!("move the clock to {} {}", clock_point_words(to), after(*at)),
        )
        .at(*at)
        .fault("clock jumps", "after a clock jump".into()),
        GuestToken::Concurrency { n, double } => setup(
            format!(
                "The workload sends {n} requests at once{}.",
                if *double {
                    ", and sends each one twice"
                } else {
                    ""
                }
            ),
            format!(
                "{n} requests at once{}",
                if *double { ", each sent twice" } else { "" }
            ),
        ),
        GuestToken::Buggify(b) => {
            let mut s = "The fault points in your code (BUGGIFY) are switched on".to_string();
            if let Some(e) = b.enable_permille {
                s.push_str(&format!(": about {e} in 1,000 of them are active"));
                if let Some(f) = b.fire_permille {
                    s.push_str(&format!(", and each fires about {f} in 1,000 times"));
                }
            }
            s.push('.');
            setup(s, "your code's own fault points switched on".into()).fault(
                "your code's fault points",
                "with your code's fault points on".into(),
            )
        }
        GuestToken::Protocol(h) => {
            let who = match h.target.split_once('.') {
                Some((x, route)) => {
                    format!(
                        "the outside service {}, on its route {},",
                        code(x),
                        code(route)
                    )
                }
                None => format!("the outside service {}", code(&h.target)),
            };
            let (acts, when) = rules_words(&h.rules);
            let when = if when.is_empty() {
                String::new()
            } else {
                format!(" {when}")
            };
            fault(
                format!("{}, {who} {acts}{when}.", cap(&during(h.from, h.to))),
                format!("{who} {acts}{when}"),
            )
            .at(h.from)
            .fault(
                "outside services that misbehave",
                "while an outside service misbehaved".into(),
            )
        }
        GuestToken::Freeze(f) => {
            let peer = if f.peer == "workload" {
                "your workload".to_string()
            } else {
                code(&f.peer)
            };
            let (verb, prep) = match f.dir {
                FreezeDir::Sends => ("sends", "to"),
                FreezeDir::Receives => ("receives", "from"),
            };
            let matching = f
                .matching
                .as_ref()
                .map(|m| format!(" that contains {}", code(m)))
                .unwrap_or_default();
            let n = f.n.unwrap_or(1);
            let svc = code(&f.svc);
            if n == 0 {
                setup(
                    format!(
                        "From {} on, hydo records the packets{matching} that {svc} {verb} \
                         {prep} {peer}, and freezes nothing.",
                        nth_step(f.step)
                    ),
                    format!("record the packets {svc} {verb}"),
                )
            } else {
                fault(
                    format!(
                        "From {} on, hydo freezes {svc} for {} at the {} packet{matching} \
                         it {verb} {prep} {peer}.",
                        nth_step(f.step),
                        ms(f.dur_ms),
                        ordinal(n)
                    ),
                    format!("freeze {svc} at the {} packet it {verb}", ordinal(n)),
                )
                .at(f.step)
                .fault("freezes", format!("while {svc} was frozen"))
            }
        }
        GuestToken::Compose(c) => {
            let (s, g) = match c {
                Compose::Default => (
                    "Your test commands run in their usual order.".to_string(),
                    "your test commands in their usual order".to_string(),
                ),
                Compose::Seed { fields, .. } => {
                    let mut more: Vec<String> = Vec::new();
                    for f in fields {
                        match f {
                            SeedField::P(n) => more.push(format!("at most {n} at once")),
                            SeedField::Q(n) => more.push(format!("at most {n} drivers in a round")),
                            SeedField::R(n) => more.push(format!("rounds of {}", ms(*n))),
                            SeedField::N(n) => more.push(format!("at most {n} commands")),
                            SeedField::T(n) => more.push(format!("for at most {}", ms(*n))),
                            SeedField::X(_) => more.push("some entries left out".into()),
                            SeedField::O(_) => more.push("some commands never run".into()),
                        }
                    }
                    let more = if more.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", and_list(&more))
                    };
                    (
                        format!("Your test commands run in a mix and order hydo picked{more}."),
                        "your test commands in an order hydo picked".to_string(),
                    )
                }
                Compose::Resolved(fields) => {
                    let n = fields
                        .iter()
                        .filter(|f| matches!(f, ResolvedField::Entry { .. }))
                        .count();
                    (
                        format!(
                            "Your test commands run in a recorded order of {n} entr{}.",
                            if n == 1 { "y" } else { "ies" }
                        ),
                        "your test commands in a recorded order".to_string(),
                    )
                }
                Compose::Saved(k) => (
                    format!("Your test commands run in the order saved as {}.", code(k)),
                    "your test commands in a saved order".to_string(),
                ),
            };
            setup(s, g)
        }
        GuestToken::React(r) => {
            let act = match &r.action {
                ReactAction::Cut => "cuts the power".to_string(),
                ReactAction::Kill(s) => format!("kills {}", code(s)),
                ReactAction::Stop(s, n) => format!("pauses {} for {}", code(s), ms(*n)),
                ReactAction::Cont(s) => format!("lets {} continue", code(s)),
            };
            let from = if r.at > 0 {
                format!(" from {} on", nth_step(r.at))
            } else {
                String::new()
            };
            let later = if r.delay_ms > 0 {
                format!(" {} later", ms(r.delay_ms))
            } else {
                String::new()
            };
            let (kind, ctx) = match &r.action {
                ReactAction::Cut => ("power cuts", "after a power cut".to_string()),
                ReactAction::Kill(s) => ("crashes", format!("after {} was killed", code(s))),
                ReactAction::Stop(s, _) => ("pauses", format!("while {} was paused", code(s))),
                ReactAction::Cont(s) => ("pauses", format!("after {} continued", code(s))),
            };
            fault(
                format!(
                    "When your system reports {}{from}, hydo{later} {act}.",
                    code(&r.event)
                ),
                format!("when {} is reported, {act}", code(&r.event)),
            )
            .at(r.at)
            .fault(kind, ctx)
        }
        GuestToken::Corrupt { svc, at, down, how } => {
            let s = code(svc);
            let sentence = match down {
                Some(d) => format!(
                    "{}, hydo stops {s}, {} and starts it again {}.",
                    cap(&after(*at)),
                    corruption_words(how),
                    after(at + d)
                ),
                None => format!(
                    "{}, while {s} runs, hydo {}.",
                    cap(&after(*at)),
                    corruption_words(how)
                ),
            };
            fault(sentence, format!("damage a file of {s} {}", after(*at)))
                .at(*at)
                .fault("damaged files", format!("after hydo damaged a file of {s}"))
        }
        GuestToken::PowerLoss { svc, at, down } => {
            let s = code(svc);
            fault(
                format!(
                    "{}, {s} loses power on its own: what it had not yet written to its disk is \
                     lost. It starts again {}.",
                    cap(&after(*at)),
                    after(at + down)
                ),
                format!("{s} loses power {}", after(*at)),
            )
            .at(*at)
            .fault(
                "power losses of one service",
                format!("after {s} lost power"),
            )
        }
        GuestToken::Knob { letter, value } => match knob_named(*letter, knobs) {
            Some(k) => {
                let name = k
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("the setting {letter}"));
                let usual = k.safe_value().filter(|d| *d != value);
                let sentence = match usual {
                    Some(d) => format!(
                        "{} is set to {}, not its safe value {}.",
                        cap(&name),
                        code(value),
                        code(d)
                    ),
                    None => format!("{} is set to {}.", cap(&name), code(value)),
                };
                let sentence = match &k.description {
                    Some(d) if !d.trim().is_empty() => {
                        format!("{sentence} {}", sentence_of(d))
                    }
                    _ => sentence,
                };
                setup(sentence, format!("{name} set to {}", code(value)))
            }
            None => setup(
                format!(
                    "Your project's setting {} is set to {}.",
                    code(&letter.to_string()),
                    code(value)
                ),
                format!("your setting {letter} set to {}", code(value)),
            ),
        },
    }
}

/// A manifest's description as a sentence: its first line, with a full stop.
fn sentence_of(d: &str) -> String {
    let l = d.trim().lines().next().unwrap_or("").trim();
    let mut s = cap(l);
    if !s.ends_with(['.', '!', '?']) {
        s.push('.');
    }
    s
}
