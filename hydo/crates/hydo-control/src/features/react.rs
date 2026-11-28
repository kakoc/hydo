//! Reactive exploration (M139, docs/design/guest-control.md step D2): the schedule token
//! `A=<event>[#<k>][@<step>][+<ms>]:<action>`, which only host control makes possible.
//!
//! A reaction is a thread of the controller, armed before workload step `step` (0 when not
//! written). It watches the guest's console for the first assertion record of `event` whose
//! outcome is the event's (`ok:true` of a `sometimes`/`reachable` assertion, `ok:false` of the
//! others: an `always` that failed, an `unreachable` that was reached) seen after it was armed
//! (with `#<k>`, Milestone 345, the k-th such record), waits `ms` of guest time (a guest `sleep`,
//! as the timers do) and acts:
//!
//! - `kill-<svc>`: SIGKILL the service's tree; it stays down until the heal's restart pass
//!   (restart_dead), as after the stack's timed kill `X=`;
//! - `stop-<svc>-<ms>`: SIGSTOP the tree, sleep, SIGCONT (the pause timer's shape);
//! - `cont-<svc>`: SIGCONT the tree: a `P=` pause healed at the reaction point (heal on no
//!   progress) instead of at its step; the step's own SIGCONT later is a no-op;
//! - `stop-workload-<ms>` (Milestone 296): the same stop of the workload's own process tree;
//! - `clock+<s>`, `clock-<s>` (Milestone 296): step the realtime clock as `T=` does, at the
//!   reaction point instead of before a step, so that the step meets whatever the guest is
//!   doing then (a call of `hydo fuzz --vm`, which reports its start as an event);
//! - `stop-workload/<thread>[#<k>]-<ms>` (Milestone 296): freeze the workload's threads of that
//!   name, as `P=workload/<thread>@...` does (schedule/threads.rs), the others running on;
//! - `kill-workload/<name>[#<k>]` (Milestone 296): SIGKILL the workload's processes of that
//!   name; the workload's other processes go on;
//! - `cut`: a power cut. The reaction only marks it; the main sequence, which waits for the
//!   workload's exit *or* the mark (see [`workload`]), leaves the step loop and takes the power
//!   cut's path: the workload's client and every service are frozen at once, then the ids are
//!   read and the recovery boot follows, as for `C=`.
//!
//! - `powerloss-<svc>` (Milestone 355): the power loss of one service, as `L=` makes it before a
//!   step (schedule/storage.rs): its threads stop, its own file system loses what was not
//!   written back, and it stays down until the heal's restart pass, as after `kill-<svc>`;
//!
//! - `kill-now-<svc>` (Milestone 305): SIGKILL the service's tree at the moment the event's record
//!   is written, before any thread of the service runs again. The kills above act when the host
//!   has seen the record at a stop and the agent has run, and the service's threads run on until
//!   then. This one is decided in the guest: see [`RELAY`].
//!
//! Reactions are live during the workload's steps only: after the last step the main sequence
//! cancels every reaction that has not seen its event and waits for the others to finish, before
//! it heals; a power cut (`C=` or a reaction's) cancels them all.
//!
//! Reproducibility: the decision is a pure function of the schedule string and of the console
//! lines seen at each numbered stop, so a run with reactions is reproduced by its schedule string
//! (the controller decides again, at the same stops) and by its control transcript (the commands
//! the reaction sent are recorded with their stop numbers, `hydo repro --transcript`). A run
//! without `A=` keeps no console lines and takes none of these paths: its transcript is the same
//! byte for byte.

use super::{
    bb, clock, die_oracle, env, log, run, secs, spawn_cmd, storage, svc_sig, threads, timeout_argv,
    xassert, Ctx, Facts, Memo, Step, F,
};
use hydo_agent_proto::msg::Sig;
use hydo_schedule::{React, ReactAction};
use std::collections::{BTreeMap, BTreeSet};

/// The declaration a run with reactions reports (its record when a reaction acts).
pub const REACT_DECL: (&str, &str, &str) = (
    "reachable",
    "fault.reacted",
    "a reaction (A=) saw its event and acted",
);

