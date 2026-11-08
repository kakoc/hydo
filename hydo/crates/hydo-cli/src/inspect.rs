//! `hydo inspect` (Milestone 178, docs/design/inspect.md): look inside a recorded run at any of
//! its stops without changing it, made for an AI agent that drives the `hydo` command.
//!
//! Every call takes arguments and prints one JSON document (results format version 1: `inspect`,
//! `inspect-error`, `timeline`, `diagnosis`), small and bounded (byte limits, `truncated`,
//! pages); `--text` prints a view for people instead. A call that fails says what was wrong and
//! the exact commands that would work (`try`).
//!
//! - [`timeline`]: `hydo inspect events <run dir>`, the run's events with their stops (no VM);
//! - [`diagnose`]: `hydo inspect diagnose <run dir>`, why a run failed (Part A; `hydo run`
//!   writes the same into `result.json`);
//! - [`drive`]: the run replayed to a stop on a fresh engine and held there;
//! - [`work`]: one call's guest work as agent commands;
//! - [`session`]: `hydo inspect start` keeps the held stop in a background process, so that
//!   later calls cost a restore and a few stops instead of a replay.

pub mod diagnose;
pub mod drive;
pub mod session;
pub mod timeline;
pub mod work;

use crate::ctx::Ctx;
use drive::{Conv, Recorded, Target};
use hydo_agent_proto::msg::{Command, Run};
use hydo_control::Plan;
use hydo_results::{
    Inspect, InspectEntry, InspectError, InspectExec, InspectProcess, InspectSocket, InspectText,
    RunPoint,
};
use serde_json::{json, Value};
use std::path::Path;
use work::{FileGot, FileReq, Work};

/// A call that failed: its `inspect-error` document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
    pub try_: Vec<String>,
}

impl Problem {
    pub fn new(code: &'static str, message: impl Into<String>, try_: Vec<String>) -> Problem {
        Problem {
            code,
            message: message.into(),
            try_,
        }
    }

    pub fn doc(&self, op: &str) -> InspectError {
        InspectError {
            op: op.into(),
            code: self.code.into(),
            message: self.message.clone(),
            try_: self.try_.clone(),
            ..InspectError::default()
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

// ---------------------------------------------------------------- calls

/// The default byte limit of what a call returns.
pub const MAX_BYTES: u64 = 16 * 1024;
/// The largest byte limit a call accepts.
pub const MAX_BYTES_LIMIT: u64 = 1024 * 1024;
/// A command's default time limit, guest milliseconds.
pub const TIMEOUT_MS: u64 = 10_000;

/// A named branch of a snapshot session (docs/design/agent-environments.md): a call on it
/// starts from the branch's state (or, for a new branch, from `from`: another branch, or the
/// session's stop when `None`) and leaves its state after the call as the branch's new state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub from: Option<String>,
}

/// One call on a held stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Req {
    Exec {
        argv: Vec<String>,
        sh: Option<String>,
        service: Option<String>,
        timeout_ms: u64,
        max_bytes: u64,
        /// Opt-in (docs/design/agent-environments.md): run on a named branch of the session's
        /// stop and keep the state after the command as that branch.
        branch: Option<Branch>,
    },
    Read {
        path: String,
        offset: u64,
        tail: Option<u64>,
        max_bytes: u64,
    },
    Ls {
        path: String,
        limit: u64,
    },
    Logs {
        service: String,
        lines: u64,
        max_bytes: u64,
    },
    Ps {
        all: bool,
        limit: u64,
    },
    Sockets {
        service: Option<String>,
        limit: u64,
    },
    Advance {
        ms: u64,
    },
    Status,
    Stop,
}

impl Req {
    pub fn op(&self) -> &'static str {
        match self {
            Req::Exec { .. } => "exec",
            Req::Read { .. } => "read",
            Req::Ls { .. } => "ls",
            Req::Logs { .. } => "logs",
            Req::Ps { .. } => "ps",
            Req::Sockets { .. } => "sockets",
            Req::Advance { .. } => "advance",
            Req::Status => "status",
            Req::Stop => "stop",
        }
    }

    /// The request as the session's socket carries it.
    pub fn to_json(&self) -> Value {
        match self {
            Req::Exec {
                argv,
                sh,
                service,
                timeout_ms,
                max_bytes,
                branch,
            } => {
                let mut v = json!({"op": "exec", "argv": argv, "sh": sh, "service": service,
                         "timeout_ms": timeout_ms, "max_bytes": max_bytes});
                if let Some(b) = branch {
                    v["branch"] = json!(b.name);
                    v["from"] = json!(b.from);
                }
                v
            }
            Req::Read {
                path,
                offset,
                tail,
                max_bytes,
            } => json!({"op": "read", "path": path, "offset": offset, "tail": tail,
                         "max_bytes": max_bytes}),
            Req::Ls { path, limit } => json!({"op": "ls", "path": path, "limit": limit}),
            Req::Logs {
                service,
                lines,
                max_bytes,
            } => json!({"op": "logs", "service": service, "lines": lines, "max_bytes": max_bytes}),
            Req::Ps { all, limit } => json!({"op": "ps", "all": all, "limit": limit}),
            Req::Sockets { service, limit } => {
                json!({"op": "sockets", "service": service, "limit": limit})
            }
            Req::Advance { ms } => json!({"op": "advance", "ms": ms}),
            Req::Status => json!({"op": "status"}),
            Req::Stop => json!({"op": "stop"}),
        }
    }

