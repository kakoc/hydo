//! One inspection call's guest work as agent commands (docs/design/inspect.md "Calls"): at most
//! one command run to its end with its stdout and stderr in files, then files read in pieces of
//! the agent's size (3,000 bytes) up to a byte limit. A pure state machine over what the
//! controller sees at each stop, so it runs the same in a snapshot's conversation and on a live
//! branch.

use hydo_agent_proto::msg::{Command, ErrCode, Event, Read, Reply, Run};
use hydo_control::{Out, Seen};

/// The agent's largest read.
pub const READ_MAX: u64 = 3000;
/// Where an inspection command's output goes in the guest (tmpfs; never the data disk).
pub const DIR: &str = "/tmp/hydo-inspect";

/// One file to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReq {
    pub path: String,
    /// The first byte to read; with `tail`, counted back from the end.
    pub offset: u64,
    pub max: u64,
    pub tail: bool,
}

/// A file as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileGot {
    pub path: String,
    pub data: Vec<u8>,
    /// The file's size (0 when it could not be read).
    pub size: u64,
    pub offset: u64,
    /// Some bytes were not UTF-8, so the reads were in hex.
    pub binary: bool,
    /// Why it could not be read (`not-found: ...`).
    pub error: Option<String>,
}

impl FileGot {
    pub fn truncated(&self) -> bool {
        self.offset > 0 || self.offset + (self.data.len() as u64) < self.size
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// How the command ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ended {
    pub status: u32,
    pub sig: Option<u32>,
    pub timed_out: bool,
    /// What the agent kept of output the command did not redirect.
    pub out: String,
}

/// The work of one call.
#[derive(Clone, Debug)]
pub struct Work {
    cmd: Option<Command>,
    files: Vec<FileReq>,
    // progress
    stage: usize,
    pending: Option<u64>,
    run_id: Option<u64>,
    hex: bool,
    probed: bool,
    pub ended: Option<Ended>,
    pub got: Vec<FileGot>,
    /// Why the work could not be done (an agent refusal).
    pub failed: Option<String>,
    pub stops: u64,
}

impl Work {
    pub fn new(cmd: Option<Command>, files: Vec<FileReq>) -> Work {
        Work {
            cmd,
            files,
            stage: 0,
            pending: None,
            run_id: None,
            hex: false,
            probed: false,
            ended: None,
            got: Vec::new(),
            failed: None,
            stops: 0,
        }
    }

    /// Read files only.
    pub fn read(files: Vec<FileReq>) -> Work {
        Work::new(None, files)
    }

    /// `argv` run to its end, its stdout and stderr in `<DIR>/<k>.out` and `.err`, both read
    /// up to `max` bytes. `script` is the shell text that runs the command (see [`shell`]).
    pub fn exec(
        busybox: &str,
        script: String,
        env: Vec<String>,
        netns: Option<String>,
        kill_after_ms: u64,
        k: u64,
        max: u64,
    ) -> Work {
        let (o, e) = (format!("{DIR}/{k}.out"), format!("{DIR}/{k}.err"));
        let text = format!(
            "{busybox} mkdir -p {DIR}; ( {script} ) >{o} 2>{e} </dev/null",
            busybox = busybox
        );
        let cmd = Command::Run(Run {
            argv: vec![busybox.into(), "sh".into(), "-c".into(), text],
            env: Some(env),
            cwd: None,
            uid: None,
            gid: None,
            netns,
            kill_after_ms: Some(kill_after_ms),
            capture: Some(400),
        });
        let f = |path: String| FileReq {
            path,
            offset: 0,
            max,
            tail: false,
        };
        Work::new(Some(cmd), vec![f(o), f(e)])
    }

    pub fn finished(&self) -> bool {
        self.failed.is_some() || (self.stage > self.files.len() && self.pending.is_none())
    }

    /// Take what this stop saw, then send the next command when the last one is answered.
    pub fn step(&mut self, seen: &[Seen], out: &mut Out) {
        self.stops += 1;
        for s in seen {
            self.see(s);
            if self.failed.is_some() {
                return;
            }
        }
        while self.pending.is_none() && self.run_id.is_none() && !self.finished() {
            self.next(out);
        }
    }

