//! The test composer's rounds driven from the host (seam `composer`; docs/HISTORY.md M86, M98,
//! M123): what `hydo/onboard/hydo-composer.sh` (the guest's `/etc/hydo/compose.sh`, staged only
//! for a plan with `test:`) does, with its decisions made here and its processes started through
//! hydo-agent.
//!
//! A composed plan reaches the generic init as compose.sh calls (hydo-manifest's `expand`): the
//! workload is `compose.sh step FROM TO`, provisioning ends with `compose.sh first`, the W= token
//! is the knob `W` whose set command is `compose.sh setw`, and the invariants are
//! `compose.sh verdict | end IDX | check IDX`. Each of those calls is replaced here:
//!
//! - [`set_knob`] (`setw`): W= is parsed as `parse_w` parses it and its composition is computed
//!   (`wplan`): the seed form by hydo-composer's generator ([`hydo_composer::plan`], the Rust
//!   twin of PLAN_AWK) with PLAN_AWK's filters, the resolved and saved forms as RES_AWK reads them,
//!   with its messages;
//! - [`provision`] (`first`): the project's own provisioning, then every `first` command;
//! - [`step`] (`step FROM TO`): opens the gate to round TO, starts the daemon the first time, and
//!   waits until the daemon waits at round TO (the last range: until it drained), or the daemon
//!   died, or `workload.timeout` minus 5 s passed (then `stop_all`);
//! - the daemon ([`Job::Daemon`]): the rounds (each `round_ms` of guest time after the previous
//!   one, later when the gate held it), each entry launched when `can_start` allows it (or skipped
//!   after the budget), the drain and its kills, the `HYDO-COMPOSE done` line;
//! - one thread per launched entry ([`Job::Run1`], `run1`): the command with HYDO_CMD,
//!   HYDO_CMD_ID, HYDO_ROUND and HYDO_SEED, its HYDO-ASSERT lines, its end line and the
//!   `compose.<id>.succeeded` / `no_violation` records;
//! - [`eval`]: the invariants `verdict` (answered from the host's state), `end IDX` and
//!   `check IDX` (their commands run in the guest).
//!
//! The console lines are compose.sh's, byte for byte, except the durations (`ms=`): the shell read
//! /proc/uptime around the command, the controller reads the agent's monotonic clock before the
//! command and after it saw the command's exit, so a duration includes how late the host saw
//! the exit (AMBIGUOUS, HANDOFF.md).

use super::text::{centis, read_bytes, records, text};
use super::{
    bb, die_oracle, env, env_with, jesc, log, post, run, run_argv, signal_cmd, spawn_wait,
    spawn_wait_sig, Ctx, Part, Ran, F,
};
use crate::busybox;
use crate::flow::{Memo, Step, Stop};
use crate::seams::Unsupported;
use hydo_agent_proto::msg::{Command, Mkdir, Sig, Spawn};
use hydo_manifest::py;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The guest composer's command line in a composed plan (hydo-composer's `SH`).
pub const SH: &str = hydo_composer::SH;

/// One line of compose.list (or compose.checks), as `line_of` reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub kind: String,
    /// Seconds (`timeout`; for a check the plan's command_timeout).
    pub timeout: i64,
    /// The command id (`<kind>_<name>`; for a check its name).
    pub id: String,
    /// The shell text (`shell_of(run)` on one line, as `read -r` gives the last field).
    pub cmd: String,
}

/// What the plan's `test:` puts into an image for the composer (manifest.sh's HYDO_COMPOSE_*
/// lines, compose.list, compose.checks, compose.saved/).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposePlan {
    pub parallel: i64,
    pub per_round: i64,
    pub round_ms: i64,
    pub command_timeout: i64,
    pub drain_timeout: i64,
    pub max_commands: i64,
    pub budget_ms: i64,
    /// compose.list, in its order (the commands sorted by id).
    pub commands: Vec<Cmd>,
    /// compose.checks.
    pub checks: Vec<Cmd>,
    /// compose.saved/<key>: the resolved compositions of the plan's regressions (`W=@<key>`).
    pub saved: BTreeMap<String, String>,
    /// Whether provisioning ends with `compose.sh first`.
    pub has_first: bool,
    /// The project's own provisioning before `) && compose.sh first`.
    pub own_provision: Option<String>,
}

fn int(v: &Value) -> i64 {
    py::int_of(v)
        .and_then(|i| i64::try_from(i).ok())
        .unwrap_or(0)
}

/// `read -r ci ck cto cid ccmd`: the last field without the blanks around it.
fn last_field(s: &str) -> String {
    s.trim_matches([' ', '\t', '\n']).to_string()
}

impl ComposePlan {
    /// The composer's part of a validated manifest whose `_compose` is the plan meta
    /// (hydo_manifest::load, or for_explorer's meta put there), as hydo-build stages it.
    pub fn of(d: &Value) -> Result<Option<ComposePlan>, Unsupported> {
        let Some(meta) = d.get("_compose").filter(|m| py::truthy(m)) else {
            return Ok(None);
        };
        let p = &meta["params"];
        let ct = int(&p["command_timeout"]);
        let commands: Vec<Cmd> = meta["commands"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|c| Cmd {
                kind: py::str_of(&c["kind"]),
                timeout: int(&c["timeout"]),
                id: py::str_of(&c["id"]),
                cmd: last_field(&hydo_composer::shell_of(&c["run"]).replace('\n', " ")),
            })
            .collect();
        let checks: Vec<Cmd> = meta["checks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|c| Cmd {
                kind: py::str_of(&c["kind"]),
                timeout: ct,
                id: py::str_of(&c["name"]),
                cmd: last_field(&hydo_composer::shell_of(&c["run"]).replace('\n', " ")),
            })
            .collect();
        // stage(): a regression's composition is saved under the key its W=@<key> names
        let mut saved = BTreeMap::new();
        for r in meta["regressions"].as_array().cloned().unwrap_or_default() {
            let w = hydo_composer::w_of(&py::str_of(&r["schedule"]));
            let c = r.get("compose").filter(|c| py::truthy(c)).map(py::str_of);
            if let (Some(w), Some(c)) = (w, c) {
                if let Some(k) = w.strip_prefix('@') {
                    if hydo_composer::key_of(&c) == k {
                        saved.insert(k.to_string(), c);
                    }
                }
            }
        }
        let prov = d.get("provision").map(py::str_of).unwrap_or_default();
        let first = format!("{SH} first");
        let tail = format!("\n) && {first}");
        let (has_first, own_provision) = if prov == first {
            (true, None)
        } else if let Some(own) = prov.strip_prefix("( ").and_then(|x| x.strip_suffix(&tail)) {
            (true, Some(own.to_string()))
        } else {
            (false, None)
        };
        Ok(Some(ComposePlan {
            parallel: int(&p["parallel"]),
            per_round: int(&p["per_round"]),
            round_ms: int(&p["round_ms"]),
            command_timeout: ct,
            drain_timeout: int(&p["drain_timeout"]),
            max_commands: int(&p["max_commands"]),
            budget_ms: int(&p["budget_ms"]),
            commands,
            checks,
            saved,
            has_first,
            own_provision,
        }))
    }
}

