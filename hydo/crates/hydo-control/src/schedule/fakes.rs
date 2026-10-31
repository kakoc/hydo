//! The fake outside world: `fakes:`, `proxies:`, the `H=` protocol faults and the world carried
//! across a power cut (docs/HISTORY.md M89, M90, M125). This is `hydo/onboard/hydo-init-fakes.sh`
//! driven through hydo-agent's commands: one function per shell function, under the same name,
//! called from the places of `schedule.rs` whose shell functions the extension wraps
//! (`start_all`, `set_knob`, `step_range`, `restart_dead`, `failed_ids`, `final_verdict`), each
//! at the point where the wrapper runs relative to the wrapped function.
//!
//! - **The world.** One `hydo-world serve /etc/hydo/world.json` process (handle `fakes:world`)
//!   serves every fake service and every fault proxy. It starts at the beginning of `start_all`,
//!   before the services, in every boot, and the controller waits until `hydo-world ctl ping`
//!   answers (a `probe` every 100 ms for 30 s, given up when the process exits: the shell's 300
//!   polls 0.1 s apart).
//! - **`H=` tokens** are knobs to the shell's token loop; `set_knob H` remembers each one whose
//!   target the world serves (the first gets the `fault.protocol` declaration). `step_range` cuts
//!   every range of steps at the windows' boundaries inside it and, before each piece, switches
//!   the rules whose window ends or starts there (`hydo-world ctl off <n>` / `ctl on <n> <token>`).
//!   The end of fault injection (`restart_dead`, no cut) heals them all once (`ctl heal`).
//! - **A power cut.** Right after `failed_ids`, the world is frozen and dumped (`ctl dump`, its
//!   output in `$H/world.dump` as the shell writes it). The shell appended the dump to the
//!   acknowledged ids, so the `hydo-explore-acked:` report line still carries it; the hand-over
//!   gives it to the host separately ([`Ending::PowerCut`]'s `world`), and the recovery
//!   conversation writes it into `$H/world.carry` with `write` commands and starts the world from
//!   it (`--carry`). The 3,584-byte limit of the recovery command line is gone; the 3,000-byte
//!   limit of the carried records is hydo-world's own (`carryMax`, a longer world is dumped as
//!   `w:trunc`) and stays as it is.
//! - **The summary.** At the start of `final_verdict` (so after any liveness wait, and in every
//!   boot that reaches a verdict) the report gets what the world saw: `hydo-world ctl summary`,
//!   each line prefixed `hydo-world: `.
//!
//! The places where the shell's behaviour is ambiguous are marked `AMBIGUOUS:` and listed in the
//! crate's HANDOFF.md.

use super::{
    chunks, decl, die_oracle, env, lines, log, post, read_all, run, sh, spawn_wait, write_cmd,
    xassert, Conversation, Ctx, F,
};
use crate::control::Ending;
use crate::flow::{Memo, Step};
use crate::plan::PlanWorld;
use hydo_agent_proto::msg::{Command, Probe, ProbeEnd, Spawn};
use std::collections::BTreeSet;

/// The handle of the world process.
pub(super) const WORLD: &str = "fakes:world";

/// The protocol-fault assertion pf_add declares and pf_point records.
const DECL: (&str, &str, &str) = (
    "reachable",
    "fault.protocol",
    "a protocol fault (H=) was switched on",
);

/// One remembered H= token (a line of `$H/pf.tokens`: `$PF_N $from $to $1`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tok {
    n: u64,
    from: i64,
    to: i64,
    text: String,
}

/// The extension's files in `$H` and its variables, kept on the host.
#[derive(Clone, Debug, Default)]
pub(super) struct Pf {
    /// `$H/world.started`
    started: bool,
    /// `$H/pf.tokens`
    tokens: Vec<Tok>,
    /// `$H/pf.on.<n>`
    on: BTreeSet<u64>,
    /// `$H/pf.done.<n>`
    done: BTreeSet<u64>,
    /// `$H/pf.healed`
    healed: bool,
    /// The recovery boot: the `w:` records the first boot carried (Ending::PowerCut's world).
    pub(super) carried: Vec<String>,
}

