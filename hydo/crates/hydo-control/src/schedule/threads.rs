//! A pause of threads of the workload (Milestone 226): `P=workload/<name>[#<k>]@<at>~<off>:<dur>`.
//!
//! Milestone 216's pause of the workload stops every thread of its processes with SIGSTOP. A race
//! between two threads of one process, such as Qdrant's flaky test (the test's thread against the
//! update worker), needs one thread held off while the others run. A signal cannot do that:
//! SIGSTOP, also sent with tgkill to one thread, stops the whole thread group.
//!
//! The guest kernel's cgroup v2 freezer can. In a threaded cgroup (`cgroup.type` `threaded`) the
//! threads of one process may sit in different cgroups, and `cgroup.freeze` stops exactly the
//! threads of its cgroup, at their next return to user space or at their next interruptible
//! sleep, as a signal stop would. The pause is a timer thread of the host controller, as the
//! timed pause of Milestone 46 is, with four guest commands:
//!
//! 1. `busybox sleep <off>`;
//! 2. [`FREEZE`]: find the workload's processes, choose the threads, move each into the threaded
//!    cgroup `hydo-pause` under its own cgroup, and write `1` to its `cgroup.freeze`;
//! 3. `busybox sleep <dur>`;
//! 4. [`THAW`]: write `0` to `cgroup.freeze` and move each thread back.
//!
//! The workload's processes are the session of its first process. The agent starts every spawned
//! process with `setsid`, and the workload's first process is the one whose parent is the agent,
//! that leads its own session, and whose environment has `HYDO_FROM` (the workload's step range;
//! no other command of the controller gets it). Its standard output does not tell: busybox ash
//! redirects a command's output in the shell itself before it starts the command. Every decision
//! is the guest's own: the same schedule finds the same threads and stops them at the same moment,
//! on every engine. Nothing changes in the agent or in an image, and a schedule without such a
//! pause takes no step of this module.

use super::{bb, die_oracle, env, log, run, secs, xassert, Ctx, F};
use crate::flow::Step;
use hydo_schedule::ThreadSel;

/// The most output of the guest commands kept.
const CAPTURE: u32 = 3000;