/// What the reactions share with the main sequence.
#[derive(Clone, Debug, Default)]
pub(super) struct State {
    /// The reactions (indices into the schedule's A= tokens) that saw their event.
    pub fired: BTreeSet<usize>,
    /// The first reaction whose power cut is due (its delay is over).
    pub cut: Option<usize>,
    /// Milestone 355: the number of assertion records seen when each reaction was armed, so that
    /// the end of the steps can say how often a reaction that never fired saw its event.
    pub from: BTreeMap<usize, usize>,
}

/// Milestone 355: the note of a reaction that never fired (`HYDO-REACT-UNFIRED {...}`). A note is
/// for people: no control transcript's hash and no fingerprint covers it.
pub const UNFIRED_PREFIX: &str = "HYDO-REACT-UNFIRED";

/// Milestone 355: remember how many assertion records the run had when reaction `i` was armed
/// (in the main sequence, before its thread starts at the same stop).
pub(super) fn armed_at(fx: &mut F<'_>, i: usize) {
    fx.effect(|h, f| {
        h.react.from.entry(i).or_insert(f.asserts.len());
        Memo::Unit
    });
}

/// Milestone 355: one `HYDO-REACT-UNFIRED {"token":..,"event":..,"nth":k,"at":step,
/// "armed":bool,"seen":n,"cut":bool}` note for each reaction of `reactions` that never acted, at
/// the end of the steps. `seen` counts the event's records after the reaction was armed; `cut`
/// says that a power cut ended the steps. Before, such a run passed without a word (the Kvrocks
/// onboarding, Milestone 351).
pub(super) fn note_unfired(fx: &mut F<'_>, reactions: &[(usize, React)], cut: bool) {
    let notes = fx.effect_text(|h, f| {
        let mut out = Vec::new();
        for (i, r) in reactions {
            if h.react.fired.contains(i) || h.react.cut == Some(*i) {
                continue;
            }
            let armed = h.react.from.get(i).copied();
            let seen = armed.map_or(0, |from| {
                f.asserts.get(from..).map_or(0, |l| {
                    l.iter().filter(|(id, ev)| *ev && *id == r.event).count()
                })
            });
            out.push(unfired_note(r, armed.is_some(), seen as u64, cut));
        }
        out.join("\n")
    });
    for n in notes.lines() {
        fx.note(n.to_string());
    }
}

/// The text of one [`note_unfired`] note.
pub fn unfired_note(r: &React, armed: bool, seen: u64, cut: bool) -> String {
    let v = serde_json::json!({
        "token": format!("A={r}"),
        "event": r.event,
        "nth": r.nth,
        "at": r.at,
        "armed": armed,
        "seen": seen,
        "cut": cut,
    });
    format!("{UNFIRED_PREFIX} {v}")
}

/// An assertion record of the console: (id, whether it is the assertion's event).
pub(super) fn record_of(line: &str) -> Option<(String, bool)> {
    let j = line.strip_prefix("HYDO-ASSERT ")?;
    let v: serde_json::Value = serde_json::from_str(j.trim()).ok()?;
    let id = v.get("id")?.as_str()?;
    let ok = v.get("ok")?.as_bool()?;
    let t = v.get("t").and_then(|t| t.as_str()).unwrap_or("");
    let event = if matches!(t, "sometimes" | "reachable") {
        ok
    } else {
        !ok
    };
    Some((id.to_string(), event))
}

/// The schedule's reactions that the plan can apply, with their index; a reaction naming a
/// service the manifest does not have is ignored with a line, as the other tokens are.
pub(super) fn armed(fx: &mut F<'_>, ctx: &Ctx) -> Vec<(usize, React)> {
    let mut out = Vec::new();
    for (i, r) in ctx.run.a.iter().enumerate() {
        if let Some(s) = r.action.svc() {
            // Milestone 296: `stop-workload-<ms>` stops the workload's own tree
            let workload = s == crate::names::WORKLOAD && matches!(r.action, ReactAction::Stop(..));
            if !workload && ctx.plan.service(s).is_none() {
                log(fx, format!("A: no service {s}, ignored"));
                continue;
            }
        }
        out.push((i, r.clone()));
    }
    out
}

/// Whether a reaction's power cut is due (read once at this point of the main sequence).
pub(super) fn cut_due(fx: &mut F<'_>) -> bool {
    fx.effect_bool(|h, _| h.react.cut.is_some())
}

