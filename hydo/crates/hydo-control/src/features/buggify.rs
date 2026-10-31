//! BUGGIFY: the schedule token `B=` for a manifest with `buggify: true`
//! (hydo/onboard/hydo-init-buggify.sh, M56/M80), driven from the host.
//!
//! The shell extension wraps set_knob: the letter B writes its value to /tmp/hydo-buggify (where
//! the SDKs read their BUGGIFY configuration at a program's first fault point), makes it mode
//! 644 and logs it; every other letter goes on to the next set_knob. The controller sends one
//! `write` with mode 0644 at the knob's place in the token order; its answer is not waited for,
//! as the shell does not look at echo's or chmod's status (HANDOFF.md, M120 decision 9). Without
//! `buggify: true` the extension is not in the image, and B= stays an unknown knob.

use super::{log, post, Step, F};
use hydo_agent_proto::msg::{Command, Write};

/// Where the SDKs read the BUGGIFY configuration.
pub const BUGGIFY_FILE: &str = "/tmp/hydo-buggify";

/// The set_knob wrapper's B branch: `echo "$2" > /tmp/hydo-buggify; chmod 644; log`.
pub(super) fn set_buggify(fx: &mut F<'_>, value: &str) -> Step<()> {
    post(
        fx,
        Command::Write(Write {
            path: BUGGIFY_FILE.into(),
            data: format!("{value}\n"),
            append: None,
            mode: Some(0o644),
        }),
    )?;
    log(fx, format!("BUGGIFY {value} ({BUGGIFY_FILE})"));
    Ok(())
}
