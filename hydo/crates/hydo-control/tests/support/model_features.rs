//! The shell model of the init extensions M122 drives, written from the shell text (not from the
//! controller): hydo-liveness.sh, hydo-init.sh's T= branch, hydo-init-clock.sh and
//! hydo-init-buggify.sh. A child of `model`, so it reads the model's state.
//!
//! - The liveness monitors (`live_monitor $i &`) are background jobs, checked each on its own as
//!   the periodic checkers are: the attempt under `timeout <within+1>`, `UP`, the comparison with
//!   the deadline in awk's floating point, `sleep <poll>`. live_resolve, in the main sequence,
//!   waits for each monitor (`wait <pid>`): the monitor's last answer must have come before the
//!   main sequence goes on, and the record is built here from the monitor's own items.
//! - `UP` (`cut -d' ' -f1 /proc/uptime`) is the agent's monotonic clock and `date +%s` its
//!   realtime clock; after `date -s` the reply's time stands for the next `date +%s`, and a
//!   carried value is never written among the ids (the controller's decisions, HANDOFF.md).

use super::{inv_detail, jesc, plain, run_of, s, Item, Model, R};
use hydo_agent_proto::msg::*;

/// A monitor the model started: its invariant, the environment of the init when it forked it,
/// and the anchor's uptime text.
#[derive(Clone, Debug)]
pub struct LiveJob {
    pub i: usize,
    pub env: Vec<String>,
    pub t0: String,
}

/// What a monitor found: `$H/live.res.<i>` and `$H/inv.<i>.out`.
#[derive(Clone, Debug)]
pub struct LiveRes {
    pub r: &'static str,
    pub el: String,
    pub rc: String,
    pub out: String,
}

/// `/proc/uptime`'s first field from a monotonic time in ms ("%lu.%02lu").
fn uptime_text(ms: u64) -> String {
    format!("{}.{:02}", ms / 1000, ms % 1000 / 10)
}

/// awk's number of a string ("" and "?" are 0).
fn awk_num(x: &str) -> f64 {
    x.parse().unwrap_or(0.0)
}

/// The answer a job waits for: a refusal, a run's end, or a reply.
enum Ans {
    Refused(String),
    Done(Done),
    Reply(Reply),
}

impl<'a> Model<'a> {
    fn has_eventually(&self) -> bool {
        self.plan.invariants.iter().any(|v| v.kind == "eventually")
    }

    /// `$(cmd)` of a clock read in the main sequence ("" when it cannot be read).
    fn clock_main(&mut self, mono: bool) -> R<String> {
        let id = self.send(Command::ClockGet(ClockGet {}))?;
        Ok(match self.reply(id)? {
            Ok(Reply::ClockGet(c)) if mono => uptime_text(c.mono_ms),
            Ok(Reply::ClockGet(c)) => c.sec.to_string(),
            _ => String::new(),
        })
    }

    // ------------------------------------------------------------------ hydo-liveness.sh

    /// live_anchor <a>: stamp it (first time only) and start its monitors.
    pub(super) fn live_anchor(&mut self, a: &str) -> R<()> {
        if !self.has_eventually() || self.live_anchors.contains_key(a) {
            return Ok(());
        }
        let up = self.clock_main(true)?;
        self.live_anchors.insert(s(a), up.clone());
        for i in 0..self.plan.invariants.len() {
            let v = &self.plan.invariants[i];
            if v.kind == "eventually" && v.live.as_ref().map(|l| l.after.as_str()) == Some(a) {
                let env = self.env(&[]);
                self.live_jobs.push(LiveJob {
                    i,
                    env,
                    t0: up.clone(),
                });
            }
        }
        self.note(format!("liveness anchor '{a}' at uptime={up}s"))
    }