/// The reaction whose power cut is due, for the cut's log line.
pub(super) fn cut_token(ctx: &Ctx, fx: &mut F<'_>) -> String {
    let i = match fx.effect(|h, _| Memo::Id(h.react.cut.unwrap_or(0) as u64)) {
        Memo::Id(i) => i as usize,
        m => unreachable!("memo out of step: {m:?}"),
    };
    ctx.run
        .a
        .get(i)
        .map(|r| format!("A={r}"))
        .unwrap_or_default()
}

/// Whether reaction `i` saw its event (read once at this point of the main sequence).
pub(super) fn fired(fx: &mut F<'_>, i: usize) -> bool {
    fx.effect_bool(|h, _| h.react.fired.contains(&i))
}

/// The workload call of steps [a, b) when the schedule has reactions: the call is not made when
/// a reaction's power cut is due, and its wait ends at the workload's exit or at the cut. Some
/// status at the exit; None when the cut came first (the client is frozen by the cut's path).
pub(super) fn workload(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<Option<u32>> {
    if cut_due(fx) {
        return Ok(None);
    }
    // Milestone 390: a walk's call gets its walk here too (before, it walked seed 0)
    let extra = super::workload_extra(fx, ctx, a, b)?;
    let e = env(fx, ctx, &extra);
    let id = fx.send(spawn_cmd(
        "workload",
        timeout_argv(ctx, &ctx.plan.workload_run),
        e,
        &format!("{}/workload.log", ctx.plan.layout.dir),
        true,
    ))?;
    if fx.reply(id)?.is_err() {
        return Ok(Some(127));
    }
    fx.until(|h, f| f.exits.contains_key(&id) || h.react.cut.is_some())?;
    let st = match fx.effect(|_, f| match f.exits.get(&id) {
        Some(e) => Memo::Id(e.status as u64),
        None => Memo::Unit,
    }) {
        Memo::Id(s) => Some(s as u32),
        _ => None,
    };
    if st.is_none() {
        log(
            fx,
            format!(
                "workload steps {a}..{} interrupted: a reaction's power cut",
                b - 1
            ),
        );
    }
    Ok(st)
}

/// Milestone 305: the console lines of a `kill-now-<svc>` reaction's relay.
pub(super) const KILLNOW_PREFIX: &str = "HYDO-KILLNOW ";

/// Milestone 305: the relay of a `kill-now-<svc>` reaction, a `busybox sh -c` script that runs at
/// real-time priority (`chrt -f 99`). Its arguments are busybox's path, the reaction's name
/// (`a<i>`), the event's assertion id, the delay in seconds (empty for none), the service's log
/// file and the service's name.
///
/// It puts a FIFO over `/dev/console` and the agent's console device (bind mounts), so a process
/// that opens the console from then on writes into the FIFO. The write wakes the relay at once,
/// in the writer's own system call, with no worker thread of the kernel between them. The relay
/// outranks every normal process, so with one vCPU no other process runs until it blocks again.
/// For each line it decides whether the line is the event's record (the test of [`record_of`],
/// on the record's text), and then hands the line to a forwarder at normal priority (`cat`) that
/// writes it to the console device, through a device node of its own. At the first event's
/// record it sleeps the delay, finds the service's processes, kills them with SIGKILL, and only
/// then forwards the record. The service's processes are the agent's children whose standard
/// output is the service's log file (`[ ... -ef ... ]`), and their descendants. Everything on
/// that path is a builtin of the shell, so no process starts and no disk is read between the
/// record and the kill. The relay reports on the console itself, through its device node:
/// `HYDO-KILLNOW a<i> armed`, `... killed <svc> <processes>`, `... none <svc>` (no process of
/// the service ran), or `... error <why>`. It matches only while `/tmp/hydo-killnow/a<i>/on`
/// exists (the end of the steps removes it), and only once.
pub(super) const RELAY: &str = r#"B=$1 I=$2 E=$3 D=$4 L=$5 S=$6
Q=/tmp/hydo-killnow/$I N=/dev/hydo-killnow-$I
trap '' HUP INT TERM PIPE USR1 USR2
c=ttyS0
read -r cl </proc/cmdline
for w in $cl; do case $w in hydo.tty=*) c=${w#hydo.tty=}; c=${c#/dev/};; esac; done
read -r mm </sys/class/tty/$c/dev || { echo "HYDO-KILLNOW $I error the console $c has no device numbers" >/dev/console; exit 1; }
$B mknod $N c ${mm%:*} ${mm#*:} || exit 1
exec 3>$N
say() { echo "HYDO-KILLNOW $I $*" >&3; }
$B mkdir -p $Q && $B mkfifo $Q/in $Q/out || { say error cannot make the pipes of the relay in $Q; exit 1; }
exec 4<>$Q/in 5<>$Q/out
exec 9<$L || { say error cannot open the log file $L of $S; exit 1; }
$B chrt -o 0 $B cat <$Q/out >&3 &
for f in /dev/console /dev/$c; do
  $B mount -o bind $Q/in $f || { say error cannot put the relay over $f; exit 1; }
  [ $c != console ] || break
done
: >$Q/on
say armed
go=1
while IFS= read -r l <&4; do
  if [ $go = 1 ] && [ -e $Q/on ]; then
    h=0
    case $l in
      "HYDO-ASSERT "*"\"id\":\"$E\""*|"HYDO-ASSERT "*"\"id\": \"$E\""*)
        case $l in
          *'"t":"sometimes"'*|*'"t": "sometimes"'*|*'"t":"reachable"'*|*'"t": "reachable"'*)
            case $l in *'"ok":true'*|*'"ok": true'*) h=1;; esac;;
          *) case $l in *'"ok":false'*|*'"ok": false'*) h=1;; esac;;
        esac;;
    esac
    if [ $h = 1 ]; then
      go=0
      [ -z "$D" ] || $B sleep $D
      a=
      for d in /proc/[0-9]*; do
        read -r s 2>/dev/null <$d/stat || continue
        p=${d#/proc/}; s=${s##*) }; set -- $s
        a="$a $p:$2"
      done
      t=" "
      for e in $a; do
        p=${e%:*}
        [ "${e#*:}" = 1 ] && [ /proc/$p/fd/1 -ef /proc/self/fd/9 ] && t="$t$p "
      done
      if [ "$t" = " " ]; then
        say none $S
      else
        ch=1
        while [ $ch = 1 ]; do
          ch=0
          for e in $a; do
            p=${e%:*}
            case $t in *" $p "*) continue;; esac
            case $t in *" ${e#*:} "*) t="$t$p "; ch=1;; esac
          done
        done
        kill -9 $t 2>/dev/null
        n=0
        for p in $t; do n=$((n+1)); done
        say killed $S $n
      fi
      $B chrt -o -p 0 $$ 2>/dev/null
    fi
  fi
  printf '%s\n' "$l" >&5