// ------------------------------------------------------------------ W= and its composition

/// parse_w's variables, as the shell keeps them (strings; WS a number).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WFields {
    pub w: String,
    pub ws: i64,
    pub wp: String,
    pub wq: String,
    pub wr: String,
    pub wn: String,
    pub wt: String,
    pub wx: String,
    pub wo: String,
    pub wv: String,
}

/// Why `wplan` cannot run W on this image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WFail {
    /// `$WERR`: setw prints `HYDO-COMPOSE error: $WERR` (empty when an awk program failed).
    Werr(String),
    /// The shell itself stops (`$((0x…))` of a seed field that is not hex): no line is printed.
    Crash,
}

/// One entry of a composition: (entry number, round, command index, kind, HYDO_SEED), a line of
/// `$C/plan`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub entry: i64,
    pub round: i64,
    pub idx: usize,
    pub kind: String,
    pub seed: i64,
}

impl Entry {
    /// The plan line PLAN_AWK and RES_AWK print.
    pub fn line(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.entry, self.round, self.idx, self.kind, self.seed
        )
    }
}

/// `IFS=,; set -- $x`: the fields of a comma list (a last empty field is not one).
fn comma_fields(x: &str) -> Vec<String> {
    let mut v: Vec<String> = x.split(',').map(str::to_string).collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// parse_w: the console lines it prints and its fields. `w` is what compose.W holds (the knob's
/// value; "" when no W= was given).
pub fn parse_w(cp: &ComposePlan, w: &str) -> (Vec<String>, Result<WFields, WFail>) {
    let mut lines = Vec::new();
    let mut f = WFields {
        w: if w == "-" {
            String::new()
        } else {
            w.to_string()
        },
        ws: 0,
        wp: cp.parallel.to_string(),
        wq: cp.per_round.to_string(),
        wr: cp.round_ms.to_string(),
        wn: cp.max_commands.to_string(),
        wt: cp.budget_ms.to_string(),
        ..WFields::default()
    };
    if f.w.is_empty() {
        return (lines, Ok(f));
    }
    if let Some(k) = f.w.strip_prefix('@') {
        if k.is_empty() || k.bytes().any(|c| !matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            let e = format!("W={}: @ takes the key of a saved composition (hex)", f.w);
            return (lines, Err(WFail::Werr(e)));
        }
        match cp.saved.get(k) {
            Some(c) if !c.trim_end_matches('\n').is_empty() => {
                f.wv = c.trim_end_matches('\n').to_string()
            }
            _ => {
                let e = format!(
                    "W={}: this image carries no saved composition {k} (build it with the regression that has it)",
                    f.w
                );
                return (lines, Err(WFail::Werr(e)));
            }
        }
    } else if f.w.starts_with(':') {
        f.wv = f.w.clone();
    }
    if !f.wv.is_empty() {
        f.wn = "0".into();
        f.wt = "0".into();
        f.wq = "-".into();
        for x in comma_fields(&f.wv[1..]) {
            // `p[0-9]*)`: the letter, a digit, then anything
            let digit = x.len() > 1 && x[1..].starts_with(|d: char| d.is_ascii_digit());
            match x.chars().next() {
                Some('p') if digit => f.wp = x[1..].to_string(),
                Some('r') if digit => f.wr = x[1..].to_string(),
                Some('t') if digit => f.wt = x[1..].to_string(),
                _ => {}
            }
        }
        return (lines, Ok(f));
    }
    let fs = comma_fields(&f.w);
    let hex = fs.first().cloned().unwrap_or_default();
    if hex.is_empty() || hex.len() > 16 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return (lines, Err(WFail::Crash));
    }
    f.ws = u64::from_str_radix(&hex, 16).unwrap_or(0) as i64;
    for x in fs.iter().skip(1) {
        let v = x.get(1..).unwrap_or("").to_string();
        match x.chars().next() {
            Some('p') => f.wp = v,
            Some('q') => f.wq = v,
            Some('r') => f.wr = v,
            Some('n') => f.wn = v,
            Some('t') => f.wt = v,
            Some('x') => f.wx = v,
            Some('o') => f.wo = v,
            _ => lines.push(format!(
                "HYDO-COMPOSE: W={}: unknown field '{x}' ignored",
                f.w
            )),
        }
    }
    (lines, Ok(f))
}

/// awk's numeric value of a string (`x + 0`): its leading number, 0 when there is none.
pub fn awk_num(s: &str) -> f64 {
    let t = s.trim_start_matches([' ', '\t', '\n']);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let d0 = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == d0 || (i == d0 + 1 && b[d0] == b'.') {
        return 0.0;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let e0 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > e0 {
            i = j;
        }
    }
    t[..i].parse().unwrap_or(0.0)
}

/// Whether awk takes a `-v` value or a split piece as a number (a "strnum").
fn strnum(s: &str) -> bool {
    let t = s.trim_matches([' ', '\t', '\n']);
    !t.is_empty() && t.parse::<f64>().is_ok() && !t.contains(['x', 'X', 'n', 'N', 'i', 'I'])
}