    pub fn from_json(v: &Value) -> Result<Req, Problem> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let u = |k: &str, d: u64| v.get(k).and_then(Value::as_u64).unwrap_or(d);
        let need = |k: &str| {
            s(k).ok_or_else(|| Problem::new("usage", format!("the request has no {k}"), vec![]))
        };
        Ok(match s("op").as_deref().unwrap_or("") {
            "exec" => Req::Exec {
                argv: v
                    .get("argv")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                sh: s("sh"),
                service: s("service"),
                timeout_ms: u("timeout_ms", TIMEOUT_MS),
                max_bytes: u("max_bytes", MAX_BYTES),
                branch: s("branch").map(|name| Branch {
                    name,
                    from: s("from"),
                }),
            },
            "read" => Req::Read {
                path: need("path")?,
                offset: u("offset", 0),
                tail: v.get("tail").and_then(Value::as_u64),
                max_bytes: u("max_bytes", MAX_BYTES),
            },
            "ls" => Req::Ls {
                path: need("path")?,
                limit: u("limit", 200),
            },
            "logs" => Req::Logs {
                service: need("service")?,
                lines: u("lines", 50),
                max_bytes: u("max_bytes", MAX_BYTES),
            },
            "ps" => Req::Ps {
                all: v.get("all").and_then(Value::as_bool).unwrap_or(false),
                limit: u("limit", 200),
            },
            "sockets" => Req::Sockets {
                service: s("service"),
                limit: u("limit", 200),
            },
            "advance" => Req::Advance { ms: u("ms", 1000) },
            "status" => Req::Status,
            "stop" => Req::Stop,
            o => {
                return Err(Problem::new(
                    "usage",
                    format!("unknown op {o:?}"),
                    vec!["hydo inspect --help".into()],
                ))
            }
        })
    }

    /// The call from command-line words (after the session or run directory).
    pub fn from_args(op: &str, args: &[String]) -> Result<Req, Problem> {
        let mut a = Args::new(args);
        let max = |a: &mut Args| -> Result<u64, Problem> {
            let m = a.num("--max-bytes")?.unwrap_or(MAX_BYTES);
            if m == 0 || m > MAX_BYTES_LIMIT {
                return Err(Problem::new(
                    "usage",
                    format!("--max-bytes {m}: between 1 and {MAX_BYTES_LIMIT}"),
                    vec![],
                ));
            }
            Ok(m)
        };
        let r = match op {
            "exec" => {
                let sh = a.val("--sh")?;
                let service = a.val("--service")?;
                let branch = a.val("--branch")?;
                let from = a.val("--from")?;
                if from.is_some() && branch.is_none() {
                    return Err(Problem::new(
                        "usage",
                        "--from needs --branch (the new branch's name)",
                        vec![
                            "hydo inspect exec <session> --branch b2 --from b1 -- make test".into(),
                        ],
                    ));
                }
                let branch = branch.map(|name| Branch { name, from });
                let timeout_ms = a.num("--timeout")?.unwrap_or(TIMEOUT_MS);
                let max_bytes = max(&mut a)?;
                let argv = a.rest_after_dashes();
                if argv.is_empty() == sh.is_none() {
                    return Err(Problem::new(
                        "usage",
                        "exec needs a command: `-- <program> <args...>` or `--sh '<shell command line>'` (one of the two)",
                        vec![
                            "hydo inspect exec <session> -- ls -l /data".into(),
                            "hydo inspect exec <session> --sh 'redis-cli KEYS \"*\" | head'".into(),
                        ],
                    ));
                }
                Req::Exec {
                    argv,
                    sh,
                    service,
                    timeout_ms,
                    max_bytes,
                    branch,
                }
            }
            "read" => {
                let offset = a.num("--offset")?.unwrap_or(0);
                let tail = a.num("--tail")?;
                let max_bytes = max(&mut a)?;
                let path = a.one("a path in the guest")?;
                Req::Read {
                    path,
                    offset,
                    tail,
                    max_bytes,
                }
            }
            "ls" => {
                let limit = a.num("--limit")?.unwrap_or(200);
                let path = a.one_or("/")?;
                Req::Ls { path, limit }
            }
            "logs" => {
                let lines = a.num("--lines")?.unwrap_or(50);
                let max_bytes = max(&mut a)?;
                let service = a.one("a service name")?;
                Req::Logs {
                    service,
                    lines,
                    max_bytes,
                }
            }
            "ps" => {
                let all = a.flag("--all");
                let limit = a.num("--limit")?.unwrap_or(200);
                Req::Ps { all, limit }
            }
            "sockets" => {
                let service = a.val("--service")?;
                let limit = a.num("--limit")?.unwrap_or(200);
                Req::Sockets { service, limit }
            }
            "advance" => Req::Advance {
                ms: a.num("--ms")?.unwrap_or(1000),
            },
            "status" => Req::Status,
            "stop" => Req::Stop,
            _ => {
                return Err(Problem::new(
                    "usage",
                    format!("unknown inspect command {op:?}"),
                    vec!["hydo inspect --help".into()],
                ))
            }
        };
        a.done()?;
        Ok(r)
    }
}