done
"#;

/// The relay's script for the `nth` record of the event (Milestone 345: `#<k>`). The first
/// record's script is [`RELAY`] byte for byte, so the runs of earlier schedules send the same
/// command; for a later record the relay counts the event's records and lets the earlier ones
/// pass on as any other line.
pub(super) fn relay_script(nth: u64) -> String {
    if nth <= 1 {
        return RELAY.to_string();
    }
    let s = RELAY.replacen("go=1\n", &format!("go=1 k={nth}\n"), 1);
    s.replacen(
        "    if [ $h = 1 ]; then\n",
        "    [ $h = 0 ] || { k=$((k-1)); [ $k = 0 ] || h=0; }\n    if [ $h = 1 ]; then\n",
        1,
    )
}

/// The relay's directory in the guest (Milestone 305).
fn relay_dir(i: usize) -> String {
    format!("/tmp/hydo-killnow/a{i}")
}

/// What the relay of reaction `i` said last: `armed`, `killed <svc> <n>`, `none <svc>` or
/// `error <why>` (Milestone 305).
fn relay_said(f: &Facts, i: usize) -> Option<String> {
    let me = format!("a{i} ");
    f.killnow
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix(&me).map(str::to_string))
}

/// How many times a kill-now relay killed `svc` in this conversation (Milestone 305). A restart
/// policy's supervisor leaves that exit to the schedule, as it does a `K=` kill's.
pub(super) fn kills_of(f: &Facts, svc: &str) -> usize {
    f.killnow
        .iter()
        .filter(|l| {
            l.split_once(' ').is_some_and(|(_, r)| {
                r.strip_prefix("killed ")
                    .and_then(|r| r.split_once(' '))
                    .is_some_and(|(s, _)| s == svc)
            })
        })
        .count()
}