fn world(ctx: &Ctx) -> Option<&PlanWorld> {
    ctx.plan.world.as_ref()
}

fn ctl(w: &PlanWorld, args: &[&str]) -> Vec<String> {
    let mut v = vec![w.bin.clone(), "ctl".to_string()];
    v.extend(args.iter().map(|a| a.to_string()));
    v
}

fn path(ctx: &Ctx, name: &str) -> String {
    format!("{}/{name}", ctx.plan.layout.dir)
}

/// A spawn with its own stdout and stderr (the shell's `> out 2> err`).
fn spawn_to(h: &str, argv: Vec<String>, env: Vec<String>, out: &str, err: Option<&str>) -> Command {
    Command::Spawn(Spawn {
        h: h.into(),
        argv,
        env: Some(env),
        cwd: None,
        uid: None,
        gid: None,
        netns: None,
        stdout: Some(out.into()),
        stderr: err.map(str::to_string),
        append: None,
        kill_after_ms: None,
    })
}

/// Whether a carried word is a world record (`grep '^w:'`).
pub(super) fn is_record(word: &str) -> bool {
    word.starts_with("w:")
}

/// A file replaced by `text` (`> file`), in chunks; an empty text still creates the file.
fn write_file(fx: &mut F<'_>, file: &str, text: &str) -> Step<()> {
    let parts = chunks(text);
    post(
        fx,
        write_cmd(file, parts.first().map(String::as_str).unwrap_or(""), false),
    )?;
    for p in parts.iter().skip(1) {
        post(fx, write_cmd(file, p, true))?;
    }
    Ok(())
}

/// pf_world_start [carry file]: start the world once per boot and wait until it answers.
fn pf_world_start(fx: &mut F<'_>, ctx: &Ctx, carry: Option<&str>) -> Step<()> {
    let Some(w) = world(ctx) else { return Ok(()) };
    let first = fx.effect_bool(|h, _| !std::mem::replace(&mut h.pf.started, true));
    if !first {
        return Ok(());
    }
    let out = path(ctx, "world.out");
    // `$BB sh -c "exec $HYDO_WORLD serve /etc/hydo/world.json ${1:+--carry $1}" > out 2>&1 &`
    let text = format!(
        "exec {} serve {} {}",
        w.bin,
        w.config,
        carry.map(|c| format!("--carry {c}")).unwrap_or_default()
    );
    let e = env(fx, ctx, &[]);
    let id = fx.send(spawn_to(WORLD, sh(ctx, text), e.clone(), &out, Some(&out)))?;
    let mut up = false;
    if fx.reply(id)?.is_ok() {
        // `while [ $n -lt 300 ]; do ctl ping && up; kill -0 || break; sleep 0.1; done`
        // (AMBIGUOUS: the agent starts ping k at k x 100 ms, or when ping k-1 ended if later; the
        // shell sleeps 0.1 s after each ping. Both make 300 pings at most when a ping is fast.)
        let pid = fx.send(Command::Probe(Probe {
            argv: ctl(w, &["ping"]),
            env: Some(e),
            cwd: None,
            uid: None,
            gid: None,
            netns: None,
            every_ms: 100,
            deadline_ms: 30_000,
            alive: Some(WORLD.to_string()),
        }))?;
        if fx.reply(pid)?.is_ok() {
            up = fx.probe_done(pid)?.why == ProbeEnd::Ok;
        }
    }
    let text = read_all(fx, &out)?;
    if up {
        // `grep -c '^hydo-world: ' world.out` (AMBIGUOUS: counted on the host from a read; the
        // shell's uptime is left out, as in every log line)
        let n = lines(&text)
            .iter()
            .filter(|l| l.starts_with("hydo-world: "))
            .count();
        log(fx, format!("fake world up: {n} endpoints"));
        return Ok(());
    }
    // `$BB tail -5 $H/world.out` (to the console: notes)
    let ls = lines(&text);
    for l in &ls[ls.len().saturating_sub(5)..] {
        fx.note(l.to_string());
    }
    die_oracle(
        fx,
        "HYDO-ORACLE: ERROR the fake outside world (hydo-world) did not start".into(),
    )
}

