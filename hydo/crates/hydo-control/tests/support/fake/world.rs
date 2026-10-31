//! The scripted agent's fake outside world: what `hydo-world` does for the controller's commands
//! (M89, M125). `serve` starts the world (rarely it cannot: its port is taken), `--carry` loads
//! the records a power cut carried, `ctl on|off|heal` switch rules (rarely a rule is refused),
//! `ctl dump` freezes it and prints its records as `w:` tokens (rarely too many: `w:trunc`), and
//! `ctl summary` prints one line per record. Every answer is a pure function of the salt, the
//! command and the world's state, like the rest of the fake.

use super::{Fake, Msg};
use hydo_agent_proto::msg::{Event, Exit, Spawn};
use std::collections::BTreeSet;

/// The program the controller runs (manifest.sh's HYDO_WORLD).
pub const BIN: &str = "/bin/hydo-world";

const DIAL: &str =
    "hydo-world ctl: dial unix /tmp/hydo/world.sock: connect: no such file or directory\n";

#[derive(Clone, Debug, Default)]
pub struct FakeWorld {
    /// The records the world started from (`--carry`).
    carried: Vec<String>,
    /// Rules switched on (`ctl on <n>`), and every rule ever switched on.
    on: BTreeSet<u64>,
    ever: BTreeSet<u64>,
    /// `ctl dump` was answered (the world is frozen).
    frozen: bool,
}

impl Fake {
    fn world_up(&self) -> bool {
        self.world
            .procs
            .get("fakes:world")
            .is_some_and(|p| p.running && !p.dying)
    }

    /// The records the world holds now: the carried ones, then one per acknowledged id (the
    /// workload's requests) and per rule switched on.
    fn world_records(&self) -> Vec<String> {
        let mut v = self.world.fw.carried.clone();
        let base = v.len() as u64;
        let acked = self
            .world
            .files
            .get("/tmp/hydo/acked")
            .map(|a| a.lines().count() as u64)
            .unwrap_or(0);
        let k = acked % 7 + self.world.fw.ever.len() as u64;
        for i in 0..k {
            v.push(format!("w:{},e,pay,charge,o{i},k{}", base + i + 1, i % 3));
        }
        v
    }

    fn exit_now(&mut self, h: &str, id: u64, status: u32) {
        let at = self.t + 4;
        self.emit(
            at,
            Msg::Event(Event::Exit(Exit {
                h: h.into(),
                id,
                status,
                sig: None,
                timed_out: None,
            })),
        );
    }

    /// A process under a `fakes:` handle: the world itself, or a `ctl` call with its output in a
    /// file.
    pub(super) fn world_spawn(&mut self, h: &str, id: u64, s: &Spawn) {
        let out = s.stdout.clone().unwrap_or_default();
        let to_out = s.stderr.as_deref() == Some(out.as_str());
        match h {
            "fakes:world" => {
                let script = s.argv.get(3).cloned().unwrap_or_default();
                let carry = script
                    .split_once("--carry ")
                    .map(|x| x.1.trim().to_string());
                self.world.fw = FakeWorld::default();
                if let Some(c) = carry {
                    let text = self.world.files.get(&c).cloned().unwrap_or_default();
                    self.world.fw.carried = text
                        .split_whitespace()
                        .filter(|w| w.starts_with("w:") && *w != "w:trunc")
                        .map(str::to_string)
                        .collect();
                }
                let fails = self.h(&["world", "fails"]).is_multiple_of(31);
                let n = 1 + self.h(&["world", "endpoints"]) % 3;
                let mut text = String::new();
                if fails {
                    text.push_str("hydo-world: payments: listen tcp 127.0.0.1:8081: bind: address already in use\n");
                    if let Some(p) = self.world.procs.get_mut(h) {
                        p.dying = true;
                    }
                    self.exit_now(h, id, 1);
                } else {
                    for i in 0..n {
                        text.push_str(&format!("hydo-world: ep{i} on 127.0.0.1:{}\n", 8080 + i));
                    }
                    text.push_str("world: serving\n");
                }
                self.world.files.insert(out, text);
            }
            "fakes:dump" => {
                let (text, st) = if !self.world_up() {
                    (DIAL.to_string(), 1)
                } else if self.h(&["dump", "long"]).is_multiple_of(9) {
                    self.world.fw.frozen = true;
                    ("w:trunc\n".to_string(), 0)
                } else {
                    self.world.fw.frozen = true;
                    let r: String = self
                        .world_records()
                        .iter()
                        .map(|x| format!("{x}\n"))
                        .collect();
                    (r, 0)
                };
                let text = if to_out || st == 0 {
                    text
                } else {
                    String::new()
                };
                self.world.files.insert(out, text);
                self.exit_now(h, id, st);
            }
            "fakes:summary" => {
                let (text, st) = if !self.world_up() {
                    // the dial error goes to stderr, which is /dev/null here
                    let e = if to_out {
                        DIAL.to_string()
                    } else {
                        String::new()
                    };
                    (e, 1)
                } else {
                    let r: String = self
                        .world_records()
                        .iter()
                        .map(|x| {
                            let f: Vec<&str> = x.trim_start_matches("w:").split(',').collect();
                            format!("{} pay.charge ref={} idem= fx=1 ok\n", f[0], f[4])
                        })
                        .collect();
                    (r, 0)
                };
                self.world.files.insert(out, text);
                self.exit_now(h, id, st);
            }
            _ => self.exit_now(h, id, 127),
        }
    }

    /// `hydo-world ctl <...>` as a `run`: (duration, status, output).
    pub(super) fn world_ctl(&mut self, argv: &[String]) -> (u64, u32, String) {
        let arg = |i: usize| argv.get(i).cloned().unwrap_or_default();
        if !self.world_up() {
            return (3, 1, DIAL.to_string());
        }
        let n: u64 = arg(3).parse().unwrap_or(0);
        match arg(2).as_str() {
            "ping" => (2, 0, "ok\n".into()),
            "on" => {
                if self.h(&["on", &arg(3), &arg(4)]).is_multiple_of(29) {
                    (
                        3,
                        1,
                        format!("error: {}: a rule this fake refuses\n", arg(4)),
                    )
                } else {
                    self.world.fw.on.insert(n);
                    self.world.fw.ever.insert(n);
                    if self.h(&["crash after on", &arg(3)]).is_multiple_of(13) {
                        // the world dies right after it answered: the later ctl calls fail
                        let p = self.world.procs.get("fakes:world").map(|p| p.spawn);
                        if let Some(spawn) = p {
                            self.world.procs.get_mut("fakes:world").unwrap().dying = true;
                            self.exit_now("fakes:world", spawn, 2);
                        }
                    }
                    (3, 0, "ok\n".into())
                }
            }
            "off" => {
                self.world.fw.on.remove(&n);
                (3, 0, "ok\n".into())
            }
            "heal" => {
                if self.h(&["heal"]).is_multiple_of(17) {
                    (3, 1, "error: heal failed in this fake\n".into())
                } else {
                    self.world.fw.on.clear();
                    (3, 0, "ok\n".into())
                }
            }
            other => (2, 1, format!("error: unknown command {other}\n")),
        }
    }
}