/// Command-line words, taken one option at a time.
pub struct Args {
    v: Vec<String>,
}

impl Args {
    pub fn new(a: &[String]) -> Args {
        Args { v: a.to_vec() }
    }
    fn dashes(&self) -> usize {
        self.v
            .iter()
            .position(|w| w == "--")
            .unwrap_or(self.v.len())
    }
    pub fn flag(&mut self, f: &str) -> bool {
        match self.v[..self.dashes()].iter().position(|w| w == f) {
            Some(i) => {
                self.v.remove(i);
                true
            }
            None => false,
        }
    }
    pub fn val(&mut self, f: &str) -> Result<Option<String>, Problem> {
        let Some(i) = self.v[..self.dashes()].iter().position(|w| w == f) else {
            return Ok(None);
        };
        if i + 1 >= self.dashes() {
            return Err(Problem::new("usage", format!("{f} needs a value"), vec![]));
        }
        let x = self.v.remove(i + 1);
        self.v.remove(i);
        Ok(Some(x))
    }
    pub fn num(&mut self, f: &str) -> Result<Option<u64>, Problem> {
        match self.val(f)? {
            None => Ok(None),
            Some(x) => x.parse().map(Some).map_err(|_| {
                Problem::new(
                    "usage",
                    format!("{f} {x}: a whole number is needed"),
                    vec![],
                )
            }),
        }
    }
    pub fn rest_after_dashes(&mut self) -> Vec<String> {
        let d = self.dashes();
        if d < self.v.len() {
            let rest = self.v.split_off(d + 1);
            self.v.truncate(d);
            rest
        } else {
            Vec::new()
        }
    }
    pub fn one(&mut self, what: &str) -> Result<String, Problem> {
        let d = self.dashes();
        match self.v[..d].iter().position(|w| !w.starts_with("--")) {
            Some(i) => Ok(self.v.remove(i)),
            None => Err(Problem::new("usage", format!("missing {what}"), vec![])),
        }
    }
    pub fn one_or(&mut self, default: &str) -> Result<String, Problem> {
        Ok(self.one("").unwrap_or_else(|_| default.to_string()))
    }
    pub fn done(&self) -> Result<(), Problem> {
        if self.v.is_empty() {
            Ok(())
        } else {
            Err(Problem::new(
                "usage",
                format!("unexpected words: {}", self.v.join(" ")),
                vec!["hydo inspect --help".into()],
            ))
        }
    }
}

// ---------------------------------------------------------------- the guest side of calls

/// The environment of an inspection command: what the host controller gives a manifest's
/// commands, and a PATH.
pub fn env(plan: &Plan) -> Vec<String> {
    let l = &plan.layout;
    vec![
        "HOME=/".into(),
        format!("HYDO_ACKED={}", l.acked()),
        format!("HYDO_FAILED={}", l.failed()),
        format!("HYDO_OUTCOMES={}", l.outcomes()),
        "PATH=/hydo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
        "TERM=linux".into(),
    ]
}

/// The network namespace of a service's commands (`network: bridge`), checked against the plan.
pub fn netns_of(plan: &Plan, service: Option<&str>) -> Result<Option<String>, Problem> {
    let Some(s) = service else { return Ok(None) };
    if plan.service(s).is_none() {
        return Err(no_service(plan, s));
    }
    Ok(plan.bridge.then(|| format!("{}/ns/{s}", plan.layout.dir)))
}

fn no_service(plan: &Plan, s: &str) -> Problem {
    let names: Vec<&str> = plan.services.iter().map(|x| x.name.as_str()).collect();
    Problem::new(
        "usage",
        format!(
            "no service {s:?} in this image (its services: {})",
            names.join(", ")
        ),
        names
            .first()
            .map(|n| vec![format!("hydo inspect logs <session> {n}")])
            .unwrap_or_default(),
    )
}

const PS: &str = r#"for p in /proc/[0-9]*; do [ -r $p/stat ] || continue; read -r st < $p/stat || continue; r=$(grep VmRSS $p/status 2>/dev/null); c=$(tr '\0' ' ' < $p/cmdline 2>/dev/null); echo "S $st"; echo "R $r"; echo "C $c"; done"#;
const SOCKETS: &str =
    r#"for f in tcp tcp6 udp udp6; do echo "@$f"; cat /proc/net/$f 2>/dev/null; done"#;

/// The script that lists a directory (one line per entry; a symlink's target on its own line).
fn ls_script(path: &str) -> String {
    let p = work::quote(path);
    format!(
        "if [ -d {p} ]; then cd {p} || exit 1; for f in .* *; do case \"$f\" in .|..) continue;; esac; \
         [ -e \"$f\" ] || [ -L \"$f\" ] || continue; stat -c '%F|%s|%a|%U|%Y|%n' -- \"$f\"; \
         if [ -L \"$f\" ]; then echo \"->$(readlink -- \"$f\")\"; fi; done; true; \
         else stat -c '%F|%s|%a|%U|%Y|%n' -- {p}; fi"
    )
}