    fn see(&mut self, s: &Seen) {
        match s {
            Seen::Reply { id, body } if Some(*id) == self.pending => {
                self.pending = None;
                match body {
                    Reply::Run(_) => {}
                    Reply::Read(r) => self.read_ok(&r.data, r.size, r.eof),
                    other => self.failed = Some(format!("unexpected answer {}", other.name())),
                }
            }
            Seen::Refused { id, error } if Some(*id) == self.pending => {
                self.pending = None;
                if Some(*id) == self.run_id {
                    self.run_id = None;
                    self.failed = Some(format!(
                        "the agent refused the command: {} {}",
                        error.err, error.msg
                    ));
                } else if error.code() == Some(ErrCode::NotUtf8) && !self.hex {
                    // read the same piece again as hex
                    self.hex = true;
                    if let Some(g) = self.got.last_mut() {
                        g.binary = true;
                    }
                } else {
                    // the file is not there or cannot be read: say so, go on
                    if let Some(g) = self.got.last_mut() {
                        g.error = Some(format!("{}: {}", error.err, error.msg));
                    }
                    self.stage += 1;
                    self.hex = false;
                    self.probed = false;
                }
            }
            Seen::Event(Event::Done(d)) if Some(d.id) == self.run_id => {
                self.run_id = None;
                self.ended = Some(Ended {
                    status: d.status,
                    sig: d.sig,
                    timed_out: d.timed_out.unwrap_or(false),
                    out: d.out.clone(),
                });
                self.stage = 1;
            }
            _ => {}
        }
    }

    fn read_ok(&mut self, data: &str, size: u64, eof: bool) {
        let req = self.files[self.stage - 1].clone();
        let g = self.got.last_mut().expect("a file being read");
        g.size = size;
        if req.tail && !self.probed {
            // the first read only learned the size: start the real reads there
            self.probed = true;
            g.offset = size.saturating_sub(req.offset.max(req.max));
            return;
        }
        let bytes = if self.hex {
            from_hex(data).unwrap_or_default()
        } else {
            data.as_bytes().to_vec()
        };
        g.data.extend_from_slice(&bytes);
        let have = g.data.len() as u64;
        if eof || bytes.is_empty() || have >= req.max || g.offset + have >= size {
            self.stage += 1;
            self.hex = false;
            self.probed = false;
        }
    }

    fn next(&mut self, out: &mut Out) {
        if self.stage == 0 {
            match self.cmd.take() {
                Some(c) => {
                    let id = out.send("inspect", c);
                    self.pending = Some(id);
                    self.run_id = Some(id);
                }
                None => self.stage = 1,
            }
            return;
        }
        let Some(req) = self.files.get(self.stage - 1).cloned() else {
            self.stage = self.files.len() + 1;
            return;
        };
        if self.got.len() < self.stage {
            self.got.push(FileGot {
                path: req.path.clone(),
                offset: if req.tail { 0 } else { req.offset },
                ..FileGot::default()
            });
        }
        let g = self.got.last().expect("a file");
        let (offset, max) = if req.tail && !self.probed {
            (0, 1)
        } else {
            let have = g.data.len() as u64;
            (g.offset + have, (req.max - have).clamp(1, READ_MAX))
        };
        let id = out.send(
            "inspect",
            Command::Read(Read {
                path: req.path,
                offset: Some(offset),
                max: max as u32,
                hex: self.hex.then_some(true),
            }),
        );
        self.pending = Some(id);
    }
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A word for a POSIX shell: as is when it is plain, else in single quotes.
pub fn quote(w: &str) -> String {
    if !w.is_empty()
        && w.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&c))
    {
        return w.to_string();
    }
    format!("'{}'", w.replace('\'', "'\\''"))
}

