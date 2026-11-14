//! Performance tests (Milestone 237, `perf:` in the manifest; `hydo guide workflow`).
//!
//! A run is deterministic, so the work its workload does can be measured exactly. The host
//! controller marks where each measured part of the workload starts and ends, as report lines in
//! the control transcript: `HYDO-PERF-MARK start <a> <b>` at the stop where it sends the
//! workload call for steps `[a, b)`, and `HYDO-PERF-MARK end <a> <b> rc=<status>` at the stop
//! where it sees that call's exit. `hydo run` then reads the engine's work count at those two
//! stops: the stop's clock from the transcript (VM exits on KVM and the Mac) and, on hvx, the
//! guest's retired branches from `<tag>.work.jsonl` (`HYDO_WORK_RECORD`).
//!
//! With `perf.steps` (the default) every step is its own workload call, so each step is measured
//! on its own. The workload's own numbers go to the file `$HYDO_PERF`, one `<name> <value>
//! [<unit>]` line each; after the verdict the controller reads the file and reports each line as
//! `HYDO-PERF <line>`.
//!
//! Without `perf:` nothing here runs, and a run sends the same commands as before.

use super::{env, read_all, spawn_wait, timeout_argv, Ctx, F};
use crate::flow::Step;
use crate::plan::PlanPerf;

/// The report line that marks a measured part of the workload.
pub const MARK: &str = "HYDO-PERF-MARK";
/// The report line of one of the workload's own numbers.
pub const RECORD: &str = "HYDO-PERF";
/// The most lines of `$HYDO_PERF` a run reports.
const MAX_LINES: usize = 1000;

/// The guest file the workload writes its own numbers to (`$HYDO_PERF`).
pub(super) fn file(ctx: &Ctx) -> String {
    format!("{}/perf", ctx.plan.layout.dir)
}

/// The workload calls for steps `[a, b)`: one per step with `perf.steps`, else one.
pub fn calls(p: &PlanPerf, a: i64, b: i64) -> Vec<(i64, i64)> {
    if p.steps {
        (a..b).map(|i| (i, i + 1)).collect()
    } else {
        vec![(a, b)]
    }
}

/// The plain workload's steps `[a, b)` of a run with `perf:`: each call between its two marks.
/// The status is the last call's (each call runs whatever the one before it returned, as the
/// steps of one call do).
pub(super) fn steps(fx: &mut F<'_>, ctx: &Ctx, p: &PlanPerf, a: i64, b: i64) -> Step<u32> {
    let mut st = 0;
    for (x, y) in calls(p, a, b) {
        fx.report(format!("{MARK} start {x} {y}"));
        let e = env(
            fx,
            ctx,
            &[
                ("HYDO_FROM", x.to_string()),
                ("HYDO_TO", y.to_string()),
                ("HYDO_PERF", file(ctx)),
            ],
        );
        st = spawn_wait(
            fx,
            "workload",
            timeout_argv(ctx, &ctx.plan.workload_run),
            e,
            &format!("{}/workload.log", ctx.plan.layout.dir),
            true,
        )?;
        fx.report(format!("{MARK} end {x} {y} rc={st}"));
    }
    Ok(st)
}

/// After the verdict: the workload's own numbers (`$HYDO_PERF`) as `HYDO-PERF` report lines.
/// Read after the verdict, the file changes nothing the run decided.
pub(super) fn report(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if ctx.plan.perf.is_none() {
        return Ok(());
    }
    let text = read_all(fx, &file(ctx))?;
    for l in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(MAX_LINES)
    {
        fx.report(format!("{RECORD} {l}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_one_call_each_unless_switched_off() {
        let on = PlanPerf {
            steps: true,
            threshold_bp: 50,
        };
        assert_eq!(calls(&on, 2, 5), vec![(2, 3), (3, 4), (4, 5)]);
        assert_eq!(calls(&on, 3, 3), Vec::<(i64, i64)>::new());
        let off = PlanPerf {
            steps: false,
            threshold_bp: 50,
        };
        assert_eq!(calls(&off, 2, 5), vec![(2, 5)]);
    }
}
