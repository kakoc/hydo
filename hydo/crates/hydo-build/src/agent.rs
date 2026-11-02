//! The image's first process: the guest agent `hydo-agent` (hydo/crates/hydo-agent), under
//! lock-step control by the host (Milestone 121; the only kind of image since Milestone 136, step
//! C3 of docs/design/guest-control.md). Every image carries `/etc/hydo/control` = `agent
//! <protocol version>`, which tells the host to boot it under lock-step control from its first
//! instruction. Everything else (the project's files, busybox, the compiled manifest, the
//! composer's files, the data disk) is staged so that the host controller can read the manifest
//! out of the image and the services find their files.
//!
//! Images built before M136 without `/etc/hydo/control` are shell-init images: the engines still
//! run them, hydo-build no longer makes them (the shell init `hydo-init.sh` and its spliced
//! extensions were deleted in M136; `git show d9202e7:hydo/onboard/hydo-init.sh` and
//! `git show d9202e7:hydo/crates/hydo-build/src/init.rs` restore the last versions).
//!
//! The agent's bytes are inside the image, so they are pinned (docs/BUILD-PINS.md, "hydo-agent"):
//! the staged binary must have the sha256 below. `HYDO_AGENT_UNPINNED=1` stages another build
//! (for work on the agent itself) and says so.

use crate::fsx;
use crate::{outln, Fail, Io};
use std::path::Path;

/// sha256 of the pinned x86_64 agent (`hydo/crates/hydo-agent/build.sh x86_64`, hydo-agent 0.3.0 with the host-call port, M140; M127 `b72c5caa…`, M131 `1f517e04…`, M117 `ff7652a1…`).
pub const AGENT_SHA256_X86_64: &str =
    "398f297e7425768a6175e58d27eca00d417fec25f0fb8b48842bcbbb53fdb8a6";
/// sha256 of the pinned aarch64 agent (`hydo/crates/hydo-agent/build.sh aarch64`, hydo-agent 0.3.0, M140; it never takes the port; M127 `340a1eec…`, M131 `585f0bdb…`, M117 `6a3ea9f8…`).
pub const AGENT_SHA256_ARM64: &str =
    "099e54440e04a89798efc5755a73a181c87aa0731f10129bc146f53de9432e87";

/// `--control <kind>`: `agent` is the only kind of image since M136 (the flag stays for the
/// scripts that pass it); `shell` is refused with a message that says why.
pub fn parse_control(s: &str) -> Result<(), String> {
    match s {
        "agent" => Ok(()),
        "shell" => Err(
            "hydo-build: --control shell was removed in Milestone 136: every image's first process is hydo-agent, driven by the host (shell-init images built before keep running)"
                .into(),
        ),
        _ => Err(
            "hydo-build: --control takes agent (the only kind of image since Milestone 136)".into(),
        ),
    }
}

/// The text of `/etc/hydo/control`.
pub fn control_text() -> String {
    format!("agent {}\n", hydo_agent_proto::PROTO)
}

/// The agent binary to stage: `$HYDO_AGENT_BIN` (x86_64) or `$HYDO_AGENT_BIN_ARM64`, else
/// `<build dir>/hydo-agent` or `<build dir>/hydo-agent-arm64` (where build.sh puts them), with
/// the pinned sha256.
pub fn binary(bdir: &Path, arm64: bool, io: &mut Io) -> Result<(String, String), Fail> {
    let (env, name, pin) = if arm64 {
        (
            "HYDO_AGENT_BIN_ARM64",
            "hydo-agent-arm64",
            AGENT_SHA256_ARM64,
        )
    } else {
        ("HYDO_AGENT_BIN", "hydo-agent", AGENT_SHA256_X86_64)
    };
    let p = crate::tool(env, &bdir.join(name).to_string_lossy(), |x| {
        Path::new(x).is_file()
    })
    .map_err(|_| {
        Fail::Exit(format!(
            "hydo-build: {env} not found at {} (set {env}; hydo/crates/hydo-agent/build.sh builds it)",
            std::env::var(env)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| bdir.join(name).display().to_string())
        ))
    })?;
    let sha = fsx::sha(Path::new(&p))?;
    if sha != pin {
        if std::env::var("HYDO_AGENT_UNPINNED").is_ok_and(|v| v == "1") {
            crate::errln(
                io,
                &format!(
                    "hydo-build: warning: {p} is not the pinned hydo-agent (sha256 {}, pinned {}); HYDO_AGENT_UNPINNED=1 stages it anyway",
                    &sha[..16],
                    &pin[..16]
                ),
            );
        } else {
            return Err(Fail::Exit(format!(
                "hydo-build: {env}={p} has sha256 {sha}; an image stages the pinned hydo-agent {pin} \
                 (docs/BUILD-PINS.md \"hydo-agent\"; hydo/crates/hydo-agent/build.sh builds it; \
                 HYDO_AGENT_UNPINNED=1 stages another build)"
            )));
        }
    }
    Ok((p, sha))
}

/// Stage the agent as the image's first process `first` (x86_64: `/sbin/init`, which the
/// switch_root initramfs starts; arm64: `/init`, which the kernel starts) and write
/// `/etc/hydo/control`.
pub fn install(src: &str, sha: &str, root: &Path, first: &str, io: &mut Io) -> Result<(), Fail> {
    let dst = crate::x86::under(root, first);
    if let Some(par) = dst.parent() {
        fsx::makedirs(par)?;
    }
    crate::x86::put_exec(src, &dst)?;
    fsx::makedirs(&root.join("etc/hydo"))?;
    fsx::write_text(&root.join("etc/hydo/control"), &control_text())?;
    outln(
        io,
        &format!(
            "hydo-build: staged hydo-agent as {first} (agent protocol {}, sha256 {})",
            hydo_agent_proto::PROTO,
            &sha[..16]
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_values() {
        assert_eq!(parse_control("agent"), Ok(()));
        assert!(parse_control("shell")
            .unwrap_err()
            .contains("removed in Milestone 136"));
        assert!(parse_control("Agent").is_err());
        assert_eq!(control_text(), "agent 1\n");
    }
}