/// The start of start_all: in the recovery boot, the world the first boot carried across the
/// power cut goes into `$H/world.carry`; then the world starts (from it, in the recovery boot).
pub(super) fn start_all(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if world(ctx).is_none() {
        return Ok(());
    }
    let mut c = None;
    if let Conversation::Recovery { acked, failed } = &ctx.conv {
        // `grep -h '^w:' $HYDO_ACKED $HYDO_FAILED > world.carry` and `sed -i '/^w:/d'` on both
        // (AMBIGUOUS: the controller never wrote the records into the id files, see
        // main_recovery; the carry is the records of the acknowledged ids, then the carried
        // world, then those of the failed ids -- the shell's order, where the world was
        // appended to the acknowledged ids)
        let carried = fx.effect_text(|h, _| h.pf.carried.join("\n"));
        let recs: Vec<&str> = acked
            .iter()
            .map(String::as_str)
            .filter(|x| is_record(x))
            .chain(carried.split('\n').filter(|x| !x.is_empty()))
            .chain(failed.iter().map(String::as_str).filter(|x| is_record(x)))
            .collect();
        let file = path(ctx, "world.carry");
        let text: String = recs.iter().map(|r| format!("{r}\n")).collect();
        write_file(fx, &file, &text)?;
        log(
            fx,
            format!(
                "recovery boot: the fake world starts from {} carried records",
                recs.len()
            ),
        );
        c = Some(file);
    }
    pf_world_start(fx, ctx, c.as_deref())
}

/// pf_known: whether the world serves an H= target.
fn pf_known(w: &PlanWorld, t: &str) -> bool {
    w.targets.iter().any(|x| x == t)
}

/// set_knob H: pf_add. Ok(false) when the plan has no world (the letter is then an unknown knob).
pub(super) fn set_knob(fx: &mut F<'_>, ctx: &Ctx, letter: &str, value: &str) -> Step<bool> {
    let Some(w) = world(ctx) else {
        return Ok(false);
    };
    if letter != "H" {
        return Ok(false);
    }
    pf_add(fx, w, value)?;
    Ok(true)
}

/// pf_add <value of H=>: remember the token; step_range switches its window.
fn pf_add(fx: &mut F<'_>, w: &PlanWorld, v: &str) -> Step<()> {
    // tgt=${1%%@*}; rest=${1#*@}; rng=${rest%%:*}; from=${rng%-*}; to=${rng#*-}
    let tgt = v.split('@').next().unwrap_or("");
    let rest = v.split_once('@').map(|x| x.1).unwrap_or(v);
    let rng = rest.split(':').next().unwrap_or("");
    let from = rng.rsplit_once('-').map(|x| x.0).unwrap_or(rng);
    let to = rng.split_once('-').map(|x| x.1).unwrap_or(rng);
    let digits = format!("{from}{to}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return die_oracle(
            fx,
            format!("HYDO-ORACLE: ERROR bad protocol fault H={v} (window)"),
        );
    }
    if rest == v || !rest.contains(':') {
        return die_oracle(fx, format!("HYDO-ORACLE: ERROR bad protocol fault H={v}"));
    }
    if !pf_known(w, tgt) {
        log(fx, format!("H: no fake or proxy {tgt}, ignored"));
        return Ok(());
    }
    let (Ok(from), Ok(to)) = (from.parse::<i64>(), to.parse::<i64>()) else {
        return die_oracle(
            fx,
            format!("HYDO-ORACLE: ERROR bad protocol fault H={v} (window)"),
        );
    };
    if fx.effect_bool(|h, _| h.pf.tokens.is_empty()) {
        fx.report(decl(DECL.0, DECL.1, DECL.2));
    }
    let text = v.to_string();
    let n = match fx.effect(|h, _| {
        let n = h.pf.tokens.len() as u64 + 1;
        h.pf.tokens.push(Tok { n, from, to, text });
        Memo::Id(n)
    }) {
        Memo::Id(n) => n,
        m => unreachable!("memo out of step: {m:?}"),
    };
    log(fx, format!("protocol fault {n}: H={v}"));
    Ok(())
}