    /// The step_range wrapper: `[ "$2" = "$HYDO_STEPS" ] && live_anchor heal`.
    pub(super) fn live_after_step_range(&mut self, b: i64) -> R<()> {
        if b.to_string() == self.plan.steps.to_string() {
            self.live_anchor("heal")?;
        }
        Ok(())
    }

    /// The start_all wrapper: `[ "$2" = recovery ] && { live_anchor heal; live_anchor start; }`.
    pub(super) fn live_before_start_all(&mut self, recovery: bool) -> R<()> {
        if recovery {
            self.live_anchor("heal")?;
            self.live_anchor("start")?;
        }
        Ok(())
    }

    /// The answer to command `id` a job waits for, with its item index.
    fn answer(&self, id: u64, is_run: bool) -> Option<(usize, Ans)> {
        match self.replies.get(&id)? {
            (ri, Err(e)) => Some((*ri, Ans::Refused(e.clone()))),
            (_, Ok(_)) if is_run => self
                .dones
                .get(&id)
                .map(|(di, d)| (*di, Ans::Done(d.clone()))),
            (ri, Ok(r)) => Some((*ri, Ans::Reply(r.clone()))),
        }
    }

    /// One monitor's items, walked as `live_monitor` runs: its result and the item index at
    /// which it was known, or None when the conversation ended before.
    pub(super) fn live_walk(&self, job: &LiveJob) -> Result<Option<(LiveRes, usize)>, String> {
        let inv = &self.plan.invariants[job.i];
        let lv = inv.live.as_ref().unwrap();
        let label = format!("live:{}", inv.key);
        let items = self.job_items(&label);
        let pos = |it: &Item| self.items.iter().position(|x| std::ptr::eq(x, it)).unwrap();
        let w = lv.within as f64;
        let mut attempt = vec![s("/bin/busybox"), s("timeout"), (lv.within + 1).to_string()];
        attempt.extend(run_of(&inv.cmd));
        let mut j = 0;
        // The next command of the job: it must be `want`, sent with the monitor's environment.
        // Then its answer: Ok(None) when it came at the end's stop or never (the job must stop
        // there), and the job's next item must come after it.
        let next = |j: &mut usize, want: &Command| -> Result<Option<(usize, Ans)>, String> {
            let Some(it) = items.get(*j) else {
                return Err(format!("{label} stopped before {want:?}"));
            };
            let Item::Send { id, cmd, .. } = it else {
                return Err(format!("{label}: expected {want:?}, got {it:?}"));
            };
            if cmd != want {
                return Err(format!("{label}: expected {want:?}\n  got {cmd:?}"));
            }
            *j += 1;
            let is_run = matches!(want, Command::Run(_));
            match self.answer(*id, is_run) {
                Some((ai, a)) if self.items[ai].stop() < self.end_stop => {
                    if let Some(nx) = items.get(*j) {
                        if pos(nx) < ai {
                            return Err(format!("{label} went on before the answer to {want:?}"));
                        }
                    }
                    Ok(Some((ai, a)))
                }
                _ => Ok(None),
            }
        };
        let run = |argv: Vec<String>, capture: Option<u32>| {
            Command::Run(Run {
                argv,
                env: Some(job.env.clone()),
                cwd: None,
                uid: None,
                gid: None,
                netns: None,
                kill_after_ms: None,
                capture,
            })
        };
        let ended = |j: usize| -> Result<Option<(LiveRes, usize)>, String> {
            if j == items.len() {
                Ok(None)
            } else {
                Err(format!(
                    "{label} went on after an answer the end cut off: {:?}",
                    &items[j..]
                ))
            }
        };
        loop {
            // $BB timeout $((w + 1)) $BB sh -c ". /etc/hydo/prelude.sh; $(iv $i cmd)" > $H/inv.$i.out 2>&1
            let Some((_, a)) = next(&mut j, &run(attempt.clone(), Some(3000)))? else {
                return ended(j);
            };
            let (rc, out) = match a {
                Ans::Refused(e) => (127, e),
                Ans::Done(d) => (d.status, d.out),
                Ans::Reply(_) => unreachable!(),
            };
            // el=$(awk -v a="$t0" -v b="$(UP)" 'BEGIN { printf "%.2f", b - a }')
            let Some((ui, a)) = next(&mut j, &Command::ClockGet(ClockGet {}))? else {
                return ended(j);
            };
            let b = match a {
                Ans::Reply(Reply::ClockGet(c)) => uptime_text(c.mono_ms),
                _ => String::new(),
            };
            let el = format!("{:.2}", awk_num(&b) - awk_num(&job.t0));
            let r = if rc == 0 {
                Some(if awk_num(&el) <= w { "ok" } else { "late" })
            } else if awk_num(&el) > w {
                Some("expired")
            } else {
                None
            };
            if let Some(r) = r {
                if j != items.len() {
                    return Err(format!(
                        "{label} went on after its result: {:?}",
                        &items[j..]
                    ));
                }
                return Ok(Some((
                    LiveRes {
                        r,
                        el,
                        rc: rc.to_string(),
                        out,
                    },
                    ui,
                )));
            }
            // $BB sleep $p
            if next(&mut j, &run(plain(&["sleep", &lv.poll.to_string()]), None))?.is_none() {
                return ended(j);
            }
        }
    }

