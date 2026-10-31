//! Native tests of the agent (Linux): the agent runs as a normal process (`hydo-agent serve
//! --root <scratch>`) on a pipe, as an unprivileged user.
//!
//! Each scenario below is a list of steps: the bytes the host types and how many agent lines
//! follow. Run once, a scenario is recorded as a transcript in `hydo/schema/examples/agent/v1/`
//! (`native-<name>.txt`), but only when the file is missing and `HYDO_BLESS=1` is set: a committed
//! transcript is never rewritten. Every committed native transcript is then replayed twice through
//! a fresh agent each time, and both runs must give exactly the recorded agent lines: the agent
//! adds nothing that differs between runs (no pid, no time, no host path in its output).
//!
//! The agent binary is `$HYDO_AGENT_BIN` (for example the static release build) or Cargo's.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

use hydo_agent_proto::json;
use hydo_agent_proto::transcript::{self, Entry};

fn agent_bin() -> PathBuf {
    std::env::var_os("HYDO_AGENT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hydo-agent")))
}

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/examples/agent/v1")
}

/// The scratch root every scenario starts from: /work and a fake tc in /bin.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("work")).unwrap();
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let tc = d.join("bin/tc");
    std::fs::write(
        &tc,
        "#!/bin/sh\n# a stand-in for the image's pinned tc: the output of `tc -s qdisc show dev lo` with a netem queue\n\
         cat <<'EOF'\nqdisc prio 1: root refcnt 2 bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1\n Sent 2000 bytes 20 pkt (dropped 0, overlimits 0 requeues 0)\n backlog 0b 0p requeues 0\n\
         qdisc netem 40: parent 1:4 limit 1000 delay 400ms\n Sent 700 bytes 7 pkt (dropped 0, overlimits 0 requeues 0)\n backlog 1022b 7p requeues 0\nEOF\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tc, std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    root: PathBuf,
}