/// `$H/pf.tokens` as it stands (it only changes in the token loop, before the steps).
fn tokens(fx: &mut F<'_>) -> Vec<Tok> {
    let t = fx.effect_text(|h, _| {
        h.pf.tokens
            .iter()
            .map(|t| format!("{} {} {} {}", t.n, t.from, t.to, t.text))
            .collect::<Vec<_>>()
            .join("\n")
    });
    t.split('\n')
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut w = l.splitn(4, ' ');
            let mut num = || w.next().unwrap_or("").to_string();
            let (n, f, t) = (num(), num(), num());
            Tok {
                n: n.parse().expect("a token number"),
                from: f.parse().expect("a step"),
                to: t.parse().expect("a step"),
                text: w.next().unwrap_or("").to_string(),
            }
        })
        .collect()
}

/// pf_point <step>: switch the H= rules whose window starts or ends before this step.
fn pf_point(fx: &mut F<'_>, ctx: &Ctx, w: &PlanWorld, toks: &[Tok], p: i64) -> Step<()> {
    for t in toks {
        let n = t.n;
        let on = fx.effect_bool(|h, _| h.pf.on.contains(&n));
        if t.to == p && on {
            // `$HYDO_WORLD ctl off $id > /dev/null` (the shell waits for it)
            let e = env(fx, ctx, &[]);
            run(fx, ctl(w, &["off", &n.to_string()]), e, None)?;
            fx.effect(|h, _| {
                h.pf.on.remove(&n);
                Memo::Unit
            });
            log(fx, format!("protocol fault {n} off before step {p}"));
        }
        if t.from == p && t.from < t.to && !fx.effect_bool(|h, _| h.pf.done.contains(&n)) {
            // `out=$($HYDO_WORLD ctl on $id "$tok") || die_oracle ...`
            // (AMBIGUOUS: the agent's output is stdout and stderr together; `$(...)` keeps stdout
            // only, so a ctl that cannot reach the world names the reason in the verdict here)
            let e = env(fx, ctx, &[]);
            let r = run(fx, ctl(w, &["on", &n.to_string(), &t.text]), e, None)?;
            if r.status != 0 {
                return die_oracle(
                    fx,
                    format!(
                        "HYDO-ORACLE: ERROR protocol fault H={}: {}",
                        t.text,
                        r.out.trim_end_matches('\n')
                    ),
                );
            }
            fx.effect(|h, _| {
                h.pf.on.insert(n);
                h.pf.done.insert(n);
                Memo::Unit
            });
            log(
                fx,
                format!("PROTOCOL FAULT {n} on before step {p}: H={}", t.text),
            );
            xassert(fx, DECL.0, DECL.1, DECL.2);
        }
    }
    Ok(())
}