/// The shell text that runs `argv` (or `sh`, a shell command line) with the project's prelude
/// sourced first, as the host controller runs a manifest's commands.
pub fn shell(prelude: &str, argv: &[String], sh: Option<&str>) -> String {
    let cmd = match sh {
        Some(t) => t.to_string(),
        None => argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" "),
    };
    format!(". {prelude} 2>/dev/null; {cmd}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydo_agent_proto::line::ErrorLine;
    use hydo_agent_proto::msg::{Done, ReadOk, RunOk};

    fn sent(out: Out) -> Vec<(u64, Command)> {
        out.into_actions()
            .into_iter()
            .filter_map(|a| match a {
                hydo_control::Action::Send { id, cmd, .. } => Some((id, cmd)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn exec_runs_then_reads_both_files_in_pieces() {
        let mut w = Work::exec(
            "/bin/busybox",
            "echo hi".into(),
            vec![],
            None,
            5000,
            3,
            4000,
        );
        let mut out = Out::new(10);
        w.step(&[], &mut out);
        let s = sent(out);
        assert_eq!(s.len(), 1);
        let Command::Run(r) = &s[0].1 else { panic!() };
        assert!(
            r.argv[3].contains("( echo hi ) >/tmp/hydo-inspect/3.out 2>/tmp/hydo-inspect/3.err")
        );
        assert_eq!(r.kill_after_ms, Some(5000));
        // the run's answer, then its done event
        let mut out = Out::new(11);
        w.step(
            &[Seen::Reply {
                id: 10,
                body: Reply::Run(RunOk {}),
            }],
            &mut out,
        );
        assert!(sent(out).is_empty(), "nothing before the command ended");
        let mut out = Out::new(11);
        w.step(
            &[Seen::Event(Event::Done(Done {
                id: 10,
                status: 0,
                sig: None,
                timed_out: None,
                out: String::new(),
                out_len: 0,
            }))],
            &mut out,
        );
        let s = sent(out);
        let Command::Read(r) = &s[0].1 else { panic!() };
        assert_eq!(
            (r.path.as_str(), r.offset, r.max),
            ("/tmp/hydo-inspect/3.out", Some(0), 3000)
        );
        // 3000 of 3500 bytes, then the rest is limited by max (4000 - 3000 = 1000)
        let mut out = Out::new(12);
        w.step(
            &[Seen::Reply {
                id: 11,
                body: Reply::Read(ReadOk {
                    data: "a".repeat(3000),
                    size: 5000,
                    eof: false,
                }),
            }],
            &mut out,
        );
        let s = sent(out);
        let Command::Read(r) = &s[0].1 else { panic!() };
        assert_eq!((r.offset, r.max), (Some(3000), 1000));
        let mut out = Out::new(13);
        w.step(
            &[Seen::Reply {
                id: 12,
                body: Reply::Read(ReadOk {
                    data: "b".repeat(1000),
                    size: 5000,
                    eof: false,
                }),
            }],
            &mut out,
        );
        // stderr next: not there
        let s = sent(out);
        let Command::Read(r) = &s[0].1 else { panic!() };
        assert_eq!(r.path, "/tmp/hydo-inspect/3.err");
        let mut out = Out::new(14);
        w.step(
            &[Seen::Refused {
                id: 13,
                error: ErrorLine {
                    v: 1,
                    seq: 0,
                    re: Some(13),
                    op: Some("read".into()),
                    err: "os-error".into(),
                    msg: "open: ENOENT".into(),
                    errno: Some(2),
                },
            }],
            &mut out,
        );
        assert!(w.finished());
        assert_eq!(w.got[0].data.len(), 4000);
        assert!(w.got[0].truncated());
        assert!(w.got[1].error.is_some());
        assert_eq!(w.ended.as_ref().unwrap().status, 0);
    }

    #[test]
    fn a_tail_read_learns_the_size_first() {
        let mut w = Work::read(vec![FileReq {
            path: "/tmp/hydo/redis.log".into(),
            offset: 0,
            max: 100,
            tail: true,
        }]);
        let mut out = Out::new(1);
        w.step(&[], &mut out);
        let Command::Read(r) = &sent(out)[0].1 else {
            panic!()
        };
        assert_eq!((r.offset, r.max), (Some(0), 1));
        let mut out = Out::new(2);
        w.step(
            &[Seen::Reply {
                id: 1,
                body: Reply::Read(ReadOk {
                    data: "x".into(),
                    size: 1000,
                    eof: false,
                }),
            }],
            &mut out,
        );
        let Command::Read(r) = &sent(out)[0].1 else {
            panic!()
        };
        assert_eq!((r.offset, r.max), (Some(900), 100));
        let mut out = Out::new(3);
        w.step(
            &[Seen::Reply {
                id: 2,
                body: Reply::Read(ReadOk {
                    data: "y".repeat(100),
                    size: 1000,
                    eof: true,
                }),
            }],
            &mut out,
        );
        assert!(w.finished());
        assert_eq!(w.got[0].offset, 900);
        assert!(w.got[0].truncated());
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("ls"), "ls");
        assert_eq!(quote("/data/redis"), "/data/redis");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote(""), "''");
        assert_eq!(
            shell(
                "/etc/hydo/prelude.sh",
                &["redis-cli".into(), "GET".into(), "k:1".into()],
                None
            ),
            ". /etc/hydo/prelude.sh 2>/dev/null; redis-cli GET k:1"
        );
    }
}