/// PLAN_AWK's `inlist(lst, v)`.
fn inlist(lst: &str, v: f64) -> bool {
    for a in lst.split('.') {
        if a.is_empty() {
            continue;
        }
        if a.contains('-') {
            let b: Vec<&str> = a.split('-').collect();
            let lo = awk_num(b.first().copied().unwrap_or(""));
            let hi = awk_num(b.get(1).copied().unwrap_or(""));
            if v >= lo && v <= hi {
                return true;
            }
        } else if v == awk_num(a) {
            return true;
        }
    }
    false
}

/// awk's `a == b` of two strings that may be strnums.
fn awk_eq(a: &str, b: &str) -> bool {
    if strnum(a) && strnum(b) {
        awk_num(a) == awk_num(b)
    } else {
        a == b
    }
}

/// PLAN_AWK on this plan: hydo-composer's generator gives the rounds' entries; X, O and N
/// filter them as the awk does.
fn plan_awk(cp: &ComposePlan, steps: i64, f: &WFields) -> Result<Vec<Entry>, WFail> {
    let kinds: Vec<String> = cp.commands.iter().map(|c| c.kind.clone()).collect();
    let q = awk_num(&f.wq) as i128;
    let es = hydo_composer::plan(f.ws as i128, q, steps as i128, &kinds)
        .map_err(|_| WFail::Werr(String::new()))?;
    let omitted = |c: usize| -> bool {
        if !f.wo.chars().any(|x| x.is_ascii_alphabetic()) {
            return inlist(&f.wo, c as f64);
        }
        f.wo.split('+').any(|a| awk_eq(a, &cp.commands[c].id))
    };
    // `N > 0 && out >= N`, with awk's comparisons of a -v value
    let n_num = strnum(&f.wn);
    let n_pos = if n_num {
        awk_num(&f.wn) > 0.0
    } else {
        f.wn.as_str() > "0"
    };
    let mut out = Vec::new();
    for (i, r, c, s) in es {
        if inlist(&f.wx, i as f64) || omitted(c) {
            continue;
        }
        let k = out.len();
        let full = if n_num {
            k as f64 >= awk_num(&f.wn)
        } else {
            k.to_string().as_str() >= f.wn.as_str()
        };
        if n_pos && full {
            break;
        }
        out.push(Entry {
            entry: i as i64,
            round: r as i64,
            idx: c,
            kind: kinds[c].clone(),
            seed: s as i64,
        });
    }
    Ok(out)
}

/// RES_AWK on this plan: the entries of a resolved composition, or its one-line message.
fn res_awk(cp: &ComposePlan, steps: i64, v: &str) -> Result<Vec<Entry>, String> {
    let ix = |id: &str| cp.commands.iter().position(|c| c.id == id);
    let all: String = cp.commands.iter().map(|c| format!(" {}", c.id)).collect();
    let fs: Vec<&str> = v.get(1..).unwrap_or("").split(',').collect();
    let (mut nc, mut ci): (i64, Vec<usize>) = (-1, Vec::new());
    let mut out = Vec::new();
    let is_digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    for x in fs {
        let first = x.chars().next();
        if matches!(first, Some('p' | 'r' | 't')) && is_digits(&x[1..]) {
            continue;
        }
        if first == Some('c') && nc < 0 {
            let cs: Vec<&str> = if x.len() > 1 {
                x[1..].split('+').collect()
            } else {
                Vec::new()
            };
            nc = cs.len() as i64;
            for c in cs {
                let Some(i) = ix(c) else {
                    return Err(format!(
                        "the composition uses command {c}, which this image does not have (commands:{all})"
                    ));
                };
                if !hydo_composer::PLAN_KINDS.contains(&cp.commands[i].kind.as_str()) {
                    return Err(format!("command {c} is not a driver or anytime command"));
                }
                ci.push(i);
            }
            continue;
        }
        let e: Vec<&str> = x.split('.').collect();
        if e.len() == 4 && e.iter().all(|p| is_digits(p)) {
            let k = awk_num(e[2]);
            if k >= nc as f64 {
                return Err(format!(
                    "entry {x} names command {} of a table of {}",
                    e[2],
                    nc.max(0)
                ));
            }
            if awk_num(e[1]) >= steps as f64 {
                return Err(format!(
                    "entry {x} is in round {}, the plan has {steps} rounds",
                    e[1]
                ));
            }
            let c = ci[k as usize];
            out.push(Entry {
                entry: awk_num(e[0]) as i64,
                round: awk_num(e[1]) as i64,
                idx: c,
                kind: cp.commands[c].kind.clone(),
                seed: awk_num(e[3]) as i64,
            });
            continue;
        }
        return Err(format!("field {x} is not understood"));
    }
    if out.is_empty() {
        return Err("no entry".into());
    }
    Ok(out)
}

/// W's fields and its entries.
pub type Composition = (WFields, Vec<Entry>);

/// wplan: the console lines parse_w prints, and W's fields and composition on this image.
pub fn wplan(cp: &ComposePlan, steps: i64, w: &str) -> (Vec<String>, Result<Composition, WFail>) {
    let (lines, r) = parse_w(cp, w);
    let f = match r {
        Ok(f) => f,
        Err(e) => return (lines, Err(e)),
    };
    let es = if f.wv.is_empty() {
        plan_awk(cp, steps, &f)
    } else {
        res_awk(cp, steps, &f.wv).map_err(|m| WFail::Werr(format!("W={}: {m}", f.w)))
    };
    (lines, es.map(|es| (f, es)))
}

/// The daemon's `HYDO-COMPOSE plan` line.
pub fn plan_line(f: &WFields, steps: i64, n: usize) -> String {
    if f.wv.is_empty() {
        let w = if f.w.is_empty() { "-" } else { &f.w };
        format!(
            "HYDO-COMPOSE plan W={w} seed={} p={} q={} r={} n={} rounds={steps} entries={n}",
            f.ws, f.wp, f.wq, f.wr, f.wn
        )
    } else {
        format!(
            "HYDO-COMPOSE plan W={} resolved p={} r={} t={} rounds={steps} entries={n}",
            f.w, f.wp, f.wr, f.wt
        )
    }
}

// ------------------------------------------------------------------ the host's state

