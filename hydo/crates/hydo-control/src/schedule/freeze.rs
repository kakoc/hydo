//! FREEZE (docs/HISTORY.md M124): `F=<svc><dir><peer>@<step>[#<n>][:<match>]:<dur>`, a
//! service frozen at the n-th packet of a conversation (M70), as `hydo/onboard/hydo-init-freeze.sh`
//! does it with the helper hydo_fc, here with the agent's own packet trigger (M117: `freeze`,
//! `unfreeze`, the `froze`/`resumed` events).
//!
//! The four init functions the extension wraps keep their places:
//! - set_knob: F= is read in token order among the knobs ([`parse_knob`]: its log line, or the
//!   ERROR verdict of a bad token);
//! - step_range: the `fault.freeze_call` declaration at the first call of a boot, and the arming
//!   before step `<step>` inside the call whose range holds it ([`step_range`]): the netfilter
//!   modules, `XTABLES_LOCKFILE` exported, the agent's `freeze` (hydo_fc's arguments as its
//!   fields), the pinned iptables rules through `run`, the log line;
//! - restart_dead: [`heal`] first (wait for a running freeze to end, remove the rules, stop the
//!   trigger, continue a service it left stopped, the "never fired" count and the `#0` trace);
//! - stop_periodic: [`kill`] first (the trigger must not continue a service after a power cut).
//!
//! hydo_fc's own console lines (FROZE, RESUMED, the `fault.freeze_call` record) come from the
//! agent's events, reported by the thread [`watch`] as they are seen.
//!
//! AMBIGUOUS (HANDOFF.md, M124): the FROZE line names the service's handle where hydo_fc names
//! its pid (the host has no pids); the marker files fc.bound/fc.fired/fc.resumed are the agent's
//! reply and events, so their `rm -f` is not sent and "helper did not start" is the agent's
//! refusal of `freeze`; the helper cannot exit early (it is the agent); a freeze whose pair gives
//! neither an address nor a port is not sent (the agent refuses it; hydo_fc counted nothing), its
//! empty log is written instead.
//!
//! With `network: bridge` (hydo-init-netns.sh's two wrappers, M131): fc_arm runs the helper as
//! `nsenter -n$NSD/<svc> hydo_fc ...`, here the `freeze` command's `netns` field (feature
//! `freeze-netns`: the agent binds the queue inside that namespace), and fc_rules puts the pair's
//! rules into the frozen service's namespace ([`rules_netns`]: OUTPUT holds what it sends, INPUT
//! what it receives, the workload peer is the bridge's address), each `iptables` a `run` with
//! that `netns`. The netfilter modules are loaded in the guest's own namespace, as the shell does.

use super::*;
use crate::flow::Stop;
use hydo_agent_proto::msg::{Dir, Freeze, Unfreeze, UnfreezeOk, WarnCode};

/// The freeze helper's queue, its log, and the rules' NFQUEUE target.
const QUEUE: u32 = 7;
const NFQ: [&str; 5] = ["-j", "NFQUEUE", "--queue-num", "7", "--queue-bypass"];

/// The decl hydo-init-freeze.sh's step_range prints once per boot.
pub const FREEZE_DECL: &str = "HYDO-ASSERT-DECL {\"t\":\"reachable\",\"id\":\"fault.freeze_call\",\"msg\":\"a service was frozen at a call (F=)\"}";
/// hydo_fc's record at the freeze.
pub const FREEZE_RECORD: &str = "HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.freeze_call\",\"ok\":true,\"msg\":\"a service was frozen at a call (F=)\"}";

/// An F= token as fc_parse leaves it (the shell's FC, fc_svc, fc_dir, ... as text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Fc {
    pub svc: String,
    /// `req` (>) or `resp` (<)
    pub dir: String,
    pub tgt: String,
    pub step: String,
    pub n: String,
    /// `-` = every packet
    pub matching: String,
    pub dur: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Bad(&'static str),
    Ignored(String, String),
    Ok(Fc),
}