    /// live_resolve: wait for every monitor, print its record, remember the first violation
    /// (the model's `violated()` reads the first `"ok":false` record, as $H/violated is written
    /// once).
    pub(super) fn live_resolve(&mut self) -> R<()> {
        if !self.has_eventually() {
            return Ok(());
        }
        for i in 0..self.plan.invariants.len() {
            let inv = self.plan.invariants[i].clone();
            if inv.kind != "eventually" {
                continue;
            }
            let lv = inv.live.clone().unwrap();
            let a = lv.after.clone();
            if !self.live_anchors.contains_key(&a) {
                self.live_anchor(&a)?;
            }
            let job = self
                .live_jobs
                .iter()
                .find(|j| j.i == i)
                .cloned()
                .expect("a monitor per anchored invariant");
            // wait $(cat $H/live.pid.$i)
            let (res, at) = match self.live_walk(&job) {
                Ok(Some(x)) => x,
                Ok(None) => return super::bad(format!("live_resolve: monitor {i} never ended")),
                Err(e) => return super::bad(e),
            };
            if at >= self.point() {
                return super::bad(format!(
                    "live_resolve went on before monitor {} ended",
                    inv.key
                ));
            }
            self.live_done.insert(i);
            let det = inv_detail(&res.out);
            let (ok, line) = match res.r {
                "ok" => (
                    true,
                    format!(
                        "satisfied {}s after {a} (within {}s): {det}",
                        res.el, lv.within
                    ),
                ),
                "late" => (
                    false,
                    format!(
                        "satisfied only {}s after {a}, deadline {}s: {det}",
                        res.el, lv.within
                    ),
                ),
                _ => (
                    false,
                    format!(
                        "not satisfied {}s after {a}, deadline {}s (last check rc={}: {det})",
                        res.el, lv.within, res.rc
                    ),
                ),
            };
            let file = format!("{}{line}\n", res.out);
            let d = inv_detail(&file);
            self.own_out.insert(inv.key.clone(), file);
            self.report(format!(
                "HYDO-ASSERT {{\"t\":\"eventually\",\"id\":\"{}\",\"ok\":{ok},\"msg\":\"{}\",\"details\":{{\"after\":\"{a}\",\"elapsed_s\":\"{}\",\"out\":\"{}\",\"within_s\":{}}}}}",
                inv.id,
                jesc(&inv.msg),
                res.el,
                jesc(&d),
                lv.within
            ))?;
            self.note(format!(
                "liveness {}: {} after {}s (deadline {}s after {a})",
                inv.id, res.r, res.el, lv.within
            ))?;
        }
        Ok(())
    }