/// One launched entry (`$C/pid.<i>` and, once it ended, `$C/rc.<i>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launched {
    pub entry: i64,
    pub kind: String,
    pub id: String,
    pub thread: usize,
    /// ok | violation | timeout | failed | killed; None while it runs.
    pub status: Option<String>,
}

/// The composer's files in $C, as host state of one conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// compose.W (the knob's value).
    pub w: String,
    /// $C/started: a step ran in this boot.
    pub started: bool,
    /// $C/gate.
    pub gate: i64,
    /// $C/at: the round the daemon waits at.
    pub at: Option<i64>,
    /// $C/done.
    pub done: bool,
    /// $C/incomplete.
    pub incomplete: bool,
    /// $C/summary.
    pub summary: Option<String>,
    /// $C/violation and violation.detail: (entry, command id, detail).
    pub violation: Option<(i64, String, String)>,
    /// $C/ok.<id>, $C/vio.<id>.
    pub ok_ids: BTreeSet<String>,
    pub vio_ids: BTreeSet<String>,
    /// The daemon's environment (the first step call's).
    pub env: Vec<String>,
    pub daemon: Option<usize>,
    /// The daemon process ended without its done file.
    pub died: Option<String>,
    /// stop_all killed the daemon.
    pub stopped: bool,
    pub launched: Vec<Launched>,
    pub skipped: i64,
}

impl State {
    fn running(&self) -> impl Iterator<Item = &Launched> {
        self.launched.iter().filter(|l| l.status.is_none())
    }

    /// can_start: (NP, NS, NG, NA) against WP.
    fn can_start(&self, kind: &str, wp: Option<i64>) -> bool {
        let Some(wp) = wp else { return false };
        let n = |k: &str| self.running().filter(|l| l.kind == k).count() as i64;
        let (np, ns, ng) = (
            n("parallel_driver"),
            n("serial_driver"),
            n("singleton_driver"),
        );
        let all = self.running().count() as i64;
        match kind {
            "parallel_driver" => all < wp && ns + ng == 0,
            "anytime" => all < wp,
            _ => np + ns + ng == 0 && all < wp,
        }
    }

    fn count(&self, st: &str) -> usize {
        self.launched
            .iter()
            .filter(|l| l.status.as_deref() == Some(st))
            .count()
    }
}

/// The composer's threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    Daemon,
    /// run1 <entry> <round> <cmd index> <entry seed>.
    Run1 {
        entry: i64,
        round: i64,
        idx: usize,
        seed: i64,
    },
}

pub(super) fn label(j: &Job) -> String {
    match j {
        Job::Daemon => "composer".into(),
        Job::Run1 { entry, .. } => format!("cmd:{entry}"),
    }
}

fn cdir(ctx: &Ctx) -> String {
    format!("{}/compose", ctx.plan.layout.dir)
}

fn plan(ctx: &Ctx) -> &ComposePlan {
    ctx.plan.compose.as_ref().expect("a composed plan")
}

/// `$BB timeout <s> $BB sh -c ". $E/prelude.sh; <cmd>"`.
fn timeout_sh(ctx: &Ctx, secs: i64, cmd: &str) -> Vec<String> {
    let mut a = vec![bb(ctx), "timeout".into(), secs.to_string()];
    a.extend(run_argv(ctx, cmd));
    a
}

