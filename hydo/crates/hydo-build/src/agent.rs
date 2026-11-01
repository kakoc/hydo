//! `--control agent` (Milestone 121; docs/design/guest-control.md, "Migration" step 1): an image
//! whose first process is the guest agent `hydo-agent` (hydo/crates/hydo-agent) instead of the
//! shell init. Such an image carries `/etc/hydo/control` = `agent <protocol version>`, which tells
//! the host to boot it under lock-step control from its first instruction; it gets no `init.rs`
//! splicing and no `/sbin/init` shell file. Everything else (the project's files, busybox, the
//! compiled manifest, the composer's files, the data disk) is staged as for a shell-init image,
//! so the host controller can read the manifest out of the image and the services find their
//! files. An image without `/etc/hydo/control` is a shell-init image (the default, unchanged).
//!
//! The agent's bytes are inside the image, so they are pinned (docs/BUILD-PINS.md, "hydo-agent"):
//! the staged binary must have the sha256 below. `HYDO_AGENT_UNPINNED=1` stages another build
//! (for work on the agent itself) and says so.

use crate::fsx;
use crate::{outln, Fail, Io};
use std::path::Path;

/// sha256 of the pinned x86_64 agent (`hydo/crates/hydo-agent/build.sh x86_64`, M131; M117 `ff7652a1…`).
pub const AGENT_SHA256_X86_64: &str =
    "1f517e04f837658a0f0fb85b0077d29b491d10eca75c2529ba0e67944ad24422";
/// sha256 of the pinned aarch64 agent (`hydo/crates/hydo-agent/build.sh aarch64`, M131; M117 `6a3ea9f8…`).
pub const AGENT_SHA256_ARM64: &str =
    "585f0bdb8cf1d03a3e95d0c8844e85617515939f42d1fe76d02fe8d2c7d72d30";

/// The image's first process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Control {
    /// The shell init (`hydo-init.sh` with its spliced extensions): every image before M121.
    #[default]
    Shell,
    /// hydo-agent, under lock-step control by the host.
    Agent,
}

impl Control {
    /// `--control shell|agent`.
    pub fn parse(s: &str) -> Option<Control> {
        match s {
            "shell" => Some(Control::Shell),
            "agent" => Some(Control::Agent),
            _ => None,
        }
    }
}

/// The text of `/etc/hydo/control` in an agent image.
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
                "hydo-build: {env}={p} has sha256 {sha}; an agent image stages the pinned hydo-agent {pin} \
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
        assert_eq!(Control::parse("agent"), Some(Control::Agent));
        assert_eq!(Control::parse("shell"), Some(Control::Shell));
        assert_eq!(Control::parse("Agent"), None);
        assert_eq!(Control::default(), Control::Shell);
        assert_eq!(control_text(), "agent 1\n");
    }
}