    /// The monitors live_resolve did not wait for (a power cut, an early verdict): each still
    /// does what the shell's loop does, up to where the conversation ended.
    pub(super) fn live_rest(&self) -> Result<(), String> {
        for job in &self.live_jobs {
            if !self.live_done.contains(&job.i) {
                self.live_walk(job)?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ T= (hydo-init.sh)

    /// The `[ "$p" = "$tat" ]` block of explore_run.
    pub(super) fn clock_jump(&mut self, p: i64, tdelta: &str) -> R<()> {
        let tnow = self.clock_main(false)?;
        let num = |x: &str| x.parse::<i64>().unwrap_or(0);
        let id = self.send(Command::ClockSet(ClockSet {
            sec: Some(num(&tnow) + num(tdelta)),
            step: None,
        }))?;
        let tnew = match self.reply(id)? {
            Ok(Reply::ClockSet(c)) => c.sec.to_string(),
            _ => {
                self.note(s("date -s failed"))?;
                self.clock_main(false)?
            }
        };
        self.note(format!(
            "CLOCK JUMP of {tdelta}s before step {p} (realtime {tnow} -> {tnew})"
        ))?;
        let tdiff = num(&tnew) - num(&tnow) - num(tdelta);
        if tdiff > 5 || tdiff < -5 {
            return self.die(format!(
                "HYDO-ORACLE: ERROR clock jump not applied ({tnow} -> {tnew}, wanted {tdelta})"
            ));
        }
        if num(tdelta) < 0 {
            self.xassert(
                "reachable",
                "fault.clock_back",
                "the realtime clock was stepped backwards",
            )
        } else {
            self.xassert(
                "reachable",
                "fault.clock_fwd",
                "the realtime clock was stepped forwards",
            )
        }
    }

    // ------------------------------------------------------------------ hydo-init-clock.sh

    /// failed_ids on the power-cut path: `echo "clock=$($BB date +%s)" >> $HYDO_ACKED` (the
    /// line the acked line then shows; nothing is written into the guest's file).
    pub(super) fn clock_at_cut(&mut self) -> R<String> {
        if self.plan.clock_after_cut.is_none() {
            return Ok(String::new());
        }
        let d = self.clock_main(false)?;
        Ok(format!("clock={d}\n"))
    }

    /// mount_data in the recovery boot: `c` from the oracle command, the clock set to
    /// c + clock_after_cut.
    pub(super) fn clock_recovery(&mut self, cmd: &str) -> R<()> {
        let Some(after) = self.plan.clock_after_cut else {
            return Ok(());
        };
        // c=$(echo "$CMD" | tr ' ' '\n' | sed -n 's/^clock=//p' | tail -1)
        let c = cmd
            .split(' ')
            .filter_map(|t| t.strip_prefix("clock="))
            .next_back()
            .unwrap_or("");
        // case "$c" in ""|*[!0-9]*) c=$HYDO_CLOCK_START;; esac
        let c: i64 = if !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()) {
            c.parse().unwrap()
        } else {
            self.plan.clock_start
        };
        let id = self.send(Command::ClockSet(ClockSet {
            sec: Some(c + after),
            step: None,
        }))?;
        if let Ok(Reply::ClockSet(r)) = self.reply(id)? {
            self.note(format!(
                "recovery boot: realtime clock set to {} (first boot at the cut: {c}, +{after}s)",
                super::utc(r.sec)
            ))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ hydo-init-buggify.sh

    /// set_knob's B branch: `echo "$2" > /tmp/hydo-buggify; chmod 644; log`.
    pub(super) fn buggify_knob(&mut self, value: &str) -> R<()> {
        self.send(Command::Write(Write {
            path: s("/tmp/hydo-buggify"),
            data: format!("{value}\n"),
            append: None,
            mode: Some(0o644),
        }))?;
        self.note(format!("BUGGIFY {value} (/tmp/hydo-buggify)"))
    }
}