/// fc_parse, as text operations on the token's value.
fn parse(plan: &Plan, fc: &str) -> Parsed {
    use netfault::{after_first, after_last, before_first, before_last};
    let head = before_first(fc, '@');
    let rest0 = after_first(fc, '@');
    if head == fc {
        return Parsed::Bad("no @");
    }
    let (dir, svc, tgt) = if head.contains('>') {
        ("req", before_first(head, '>'), after_first(head, '>'))
    } else if head.contains('<') {
        ("resp", before_first(head, '<'), after_first(head, '<'))
    } else {
        return Parsed::Bad("no > or <");
    };
    let dur = after_last(rest0, ':');
    let mut rest = before_last(rest0, ':');
    if rest == rest0 {
        return Parsed::Bad("no :<dur>");
    }
    let mut matching = "-";
    if rest.contains(':') {
        matching = after_first(rest, ':');
        rest = before_first(rest, ':');
    }
    if matching.is_empty() {
        matching = "-";
    }
    let (step, n) = if rest.contains('#') {
        (before_first(rest, '#'), after_first(rest, '#'))
    } else {
        (rest, "1")
    };
    let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
    if !digits(step) || !digits(n) || !digits(dur) {
        return Parsed::Bad("step, n and dur must be numbers");
    }
    let known = |s: &str| plan.service(s).is_some();
    if !known(svc) || (tgt != "workload" && !known(tgt)) || svc == tgt {
        return Parsed::Ignored(svc.into(), tgt.into());
    }
    Parsed::Ok(Fc {
        svc: svc.into(),
        dir: dir.into(),
        tgt: tgt.into(),
        step: step.into(),
        n: n.into(),
        matching: matching.into(),
        dur: dur.into(),
    })
}

/// hydo-init-freeze.sh's set_knob for F=: fc_parse with its log line or the ERROR verdict.
pub(super) fn parse_knob(fx: &mut F<'_>, value: &str) -> Step<()> {
    let ctx = fx.shared.ctx.clone();
    match parse(&ctx.plan, value) {
        Parsed::Bad(why) => die_oracle(
            fx,
            format!("HYDO-ORACLE: ERROR bad freeze token F={value} ({why})"),
        ),
        Parsed::Ignored(s, t) => {
            log(fx, format!("F: no services {s} and {t}, ignored"));
            Ok(())
        }
        Parsed::Ok(f) => {
            log(
                fx,
                format!(
                    "freeze at a call: {} ({} with {}) from step {}, event #{}, match '{}', for {}ms",
                    f.svc, f.dir, f.tgt, f.step, f.n, f.matching, f.dur
                ),
            );
            Ok(())
        }
    }
}

/// The freeze in force after the knobs (the last F= decides: FC is set by each fc_parse, and a
/// token that is ignored clears it).
fn effective(ctx: &Ctx) -> Option<Fc> {
    let (_, v) = ctx.run.knobs.iter().rev().find(|(l, _)| l == "F")?;
    match parse(&ctx.plan, v) {
        Parsed::Ok(f) => Some(f),
        _ => None,
    }
}