/// Arm a `kill-now-<svc>` reaction (Milestone 305), in the main sequence before the step it is
/// armed at: start its relay and wait until the relay says that it watches the console. A relay
/// that cannot start ends the run as an oracle error, so that a schedule never passes because
/// its fault could not act.
pub(super) fn arm_now(fx: &mut F<'_>, ctx: &Ctx, i: usize) -> Step<()> {
    let r = ctx.run.a[i].clone();
    let ReactAction::KillNow(svc) = &r.action else {
        return Ok(());
    };
    let e = env(fx, ctx, &[]);
    let delay = if r.delay_ms > 0 {
        secs(r.delay_ms, 3)
    } else {
        String::new()
    };
    let argv = vec![
        bb(ctx),
        "chrt".into(),
        "-f".into(),
        "99".into(),
        bb(ctx),
        "sh".into(),
        "-c".into(),
        relay_script(r.nth),
        "hydo-killnow".into(),
        bb(ctx),
        format!("a{i}"),
        r.event.clone(),
        delay,
        ctx.plan.layout.log_of(svc),
        svc.clone(),
    ];
    let id = fx.send(spawn_cmd(
        &format!("killnow:a{i}"),
        argv,
        e,
        &format!("/tmp/hydo-killnow-a{i}.log"),
        false,
    ))?;
    fx.until(|_, f| {
        relay_said(f, i).is_some()
            || f.exits.contains_key(&id)
            || matches!(f.replies.get(&id), Some(Err(_)))
    })?;
    let said = fx.effect_text(|_, f| relay_said(f, i).unwrap_or_default());
    if said == "armed" {
        log(
            fx,
            format!("REACT A={r}: armed (a real-time relay watches the console)"),
        );
        return Ok(());
    }
    let why = match said.strip_prefix("error ") {
        Some(w) => w.to_string(),
        None => {
            format!("its relay ended before it watched the console (/tmp/hydo-killnow-a{i}.log)")
        }
    };
    die_oracle(fx, format!("HYDO-ORACLE: ERROR A={r}: {why}"))
}

/// The end of a `kill-now-<svc>` reaction that saw no event by the end of the steps (Milestone
/// 305): its relay stops matching and only forwards the console's lines.
pub(super) fn disarm(fx: &mut F<'_>, ctx: &Ctx, i: usize) -> Step<()> {
    if !matches!(ctx.run.a[i].action, ReactAction::KillNow(_)) {
        return Ok(());
    }
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![
            bb(ctx),
            "rm".into(),
            "-f".into(),
            format!("{}/on", relay_dir(i)),
        ],
        e,
        None,
    )?;
    Ok(())
}

/// A `kill-now-<svc>` reaction's thread (Milestone 305): the relay decides in the guest; the
/// host waits for what it says and keeps the books (the reaction fired, the service stays down
/// until the heal).
fn watch_relay(fx: &mut F<'_>, ctx: &Ctx, i: usize, svc: &str) -> Step<()> {
    let r = ctx.run.a[i].clone();
    fx.until(|_, f| relay_said(f, i).is_some_and(|l| l != "armed"))?;
    if fx.is_cancelled() {
        return Ok(());
    }
    let said = fx.effect_text(|h, f| {
        let said = relay_said(f, i).unwrap_or_default();
        h.react.fired.insert(i);
        if said.starts_with("killed ") {
            if let Some(v) = h.services.get_mut(svc) {
                v.killed = true;
            }
        }
        said
    });
    log(fx, format!("REACT A={r}: saw {}", r.event));
    match said
        .strip_prefix("killed ")
        .and_then(|x| x.rsplit_once(' '))
    {
        Some((_, n)) => log(
            fx,
            format!("KILLED {svc} at the event (reaction A={r}, {n} processes)"),
        ),
        None => log(
            fx,
            format!("{svc} was not running at the event (reaction A={r}); nothing killed"),
        ),
    }
    xassert(fx, REACT_DECL.0, REACT_DECL.1, REACT_DECL.2);
    Ok(())
}

