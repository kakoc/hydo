//! `--control agent` (M121): the command line, and the staging of the agent into a root. The
//! image bytes themselves are checked on the machines that have the build tools (docs/HISTORY.md
//! M121: two builds identical on each architecture; every shell image keeps its recorded hash).

use hydo_build::agent::{self, Control};
use hydo_build::{main_with, Ctx, Io};
use std::path::{Path, PathBuf};

fn redis() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../onboard/examples/redis/hydo.yaml")
        .display()
        .to_string()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let ctx = Ctx::standalone();
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
    let (rc, _, err) = run(&[&m, "--control", "init"]);
    assert_eq!(rc, 2);
    assert_eq!(err, "hydo-build: --control is shell or agent\n");
    let (rc, _, err) = run(&[&m, "--control"]);
    assert_eq!(
        (rc, err.as_str()),
        (2, "hydo-build: --control is shell or agent\n")
    );
    // --print-init: the shell init, unchanged with --control shell; none for an agent image
    let (rc, a, _) = run(&[&m, "--print-init"]);
    let (rc2, b, _) = run(&[&m, "--control", "shell", "--print-init"]);
    assert_eq!((rc, rc2), (0, 0));
    assert_eq!(a, b);
    assert!(a.starts_with("#!"), "{}", &a[..40.min(a.len())]);
    let (rc, out, err) = run(&[&m, "--control", "agent", "--print-init"]);
    assert_eq!((rc, out.as_str()), (1, ""));
    assert!(err.contains("has no shell init"), "{err}");
    // --check is the same for both
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
    // a binary that is not the pinned agent is refused
    std::env::remove_var("HYDO_AGENT_UNPINNED");
    std::env::remove_var("HYDO_AGENT_BIN");
    let msg = match agent::binary(&d, false, &mut io) {
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
    assert_eq!(Control::parse("agent"), Some(Control::Agent));
    let _ = std::fs::remove_dir_all(&d);
}