/// Step 2 as a `busybox sh -c` script. Its arguments are busybox's path, the name pattern, and
/// the index (empty for every matching thread). It prints one line per thread stopped,
/// `T <tid> <cgroup dir> <name>`; when no thread matches, `N <count> <name>` for each name of
/// the workload's threads (or `W` when the workload is not running); and `E <why>` when the
/// freezer refuses, with exit status 1.
pub(super) const FREEZE: &str = r#"B=$1 P=$2 K=$3 r=
for d in /proc/[0-9]*; do
  read -r s 2>/dev/null <$d/stat || continue
  s=${s##*) }; set -- $s
  [ "$2" = 1 ] && [ "$4" = "${d#/proc/}" ] || continue
  $B grep -q -a -e HYDO_FROM= $d/environ 2>/dev/null && { r=$4; break; }
done
[ -n "$r" ] || { echo W; exit 0; }
a=$(for d in /proc/[0-9]*; do
  read -r s 2>/dev/null <$d/stat || continue
  s=${s##*) }; set -- $s
  [ "$4" = "$r" ] || continue
  for t in $d/task/[0-9]*; do read -r c 2>/dev/null <$t/comm && echo "${t##*/} $c"; done
done | $B sort -n)
m=$(echo "$a" | while read -r t c; do [ -n "$t" ] && case $c in $P) echo "$t $c";; esac; done)
[ -z "$K" ] || m=$(echo "$m" | $B sed -n "${K}p")
if [ -z "$m" ]; then
  echo "$a" | while read -r t c; do [ -n "$t" ] && echo "$c"; done | $B sort | $B uniq -c |
    while read -r n c; do echo "N $n $c"; done
  exit 0
fi
G=/sys/fs/cgroup
$B grep -q -w cgroup2 /proc/filesystems && [ -d $G ] || { echo "E this guest kernel has no cgroup v2, whose freezer stops the threads"; exit 1; }
[ -f $G/cgroup.controllers ] || $B mount -t cgroup2 none $G 2>/dev/null || { echo "E cannot mount cgroup2 on $G"; exit 1; }
while read -r t c; do
  g=/
  while read -r l; do case $l in 0::*) g=${l#0::};; esac; done 2>/dev/null </proc/$t/cgroup
  D=$G${g%/}/hydo-pause
  [ -d $D ] || $B mkdir $D || { echo "E cannot make the cgroup $D"; exit 1; }
  read -r y <$D/cgroup.type
  [ "$y" = threaded ] || echo threaded >$D/cgroup.type || { echo "E the kernel refused to make $D threaded (cgroup.type)"; exit 1; }
  echo $t >$D/cgroup.threads || { echo "E the kernel refused to move the thread $t ($c) into $D"; exit 1; }
  echo 1 >$D/cgroup.freeze || { echo "E the kernel refused to freeze $D"; exit 1; }
  echo "T $t $D $c"
done <<EOF
$m
EOF
"#;

/// Step 4 as a `busybox sh -c` script; its arguments are pairs of a cgroup dir and a thread id
/// from [`FREEZE`]'s `T` lines. A thread that exited meanwhile cannot be moved back, which is
/// not an error.
pub(super) const THAW: &str = r#"while [ $# -gt 1 ]; do
  echo 0 >$1/cgroup.freeze || { echo "E the kernel refused to thaw $1"; exit 1; }
  echo $2 >${1%/hydo-pause}/cgroup.threads 2>/dev/null
  shift 2
done
"#;

/// A thread that [`FREEZE`] stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stopped {
    pub tid: u64,
    pub dir: String,
    pub name: String,
}

/// What [`FREEZE`] printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Frozen {
    /// The threads stopped, in the order of their thread ids.
    Threads(Vec<Stopped>),
    /// No thread matched; the names of the workload's threads, with how many have each.
    NoMatch(Vec<(u64, String)>),
    /// The workload was not running.
    NoWorkload,
    /// The freezer refused (the message).
    Refused(String),
}

/// Read [`FREEZE`]'s output.
pub(super) fn read_frozen(status: u32, out: &str) -> Frozen {
    let mut threads = Vec::new();
    let mut names = Vec::new();
    for l in out.lines() {
        if let Some(e) = l.strip_prefix("E ") {
            return Frozen::Refused(e.to_string());
        }
        if l == "W" {
            return Frozen::NoWorkload;
        }
        if let Some(r) = l.strip_prefix("T ") {
            let mut f = r.splitn(3, ' ');
            if let (Some(t), Some(d), Some(n)) = (f.next(), f.next(), f.next()) {
                if let Ok(tid) = t.parse() {
                    threads.push(Stopped {
                        tid,
                        dir: d.to_string(),
                        name: n.to_string(),
                    });
                    continue;
                }
            }
        }
        if let Some(r) = l.strip_prefix("N ") {
            if let Some((n, name)) = r.split_once(' ') {
                if let Ok(n) = n.parse() {
                    names.push((n, name.to_string()));
                    continue;
                }
            }
        }
        if !l.trim().is_empty() {
            return Frozen::Refused(format!("unexpected output {l:?}"));
        }
    }
    if status != 0 {
        return Frozen::Refused(format!("exit status {status}"));
    }
    if threads.is_empty() {
        Frozen::NoMatch(names)
    } else {
        Frozen::Threads(threads)
    }
}