/// One reaction's thread (`Part::React(i)`).
pub(super) fn watch(fx: &mut F<'_>, ctx: &Ctx, i: usize) -> Step<()> {
    let r = ctx.run.a[i].clone();
    if let ReactAction::KillNow(s) = &r.action {
        return watch_relay(fx, ctx, i, s);
    }
    // the records seen up to the arming point do not count
    let from = match fx.effect(|_, f| Memo::Id(f.asserts.len() as u64)) {
        Memo::Id(n) => n as usize,
        m => unreachable!("memo out of step: {m:?}"),
    };
    // Milestone 345: the k-th record of the event (`#<k>`; the first without it)
    fx.until(|_, f| {
        f.asserts.get(from..).is_some_and(|l| {
            l.iter().filter(|(id, ev)| *ev && *id == r.event).count() as u64 >= r.nth
        })
    })?;
    if fx.is_cancelled() {
        // cancelled at the stop where its event was seen: the main sequence decided first
        return Ok(());
    }
    fx.effect(|h, _| {
        h.react.fired.insert(i);
        Memo::Unit
    });
    log(fx, format!("REACT A={r}: saw {}", r.event));
    let e = env(fx, ctx, &[]);
    if r.delay_ms > 0 {
        run(
            fx,
            vec![bb(ctx), "sleep".into(), secs(r.delay_ms, 3)],
            e.clone(),
            None,
        )?;
    }
    match &r.action {
        ReactAction::Cut => {
            fx.effect(|h, _| {
                h.react.cut.get_or_insert(i);
                Memo::Unit
            });
            log(fx, format!("REACT A={r}: power cut due"));
        }
        ReactAction::Kill(s) => {
            if ctx.plan.service(s).is_some_and(|x| x.restart.restarts()) {
                // a restart policy's start in progress ends first (M134)
                fx.until(|h, _| !h.starting.contains(s))?;
            }
            svc_sig(fx, s, Sig::Kill)?;
            log(fx, format!("KILLED {s} (reaction A={r})"));
        }
        ReactAction::Stop(s, ms) => {
            svc_sig(fx, s, Sig::Stop)?;
            log(fx, format!("PAUSED {s} (reaction A={r})"));
            run(fx, vec![bb(ctx), "sleep".into(), secs(*ms, 3)], e, None)?;
            svc_sig(fx, s, Sig::Cont)?;
            log(fx, format!("RESUMED {s} (reaction A={r})"));
        }
        ReactAction::Cont(s) => {
            svc_sig(fx, s, Sig::Cont)?;
            log(fx, format!("RESUMED {s} (reaction A={r})"));
        }
        ReactAction::Clock(d) => {
            clock::clock_jump_at(fx, &format!("at the reaction A={r}"), *d)?;
        }
        ReactAction::StopThreads(t, ms) => {
            threads::pause_threads_now(fx, ctx, t, *ms, &format!("reaction A={r}"))?;
        }
        ReactAction::KillProcs(t) => {
            threads::kill_procs_now(fx, ctx, t, &format!("reaction A={r}"))?;
        }
        ReactAction::PowerLoss(s) => {
            // Milestone 355: one service's power loss, as L= makes it before a step; the
            // service stays down until the heal's restart pass, as after kill-<svc>
            if ctx.plan.service(s).is_some_and(|x| x.restart.restarts()) {
                fx.until(|h, _| !h.starting.contains(s))?;
            }
            storage::power_loss(fx, ctx, s, &format!("at the reaction A={r}"), None)?;
        }
        ReactAction::KillNow(_) => unreachable!("watch_relay acts for it"),
    }
    xassert(fx, REACT_DECL.0, REACT_DECL.1, REACT_DECL.2);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{relay_script, RELAY};

    /// Milestone 345: the first record's relay is the script of Milestone 305 byte for byte, and
    /// a later record's relay lets the earlier records pass before it decides as before.
    #[test]
    fn a_relay_for_a_later_record_counts_them() {
        assert_eq!(relay_script(1), RELAY);
        let s = relay_script(3);
        assert_eq!(s.matches("go=1 k=3\n").count(), 1);
        let count = "    [ $h = 0 ] || { k=$((k-1)); [ $k = 0 ] || h=0; }\n";
        assert_eq!(s.matches(count).count(), 1);
        assert!(s.find(count) < s.find("    if [ $h = 1 ]; then\n"));
        // the counting line in a shell: the third matching line is the one that acts
        let sh = format!("k=3\nfor h in 1 0 1 1 1; do\n{count}    printf %s $h\ndone");
        let out = std::process::Command::new("sh")
            .args(["-c", &sh])
            .output()
            .expect("sh runs");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "00010");
    }
}
