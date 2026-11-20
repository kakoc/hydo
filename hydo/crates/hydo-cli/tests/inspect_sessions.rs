//! `hydo inspect stop --all` (Milestone 328). A user's `hydo inspect list` showed old sessions,
//! and a script that lost a session's id left the session holding a guest's memory. Stand-in
//! sessions here: a process whose command line names `__serve <id>`, as a session's does, and a
//! session whose process number now belongs to another program. No VM runs.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

struct T {
    dir: PathBuf,
    kids: Vec<Child>,
}

impl T {
    fn new() -> T {
        let dir = std::env::temp_dir().join(format!("hydo-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home/inspect")).unwrap();
        T {
            dir,
            kids: Vec::new(),
        }
    }

    /// A session directory whose `session.json` names `pid`.
    fn session(&self, id: &str, pid: u32, ram_mib: u64) {
        let d = self.dir.join("home/inspect").join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("request.json"),
            format!("{{\"run_dir\": \"/nowhere\", \"at\": \"run:9\", \"ram_mib\": {ram_mib}}}"),
        )
        .unwrap();
        std::fs::write(
            d.join("session.json"),
            format!(
                "{{\"pid\": {pid}, \"session\": {{\"id\": \"{id}\", \"run_dir\": \"/nowhere\", \
                 \"at\": {{\"point\": \"run:9\", \"conversation\": \"run\", \"stop\": 9, \
                 \"at\": 0}}, \"state\": \"snapshot\", \
                 \"status\": \"ready\", \"idle_timeout_s\": 300, \"engine\": \"hvm\", \"calls\": 0}}}}"
            ),
        )
        .unwrap();
    }

    /// A stand-in session process: a shell whose command line names `__serve <id>`, in a process
    /// group of its own (as `hydo inspect start` makes it).
    fn serve(&mut self, id: &str) -> u32 {
        use std::os::unix::process::CommandExt;
        let c = Command::new("/bin/sh")
            .args(["-c", "sleep 60; true", "__serve", id])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = c.id();
        self.kids.push(c);
        pid
    }

    fn inspect(&self, args: &[&str]) -> (i32, Value) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .arg("inspect")
            .args(args)
            .env("HYDO_HOME", self.dir.join("home"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        let v = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
        (o.status.code().unwrap_or(-1), v)
    }

    fn reap(&mut self) -> Vec<bool> {
        self.kids
            .iter_mut()
            .map(|c| {
                // ended by the signal: at most a few seconds
                for _ in 0..100 {
                    if let Ok(Some(_)) = c.try_wait() {
                        return true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                let _ = c.kill();
                let _ = c.wait();
                false
            })
            .collect()
    }
}

impl Drop for T {
    fn drop(&mut self) {
        for c in &mut self.kids {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ids(v: &Value) -> Vec<String> {
    v["sessions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| s["id"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn stop_all_stops_every_running_session_and_says_how_many() {
    let mut t = T::new();
    let a = t.serve("i-0000000a");
    let b = t.serve("i-0000000b");
    t.session("i-0000000a", a, 6144);
    t.session("i-0000000b", b, 1024);
    // an ended session whose process number is now this test's own: not a session, never
    // signalled
    t.session("i-0000000c", std::process::id(), 2048);
    let (rc, v) = t.inspect(&["stop", "--all"]);
    assert_eq!(rc, 0, "{v}");
    assert_eq!(v["op"], "stop");
    assert_eq!(ids(&v), ["i-0000000a", "i-0000000b"], "{v}");
    assert_eq!(
        v["notes"][0], "stopped 2 sessions, which held 7168 MiB of guest memory",
        "{v}"
    );
    assert_eq!(t.reap(), [true, true]);
    // nothing runs any more
    let (rc, v) = t.inspect(&["stop", "--all"]);
    assert_eq!(rc, 0, "{v}");
    assert_eq!(v["notes"][0], "no session was running", "{v}");
    let (rc, v) = t.inspect(&["stop", "--all", "i-0000000a"]);
    assert_eq!((rc, v["code"].as_str()), (2, Some("usage")), "{v}");
    assert!(Path::new(&t.dir).exists());
}