/// step_range [a, b): split at the H= window boundaries inside it; `core` is the wrapped
/// step_range. None when no H= token was remembered (the shell calls the original at once).
pub(super) fn step_range(
    fx: &mut F<'_>,
    ctx: &Ctx,
    a: i64,
    b: i64,
    core: fn(&mut F<'_>, &Ctx, i64, i64) -> Step<()>,
) -> Option<Step<()>> {
    let w = world(ctx)?;
    let toks = tokens(fx);
    if toks.is_empty() {
        return None;
    }
    Some((|| {
        pf_point(fx, ctx, w, &toks, a)?;
        // awk '{ if ($2 > a && $2 < b) print $2; if ($3 > a && $3 < b) print $3 }' | sort -n -u
        let mut ps: Vec<i64> = toks
            .iter()
            .flat_map(|t| [t.from, t.to])
            .filter(|x| *x > a && *x < b)
            .collect();
        ps.sort();
        ps.dedup();
        let mut prev = a;
        for p in ps {
            core(fx, ctx, prev, p)?;
            pf_point(fx, ctx, w, &toks, p)?;
            prev = p;
        }
        core(fx, ctx, prev, b)
    })())
}

/// The start of restart_dead: at the end of fault injection (no cut) every protocol fault is
/// healed, once.
pub(super) fn restart_dead(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let Some(w) = world(ctx) else { return Ok(()) };
    let heal = fx.effect_bool(|h, _| {
        if h.pf.tokens.is_empty() || h.pf.healed {
            return false;
        }
        h.pf.healed = true;
        true
    });
    if heal {
        // `$HYDO_WORLD ctl heal > /dev/null && log "protocol faults healed"`
        let e = env(fx, ctx, &[]);
        if run(fx, ctl(w, &["heal"]), e, None)?.status == 0 {
            log(fx, "protocol faults healed");
        }
    }
    Ok(())
}

/// failed_ids at a power cut: freeze the world and dump its records (the text the shell appends
/// to $HYDO_ACKED; "" without a world).
pub(super) fn failed_ids_at_cut(fx: &mut F<'_>, ctx: &Ctx) -> Step<String> {
    let Some(w) = world(ctx) else {
        return Ok(String::new());
    };
    let file = path(ctx, "world.dump");
    // `$HYDO_WORLD ctl dump > $H/world.dump 2>&1 || { log ...; echo "w:trunc" > $H/world.dump; }`
    let e = env(fx, ctx, &[]);
    let st = spawn_wait(fx, "fakes:dump", ctl(w, &["dump"]), e, &file, false)?;
    let mut text = read_all(fx, &file)?;
    if st != 0 {
        log(
            fx,
            format!(
                "world dump failed: {}",
                text.split('\n').next().unwrap_or("")
            ),
        );
        // AMBIGUOUS: the shell also writes w:trunc into $H/world.dump and appends the dump to
        // $HYDO_ACKED in the guest; nothing in the guest reads either after the cut, so the
        // controller keeps both on the host.
        text = "w:trunc\n".into();
    }
    log(
        fx,
        format!(
            "fake world frozen at the power cut: {} records carried ({} bytes)",
            text.matches('\n').count(),
            text.len()
        ),
    );
    Ok(text)
}

/// The power cut's hand-over for a plan with a world: the acknowledged ids, the failed ids and
/// the carried records, from the text of `$HYDO_ACKED` with the dump appended (the words of the
/// shell's `hydo-explore-acked:` line). Without a world every word is an id, as in the core.
pub(super) fn power_cut(ctx: &Ctx, acked: &str, failed: &str) -> Ending {
    let words = |t: &str| t.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let (world_, acked): (Vec<String>, Vec<String>) = if world(ctx).is_some() {
        words(acked).into_iter().partition(|x| is_record(x))
    } else {
        (Vec::new(), words(acked))
    };
    Ending::PowerCut {
        acked,
        failed: words(failed),
        world: world_,
    }
}

/// The ids main_recovery writes into an id file: without the world records, which start_all
/// carries into the world instead.
pub(super) fn ids_of<'a>(ctx: &Ctx, ids: &'a [String]) -> Vec<&'a String> {
    ids.iter()
        .filter(|x| world(ctx).is_none() || !is_record(x))
        .collect()
}

/// The start of final_verdict: what the outside world saw, one report line per summary line.
pub(super) fn final_verdict(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let Some(w) = world(ctx) else { return Ok(()) };
    // `$HYDO_WORLD ctl summary 2> /dev/null | $BB sed 's/^/hydo-world: /'`
    // (AMBIGUOUS: the summary goes through a file in $H and a read, so that no output limit
    // applies; the shell pipes it to the console)
    let file = path(ctx, "world.summary");
    let e = env(fx, ctx, &[]);
    let id = fx.send(spawn_to(
        "fakes:summary",
        ctl(w, &["summary"]),
        e,
        &file,
        None,
    ))?;
    if fx.reply(id)?.is_ok() {
        fx.exit(id)?;
    }
    let text = read_all(fx, &file)?;
    for l in lines(&text) {
        fx.report(format!("hydo-world: {l}"));
    }
    Ok(())
}

/// The recovery's `command:` log line: the acknowledged ids with the carried records after them
/// (where the shell's line had them), then the failed ids.
pub(super) fn recovery_words(fx: &mut F<'_>, ctx: &Ctx, acked: &[String]) -> Vec<String> {
    let mut v: Vec<String> = acked.to_vec();
    if world(ctx).is_some() {
        let carried = fx.effect_text(|h, _| h.pf.carried.join("\n"));
        v.extend(
            carried
                .split('\n')
                .filter(|x| !x.is_empty())
                .map(str::to_string),
        );
    }
    v
}