/// C's atol.
fn atol(s: &str) -> i64 {
    let s = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (neg, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut n: i64 = 0;
    for c in s.bytes() {
        if !c.is_ascii_digit() {
            break;
        }
        n = n.wrapping_mul(10).wrapping_add((c - b'0') as i64);
    }
    if neg {
        -n
    } else {
        n
    }
}

fn mark(fx: &mut F<'_>, name: &str) -> bool {
    fx.effect_bool(|h, _| h.marks.insert(name.to_string()))
}

fn has_mark(fx: &mut F<'_>, name: &str) -> bool {
    fx.effect_bool(|h, _| h.marks.contains(name))
}

fn addr(ctx: &Ctx, s: &str) -> String {
    netfault::svc_addr(ctx, s)
}

fn port(ctx: &Ctx, s: &str) -> String {
    ctx.plan
        .service(s)
        .map(|v| v.port.clone())
        .unwrap_or_default()
}

fn fc_log(ctx: &Ctx) -> String {
    format!("{}/fc.log", ctx.plan.layout.dir)
}

/// hydo-init-freeze.sh's step_range: the declaration once per boot, the arming before fc_step.
pub(super) fn step_range(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<()> {
    if mark(fx, "fc.decl") {
        fx.report(FREEZE_DECL);
    }
    if let Some(fc) = effective(ctx) {
        let st: i64 = fc.step.parse().unwrap_or(i64::MAX);
        if !has_mark(fx, "fc.on") && st >= a && st < b {
            step_range_core(fx, ctx, a, st)?;
            arm(fx, ctx, &fc)?;
            return step_range_core(fx, ctx, st, b);
        }
    }
    step_range_core(fx, ctx, a, b)
}

/// fc_arm, before step fc_step.
fn arm(fx: &mut F<'_>, ctx: &Ctx, fc: &Fc) -> Step<()> {
    let mut sa = addr(ctx, &fc.svc);
    let mut tp = port(ctx, &fc.tgt);
    if fc.tgt == "workload" {
        // classify by the service's own port
        tp = format!("s{}", port(ctx, &fc.svc));
        sa = String::new();
    }
    mark(fx, "fc.on");
    let lock = format!("{}/xtables.lock", ctx.plan.layout.dir);
    fx.effect(|h, _| {
        h.env.insert("XTABLES_LOCKFILE".into(), lock);
        Memo::Unit
    });
    netfault::insmods(
        fx,
        ctx,
        &[
            "x_tables",
            "ip_tables",
            "iptable_filter",
            "xt_tcpudp",
            "xt_NFQUEUE",
            "nfnetlink",
            "nfnetlink_queue",
        ],
    )?;
    // hydo_fc <queue> <pidfile> <sa|-> <tp|s<own>|-> <dir> <n> <match|-> <dur> <log> <dir>
    let (port_, own_port) = match tp.strip_prefix('s') {
        _ if tp.is_empty() => (None, None),
        Some(own) => (None, Some(atol(own) as u32)),
        None => (Some(atol(&tp) as u32), None),
    };
    let sip = (!sa.is_empty()).then(|| sa.clone());
    if sip.is_none() && port_.is_none() && own_port.is_none() {
        // the rules give 3 below; hydo_fc would only have opened its log
        post(fx, write_cmd(&fc_log(ctx), "", false))?;
    } else {
        let id = fx.send(Command::Freeze(Freeze {
            h: fc.svc.clone(),
            queue: QUEUE,
            sip,
            port: port_,
            own_port,
            dir: if fc.dir == "req" { Dir::Req } else { Dir::Resp },
            n: atol(&fc.n).max(0) as u64,
            pattern: (fc.matching != "-").then(|| fc.matching.clone()),
            dur_ms: atol(&fc.dur).max(0) as u64,
            log: Some(fc_log(ctx)),
            // hydo-init-netns.sh's fc_arm: HYDO_FC="$BB nsenter -n$NSD/$fc_svc $_fc"
            netns: netns::of(ctx, &fc.svc),
        }))?;
        fx.start(Part::FreezeWatch {
            id,
            dur: fc.dur.clone(),
        })?;
        if fx.reply(id)?.is_err() {
            return die_oracle(fx, "HYDO-ORACLE: ERROR freeze helper did not start".into());
        }
        fx.effect(|h, _| {
            h.fc_armed = Some(id);
            Memo::Unit
        });
    }
    let (rc, out) = rules(fx, ctx, fc, "-A", Some(CHUNK as u32))?;
    if rc == 3 {
        log(
            fx,
            format!(
                "freeze: {} and {} have neither addresses nor a port, skipped",
                fc.svc, fc.tgt
            ),
        );
        stop_helper(fx)?;
        return Ok(());
    }
    if rc != 0 {
        // `$BB cat $H/fc.ipt`
        for l in lines(&out) {
            fx.note(l.to_string());
        }
        return die_oracle(fx, "HYDO-ORACLE: ERROR freeze rules failed".into());
    }
    log(
        fx,
        format!(
            "FREEZE ARMED before step {}: {} at {} #{} with {}",
            fc.step, fc.svc, fc.dir, fc.n, fc.tgt
        ),
    );
    Ok(())
}

/// fc_rules <-A|-D>: the netfilter rules that queue the pair's packets; (status, output), 3 when
/// the pair has neither addresses nor a port. With `network: bridge` hydo-init-netns.sh's version.
fn rules(
    fx: &mut F<'_>,
    ctx: &Ctx,
    fc: &Fc,
    op: &str,
    capture: Option<u32>,
) -> Step<(u32, String)> {
    if ctx.plan.bridge {
        return rules_netns(fx, ctx, fc, op, capture);
    }
    let sa = addr(ctx, &fc.svc);
    let ta = addr(ctx, &fc.tgt);
    let mut tp = port(ctx, &fc.tgt);
    let w = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
    let opt = |flag: &str, v: &str| -> Vec<String> {
        if v.is_empty() {
            Vec::new()
        } else {
            vec![flag.to_string(), v.to_string()]
        }
    };
    let (mut r1, mut r2) = (w(&[op, "OUTPUT"]), w(&[op, "OUTPUT"]));
    if fc.tgt == "workload" {
        tp = port(ctx, &fc.svc);
        if tp.is_empty() {
            return Ok((3, String::new()));
        }
        r1.extend(w(&["-s", "127.0.0.1"]));
        r1.extend(opt("-d", &sa));
        r1.extend(w(&["-p", "tcp", "--dport", &tp]));
        r2.extend(w(&["-d", "127.0.0.1"]));
        r2.extend(opt("-s", &sa));
        r2.extend(w(&["-p", "tcp", "--sport", &tp]));
    } else if !sa.is_empty() && !ta.is_empty() {
        r1.extend(w(&["-s", &sa, "-d", &ta]));
        r2.extend(w(&["-s", &ta, "-d", &sa]));
    } else if !tp.is_empty() {
        r1.extend(opt("-s", &sa));
        r1.extend(w(&["-p", "tcp", "--dport", &tp]));
        r2.extend(opt("-d", &sa));
        r2.extend(w(&["-p", "tcp", "--sport", &tp]));
    } else {
        return Ok((3, String::new()));
    }
    let mut out = String::new();
    let mut rc = 0;
    for mut r in [r1, r2] {
        r.extend(w(&NFQ));
        let mut argv: Vec<String> = ctx
            .plan
            .layout
            .iptables
            .split_whitespace()
            .map(str::to_string)
            .collect();
        argv.extend(r);
        let e = env(fx, ctx, &[]);
        let ran = run(fx, argv, e, capture)?;
        out.push_str(&ran.out);
        rc = ran.status;
        if rc != 0 {
            break;
        }
    }
    Ok((rc, out))
}

/// hydo-init-netns.sh's fc_rules <-A|-D>: the pair's packets, in the frozen service's namespace
/// (`ipt="$BB nsenter -n$NSD/$fc_svc $HYDO_IPT"`). The shell's words are unquoted, so an empty
/// address leaves its flag without a value, as here (the line is split into words).
fn rules_netns(
    fx: &mut F<'_>,
    ctx: &Ctx,
    fc: &Fc,
    op: &str,
    capture: Option<u32>,
) -> Step<(u32, String)> {
    let sa = addr(ctx, &fc.svc);
    let ta = addr(ctx, &fc.tgt);
    let q = NFQ.join(" ");
    let (r1, r2) = if fc.tgt == "workload" {
        // the workload's clients connect from the bridge's address
        let tp = port(ctx, &fc.svc);
        if tp.is_empty() {
            return Ok((3, String::new()));
        }
        let gw = crate::plan::NET_GW;
        (
            format!("{op} INPUT -s {gw} -d {sa} -p tcp --dport {tp} {q}"),
            format!("{op} OUTPUT -s {sa} -d {gw} -p tcp --sport {tp} {q}"),
        )
    } else {
        (
            format!("{op} OUTPUT -s {sa} -d {ta} {q}"),
            format!("{op} INPUT -s {ta} -d {sa} {q}"),
        )
    };
    let ns = netns::of(ctx, &fc.svc);
    let mut out = String::new();
    let mut rc = 0;
    for r in [r1, r2] {
        let mut argv: Vec<String> = ctx
            .plan
            .layout
            .iptables
            .split_whitespace()
            .map(str::to_string)
            .collect();
        argv.extend(r.split_whitespace().map(str::to_string));
        let e = env(fx, ctx, &[]);
        let ran = run_in(fx, argv, e, capture, ns.clone())?;
        out.push_str(&ran.out);
        rc = ran.status;
        if rc != 0 {
            break;
        }
    }
    Ok((rc, out))
}

/// fc_stop_helper / fc_kill: `unfreeze` when the trigger is armed; its answer.
fn stop_helper(fx: &mut F<'_>) -> Step<Option<UnfreezeOk>> {
    let armed = fx.effect_text(|h, _| h.fc_armed.map(|i| i.to_string()).unwrap_or_default());
    if armed.is_empty() {
        return Ok(None);
    }
    let r = call(fx, Command::Unfreeze(Unfreeze {}))?;
    fx.effect(|h, _| {
        h.fc_armed = None;
        Memo::Unit
    });
    Ok(match r {
        Ok(Reply::Unfreeze(u)) => Some(u),
        _ => None,
    })
}

/// fc_kill (stop_periodic's wrapper): the trigger must not continue the service any more.
pub(super) fn kill(fx: &mut F<'_>) -> Step<()> {
    if !fx.shared.ctx.plan.freeze {
        return Ok(());
    }
    stop_helper(fx)?;
    Ok(())
}

fn fired_in(f: &Facts) -> bool {
    f.events.iter().any(|e| {
        matches!(e, Event::Froze(_))
            || matches!(e, Event::Warn(w) if w.code == WarnCode::FreezeNoProcess)
    })
}

fn resumed_in(f: &Facts) -> bool {
    f.events.iter().any(|e| matches!(e, Event::Resumed(_)))
}

/// fc_heal (restart_dead's wrapper): the end of fault injection without a cut.
pub(super) fn heal(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if !ctx.plan.freeze {
        return Ok(());
    }
    // [ $fc_on = 1 ] || return 0; fc_on=2
    if !has_mark(fx, "fc.on") || !mark(fx, "fc.healed") {
        return Ok(());
    }
    let Some(fc) = effective(ctx) else {
        return Ok(());
    };
    let fired = fx.effect_bool(|_, f| fired_in(f));
    if fired && !fx.effect_bool(|_, f| resumed_in(f)) {
        let lim = atol(&fc.dur) / 1000 + 30;
        let mut n = 0;
        while n < lim {
            let e = env(fx, ctx, &[]);
            run(fx, vec![bb(ctx), "sleep".into(), "1".into()], e, None)?;
            n += 1;
            if fx.effect_bool(|_, f| resumed_in(f)) {
                break;
            }
        }
    }
    rules(fx, ctx, &fc, "-D", None)?;
    let (fired, resumed) = match stop_helper(fx)? {
        Some(u) => (u.fired, u.resumed),
        None => (
            fx.effect_bool(|_, f| fired_in(f)),
            fx.effect_bool(|_, f| resumed_in(f)),
        ),
    };
    if fired && !resumed {
        svc_sig(fx, &fc.svc, Sig::Cont)?;
        log(fx, format!("freeze of {} ended by the init", fc.svc));
    }
    if fired {
        log(fx, "freeze healed");
        return Ok(());
    }
    let e = env(fx, ctx, &[]);
    let r = run(
        fx,
        sh(
            ctx,
            format!("{} grep -c ' #' {} 2>/dev/null", bb(ctx), fc_log(ctx)),
        ),
        e,
        None,
    )?;
    log(
        fx,
        format!(
            "freeze never fired: {} of {} matching events after step {}",
            r.out.trim_end_matches('\n'),
            fc.n,
            fc.step
        ),
    );
    if fc.n == "0" {
        let t = read_all(fx, &fc_log(ctx))?;
        for l in trace_lines(&t, 600) {
            fx.report(l);
        }
    }
    Ok(())
}

/// The trace awk of fc_heal for `#0`: packet, length, [#count,] time, and the first two words of
/// the payload (4+ of A-Z a-z 0-9 _ : -, not "def", at most 32 characters each).
pub fn trace_lines(log: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let recs = lines(log);
    for rec in recs.iter().take(max) {
        let f: Vec<&str> = rec.split_whitespace().collect();
        let fl = |i: usize| f.get(i.wrapping_sub(1)).copied().unwrap_or("");
        let h = fl(4).starts_with('#');
        let mut w = String::new();
        let mut c = 0;
        let mut i = if h { 6 } else { 5 };
        while i <= f.len() && c < 2 {
            let s: String = fl(i)
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == ':' || ch == '-' {
                        ch
                    } else {
                        ' '
                    }
                })
                .collect();
            for a in s.split_whitespace() {
                if c >= 2 {
                    break;
                }
                if a.chars().count() >= 4 && a != "def" {
                    w.push(' ');
                    w.extend(a.chars().take(32));
                    c += 1;
                }
            }
            i += 1;
        }
        out.push(format!(
            "hydo-fc-trace: {} {}{} {}{w}",
            fl(1),
            fl(3),
            if h {
                format!(" {}", fl(4))
            } else {
                String::new()
            },
            fl(if h { 5 } else { 4 })
        ));
    }
    if recs.len() > max {
        out.push(format!(
            "hydo-fc-trace: ... {} more packets not shown",
            recs.len() - max
        ));
    }
    out
}