impl Running {
    fn start(name: &str) -> Running {
        let root = scratch(name);
        let mut child = Command::new(agent_bin())
            .args(["serve", "--root"])
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env_clear()
            .spawn()
            .expect("start the agent");
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines() {
                let Ok(l) = l else { break };
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Running {
            child,
            stdin,
            lines: rx,
            root,
        }
    }

    fn type_bytes(&mut self, b: &[u8]) {
        self.stdin.write_all(b).unwrap();
        self.stdin.flush().unwrap();
    }

    fn take(&self, n: usize, what: &str) -> Vec<String> {
        (0..n)
            .map(|i| match self.lines.recv_timeout(Duration::from_secs(20)) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("{what}: agent line {} of {n} did not come", i + 1)
                }
                Err(RecvTimeoutError::Disconnected) => panic!("{what}: the agent ended"),
            })
            .collect()
    }

    /// No further line within a short while.
    fn quiet(&self, what: &str) {
        if let Ok(l) = self.lines.recv_timeout(Duration::from_millis(300)) {
            panic!("{what}: an unexpected agent line: {l}");
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A host line from readable JSON: its canonical form and the newline.
fn host(j: &str) -> Vec<u8> {
    let mut b = json::parse(j.as_bytes())
        .unwrap_or_else(|e| panic!("{e}: {j}"))
        .to_canonical()
        .into_bytes();
    b.push(b'\n');
    b
}

/// One step: bytes typed, and how many agent lines follow.
type Step = (Vec<u8>, usize);

fn h(j: &str, n: usize) -> Step {
    (host(j), n)
}

fn raw(b: &[u8], n: usize) -> Step {
    (b.to_vec(), n)
}

const PATH: &str = r#""env":["PATH=/usr/local/bin:/usr/bin:/bin"]"#;

fn scenarios() -> Vec<(&'static str, &'static str, Vec<Step>)> {
    let p = PATH;
    let long = {
        let mut b = vec![b'x'; 4100];
        b.push(b'\n');
        b
    };
    vec![
        (
            "hello-sync-refusals",
            "the hello, a sync, and every way a host line is refused (bad JSON, not an object, no id, unknown op, unknown field, bad value, another version, a repeated id, a too-long line, a partial line dropped by CAN)",
            vec![
                h(r#"{"v":1,"id":1,"op":"sync","nonce":42}"#, 1),
                raw(b"{not json\n", 1),
                raw(b"[1,2]\n", 1),
                h(r#"{"v":1,"op":"syncfs"}"#, 1),
                h(r#"{"v":1,"id":2,"op":"fly"}"#, 1),
                h(r#"{"v":1,"id":3,"op":"syncfs","extra":true}"#, 1),
                h(r#"{"v":1,"id":4,"op":"signal","h":"a","sig":"KIL","tree":true}"#, 1),
                h(r#"{"v":2,"id":5,"op":"syncfs"}"#, 1),
                h(r#"{"v":1,"id":5,"op":"syncfs"}"#, 1),
                h(r#"{"v":1,"id":6,"op":"syncfs"}"#, 1),
                raw(br#"{"v":1,"id":7,"op"#, 0),
                raw(b"\x18{\"id\":7,\"nonce\":8,\"op\":\"sync\",\"v\":1}\n", 2),
                raw(&long, 1),
                h(r#"{"v":1,"id":8,"op":"spawn","h":"a","argv":[]}"#, 1),
                h(r#"{"v":1,"id":9,"op":"signal","h":"nobody","sig":"KILL","tree":true}"#, 1),
                h(r#"{"v":1,"id":10,"op":"untail","name":"x"}"#, 1),
                h(r#"{"v":1,"id":11,"op":"unfreeze"}"#, 1),
                h(r#"{"v":1,"id":12,"op":"clock_set"}"#, 1),
                h(r#"{"v":1,"id":13,"op":"sync","nonce":18446744073709551615}"#, 1),
            ],
        ),
        (
            "files",
            "mkdir, write, append, read as text and as hex, at an offset, a file that is not UTF-8, a missing file, syncfs",
            vec![
                h(r#"{"v":1,"id":1,"op":"mkdir","path":"/work/d/e"}"#, 1),
                h(r#"{"v":1,"id":2,"op":"write","path":"/work/d/e/f.txt","data":"hello\n"}"#, 1),
                h(r#"{"v":1,"id":3,"op":"write","path":"/work/d/e/f.txt","data":"world é\n","append":true}"#, 1),
                h(r#"{"v":1,"id":4,"op":"read","path":"/work/d/e/f.txt","max":100}"#, 1),
                h(r#"{"v":1,"id":5,"op":"read","path":"/work/d/e/f.txt","offset":6,"max":3}"#, 1),
                h(r#"{"v":1,"id":6,"op":"read","path":"/work/d/e/f.txt","max":5,"hex":true}"#, 1),
                h(r#"{"v":1,"id":7,"op":"write","path":"/work/new/dir/secret","data":"s","mode":384}"#, 1),
                h(r#"{"v":1,"id":8,"op":"read","path":"/work/missing","max":10}"#, 1),
                h(&format!(r#"{{"v":1,"id":9,"op":"run","argv":["sh","-c","printf '\\377\\376' > bin.dat"],"cwd":"/work",{p}}}"#), 2),
                h(r#"{"v":1,"id":10,"op":"read","path":"/work/bin.dat","max":10}"#, 1),
                h(r#"{"v":1,"id":11,"op":"read","path":"/work/bin.dat","max":10,"hex":true}"#, 1),
                h(r#"{"v":1,"id":12,"op":"syncfs"}"#, 1),
            ],
        ),
        (
            "processes",
            "spawn a process tree, signal it (STOP with waiting, CONT, KILL), exit events with status and signal, a handle used again, a busy handle, kill_after_ms, the environment, output files, a program that does not exist",
            vec![
                h(&format!(r#"{{"v":1,"id":1,"op":"spawn","h":"svc","argv":["sh","-c","sleep 100 & sleep 100 & echo up > up; wait"],"cwd":"/work","stdout":"/work/svc.log","stderr":"/work/svc.log",{p}}}"#), 1),
                h(&format!(r#"{{"v":1,"id":2,"op":"run","argv":["sh","-c","while [ ! -f up ]; do sleep 0.01; done; echo seen"],"cwd":"/work",{p}}}"#), 2),
                h(r#"{"v":1,"id":3,"op":"signal","h":"svc","sig":"STOP","tree":true,"wait_stopped":true}"#, 1),
                h(r#"{"v":1,"id":4,"op":"signal","h":"svc","sig":"CONT","tree":true}"#, 1),
                h(r#"{"v":1,"id":5,"op":"signal","h":"svc","sig":"KILL","tree":true}"#, 2),
                h(r#"{"v":1,"id":6,"op":"signal","h":"svc","sig":"KILL","tree":true}"#, 1),
                h(&format!(r#"{{"v":1,"id":7,"op":"spawn","h":"svc","argv":["sh","-c","echo second >> svc.log; exit 3"],"cwd":"/work",{p}}}"#), 2),
                h(r#"{"v":1,"id":8,"op":"read","path":"/work/svc.log","max":100}"#, 1),
                h(&format!(r#"{{"v":1,"id":9,"op":"spawn","h":"slow","argv":["sleep","100"],"kill_after_ms":200,{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":10,"op":"spawn","h":"b","argv":["sleep","100"],{p}}}"#), 1),
                h(&format!(r#"{{"v":1,"id":11,"op":"spawn","h":"b","argv":["sleep","100"],{p}}}"#), 1),
                h(r#"{"v":1,"id":12,"op":"signal","h":"b","sig":"TERM","tree":false}"#, 2),
                h(r#"{"v":1,"id":13,"op":"spawn","h":"env","argv":["sh","-c","echo \"$X\" > env.out"],"cwd":"/work","env":["PATH=/usr/bin:/bin","X=hello there"]}"#, 2),
                h(r#"{"v":1,"id":14,"op":"read","path":"/work/env.out","max":100}"#, 1),
                h(&format!(r#"{{"v":1,"id":15,"op":"spawn","h":"x","argv":["no-such-program"],{p}}}"#), 1),
                h(&format!(r#"{{"v":1,"id":16,"op":"spawn","h":"x","argv":["true"],"netns":"/no/such/ns",{p}}}"#), 1),
                h(r#"{"v":1,"id":17,"op":"spawn","h":"x","argv":["true"],"env":["NOEQUALS"]}"#, 1),
            ],
        ),
        (
            "run-probe-qlen",
            "run with output, with a capture limit, with kill_after_ms, and a program that does not exist; probes that succeed at once, reach their deadline, kill a hanging attempt, and end because a process died; qlen",
            vec![
                h(&format!(r#"{{"v":1,"id":1,"op":"run","argv":["sh","-c","echo out; echo err >&2; exit 3"],{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":2,"op":"run","argv":["sh","-c","i=0; while [ $i -lt 100 ]; do echo line$i; i=$((i+1)); done"],"capture":10,{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":3,"op":"run","argv":["sleep","100"],"kill_after_ms":200,{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":4,"op":"run","argv":["no-such-program"],{p}}}"#), 1),
                h(&format!(r#"{{"v":1,"id":5,"op":"probe","argv":["true"],"every_ms":100,"deadline_ms":1000,{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":6,"op":"probe","argv":["false"],"every_ms":400,"deadline_ms":1000,{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":7,"op":"probe","argv":["sleep","100"],"every_ms":100,"deadline_ms":300,{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":8,"op":"spawn","h":"short","argv":["sh","-c","exit 7"],{p}}}"#), 2),
                h(&format!(r#"{{"v":1,"id":9,"op":"probe","argv":["false"],"every_ms":100,"deadline_ms":5000,"alive":"short",{p}}}"#), 2),
                h(r#"{"v":1,"id":10,"op":"qlen","tc":"./bin/tc","dev":"lo","handle":"40:"}"#, 1),
                h(r#"{"v":1,"id":11,"op":"qlen","tc":"./bin/tc","dev":"lo","handle":"50:"}"#, 1),
                h(r#"{"v":1,"id":12,"op":"qlen","tc":"./bin/no-tc","dev":"lo","handle":"40:"}"#, 1),
            ],
        ),
        (
            "tail",
            "tailing a file that does not exist yet, from its start; whole and partial lines, a truncated file, a line cut at 2,000 bytes, untail; a tail from the end; a busy name; a missing directory",
            vec![
                h(r#"{"v":1,"id":1,"op":"tail","name":"log","path":"/work/t.log","from_start":true}"#, 1),
                h(r#"{"v":1,"id":2,"op":"write","path":"/work/t.log","data":"one\ntwo\n"}"#, 3),
                h(r#"{"v":1,"id":3,"op":"write","path":"/work/t.log","data":"thr","append":true}"#, 1),
                h(r#"{"v":1,"id":4,"op":"write","path":"/work/t.log","data":"ee\n","append":true}"#, 2),
                h(r#"{"v":1,"id":5,"op":"write","path":"/work/t.log","data":"new\n"}"#, 2),
                h(&format!(r#"{{"v":1,"id":6,"op":"write","path":"/work/t.log","data":"{}\n","append":true}}"#, "a".repeat(2100)), 2),
                h(r#"{"v":1,"id":7,"op":"tail","name":"log","path":"/work/t.log"}"#, 1),
                h(r#"{"v":1,"id":8,"op":"untail","name":"log"}"#, 1),
                h(r#"{"v":1,"id":9,"op":"tail","name":"end","path":"/work/t.log"}"#, 1),
                h(r#"{"v":1,"id":10,"op":"write","path":"/work/t.log","data":"x\n","append":true}"#, 2),
                h(r#"{"v":1,"id":11,"op":"untail","name":"end"}"#, 1),
                h(r#"{"v":1,"id":12,"op":"tail","name":"gone","path":"/no/such/dir/f"}"#, 1),
            ],
        ),
        (
            "unprivileged",
            "what an agent that is not root cannot do: set the clock, mount, bind a netfilter queue",
            vec![
                h(r#"{"v":1,"id":1,"op":"clock_set","sec":1700000000}"#, 1),
                h(r#"{"v":1,"id":2,"op":"clock_set","step":-5}"#, 1),
                h(r#"{"v":1,"id":3,"op":"mount","src":"none","dst":"/work/m","fstype":"tmpfs","flags":["nosuid","nodev"],"data":"size=1m"}"#, 1),
                h(&format!(r#"{{"v":1,"id":4,"op":"spawn","h":"svc","argv":["sleep","100"],{p}}}"#), 1),
                h(r#"{"v":1,"id":5,"op":"freeze","h":"nobody","queue":7,"port":6379,"dir":"req","n":1,"dur_ms":100}"#, 1),
                h(r#"{"v":1,"id":6,"op":"freeze","h":"svc","queue":7,"dir":"req","n":1,"dur_ms":100}"#, 1),
                h(r#"{"v":1,"id":7,"op":"freeze","h":"svc","queue":7,"port":6379,"dir":"req","n":1,"pattern":"SET","dur_ms":100,"log":"/work/fc.log"}"#, 1),
                h(r#"{"v":1,"id":8,"op":"signal","h":"svc","sig":"KILL","tree":true}"#, 2),
            ],
        ),
    ]
}

/// Runs steps through a fresh agent; returns the transcript entries.
fn run_steps(name: &str, steps: &[Step]) -> Vec<Entry> {
    let mut a = Running::start(name);
    let mut out = Vec::new();
    for l in a.take(1, "hello") {
        out.push(Entry::Agent(l));
    }
    for (i, (bytes, n)) in steps.iter().enumerate() {
        a.type_bytes(bytes);
        out.push(Entry::Host(bytes.clone()));
        for l in a.take(*n, &format!("{name} step {}", i + 1)) {
            out.push(Entry::Agent(l));
        }
    }
    a.quiet(name);
    out
}

/// The steps of a recorded transcript: each host entry and the agent lines after it.
fn steps_of(entries: &[Entry]) -> (usize, Vec<(Vec<u8>, Vec<String>)>) {
    let mut first = 0;
    let mut steps: Vec<(Vec<u8>, Vec<String>)> = Vec::new();
    for e in entries {
        match e {
            Entry::Comment(_) => {}
            Entry::Host(b) => steps.push((b.clone(), Vec::new())),
            Entry::Agent(l) => match steps.last_mut() {
                Some(s) => s.1.push(l.clone()),
                None => first += 1,
            },
        }
    }
    (first, steps)
}

#[test]
fn record_missing_transcripts() {
    let bless = std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1");
    for (name, about, steps) in scenarios() {
        let p = examples().join(format!("native-{name}.txt"));
        if p.exists() || !bless {
            continue;
        }
        let mut entries = vec![
            Entry::Comment(format!("hydo agent transcript, protocol 1: {about}.")),
            Entry::Comment("Recorded from `hydo-agent serve --root <scratch>` on Linux as an unprivileged user (hydo/crates/hydo-agent/tests/native.rs); never edited. replay: native".into()),
        ];
        entries.extend(run_steps(name, &steps));
        transcript::check(&entries).unwrap_or_else(|e| panic!("{name}: {e}"));
        std::fs::write(&p, transcript::write(&entries)).unwrap();
        eprintln!("recorded {}", p.display());
    }
    // The clocks: what clock_get answers depends on when it ran, so this one is recorded once
    // and read (by the protocol crate's tests), not replayed.
    let p = examples().join("native-clock.txt");
    if bless && !p.exists() {
        let mut entries = vec![
            Entry::Comment("hydo agent transcript, protocol 1: reading the realtime and the monotonic clock (the values are those of the recording run).".into()),
            Entry::Comment("Recorded from `hydo-agent serve --root <scratch>` on Linux as an unprivileged user (hydo/crates/hydo-agent/tests/native.rs); never edited. Not replayed: the times differ in every run.".into()),
        ];
        entries.extend(run_steps(
            "clock-record",
            &[h(r#"{"v":1,"id":1,"op":"clock_get"}"#, 1)],
        ));
        transcript::check(&entries).unwrap();
        std::fs::write(&p, transcript::write(&entries)).unwrap();
    }
}

/// A pseudo-terminal: the master as a file and the slave's path.
fn pty() -> (std::fs::File, String) {
    use std::os::fd::FromRawFd;
    unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(m >= 0, "posix_openpt");
        assert_eq!(libc::grantpt(m), 0);
        assert_eq!(libc::unlockpt(m), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(libc::ptsname_r(m, name.as_mut_ptr(), name.len()), 0);
        let path = std::ffi::CStr::from_ptr(name.as_ptr())
            .to_string_lossy()
            .into_owned();
        (std::fs::File::from_raw_fd(m), path)
    }
}

const PID1_STEPS: &str =
    "the agent as the first process of a private PID and mount namespace, on a pty: \
                          the hello says pid1, it mounted /proc for its namespace, a tmpfs mount, \
                          and an orphaned grandchild reaped by the agent (no zombie is left)";

fn pid1_steps() -> Vec<Step> {
    let p = PATH;
    vec![
        h(r#"{"v":1,"id":1,"op":"sync","nonce":1}"#, 1),
        h(
            r#"{"v":1,"id":2,"op":"mount","src":"none","dst":"/tmp/m","fstype":"tmpfs","flags":["nosuid","nodev"],"data":"size=1m"}"#,
            1,
        ),
        h(
            r#"{"v":1,"id":3,"op":"write","path":"/tmp/m/x","data":"in a tmpfs\n"}"#,
            1,
        ),
        h(
            &format!(
                r#"{{"v":1,"id":4,"op":"run","argv":["sh","-c","grep ' /tmp/m ' /proc/mounts | cut -d' ' -f1-3; cat /tmp/m/x; cat /proc/1/comm"],{p}}}"#
            ),
            2,
        ),
        h(
            &format!(
                r#"{{"v":1,"id":5,"op":"spawn","h":"parent","argv":["sh","-c","sleep 0.3 & exit 0"],{p}}}"#
            ),
            2,
        ),
        h(
            &format!(
                r#"{{"v":1,"id":6,"op":"run","argv":["sh","-c","sleep 1; z=0; for f in /proc/[0-9]*/stat; do set -- $(cat $f); [ \"$3\" = Z ] && z=$((z+1)); done; echo zombies=$z"],{p}}}"#
            ),
            2,
        ),
    ]
}

/// Runs the agent as PID 1 (sudo, unshare) on a pty; returns the console lines read after each
/// step, after checking that the agent put the terminal into raw mode without echo.
fn run_pid1(steps: &[Step]) -> Vec<Entry> {
    use std::os::fd::AsRawFd;
    let (master, slave) = pty();
    // A PID 1 never exits, and `unshare --fork` ignores SIGTERM: only SIGKILL of unshare ends the
    // namespace (--kill-child then kills the agent), so the timeout sends SIGKILL.
    let mut child = Command::new("sudo")
        .args([
            "-n",
            "timeout",
            "-s",
            "KILL",
            "10",
            "env",
            &format!("HYDO_AGENT_TTY={slave}"),
            "unshare",
            "--pid",
            "--fork",
            "--mount",
            "--propagation",
            "private",
            "--kill-child",
        ])
        .arg(agent_bin())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("sudo");
    let mut w = master.try_clone().unwrap();
    let (tx, rx) = channel();
    let r = master.try_clone().unwrap();
    std::thread::spawn(move || {
        for l in BufReader::new(r).lines() {
            let Ok(l) = l else { break };
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let take = |n: usize, what: &str| -> Vec<String> {
        (0..n)
            .map(|i| {
                rx.recv_timeout(Duration::from_secs(15))
                    .unwrap_or_else(|_| panic!("{what}: line {} of {n}", i + 1))
            })
            .collect()
    };
    let mut out: Vec<Entry> = take(1, "hello").into_iter().map(Entry::Agent).collect();
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(master.as_raw_fd(), &mut t) }, 0);
    assert_eq!(
        t.c_lflag & (libc::ECHO | libc::ICANON),
        0,
        "the console is not raw"
    );
    for (i, (b, n)) in steps.iter().enumerate() {
        w.write_all(b).unwrap();
        out.push(Entry::Host(b.clone()));
        out.extend(
            take(*n, &format!("pid1 step {}", i + 1))
                .into_iter()
                .map(Entry::Agent),
        );
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "an unexpected line"
    );
    drop(w);
    drop(master);
    let _ = child.wait();
    out
}

#[test]
#[ignore = "needs sudo -n: the agent as PID 1 of a private PID and mount namespace (bare-metal.sh -- cargo test -- --ignored)"]
fn pid1_in_a_namespace() {
    let p = examples().join("pid1-namespace.txt");
    let bless = std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1");
    if bless && !p.exists() {
        let mut entries = vec![
            Entry::Comment(format!("hydo agent transcript, protocol 1: {PID1_STEPS}.")),
            Entry::Comment("Recorded on Linux with `sudo unshare --pid --fork --mount --propagation private hydo-agent`, HYDO_AGENT_TTY = a pty (hydo/crates/hydo-agent/tests/native.rs, pid1_in_a_namespace); never edited. replay: pid1".into()),
        ];
        entries.extend(run_pid1(&pid1_steps()));
        transcript::check(&entries).unwrap();
        std::fs::write(&p, transcript::write(&entries)).unwrap();
    }
    let entries = transcript::parse(
        &std::fs::read_to_string(&p).expect("pid1-namespace.txt (record it with HYDO_BLESS=1)"),
    )
    .unwrap();
    let (_, steps) = steps_of(&entries);
    let want: Vec<&Entry> = entries
        .iter()
        .filter(|e| !matches!(e, Entry::Comment(_)))
        .collect();
    for round in 1..=2 {
        let got = run_pid1(
            &steps
                .iter()
                .map(|(b, l)| (b.clone(), l.len()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(got.iter().collect::<Vec<_>>(), want, "pid1 run {round}");
    }
}

#[test]
fn native_transcripts_replay_byte_for_byte_twice() {
    let mut n = 0;
    let mut files: Vec<_> = std::fs::read_dir(examples())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    for p in files {
        let text = std::fs::read_to_string(&p).unwrap();
        if !text
            .lines()
            .take(5)
            .any(|l| l.starts_with('#') && l.ends_with("replay: native"))
        {
            continue;
        }
        let entries = transcript::parse(&text).unwrap();
        let (first, steps) = steps_of(&entries);
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        for round in 1..=2 {
            let mut a = Running::start(&format!("{name}-{round}"));
            let want_first: Vec<String> = entries
                .iter()
                .filter_map(|e| {
                    if let Entry::Agent(l) = e {
                        Some(l.clone())
                    } else {
                        None
                    }
                })
                .take(first)
                .collect();
            assert_eq!(a.take(first, &name), want_first, "{name}: the hello");
            for (i, (bytes, want)) in steps.iter().enumerate() {
                a.type_bytes(bytes);
                let got = a.take(want.len(), &format!("{name} step {}", i + 1));
                assert_eq!(
                    &got,
                    want,
                    "{name}, run {round}, step {} ({})",
                    i + 1,
                    String::from_utf8_lossy(bytes)
                );
            }
            a.quiet(&name);
        }
        n += 1;
    }
    assert!(
        n >= scenarios().len(),
        "only {n} native transcripts (record them with HYDO_BLESS=1)"
    );
}

#[test]
fn clocks_and_the_hello() {
    let mut a = Running::start("clock");
    let hello = a.take(1, "hello").remove(0);
    assert!(
        hello.contains(r#""ev":"hello""#) && hello.contains(r#""pid1":false"#),
        "{hello}"
    );
    a.type_bytes(&host(r#"{"v":1,"id":1,"op":"clock_get"}"#));
    let l = a.take(1, "clock_get").remove(0);
    let json = l.splitn(4, ' ').nth(3).unwrap();
    let v = json::parse(json.as_bytes()).unwrap();
    let json::Value::Obj(m) = v else { panic!() };
    let real = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i128;
    assert!(
        matches!(m.get("sec"), Some(json::Value::Int(s)) if (s - real).abs() < 5),
        "{l}"
    );
    assert!(matches!(m.get("mono_ms"), Some(json::Value::Int(_))), "{l}");
}

#[test]
fn an_idle_agent_waits_without_waking() {
    // The agent sleeps in poll with no timeout when nothing is armed: over a second it uses no CPU.
    let mut a = Running::start("idle");
    a.take(1, "hello");
    let pid = a.child.id();
    let ticks = |pid: u32| -> u64 {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let rest = &s[s.rfind(')').unwrap() + 2..];
        let f: Vec<&str> = rest.split(' ').collect();
        f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap()
    };
    let t0 = ticks(pid);
    std::thread::sleep(Duration::from_millis(1000));
    assert!(ticks(pid) - t0 <= 1, "the idle agent used CPU time");
    let st = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
    assert!(
        st.is_empty() || st.contains("poll") || st == "0",
        "waiting in {st}"
    );
    a.type_bytes(&host(r#"{"v":1,"id":1,"op":"syncfs"}"#));
    a.take(1, "syncfs");
}