/// An environment with extra variables (a prefix assignment overrides an inherited one).
fn with_vars(base: &[String], extra: &[(&str, String)]) -> Vec<String> {
    let mut m: BTreeMap<String, String> = base
        .iter()
        .filter_map(|e| e.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for (k, v) in extra {
        m.insert(k.to_string(), v.clone());
    }
    m.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

fn mkdir_c(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    post(
        fx,
        Command::Mkdir(Mkdir {
            path: cdir(ctx),
            mode: None,
        }),
    )
}

/// A spawn whose output goes to `out` (truncated).
fn spawn_to(h: &str, argv: Vec<String>, env: Vec<String>, out: &str) -> Command {
    Command::Spawn(Spawn {
        h: h.into(),
        argv,
        env: Some(env),
        cwd: None,
        uid: None,
        gid: None,
        netns: None,
        stdout: Some(out.into()),
        stderr: Some(out.into()),
        append: None,
        kill_after_ms: None,
    })
}

/// A guest timer: `sleep <secs>` under a handle of its own, killed when it is not needed.
fn timer(fx: &mut F<'_>, ctx: &Ctx, h: &str, secs: &str) -> Step<u64> {
    // the daemon's own timers run with its environment; the step's with the init's
    let e = if h == "compose-drain" {
        daemon_env(fx)
    } else {
        env(fx, ctx, &[])
    };
    fx.send(spawn_to(
        h,
        vec![bb(ctx), "sleep".into(), secs.into()],
        e,
        "/dev/null",
    ))
}

/// The daemon's environment: the init's when the first step call forked it (later restarts do
/// not change it, as in the shell's background process).
fn daemon_env(fx: &mut F<'_>) -> Vec<String> {
    let base = fx.effect_text(|h, _| h.compose.env.join("\n"));
    base.lines().map(str::to_string).collect()
}

/// `grep -a '^HYDO-ASSERT' log` and `grep -av '^HYDO-ASSERT' log | tail -1`.
fn asserts_and_last(b: &[u8]) -> (Vec<String>, String) {
    let ls = records(b);
    let a = ls
        .iter()
        .filter(|l| l.starts_with(b"HYDO-ASSERT"))
        .map(|l| text(l))
        .collect();
    let last = ls
        .iter()
        .rev()
        .find(|l| !l.starts_with(b"HYDO-ASSERT"))
        .map(|l| text(l))
        .unwrap_or_default();
    (a, last)
}

/// Stop a composer thread that the shell would have killed (the daemon after stop_all, an
/// entry after the drain killed it): it does nothing more.
fn guard(fx: &mut F<'_>, entry: Option<i64>) -> Step<()> {
    let dead = fx.effect_bool(|h, _| match entry {
        None => h.compose.stopped,
        Some(e) => h
            .compose
            .launched
            .iter()
            .any(|l| l.entry == e && l.status.as_deref() == Some("killed")),
    });
    if dead {
        Err(Stop::Cancelled)
    } else {
        Ok(())
    }
}

// ------------------------------------------------------------------ the knob, first, step

/// set_knob for the composer's knob W (default `-`): `compose.sh setw VALUE`.
pub(super) fn set_knob(fx: &mut F<'_>, ctx: &Ctx, value: &str) -> Step<()> {
    if value == "-" {
        return Ok(());
    }
    let cp = plan(ctx);
    let v = value.to_string();
    fx.effect(|h, _| {
        h.compose.w = v;
        Memo::Unit
    });
    let (lines, r) = wplan(cp, ctx.plan.steps, value);
    for l in lines {
        fx.report(l);
    }
    match r {
        Ok(_) => {}
        Err(WFail::Werr(e)) => {
            fx.report(format!("HYDO-COMPOSE error: {e}"));
            return die_oracle(fx, format!("HYDO-ORACLE: ERROR knob W={value} failed"));
        }
        Err(WFail::Crash) => {
            return die_oracle(fx, format!("HYDO-ORACLE: ERROR knob W={value} failed"));
        }
    }
    log(fx, format!("knob W={value} (test composer (W=))"));
    Ok(())
}

/// provision's command for a plan with `first` commands: the project's own provisioning, then
/// `compose.sh first`. None when provisioning is not composed; else its exit status.
pub(super) fn provision(fx: &mut F<'_>, ctx: &Ctx) -> Step<Option<u32>> {
    let Some(cp) = ctx.plan.compose.as_ref().filter(|c| c.has_first) else {
        return Ok(None);
    };
    let l = &ctx.plan.layout;
    if let Some(own) = &cp.own_provision {
        let e = env(fx, ctx, &[]);
        let st = spawn_wait(
            fx,
            "provision",
            run_argv(ctx, &format!("( {own}\n)")),
            e,
            &format!("{}/provision.log", l.dir),
            false,
        )?;
        if st != 0 {
            return Ok(Some(st));
        }
    }
    mkdir_c(fx, ctx)?;
    let mut rc = 0;
    for (ci, c) in cp.commands.iter().enumerate() {
        if c.kind != "first" {
            continue;
        }
        fx.report(format!("HYDO-CMD start i=f{ci} kind=first cmd={}", c.id));
        let t1 = centis(fx)?;
        let e = env(fx, ctx, &[("HYDO_CMD", c.id.clone())]);
        let st = spawn_wait(
            fx,
            &format!("first:{ci}"),
            timeout_sh(ctx, c.timeout, &c.cmd),
            e,
            &format!("{}/log.f{ci}", cdir(ctx)),
            false,
        )?;
        let t2 = centis(fx)?;
        fx.report(format!(
            "HYDO-CMD end i=f{ci} cmd={} rc={st} ms={}",
            c.id,
            (t2 - t1) * 10
        ));
        if st != 0 {
            // AMBIGUOUS: the shell also writes "<id> exited rc=<rc>:" and the log's last 5 lines
            // into provision.log; nothing reads that file.
            rc = 1;
        }
    }
    Ok(Some(rc))
}

/// The seconds `step` gives the composition before it stops it (the init kills the call at
/// workload.timeout).
fn step_budget(t: i64) -> i64 {
    if t > 10 {
        t - 5
    } else {
        t / 2 + 1
    }
}

/// step_range's workload for a composed plan (`compose.sh step FROM TO`): its exit status.
pub(super) fn step(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<u32> {
    let steps = ctx.plan.steps;
    mkdir_c(fx, ctx)?;
    let first = fx.effect_bool(|h, _| {
        h.compose.gate = b;
        !std::mem::replace(&mut h.compose.started, true)
    });
    if first {
        let e = env(
            fx,
            ctx,
            &[("HYDO_FROM", a.to_string()), ("HYDO_TO", b.to_string())],
        );
        let t = fx.start(Part::Compose(Job::Daemon))?;
        fx.effect(|h, _| {
            h.compose.env = e;
            h.compose.daemon = Some(t);
            Memo::Unit
        });
    }
    let secs = step_budget(ctx.plan.workload_timeout);
    let tm = timer(fx, ctx, &format!("compose-step:{b}"), &secs.to_string())?;
    fx.until(|h, f| {
        let c = &h.compose;
        (b >= steps && c.done)
            || (b < steps && c.at == Some(b))
            || c.died.is_some()
            || c.stopped
            || f.exits.contains_key(&tm)
            || matches!(f.replies.get(&tm), Some(Err(_)))
    })?;
    // the loop's order: the round reached (or the drain done), the daemon gone, the time up
    let outcome = fx.effect_text(|h, _| {
        let c = &h.compose;
        if (b >= steps && c.done) || (b < steps && c.at == Some(b)) {
            "ok".into()
        } else if c.died.is_some() || c.stopped {
            // `kill -0 $_dp` fails: the daemon died, or stop_all killed it
            if c.done {
                "ok".into()
            } else {
                "died".into()
            }
        } else if c.done {
            "late".into()
        } else {
            "timeout".into()
        }
    });
    match outcome.as_str() {
        "ok" => {
            post(fx, signal_cmd(&format!("compose-step:{b}"), Sig::Kill))?;
            Ok(0)
        }
        "died" => {
            post(fx, signal_cmd(&format!("compose-step:{b}"), Sig::Kill))?;
            let werr = fx.effect_text(|h, _| h.compose.died.clone().unwrap_or_default());
            let mut t = String::from("compose.sh: the composer daemon died\n");
            if !werr.is_empty() {
                t.push_str(&werr);
                t.push('\n');
            }
            post(
                fx,
                super::write_cmd(&format!("{}/workload.log", ctx.plan.layout.dir), &t, true),
            )?;
            Ok(1)
        }
        "late" => Ok(1),
        _ => {
            stop_all(fx, ctx, a, b)?;
            Ok(1)
        }
    }
}

/// stop_all: the step ran into workload.timeout: the daemon and every running command are
/// killed, and the ones that had not ended are recorded as killed.
fn stop_all(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<()> {
    let dt = fx.effect_text(|h, _| {
        h.compose.stopped = true;
        h.compose.daemon.map(|d| d.to_string()).unwrap_or_default()
    });
    if let Ok(d) = dt.parse::<usize>() {
        fx.cancel(d);
    }
    // `for _p in $C/pid.*`: the entries in the glob's (byte) order of their numbers
    let open = fx.effect_text(|h, _| {
        let mut v: Vec<&Launched> = h.compose.running().collect();
        v.sort_by_key(|l| l.entry.to_string());
        v.iter()
            .map(|l| format!("{} {} {}", l.entry, l.thread, l.id))
            .collect::<Vec<_>>()
            .join("\n")
    });
    let open: Vec<(i64, usize, String)> = open
        .lines()
        .map(|l| {
            let mut w = l.splitn(3, ' ');
            (
                w.next().unwrap().parse().unwrap(),
                w.next().unwrap().parse().unwrap(),
                w.next().unwrap().to_string(),
            )
        })
        .collect();
    // `kill -9 $(tree $_dp)`: the daemon's tree, which holds every running command
    for (e, t, _) in &open {
        post(fx, signal_cmd(&format!("cmd:{e}"), Sig::Kill))?;
        fx.cancel(*t);
    }
    for (e, _, id) in &open {
        fx.report(format!(
            "HYDO-CMD end i={e} cmd={id} rc=137 status=killed (workload.timeout)"
        ));
    }
    let sum = fx.effect_text(|h, _| {
        let c = &mut h.compose;
        for l in c.launched.iter_mut() {
            if l.status.is_none() {
                l.status = Some("killed".into());
            }
        }
        let s = format!(
            "started={} ok={} failed={} timeout={} violation={} killed={} skipped=0",
            c.launched.len(),
            c.count("ok"),
            c.count("failed"),
            c.count("timeout"),
            c.count("violation"),
            c.count("killed")
        );
        c.summary = Some(s.clone());
        c.incomplete = true;
        s
    });
    fx.report(format!(
        "HYDO-COMPOSE done {sum} (stopped: steps {a}..{} ran into workload.timeout={}s)",
        b - 1,
        ctx.plan.workload_timeout
    ));
    fx.effect(|h, _| {
        h.compose.done = true;
        Memo::Unit
    });
    Ok(())
}

// ------------------------------------------------------------------ the daemon and run1

pub(super) fn job(fx: &mut F<'_>, ctx: &Ctx, j: &Job) -> Step<()> {
    match j {
        Job::Daemon => daemon(fx, ctx),
        Job::Run1 {
            entry,
            round,
            idx,
            seed,
        } => run1(fx, ctx, *entry, *round, *idx, *seed),
    }
}

fn daemon(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let cp = plan(ctx);
    let steps = ctx.plan.steps;
    let w = fx.effect_text(|h, _| h.compose.w.clone());
    let (lines, r) = wplan(cp, steps, &w);
    guard(fx, None)?;
    for l in lines {
        fx.report(l);
    }
    let (f, plan) = match r {
        Ok(x) => x,
        Err(e) => {
            let werr = match e {
                WFail::Werr(m) => {
                    fx.report(format!("HYDO-COMPOSE error: {m}"));
                    m
                }
                WFail::Crash => String::new(),
            };
            fx.effect(|h, _| {
                h.compose.died = Some(werr);
                Memo::Unit
            });
            return Ok(());
        }
    };
    fx.report(plan_line(&f, steps, plan.len()));
    for c in &cp.commands {
        if hydo_composer::PLAN_KINDS.contains(&c.kind.as_str()) {
            fx.report(format!(
                "HYDO-ASSERT-DECL {{\"t\":\"sometimes\",\"id\":\"compose.{}.succeeded\",\"msg\":\"{} exited 0 at least once\"}}",
                c.id, c.id
            ));
            fx.report(format!(
                "HYDO-ASSERT-DECL {{\"t\":\"always_or_unreachable\",\"id\":\"compose.{}.no_violation\",\"msg\":\"{} never reports a violation (exit 100)\"}}",
                c.id, c.id
            ));
        }
    }
    let wp: Option<i64> = f.wp.trim().parse().ok();
    let wt: i64 = f.wt.trim().parse().unwrap_or(0);
    let rc = (f.wr.trim().parse::<i64>().unwrap_or(0) / 10).max(1);
    let t0 = centis(fx)?;
    let mut t = t0;
    let mut r = -1i64;
    let mut k = 0usize;
    for e in &plan {
        while r < e.round {
            r += 1;
            round_start(fx, ctx, r, rc, &mut t, &mut k)?;
        }
        launch(fx, ctx, e, r, wp, wt, t0)?;
    }
    while r < steps - 1 {
        r += 1;
        round_start(fx, ctx, r, rc, &mut t, &mut k)?;
    }
    drain(fx, ctx)
}

/// round_start: wait for the round's time, then for the gate. `k` numbers the round waits (the
/// handles of their timers).
fn round_start(fx: &mut F<'_>, ctx: &Ctx, r: i64, rc: i64, t: &mut i64, k: &mut usize) -> Step<()> {
    if r > 0 {
        *t += rc;
        let now = centis(fx)?;
        if now < *t {
            let d = *t - now;
            let e = daemon_env(fx);
            *k += 1;
            run(
                fx,
                vec![
                    bb(ctx),
                    "sleep".into(),
                    format!("{}.{:02}", d / 100, d % 100),
                ],
                e,
                None,
            )?;
        }
    }
    let wait = fx.effect_bool(|h, _| {
        if r >= h.compose.gate {
            h.compose.at = Some(r);
            true
        } else {
            false
        }
    });
    if wait {
        fx.until(|h, _| r < h.compose.gate)?;
    }
    let now = centis(fx)?;
    if now > *t {
        *t = now;
    }
    Ok(())
}

fn launch(
    fx: &mut F<'_>,
    ctx: &Ctx,
    e: &Entry,
    r: i64,
    wp: Option<i64>,
    wt: i64,
    t0: i64,
) -> Step<()> {
    let cp = plan(ctx);
    let kind = e.kind.clone();
    fx.until(|h, _| h.compose.can_start(&kind, wp))?;
    let now = centis(fx)?;
    guard(fx, None)?;
    let id = &cp.commands[e.idx].id;
    if wt > 0 && (now - t0) * 10 >= wt {
        fx.report(format!(
            "HYDO-CMD skip i={} r={r} cmd={id} budget_ms={wt}",
            e.entry
        ));
        fx.effect(|h, _| {
            h.compose.skipped += 1;
            Memo::Unit
        });
        return Ok(());
    }
    fx.report(format!(
        "HYDO-CMD start i={} r={r} kind={} cmd={id}",
        e.entry, e.kind
    ));
    let t = fx.start(Part::Compose(Job::Run1 {
        entry: e.entry,
        round: r,
        idx: e.idx,
        seed: e.seed,
    }))?;
    let (entry, id) = (e.entry, id.clone());
    fx.effect(|h, _| {
        h.compose.launched.push(Launched {
            entry,
            kind,
            id,
            thread: t,
            status: None,
        });
        Memo::Unit
    });
    Ok(())
}

fn drain(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let cp = plan(ctx);
    let tm = timer(fx, ctx, "compose-drain", &cp.drain_timeout.to_string())?;
    fx.until(|h, f| {
        h.compose.running().next().is_none()
            || f.exits.contains_key(&tm)
            || matches!(f.replies.get(&tm), Some(Err(_)))
    })?;
    guard(fx, None)?;
    // the entries still running, in launch order ($RUNNING), are killed now
    let open = fx.effect_text(|h, _| {
        let v: Vec<String> = h
            .compose
            .running()
            .map(|l| format!("{} {} {}", l.entry, l.thread, l.id))
            .collect();
        for l in h.compose.launched.iter_mut() {
            if l.status.is_none() {
                l.status = Some("killed".into());
            }
        }
        v.join("\n")
    });
    if open.is_empty() {
        post(fx, signal_cmd("compose-drain", Sig::Kill))?;
    }
    for l in open.lines() {
        let mut w = l.splitn(3, ' ');
        let (e, t, id) = (
            w.next().unwrap(),
            w.next().unwrap().parse::<usize>().unwrap(),
            w.next().unwrap(),
        );
        post(fx, signal_cmd(&format!("cmd:{e}"), Sig::Kill))?;
        fx.cancel(t);
        fx.report(format!(
            "HYDO-CMD end i={e} cmd={id} rc=137 status=killed (still running {}s after the last round)",
            cp.drain_timeout
        ));
    }
    let sum = fx.effect_text(|h, _| {
        let c = &mut h.compose;
        let nkill = c.count("killed");
        c.incomplete = nkill + c.count("timeout") > 0;
        let s = format!(
            "started={} ok={} failed={} timeout={} violation={} killed={nkill} skipped={}",
            c.launched.len(),
            c.count("ok"),
            c.count("failed"),
            c.count("timeout"),
            c.count("violation"),
            c.skipped
        );
        c.summary = Some(s.clone());
        s
    });
    fx.report(format!("HYDO-COMPOSE done {sum}"));
    fx.effect(|h, _| {
        h.compose.done = true;
        Memo::Unit
    });
    Ok(())
}

fn run1(fx: &mut F<'_>, ctx: &Ctx, entry: i64, round: i64, idx: usize, seed: i64) -> Step<()> {
    let c = &plan(ctx).commands[idx];
    let t1 = centis(fx)?;
    let base = fx.effect_text(|h, _| h.compose.env.join("\n"));
    let base: Vec<String> = base.lines().map(str::to_string).collect();
    let e = with_vars(
        &base,
        &[
            ("HYDO_CMD", c.id.clone()),
            ("HYDO_CMD_ID", entry.to_string()),
            ("HYDO_ROUND", round.to_string()),
            ("HYDO_SEED", seed.to_string()),
        ],
    );
    let log_file = format!("{}/log.{entry}", cdir(ctx));
    let id = fx.send(spawn_to(
        &format!("cmd:{entry}"),
        timeout_sh(ctx, c.timeout, &c.cmd),
        e,
        &log_file,
    ))?;
    let (rc, sig) = match fx.reply(id)? {
        Err(_) => (127, None),
        Ok(_) => {
            let x = fx.exit(id)?;
            (x.status, x.sig)
        }
    };
    let t2 = centis(fx)?;
    let st = match rc {
        0 => "ok",
        100 => "violation",
        143 => "timeout",
        _ => "failed",
    };
    // run1's `> $C/log.$1 2>&1`: compose.sh's shell writes its line after a signal there
    let mut out = read_bytes(fx, &log_file)?;
    out.extend(
        busybox::foreground_signal_line(sig)
            .unwrap_or_default()
            .bytes(),
    );
    guard(fx, Some(entry))?;
    let (asserts, last) = asserts_and_last(&out);
    for a in asserts {
        fx.report(a);
    }
    fx.report(format!(
        "HYDO-CMD end i={entry} cmd={} rc={rc} ms={} status={st}",
        c.id,
        (t2 - t1) * 10
    ));
    let cid = c.id.clone();
    match st {
        "ok" => {
            if fx.effect_bool(|h, _| h.compose.ok_ids.insert(cid)) {
                fx.report(format!(
                    "HYDO-ASSERT {{\"t\":\"sometimes\",\"id\":\"compose.{}.succeeded\",\"ok\":true,\"msg\":\"{} exited 0 at least once\"}}",
                    c.id, c.id
                ));
            }
        }
        "violation" => {
            let (cid2, d) = (cid.clone(), last.clone());
            fx.effect(|h, _| {
                if h.compose.violation.is_none() {
                    h.compose.violation = Some((entry, cid2, d));
                }
                Memo::Unit
            });
            if fx.effect_bool(|h, _| h.compose.vio_ids.insert(cid)) {
                fx.report(format!(
                    "HYDO-ASSERT {{\"t\":\"always_or_unreachable\",\"id\":\"compose.{}.no_violation\",\"ok\":false,\"msg\":\"{} never reports a violation (exit 100)\",\"details\":{{\"entry\":{entry},\"out\":\"{}\"}}}}",
                    c.id,
                    c.id,
                    jesc(&last)
                ));
            }
        }
        _ => {}
    }
    let st = st.to_string();
    fx.effect(|h, _| {
        if let Some(l) = h
            .compose
            .launched
            .iter_mut()
            .find(|l| l.entry == entry && l.status.is_none())
        {
            l.status = Some(st);
        }
        Memo::Unit
    });
    Ok(())
}

/// pause_timer's and kill_timer's `pgrep -f "$HYDO_WORKLOAD_PGREP"` for a composed plan
/// (`compose.sh run1`): whether a composed command runs, as far as the host knows.
pub(super) fn in_flight(fx: &mut F<'_>) -> bool {
    fx.effect_bool(|h, _| h.compose.running().next().is_some())
}

// ------------------------------------------------------------------ the invariants

/// An invariant's command when it is one of the composer's: its exit status and output. `exports`
/// are those of the shell process that evaluates it (a periodic checker keeps those of its fork).
pub(super) fn eval(
    fx: &mut F<'_>,
    ctx: &Ctx,
    cmd: &str,
    exports: &BTreeMap<String, String>,
) -> Step<Option<Ran>> {
    let Some(cp) = ctx.plan.compose.as_ref() else {
        return Ok(None);
    };
    let Some(rest) = cmd.strip_prefix(SH).and_then(|r| r.strip_prefix(' ')) else {
        return Ok(None);
    };
    if rest == "verdict" {
        let t = fx.effect_text(|h, _| {
            let c = &h.compose;
            if let Some((i, id, d)) = &c.violation {
                format!("1command {id} (entry {i}) reported a violation (exit 100): {d}\n")
            } else if let Some(s) = &c.summary {
                format!("0{s}\n")
            } else {
                "0no composed workload ran in this boot\n".into()
            }
        });
        return Ok(Some(Ran {
            status: if t.starts_with('1') { 1 } else { 0 },
            out: t[1..].to_string(),
            sig: None,
        }));
    }
    let num = |s: &str| s.parse::<usize>().ok();
    if let Some(i) = rest.strip_prefix("end ").and_then(num) {
        if i < cp.commands.len() {
            return end_cmd(fx, ctx, i, exports).map(Some);
        }
    }
    if let Some(i) = rest.strip_prefix("check ").and_then(num) {
        if i < cp.checks.len() {
            return check_cmd(fx, ctx, i, exports).map(Some);
        }
    }
    Ok(None)
}

/// endc IDX: an eventually or finally command as an invariant.
fn end_cmd(fx: &mut F<'_>, ctx: &Ctx, i: usize, exports: &BTreeMap<String, String>) -> Step<Ran> {
    let c = &plan(ctx).commands[i];
    mkdir_c(fx, ctx)?;
    if c.kind == "finally" {
        let why = fx.effect_text(|h, _| {
            let s = &h.compose;
            if !s.started {
                "skipped: no composed workload ran in this boot (recovery boot after a power cut)"
                    .into()
            } else if !s.done || s.incomplete {
                "skipped: not every started command ran to completion".into()
            } else {
                String::new()
            }
        });
        if !why.is_empty() {
            return Ok(Ran {
                status: 0,
                out: format!("{why}\n"),
                sig: None,
            });
        }
    }
    fx.report(format!(
        "HYDO-CMD start i=e{i} kind={} cmd={}",
        c.kind, c.id
    ));
    let t1 = centis(fx)?;
    let e = env_with(ctx, exports, &[("HYDO_CMD", c.id.clone())]);
    let log_file = format!("{}/log.e{i}", cdir(ctx));
    let (st, sig) = spawn_wait_sig(
        fx,
        &format!("end:{i}"),
        timeout_sh(ctx, c.timeout, &c.cmd),
        e,
        &log_file,
        false,
        None,
    )?;
    let t2 = centis(fx)?;
    let ms = (t2 - t1) * 10;
    fx.report(format!("HYDO-CMD end i=e{i} cmd={} rc={st} ms={ms}", c.id));
    // endc's `> $C/log.e$1 2>&1`: compose.sh's shell writes its line after a signal there
    let mut out = read_bytes(fx, &log_file)?;
    out.extend(
        busybox::foreground_signal_line(sig)
            .unwrap_or_default()
            .bytes(),
    );
    let (asserts, last) = asserts_and_last(&out);
    for a in asserts {
        fx.report(a);
    }
    Ok(if st == 0 {
        Ran {
            status: 0,
            out: format!("{} ok ({ms} ms)\n", c.id),
            sig: None,
        }
    } else {
        Ran {
            status: 1,
            out: format!("{} exited rc={st}: {last}\n", c.id),
            sig: None,
        }
    })
}

/// check IDX: a final/always check (0 holds, 1 violated, 2 not evaluable, a timeout too).
fn check_cmd(fx: &mut F<'_>, ctx: &Ctx, i: usize, exports: &BTreeMap<String, String>) -> Step<Ran> {
    let c = &plan(ctx).checks[i];
    let e = env_with(ctx, exports, &[("HYDO_CHECK", c.id.clone())]);
    let r = run(
        fx,
        timeout_sh(ctx, c.timeout, &c.cmd),
        e,
        Some(super::CHUNK as u32),
    )?;
    // check's command has no redirection of its own: compose.sh's shell writes its line after a
    // signal to its error output, which eval_inv's `> $H/inv.$i.out 2>&1` made that file too,
    // between the check's output and the timeout line
    let out = busybox::with_signal_line(r.out, r.sig);
    Ok(match r.status {
        0..=2 => Ran {
            out,
            sig: None,
            ..r
        },
        143 => Ran {
            status: 2,
            out: format!("{out}check {} timed out after {}s\n", c.id, c.timeout),
            sig: None,
        },
        _ => Ran {
            status: 1,
            out,
            sig: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awk_numbers() {
        assert_eq!(awk_num("3x"), 3.0);
        assert_eq!(awk_num(""), 0.0);
        assert_eq!(awk_num(" -2.5e1z"), -25.0);
        assert!(inlist("0.3-4", 4.0) && inlist("0.3-4", 0.0) && !inlist("0.3-4", 2.0));
        assert!(strnum("12") && !strnum("") && !strnum("ab"));
        assert_eq!(comma_fields("a,,b,"), vec!["a", "", "b"]);
        assert_eq!(step_budget(300), 295);
        assert_eq!(step_budget(10), 6);
    }
}
