//! The image's first process (M121; the only kind of image since M136): the command line, and
//! the staging of the agent into a root. The image bytes themselves are checked on the machines
//! that have the build tools (docs/HISTORY.md M121 and M136: two builds identical on each
//! architecture).

use hydo_build::agent;
use hydo_build::{main_with, Ctx, Io};
use std::path::{Path, PathBuf};

fn redis() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../onboard/examples/redis/hydo.yaml")
        .display()
        .to_string()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let ctx = Ctx::standalone(Default::default());
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let rc = {
        let mut io = Io {
            out: &mut o,
            err: &mut e,
        };
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        main_with(&ctx, &argv, &mut io)
    };
    (
        rc,
        String::from_utf8(o).unwrap(),
        String::from_utf8(e).unwrap(),
    )
}

#[test]
fn control_flag() {
    let m = redis();
    let only = "hydo-build: --control takes agent (the only kind of image since Milestone 136)\n";
    let (rc, _, err) = run(&[&m, "--control", "init"]);
    assert_eq!((rc, err.as_str()), (2, only));
    let (rc, _, err) = run(&[&m, "--control"]);
    assert_eq!((rc, err.as_str()), (2, only));
    // shell images are gone (M136): --control shell and --print-init say why
    let (rc, out, err) = run(&[&m, "--control", "shell", "--check"]);
    assert_eq!((rc, out.as_str()), (2, ""));
    assert!(
        err.contains("--control shell was removed in Milestone 136"),
        "{err}"
    );
    let (rc, out, err) = run(&[&m, "--print-init"]);
    assert_eq!((rc, out.as_str()), (2, ""));
    assert!(
        err.contains("--print-init was removed in Milestone 136"),
        "{err}"
    );
    // --control agent is accepted and changes nothing
    let (rc, a, _) = run(&[&m, "--check"]);
    let (rc2, b, _) = run(&[&m, "--control", "agent", "--check"]);
    assert_eq!((rc, rc2), (0, 0));
    assert_eq!(a, b);
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-build-control-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The pins are hydo-agent 0.3.0's (Milestone 140) and docs/BUILD-PINS.md's "Pinned" table
/// lists them: a new agent changes the constants and the table together.
#[test]
fn the_pins_are_the_documented_ones() {
    assert_eq!(
        agent::AGENT_SHA256_X86_64,
        "398f297e7425768a6175e58d27eca00d417fec25f0fb8b48842bcbbb53fdb8a6"
    );
    assert_eq!(
        agent::AGENT_SHA256_ARM64,
        "099e54440e04a89798efc5755a73a181c87aa0731f10129bc146f53de9432e87"
    );
    let doc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/BUILD-PINS.md");
    let doc = std::fs::read_to_string(&doc).unwrap();
    let (_, pinned) = doc.split_once("- **Pinned (").expect("no Pinned entry");
    let (table, _) = pinned.split_once("Before (").unwrap();
    assert!(table.contains(agent::AGENT_SHA256_X86_64), "{table}");
    assert!(table.contains(agent::AGENT_SHA256_ARM64), "{table}");
}

#[test]
fn staging_and_the_pin() {
    let d = scratch("stage");
    let bin = d.join("hydo-agent");
    std::fs::write(&bin, b"not the agent").unwrap();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let mut io = Io {
        out: &mut o,
        err: &mut e,
    };
    // a binary that is not the pinned agent is refused (no HYDO_AGENT_* configured)
    let ctx = Ctx::standalone(Default::default());
    let msg = match agent::binary(&ctx, &d, false, &mut io) {
        Err(hydo_build::Fail::Exit(m)) => m,
        _ => panic!("an unpinned agent was accepted"),
    };
    assert!(msg.contains(agent::AGENT_SHA256_X86_64), "{msg}");
    // staged as the first process, with the control file
    let root = d.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let sha = "0".repeat(64);
    agent::install(&bin.to_string_lossy(), &sha, &root, "/sbin/init", &mut io).unwrap();
    assert_eq!(
        std::fs::read(root.join("sbin/init")).unwrap(),
        b"not the agent"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(root.join("sbin/init"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(
        std::fs::read_to_string(root.join("etc/hydo/control")).unwrap(),
        "agent 1\n"
    );
    assert_eq!(
        String::from_utf8(o).unwrap(),
        "hydo-build: staged hydo-agent as /sbin/init (agent protocol 1, sha256 0000000000000000)\n"
    );
    assert_eq!(agent::parse_control("agent"), Ok(()));
    let _ = std::fs::remove_dir_all(&d);
}