/// The timer of `P=workload/<sel>@<at>~<off>:<dur>`: after `off` ms of guest time, stop the
/// threads `sel` names, for `dur` ms.
pub(super) fn pause_threads(
    fx: &mut F<'_>,
    ctx: &Ctx,
    sel: &ThreadSel,
    off: u64,
    dur: u64,
) -> Step<()> {
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![bb(ctx), "sleep".into(), secs(off, 6)],
        e.clone(),
        None,
    )?;
    let what = format!("workload/{sel}");
    let argv = vec![
        bb(ctx),
        "sh".into(),
        "-c".into(),
        FREEZE.into(),
        "hydo-pause".into(),
        bb(ctx),
        sel.name.clone(),
        sel.nth.map(|k| k.to_string()).unwrap_or_default(),
    ];
    let r = run(fx, argv, e.clone(), Some(CAPTURE))?;
    let stopped = match read_frozen(r.status, &r.out) {
        Frozen::Threads(t) => t,
        Frozen::NoMatch(names) => {
            let seen: Vec<String> = names
                .iter()
                .map(|(n, s)| {
                    if *n == 1 {
                        s.clone()
                    } else {
                        format!("{s} ({n})")
                    }
                })
                .collect();
            log(
                fx,
                format!(
                    "PAUSED nothing: no thread of the workload matches {what} (timer; its \
                     threads: {})",
                    seen.join(", ")
                ),
            );
            return Ok(());
        }
        Frozen::NoWorkload => {
            log(
                fx,
                format!("PAUSED nothing: the workload was not running for {what} (timer)"),
            );
            return Ok(());
        }
        Frozen::Refused(why) => {
            return die_oracle(fx, format!("HYDO-ORACLE: ERROR P={what}: {why}"));
        }
    };
    let list: Vec<String> = stopped
        .iter()
        .map(|s| format!("{} {}", s.tid, s.name))
        .collect();
    log(
        fx,
        format!("PAUSED {what} (timer, threads {})", list.join(", ")),
    );
    xassert(
        fx,
        "reachable",
        "fault.paused",
        "a service was paused (SIGSTOP)",
    );
    run(
        fx,
        vec![bb(ctx), "sleep".into(), secs(dur, 3)],
        e.clone(),
        None,
    )?;
    let mut argv = vec![
        bb(ctx),
        "sh".into(),
        "-c".into(),
        THAW.into(),
        "hydo-pause".into(),
    ];
    for s in &stopped {
        argv.push(s.dir.clone());
        argv.push(s.tid.to_string());
    }
    let r = run(fx, argv, e, Some(CAPTURE))?;
    if r.status != 0 {
        let why = r
            .out
            .lines()
            .find_map(|l| l.strip_prefix("E "))
            .unwrap_or("");
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR P={what}: {why} (exit status {})",
                r.status
            ),
        );
    }
    log(fx, format!("RESUMED {what} (timer)"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_freeze_output_is_read() {
        assert_eq!(
            read_frozen(
                0,
                "T 212 /sys/fs/cgroup/hydo-pause tests::wal_reco\nT 230 /sys/fs/cgroup/hydo-pause a b\n"
            ),
            Frozen::Threads(vec![
                Stopped {
                    tid: 212,
                    dir: "/sys/fs/cgroup/hydo-pause".into(),
                    name: "tests::wal_reco".into()
                },
                Stopped {
                    tid: 230,
                    dir: "/sys/fs/cgroup/hydo-pause".into(),
                    name: "a b".into()
                },
            ])
        );
        assert_eq!(
            read_frozen(0, "N 1 collection.test\nN 4 tokio-runtime-w\n"),
            Frozen::NoMatch(vec![
                (1, "collection.test".into()),
                (4, "tokio-runtime-w".into())
            ])
        );
        assert_eq!(read_frozen(0, "W\n"), Frozen::NoWorkload);
        assert_eq!(
            read_frozen(1, "E the kernel refused to freeze /x\n"),
            Frozen::Refused("the kernel refused to freeze /x".into())
        );
        assert!(matches!(read_frozen(1, ""), Frozen::Refused(_)));
        assert!(matches!(read_frozen(0, "sh: oops\n"), Frozen::Refused(_)));
    }
}