/// The guest work of a call (`k` numbers the output files).
pub fn work_for(req: &Req, plan: &Plan, k: u64) -> Result<Work, Problem> {
    let bb = plan.layout.busybox.as_str();
    let prelude = plan.layout.prelude.as_str();
    let ex = |script: String, ns: Option<String>, t: u64, max: u64| {
        Work::exec(bb, script, env(plan), ns, t, k, max)
    };
    Ok(match req {
        Req::Exec {
            argv,
            sh,
            service,
            timeout_ms,
            max_bytes,
            ..
        } => {
            let ns = netns_of(plan, service.as_deref())?;
            ex(
                work::shell(prelude, argv, sh.as_deref()),
                ns,
                *timeout_ms,
                *max_bytes,
            )
        }
        Req::Read {
            path,
            offset,
            tail,
            max_bytes,
        } => Work::read(vec![FileReq {
            path: path.clone(),
            offset: tail.unwrap_or(*offset),
            max: tail.map(|t| t.min(*max_bytes)).unwrap_or(*max_bytes),
            tail: tail.is_some(),
        }]),
        Req::Ls { path, .. } => ex(ls_script(path), None, TIMEOUT_MS, 256 * 1024),
        Req::Logs {
            service,
            lines,
            max_bytes,
        } => {
            if plan.service(service).is_none() {
                return Err(no_service(plan, service));
            }
            let log = plan.layout.log_of(service);
            ex(
                format!("tail -n {lines} {}", work::quote(&log)),
                None,
                TIMEOUT_MS,
                *max_bytes,
            )
        }
        Req::Ps { .. } => ex(PS.into(), None, TIMEOUT_MS, 512 * 1024),
        Req::Sockets { service, .. } => {
            let ns = netns_of(plan, service.as_deref())?;
            ex(SOCKETS.into(), ns, TIMEOUT_MS, 512 * 1024)
        }
        Req::Advance { ms } => ex(
            format!("usleep {}", ms.saturating_mul(1000)),
            None,
            ms + TIMEOUT_MS,
            1024,
        ),
        Req::Status | Req::Stop => Work::read(Vec::new()),
    })
}

/// `--freeze`: SIGSTOP every process but the agent and the command itself (no subshell, so `$$`
/// is the process that signals).
pub fn freeze_work(plan: &Plan) -> Work {
    let script = "n=0; for p in /proc/[0-9]*; do i=${p#/proc/}; [ \"$i\" = 1 ] && continue; \
                  [ \"$i\" = $$ ] && continue; [ -s $p/cmdline ] || continue; \
                  kill -STOP $i 2>/dev/null && n=$((n+1)); done; echo stopped $n";
    Work::new(
        Some(Command::Run(Run {
            argv: vec![
                plan.layout.busybox.clone(),
                "sh".into(),
                "-c".into(),
                script.into(),
            ],
            env: Some(env(plan)),
            cwd: None,
            uid: None,
            gid: None,
            netns: None,
            kill_after_ms: Some(TIMEOUT_MS),
            capture: Some(200),
        })),
        Vec::new(),
    )
}

fn text_of(g: Option<&FileGot>) -> InspectText {
    let Some(g) = g else {
        return InspectText::default();
    };
    if g.error.is_some() && g.data.is_empty() {
        return InspectText::default();
    }
    InspectText {
        text: (!g.binary).then(|| g.text()),
        hex: g.binary.then(|| work::to_hex(&g.data)),
        bytes: g.size,
        offset: (g.offset > 0).then_some(g.offset),
        truncated: g.truncated(),
    }
}

