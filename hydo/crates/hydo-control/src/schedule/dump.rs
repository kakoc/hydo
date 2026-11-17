//! The stored state for comparisons (Milestone 298, `services[].compare`; docs/design/differential.md).
//!
//! `hydo compare-versions` runs one schedule on two versions of a system and compares what each
//! run stored. A service's `compare:` command prints that state, for example a SQL query or a
//! scan of every key. After the verdict, in the conversation that gives it (the run, or the
//! recovery boot after a power cut), the controller runs each service's command with its output
//! in a file of the guest's work folder, reads at most [`MAX_BYTES`] of the file, and sends each
//! line to the host as a note, `HYDO-STATE <service>: <line>`, then one
//! `HYDO-STATE-END <service> rc=<status> lines=<n> cut=<0|1>`.
//!
//! A note is for people and for the host's tools: no control transcript's hash covers it. The
//! commands run after the verdict, so they change nothing the run decided. Without `compare:`
//! nothing here runs, and a run sends the same commands as before.

use super::{call, env, spawn_wait, timeout_argv, Ctx, CHUNK, F};
use crate::flow::Step;
use hydo_agent_proto::msg::{Command, Read, Reply};

/// The note of one line of a service's state.
pub const NOTE: &str = "HYDO-STATE";
/// The note after a service's last line.
pub const END: &str = "HYDO-STATE-END";
/// The most bytes of one service's output that a run reports.
pub const MAX_BYTES: usize = 64 * 1024;
/// The longest line a note keeps (longer lines are cut, with `...`).
pub const MAX_LINE: usize = 1000;

/// The guest file of a service's output.
fn file(ctx: &Ctx, svc: &str) -> String {
    format!("{}/compare.{svc}.out", ctx.plan.layout.dir)
}

/// The first `max` bytes of a file (chunked reads), and whether the file had more.
fn read_head(fx: &mut F<'_>, path: &str, max: usize) -> Step<(String, bool)> {
    let mut text = String::new();
    loop {
        let left = max.saturating_sub(text.len());
        if left == 0 {
            // one more byte says whether the file goes on
            let r = call(
                fx,
                Command::Read(Read {
                    path: path.into(),
                    offset: Some(text.len() as u64),
                    max: 1,
                    hex: None,
                }),
            )?;
            let more = matches!(r, Ok(Reply::Read(r)) if !r.data.is_empty());
            return Ok((text, more));
        }
        let r = call(
            fx,
            Command::Read(Read {
                path: path.into(),
                offset: (!text.is_empty()).then_some(text.len() as u64),
                max: left.min(CHUNK) as u32,
                hex: None,
            }),
        )?;
        match r {
            Ok(Reply::Read(r)) => {
                let done = r.eof || r.data.is_empty();
                text.push_str(&r.data);
                if done {
                    return Ok((text, false));
                }
            }
            _ => return Ok((text, false)),
        }
    }
}

/// The notes of one service's output (pure: the unit tests' subject).
pub fn notes(svc: &str, out: &str, rc: u32, cut: bool) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for l in out.lines() {
        let l = l.trim_end_matches('\r');
        let l = if l.len() > MAX_LINE {
            let mut i = MAX_LINE - 3;
            while !l.is_char_boundary(i) {
                i -= 1;
            }
            format!("{}...", &l[..i])
        } else {
            l.to_string()
        };
        v.push(format!("{NOTE} {svc}: {l}"));
    }
    let n = v.len();
    v.push(format!("{END} {svc} rc={rc} lines={n} cut={}", cut as u8));
    v
}

/// After the verdict: each service's `compare:` output as `HYDO-STATE` notes.
pub(super) fn collect(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    for (i, s) in ctx.plan.services.iter().enumerate() {
        if s.compare.is_empty() {
            continue;
        }
        let out = file(ctx, &s.name);
        let e = env(fx, ctx, &[("HYDO_SVC", s.name.clone())]);
        let rc = spawn_wait(
            fx,
            &format!("compare:{i}"),
            timeout_argv(ctx, &s.compare),
            e,
            &out,
            false,
        )?;
        let (text, cut) = read_head(fx, &out, MAX_BYTES)?;
        for n in notes(&s.name, &text, rc, cut) {
            fx.note(n);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_line_is_a_note_and_the_end_counts_them() {
        assert_eq!(
            notes("redis", "k:1 v1\nk:2 v2\n", 0, false),
            vec![
                "HYDO-STATE redis: k:1 v1",
                "HYDO-STATE redis: k:2 v2",
                "HYDO-STATE-END redis rc=0 lines=2 cut=0"
            ]
        );
        assert_eq!(
            notes("db", "", 1, true),
            vec!["HYDO-STATE-END db rc=1 lines=0 cut=1"]
        );
        let long = "x".repeat(MAX_LINE + 50);
        let n = notes("db", &long, 0, false);
        assert_eq!(n[0].len(), "HYDO-STATE db: ".len() + MAX_LINE);
        assert!(n[0].ends_with("..."));
    }
}