fn dir_name(d: Dir) -> &'static str {
    match d {
        Dir::Req => "req",
        Dir::Resp => "resp",
    }
}

/// The freeze's events among everything the agent reported (one freeze per conversation, so the
/// trigger's warnings are its own).
fn its_event(e: &Event, id: u64) -> bool {
    match e {
        Event::Froze(f) => f.id == id,
        Event::Resumed(r) => r.id == id,
        Event::Warn(w) => matches!(
            w.code,
            WarnCode::FreezeNoProcess | WarnCode::NetlinkEnobufs | WarnCode::VerdictFailed
        ),
        _ => false,
    }
}

/// The thread for hydo_fc's console lines: one report (or two) per event, as they are seen.
pub(super) fn watch(fx: &mut F<'_>, id: u64, dur: &str) -> Step<()> {
    let dur = atol(dur);
    let mut k = 0;
    loop {
        let ev = fx
            .facts
            .events
            .iter()
            .filter(|e| its_event(e, id))
            .nth(k)
            .cloned();
        let Some(ev) = ev else {
            return Err(Stop::Pending);
        };
        match ev {
            Event::Froze(f) => {
                fx.report(format!(
                    "hydo-fc: FROZE {} ({} processes{}) at {} #{} ({}{}, {}>{} len={}) for {dur} ms: {}",
                    f.h,
                    f.procs,
                    if f.all_stopped {
                        ""
                    } else {
                        ", NOT all threads stopped"
                    },
                    dir_name(f.dir),
                    f.count,
                    dir_name(f.kdir),
                    f.k,
                    f.sport,
                    f.dport,
                    f.len,
                    f.snippet
                ));
                fx.report(FREEZE_RECORD);
            }
            Event::Resumed(r) => {
                fx.report(format!(
                    "hydo-fc: RESUMED after {dur} ms ({} processes)",
                    r.procs
                ));
            }
            Event::Warn(w) => fx.report(format!("hydo-fc: {}", w.msg)),
            _ => {}
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        let m = hydo_manifest::Manifest::from_validated(serde_json::json!({
            "name": "t",
            "services": [
                {"name": "a", "cmd": "a", "ready": "true", "port": 1},
                {"name": "b", "cmd": "b", "ready": "true"}
            ],
            "workload": {"steps": 4, "run": "w"},
            "invariants": [],
            "faults": ["freeze"]
        }));
        Plan::from_manifest(&m).unwrap()
    }

    #[test]
    fn fc_parse_as_the_shell_reads_it() {
        let p = plan();
        let ok = |svc: &str, dir: &str, tgt: &str, step: &str, n: &str, m: &str, dur: &str| {
            Parsed::Ok(Fc {
                svc: svc.into(),
                dir: dir.into(),
                tgt: tgt.into(),
                step: step.into(),
                n: n.into(),
                matching: m.into(),
                dur: dur.into(),
            })
        };
        assert_eq!(
            parse(&p, "a>b@3#2:abc:500"),
            ok("a", "req", "b", "3", "2", "abc", "500")
        );
        assert_eq!(
            parse(&p, "a<workload@0:1"),
            ok("a", "resp", "workload", "0", "1", "-", "1")
        );
        assert_eq!(
            parse(&p, "a>b@3#0::9"),
            ok("a", "req", "b", "3", "0", "-", "9")
        );
        assert_eq!(
            parse(&p, "a>b@3:x:y:9"),
            ok("a", "req", "b", "3", "1", "x:y", "9")
        );
        assert_eq!(parse(&p, "a>b"), Parsed::Bad("no @"));
        assert_eq!(parse(&p, "ab@3:9"), Parsed::Bad("no > or <"));
        assert_eq!(parse(&p, "a>b@3"), Parsed::Bad("no :<dur>"));
        assert_eq!(
            parse(&p, "a>b@x:9"),
            Parsed::Bad("step, n and dur must be numbers")
        );
        assert_eq!(
            parse(&p, "a>a@1:9"),
            Parsed::Ignored("a".into(), "a".into())
        );
        assert_eq!(
            parse(&p, "a>z@1:9"),
            Parsed::Ignored("a".into(), "z".into())
        );
        assert_eq!(atol(" 12x"), 12);
        assert_eq!(atol(""), 0);
    }

    #[test]
    fn the_trace_awk() {
        let log = "req1 4000>6379 len=27 #1 t=100 *2..$3..SET..$3..k:1..$2..v1..\nresp1 6379>4000 len=5 t=101 +OK..\n\n";
        assert_eq!(
            trace_lines(log, 600),
            vec![
                "hydo-fc-trace: req1 len=27 #1 t=100".to_string(),
                "hydo-fc-trace: resp1 len=5 t=101".to_string(),
                "hydo-fc-trace:   ".to_string(),
            ]
        );
        let log = "req2 1>2 len=9 #3 t=7 abcd..defg.def.hijklmnopq\n";
        assert_eq!(
            trace_lines(log, 600),
            vec!["hydo-fc-trace: req2 len=9 #3 t=7 abcd defg".to_string()]
        );
        let many = "r 1 l t\n".repeat(5);
        let t = trace_lines(&many, 3);
        assert_eq!(t.len(), 4);
        assert_eq!(t[3], "hydo-fc-trace: ... 2 more packets not shown");
    }
}