/// The answer to a call whose work is done (or failed).
pub fn answer(
    req: &Req,
    w: &Work,
    at: &RunPoint,
    state: &str,
    wall_ms: u64,
) -> Result<Inspect, Problem> {
    if let Some(f) = &w.failed {
        return Err(Problem::new(
            if f.contains("no answer within") {
                "timeout"
            } else {
                "agent"
            },
            f.clone(),
            vec![],
        ));
    }
    let mut a = Inspect {
        op: req.op().into(),
        at: Some(at.clone()),
        cost: Some(hydo_results::InspectCost {
            wall_ms,
            stops: w.stops,
        }),
        ..Inspect::default()
    };
    if state == "live" {
        a.notes.push(
            "live branch: this call ran after the session's earlier calls, on the same guest"
                .into(),
        );
    }
    let ended = w.ended.clone().unwrap_or_default();
    let exec = |argv: Vec<String>, service: Option<String>| InspectExec {
        argv,
        service,
        status: ended.status as u64,
        signal: ended.sig.map(u64::from),
        timed_out: ended.timed_out,
        stdout: text_of(w.got.first()),
        stderr: text_of(w.got.get(1)),
    };
    let stdout = w.got.first().map(FileGot::text).unwrap_or_default();
    let stderr = w.got.get(1).map(FileGot::text).unwrap_or_default();
    if ended.timed_out {
        a.notes.push(
            "the command ran out of time (--timeout, guest milliseconds) and was killed".into(),
        );
    }
    match req {
        Req::Exec {
            argv, sh, service, ..
        } => {
            let argv = match sh {
                Some(t) => vec!["sh".into(), "-c".into(), t.clone()],
                None => argv.clone(),
            };
            a.exec = Some(exec(argv, service.clone()));
            if ended.status == 127 {
                a.notes.push("status 127: the program was not found (not in the image, or not on PATH; give its full path)".into());
            }
        }
        Req::Logs { service, lines, .. } => {
            a.exec = Some(exec(
                vec![
                    "tail".into(),
                    "-n".into(),
                    lines.to_string(),
                    format!("/tmp/hydo/{service}.log"),
                ],
                Some(service.clone()),
            ));
            if stdout.is_empty() {
                a.notes.push(format!(
                    "the log of {service} is empty or not there at this stop (it has not started yet?)"
                ));
            }
        }
        Req::Read { path, .. } => {
            a.path = Some(path.clone());
            let g = w.got.first();
            if let Some(e) = g.and_then(|g| g.error.clone()) {
                return Err(Problem::new(
                    "agent",
                    format!("{path}: {e}"),
                    vec![format!(
                        "hydo inspect ls <session> {}",
                        Path::new(path)
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "/".into())
                    )],
                ));
            }
            a.file = Some(text_of(g));
        }
        Req::Ls { path, limit } => {
            if ended.status != 0 {
                return Err(Problem::new(
                    "agent",
                    format!("{path}: {}", stderr.trim()),
                    vec!["hydo inspect ls <session> /".into()],
                ));
            }
            let all = parse_ls(&stdout);
            a.truncated = Some(all.len() as u64 > *limit);
            a.entries = all.into_iter().take(*limit as usize).collect();
            a.path = Some(path.clone());
        }
        Req::Ps { all, limit } => {
            let ps: Vec<InspectProcess> = parse_ps(&stdout)
                .into_iter()
                .filter(|p| *all || !p.command.starts_with('['))
                .collect();
            a.truncated = Some(ps.len() as u64 > *limit);
            a.processes = ps.into_iter().take(*limit as usize).collect();
        }
        Req::Sockets { limit, service } => {
            let s = parse_sockets(&stdout);
            a.truncated = Some(s.len() as u64 > *limit);
            a.sockets = s.into_iter().take(*limit as usize).collect();
            if service.is_none() {
                a.notes.push("the guest's own network namespace; with network: bridge each service has its own (--service NAME)".into());
            }
        }
        Req::Advance { ms } => {
            a.notes.push(format!(
                "the guest ran {ms} ms of guest time further; later calls start from here"
            ));
        }
        Req::Status | Req::Stop => {}
    }
    Ok(a)
}

/// `stat -c '%F|%s|%a|%U|%Y|%n'` lines, a symlink's `->target` after its line.
pub fn parse_ls(out: &str) -> Vec<InspectEntry> {
    let mut v: Vec<InspectEntry> = Vec::new();
    for l in out.lines() {
        if let Some(t) = l.strip_prefix("->") {
            if let Some(e) = v.last_mut() {
                e.target = Some(t.to_string());
            }
            continue;
        }
        let f: Vec<&str> = l.splitn(6, '|').collect();
        if f.len() < 6 {
            continue;
        }
        let kind = match f[0] {
            "regular file" | "regular empty file" => "file",
            "directory" => "dir",
            "symbolic link" => "link",
            "socket" => "socket",
            "fifo" => "fifo",
            k if k.contains("special") => "device",
            _ => "other",
        };
        v.push(InspectEntry {
            name: f[5].to_string(),
            kind: kind.into(),
            bytes: f[1].parse().unwrap_or(0),
            mode: f[2].to_string(),
            owner: Some(f[3].to_string()),
            mtime: f[4].parse().ok(),
            target: None,
        });
    }
    v
}

/// The `ps` script's groups of three lines (`S <stat>`, `R VmRSS: <n> kB`, `C <cmdline>`).
pub fn parse_ps(out: &str) -> Vec<InspectProcess> {
    let mut v = Vec::new();
    let mut cur: Option<InspectProcess> = None;
    for l in out.lines() {
        if let Some(st) = l.strip_prefix("S ") {
            if let Some(p) = cur.take() {
                v.push(p);
            }
            let (Some(o), Some(c)) = (st.find('('), st.rfind(')')) else {
                continue;
            };
            let pid = st[..o].trim().parse().unwrap_or(0);
            let comm = &st[o + 1..c];
            let f: Vec<&str> = st[c + 1..].split_whitespace().collect();
            cur = Some(InspectProcess {
                pid,
                ppid: f.get(1).and_then(|x| x.parse().ok()).unwrap_or(0),
                state: f.first().unwrap_or(&"?").to_string(),
                rss_kib: 0,
                threads: f.get(17).and_then(|x| x.parse().ok()).unwrap_or(0),
                command: format!("[{comm}]"),
            });
        } else if let Some(r) = l.strip_prefix("R ") {
            if let Some(p) = cur.as_mut() {
                p.rss_kib = r
                    .split_whitespace()
                    .nth(1)
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0);
            }
        } else if let Some(c) = l.strip_prefix("C ") {
            if let Some(p) = cur.as_mut() {
                if !c.trim().is_empty() {
                    p.command = c.trim().to_string();
                }
            }
        }
    }
    v.extend(cur);
    v.sort_by_key(|p| p.pid);
    v
}

fn tcp_state(h: &str) -> &'static str {
    match h {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        _ => "?",
    }
}

/// `/proc/net/{tcp,udp}{,6}`'s `0100007F:18EB` as `127.0.0.1:6379`.
fn addr(a: &str) -> String {
    let Some((h, p)) = a.split_once(':') else {
        return a.into();
    };
    let port = u16::from_str_radix(p, 16).unwrap_or(0);
    let b = work::from_hex(h).unwrap_or_default();
    match b.len() {
        4 => format!("{}.{}.{}.{}:{port}", b[3], b[2], b[1], b[0]),
        16 => {
            // four little-endian 32-bit words
            let mut x = [0u8; 16];
            for w in 0..4 {
                for i in 0..4 {
                    x[w * 4 + i] = b[w * 4 + 3 - i];
                }
            }
            format!("[{}]:{port}", std::net::Ipv6Addr::from(x))
        }
        _ => a.into(),
    }
}

pub fn parse_sockets(out: &str) -> Vec<InspectSocket> {
    let mut v = Vec::new();
    let mut proto = "";
    for l in out.lines() {
        if let Some(p) = l.strip_prefix('@') {
            proto = match p {
                "tcp" => "tcp",
                "tcp6" => "tcp6",
                "udp" => "udp",
                "udp6" => "udp6",
                _ => "",
            };
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 5 || !f[0].ends_with(':') || proto.is_empty() {
            continue;
        }
        let (tx, rx) = f[4].split_once(':').unwrap_or(("0", "0"));
        let state = if proto.starts_with("tcp") {
            tcp_state(f[3]).to_string()
        } else if f[3] == "07" {
            "UNCONN".into()
        } else {
            tcp_state(f[3]).to_string()
        };
        let remote = addr(f[2]);
        v.push(InspectSocket {
            proto: proto.into(),
            local: addr(f[1]),
            remote: (!remote.ends_with(":0") || state != "LISTEN" && state != "UNCONN")
                .then_some(remote),
            state,
            send_queue: u64::from_str_radix(tx, 16).unwrap_or(0),
            recv_queue: u64::from_str_radix(rx, 16).unwrap_or(0),
        });
    }
    v
}

// ---------------------------------------------------------------- points

/// `--at`: `end` (where the verdict was given), `<conversation>:<stop>`, or a stop number of the
/// run's last conversation.
pub fn target_of(rec: &Recorded, at: &str) -> Result<Target, Problem> {
    let tries = || {
        let mut t = vec![format!(
            "hydo inspect events {}   (every event with its point)",
            rec.dir.display()
        )];
        if rec.end().is_some() {
            t.push(format!("hydo inspect start {} --at end", rec.dir.display()));
        }
        t
    };
    let t = if at == "end" {
        rec.end().ok_or_else(|| {
            Problem::new(
                "bad-point",
                "the run has no ending recorded (its last conversation has no end record)",
                tries(),
            )
        })?
    } else {
        let (c, n) = match at.split_once(':') {
            Some((c, n)) => (
                Conv::parse(c).ok_or_else(|| {
                    Problem::new(
                        "bad-point",
                        format!("--at {at}: the conversation is boot, run or recovery"),
                        tries(),
                    )
                })?,
                n,
            ),
            None => (rec.transcripts.last().map(|x| x.0).unwrap_or(Conv::Run), at),
        };
        let stop = n.parse().map_err(|_| {
            Problem::new(
                "bad-point",
                format!(
                    "--at {at}: expected end, <conversation>:<stop> (boot:172) or a stop number"
                ),
                tries(),
            )
        })?;
        Target { conv: c, stop }
    };
    let Some((first, last)) = rec.span(t.conv) else {
        return Err(Problem::new(
            "bad-point",
            format!("the run has no {} conversation", t.conv.name()),
            tries(),
        ));
    };
    let hello = rec.first_agent_stop(t.conv).unwrap_or(first);
    if t.stop < hello.max(first) || t.stop > last {
        return Err(Problem::new(
            "bad-point",
            format!(
                "stop {} is outside the {} conversation's stops where the agent answers ({}..{})",
                t.stop,
                t.conv.name(),
                hello.max(first),
                last
            ),
            tries(),
        ));
    }
    Ok(t)
}

pub fn run_point(rec: &Recorded, t: Target) -> RunPoint {
    RunPoint {
        point: t.point(),
        conversation: t.conv.name().into(),
        stop: t.stop,
        at: rec.at_of(t),
    }
}

// ---------------------------------------------------------------- the command

pub const HELP: &str = r#"hydo inspect -- look inside a recorded run at any of its stops, without changing it.

Every call prints one JSON document (results format v1: "inspect", "inspect-error",
"timeline", "diagnosis"); --text prints a view for people instead. A failed call prints an
"inspect-error" with "try": commands that would work. Exit status: 0 answered, 2 inspect-error.

Find where to look (no VM; instant):
  hydo inspect diagnose <run dir> [--evidence]   why the run failed (--evidence: one replay
                                                 reads logs, dmesg and processes in the guest)
  hydo inspect events <run dir> [--kind K,...] [--grep TEXT] [--conversation C]
                      [--from POINT] [--to POINT] [--limit N] [--offset N]
                                                 the run's events, each with its point (boot:172)

Hold a stop (one replay of the run up to it: about as long as the run took to get there):
  hydo inspect start <run dir> [--at end|<conv>:<stop>|<stop>] [--freeze] [--idle SECS]
                                                 -> a session id; --at end (the default) is the
                                                 stop where the verdict was given
  hydo inspect list | status <id> | stop <id>

Look (each call restores the stop's snapshot, so every call sees the same state; a fraction of a
second to a few seconds each):
  hydo inspect exec    <id> [--service S] [--timeout MS] [--max-bytes N] -- <program> <args...>
  hydo inspect exec    <id> [--service S] --sh '<shell command line>'
  hydo inspect read    <id> <path> [--offset N | --tail N] [--max-bytes N]
  hydo inspect ls      <id> [<path>] [--limit N]
  hydo inspect logs    <id> <service> [--lines N]
  hydo inspect ps      <id> [--all] [--limit N]
  hydo inspect sockets <id> [--service S]
  hydo inspect advance <id> [--ms N]            let the guest run N ms further; later calls start there

Branches (a snapshot session only; docs/design/agent-environments.md):
  hydo inspect exec <id> --branch NAME [--from OTHER] ... -- <program> <args...>
                                                 run on branch NAME and keep the state after it as
                                                 NAME; a new branch starts from OTHER (another
                                                 branch) or from the stop; the answer's state_fp is
                                                 the guest's whole state after the call

<id> may also be a run directory: then the call replays to --at (default end), does the one
thing and ends (no session; each such call costs a replay). --service runs in that service's
network namespace (network: bridge). Stops after the workload's end, in a recovery boot or under
timing tokens cannot be snapshotted: there the session is "live" and calls run one after another
on one branch. The recorded run's directory is never written. Details: hydo guide inspect.
"#;

/// Print a document (compact JSON, or a view with `--text`); the exit status.
pub fn print_doc(v: &Value, text: bool) -> i32 {
    let err = v.get("type").and_then(Value::as_str) == Some("inspect-error");
    if text {
        print!("{}", view(v));
    } else {
        println!("{}", serde_json::to_string(v).unwrap_or_default());
    }
    if err {
        2
    } else {
        0
    }
}

/// The `--text` view: an exec's output as the command printed it, anything else indented.
fn view(v: &Value) -> String {
    if let Some(e) = v.get("exec") {
        let mut s = String::new();
        if let Some(t) = e.pointer("/stdout/text").and_then(Value::as_str) {
            s.push_str(t);
        }
        if let Some(t) = e.pointer("/stderr/text").and_then(Value::as_str) {
            if !t.is_empty() {
                s.push_str("--- stderr\n");
                s.push_str(t);
            }
        }
        s.push_str(&format!(
            "--- status {}\n",
            e.get("status").and_then(Value::as_u64).unwrap_or(0)
        ));
        return s;
    }
    if let Some(t) = v.pointer("/file/text").and_then(Value::as_str) {
        return t.to_string();
    }
    serde_json::to_string_pretty(v).unwrap_or_default() + "\n"
}

pub fn err_doc(op: &str, p: &Problem) -> Value {
    serde_json::to_value(p.doc(op)).unwrap_or(Value::Null)
}

pub fn cmd_inspect(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let text = match args.iter().position(|a| a == "--text") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let op = args.first().cloned().unwrap_or_default();
    let rest = if args.is_empty() {
        vec![]
    } else {
        args[1..].to_vec()
    };
    let r: Result<Value, Problem> = match op.as_str() {
        "" | "-h" | "--help" | "help" => {
            print!("{HELP}");
            return if op.is_empty() { 2 } else { 0 };
        }
        "__serve" => return session::serve(ctx, rest.first().map(String::as_str).unwrap_or("")),
        "events" => timeline::cmd(&rest),
        "diagnose" => diagnose::cmd(ctx, &rest),
        "start" => session::start(ctx, &rest),
        "list" => session::list(ctx),
        "status" | "stop" | "exec" | "read" | "ls" | "logs" | "ps" | "sockets" | "advance" => {
            call(ctx, &op, &rest)
        }
        o => Err(Problem::new(
            "usage",
            format!("unknown inspect command {o:?}"),
            vec!["hydo inspect --help".into()],
        )),
    };
    match r {
        Ok(v) => print_doc(&v, text),
        Err(p) => print_doc(&err_doc(if op.is_empty() { "?" } else { &op }, &p), text),
    }
}

/// A call on a session, or (with a run directory instead of a session id) a one-shot call.
fn call(ctx: &Ctx, op: &str, rest: &[String]) -> Result<Value, Problem> {
    let Some(first) = rest.first() else {
        return Err(Problem::new(
            "usage",
            format!("hydo inspect {op} needs a session id or a run directory first"),
            vec![
                "hydo inspect list".into(),
                "hydo inspect start <run dir> --at end".into(),
            ],
        ));
    };
    let p = Path::new(first);
    if p.join(hydo_results::RUN_DOC).is_file() {
        let mut a = Args::new(&rest[1..]);
        let at = a.val("--at")?.unwrap_or_else(|| "end".into());
        let words: Vec<String> = a.v.clone();
        let req = Req::from_args(op, &words)?;
        if matches!(req, Req::Status | Req::Stop) {
            return Err(Problem::new(
                "usage",
                format!("{op} takes a session id (a run directory has no session)"),
                vec!["hydo inspect list".into()],
            ));
        }
        return session::oneshot(ctx, p, &at, req);
    }
    let req = Req::from_args(op, &rest[1..])?;
    session::call(ctx, first, req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_json() {
        for r in [
            Req::Exec {
                argv: vec!["ls".into(), "-l".into()],
                sh: None,
                service: Some("db".into()),
                timeout_ms: 5,
                max_bytes: 9,
                branch: None,
            },
            Req::Exec {
                argv: vec!["true".into()],
                sh: None,
                service: None,
                timeout_ms: 5,
                max_bytes: 9,
                branch: Some(Branch {
                    name: "b2".into(),
                    from: Some("b1".into()),
                }),
            },
            Req::Read {
                path: "/x".into(),
                offset: 3,
                tail: Some(10),
                max_bytes: 100,
            },
            Req::Ls {
                path: "/".into(),
                limit: 2,
            },
            Req::Logs {
                service: "redis".into(),
                lines: 7,
                max_bytes: 100,
            },
            Req::Ps {
                all: true,
                limit: 5,
            },
            Req::Sockets {
                service: None,
                limit: 3,
            },
            Req::Advance { ms: 250 },
            Req::Status,
            Req::Stop,
        ] {
            assert_eq!(Req::from_json(&r.to_json()).unwrap(), r);
        }
    }

    #[test]
    fn arguments() {
        let w = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            Req::from_args("exec", &w("--service db --timeout 500 -- psql -c select 1")).unwrap(),
            Req::Exec {
                argv: w("psql -c select 1"),
                sh: None,
                service: Some("db".into()),
                timeout_ms: 500,
                max_bytes: MAX_BYTES,
                branch: None,
            }
        );
        let e = Req::from_args("exec", &[]).unwrap_err();
        assert_eq!(e.code, "usage");
        assert!(!e.try_.is_empty());
        assert_eq!(
            Req::from_args("read", &w("/data/x --tail 100")).unwrap(),
            Req::Read {
                path: "/data/x".into(),
                offset: 100,
                tail: Some(100),
                max_bytes: MAX_BYTES
            }
            .clone_with_offset(0)
        );
        assert!(Req::from_args("read", &w("/a /b")).is_err());
        assert_eq!(
            Req::from_args("ls", &[]).unwrap(),
            Req::Ls {
                path: "/".into(),
                limit: 200
            }
        );
        assert!(Req::from_args("ps", &w("--bogus")).is_err());
        assert!(Req::from_args("read", &w("/x --max-bytes 0")).is_err());
    }

    impl Req {
        fn clone_with_offset(&self, o: u64) -> Req {
            match self.clone() {
                Req::Read {
                    path,
                    tail,
                    max_bytes,
                    ..
                } => Req::Read {
                    path,
                    offset: o,
                    tail,
                    max_bytes,
                },
                r => r,
            }
        }
    }

    #[test]
    fn listings_parse() {
        let ls = "directory|4096|755|root|1767225600|redis\n\
                  regular file|12|644|root|1767225601|appendonly.aof\n\
                  symbolic link|3|777|root|1767225602|l\n->appendonly.aof\n";
        let e = parse_ls(ls);
        assert_eq!(e.len(), 3);
        assert_eq!((e[0].kind.as_str(), e[0].name.as_str()), ("dir", "redis"));
        assert_eq!((e[1].kind.as_str(), e[1].bytes), ("file", 12));
        assert_eq!(e[2].target.as_deref(), Some("appendonly.aof"));
        let ps =
            "S 1 (hydo-agent) S 0 1 1 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 1 2 3\nR \nC /init\n\
                  S 57 (redis-server) S 1 57 57 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 5 0 1 2 3\n\
                  R VmRSS:\t    8812 kB\nC /bin/redis-server /etc/redis.conf \n\
                  S 2 (kthreadd) S 0 0 0 0 -1 1 0 0 0 0 0 0 0 0 20 0 1 0 1 0 0\nR \nC \n";
        let p = parse_ps(ps);
        assert_eq!(p.len(), 3);
        assert_eq!(p[1].command, "[kthreadd]");
        let r = &p[2];
        assert_eq!((r.pid, r.ppid, r.rss_kib, r.threads), (57, 1, 8812, 5));
        assert_eq!(r.command, "/bin/redis-server /etc/redis.conf");
        let net = "@tcp\n  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                   0: 0100007F:18EB 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1\n\
                   1: 0100007F:18EB 0100007F:9C40 01 00000010:00000002 00:00000000 00000000     0        0 2\n\
                   @tcp6\n  sl  local_address remote_address st\n\
                   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000\n";
        let s = parse_sockets(net);
        assert_eq!(s.len(), 3);
        assert_eq!(
            (s[0].local.as_str(), s[0].state.as_str()),
            ("127.0.0.1:6379", "LISTEN")
        );
        assert_eq!(s[0].remote, None);
        assert_eq!(s[1].remote.as_deref(), Some("127.0.0.1:40000"));
        assert_eq!((s[1].send_queue, s[1].recv_queue), (16, 2));
        assert_eq!(s[2].local, "[::1]:8080");
    }
}
