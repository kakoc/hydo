//! The scripted agent's world for the feature controllers of M123: the processes the history
//! read-backs, the composer and its timers start (`history:c<i>`, `first:<i>`, `cmd:<i>`,
//! `end:<i>`, `compose-step:<n>`, `compose-drain`), service logs with error lines of every shape
//! the error-log scan knows (Redis, MySQL, Kafka with Java traces, Go, logfmt, control
//! characters, long lines, a character the 3,000-byte reads can cut), the error-log bookkeeping
//! pipelines (`cat … | wc -l`), the agent's monotonic clock, and reads that refuse text cut inside
//! a character (the controller then reads hex).

use super::Fake;
use hydo_agent_proto::msg::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub struct Ext {
    /// Files the error-log pipelines named (service logs, setup logs, data-disk files).
    pub streams: BTreeSet<String>,
    /// Lines written so far (numbers them).
    pub lines: u64,
}

const LINES: &[&str] = &[
    "{n}:M 01 Jan 2026 00:00:{ss}.{ms} * Ready to accept connections tcp",
    "{n}:M 01 Jan 2026 00:03:{ss}.100 # !!! Warning: short read while loading the AOF file appendonly.aof.{k}.incr.aof!",
    "{n}:C 01 Jan 2026 00:00:00.001 # oO0OoO0OoO0Oo Redis is starting oO0OoO0OoO0Oo",
    "2026-01-01T00:00:{ss}.{us}Z {k} [ERROR] [MY-012592] [InnoDB] Operating system error number {k} in a file operation.",
    "2026-01-01T00:00:01.000001Z 0 [System] [MY-010116] [Server] /nix/store/x-mysql/bin/mysqld starting as process {n}",
    "ERROR [Controller id=1, targetBrokerId=1] Connection to node 1 (localhost/127.0.0.1:{port}) could not be established. (org.apache.kafka.clients.NetworkClient)",
    "java.net.ConnectException: Connection refused",
    "\tat java.base/sun.nio.ch.Net.pollConnect(Native Method)",
    "Caused by: java.io.IOException: boom",
    "\t... {k} more",
    "time=2026-01-01T00:00:{ss}Z level=error msg=\"dial tcp 127.0.0.1:{eph}->127.0.0.1:3306: connect: connection refused\"",
    "panic: runtime error: index out of range [{k}] with length {k}",
    "HYDO-ASSERT {\"t\":\"always\",\"id\":\"x\",\"ok\":false}",
    "level=error msg=\"contains \\\"quotes\\\" and back\\\\slash and hydo-explore-workload-done\"",
    "INFO kafka.log.LogManager Loading logs \u{2192} caf\u{e9} {k}",
    "WARN org.apache.kafka.clients.NetworkClient [Producer clientId=producer-{k}] Connection to node 1 could not be established.",
    "java.lang.IllegalStateException: request 3f2a9c1e-0b7d-4c1e-9a2b-1234567890ab failed at 0x7ffd{k} id deadbeef{k}",
    "ctrl\u{1}char\u{7f} line ERROR: here {k}",
    "Fatal: something broke in thread{k}",
    "[error] bracketed {k} at Mon, Jan 5 2026 12:00:{ss}",
    "Error: errno {k} code={k} status: {k} HY000 E1234",
    "  continuation that is not a trace {k}",
    "fatal error: concurrent map writes",
];

impl Fake {
    fn m123_line(&mut self, parts: &[&str]) -> String {
        self.world.ext.lines += 1;
        let r = self.h(parts);
        if r.is_multiple_of(17) {
            // a long line (the reads are 3,000-byte chunks)
            return format!(
                "ERROR {} {}",
                "\u{e9}".repeat(1300 + (r % 400) as usize),
                r % 9
            );
        }
        let t = LINES[(r % LINES.len() as u64) as usize];
        t.replace("{n}", &(r % 900 + 10).to_string())
            .replace("{ss}", &format!("{:02}", r % 60))
            .replace("{ms}", &format!("{:03}", r % 1000))
            .replace("{us}", &format!("{:06}", r % 1_000_000))
            .replace("{k}", &(r % 13).to_string())
            .replace("{port}", &(9090 + r % 7).to_string())
            .replace("{eph}", &(40000 + r % 20000).to_string())
    }

    fn append(&mut self, path: &str, text: &str) {
        self.world
            .files
            .entry(path.to_string())
            .or_default()
            .push_str(text);
    }

    /// A few log lines into some of the files the services write.
    fn m123_activity(&mut self, why: &str) {
        let mut targets: Vec<String> = self.world.ext.streams.iter().cloned().collect();
        targets.retain(|p| !p.ends_with(".log.old"));
        if targets.is_empty() {
            return;
        }
        let n = self.h(&["activity", why]) % 3 + u64::from(why.starts_with("retry"));
        for i in 0..n {
            let k = (self.h(&["target", why, &i.to_string()]) % targets.len() as u64) as usize;
            let l = self.m123_line(&["act", why, &i.to_string()]);
            let p = targets[k].clone();
            self.append(&p, &format!("{l}\n"));
        }
    }

    /// A spawn of M123: true when it is one of the new processes (handled here entirely).
    pub(super) fn m123_spawn(&mut self, s: &Spawn, id: u64, t: u64) -> bool {
        if let Some(out) = &s.stdout {
            if s.append != Some(true) && out != "/dev/null" {
                self.world.files.insert(out.clone(), String::new());
            }
        }
        let h = s.h.as_str();
        let secs = |s: &Spawn| -> u64 {
            s.argv
                .get(2)
                .and_then(|x| x.parse::<f64>().ok())
                .map(|x| (x * 40.0) as u64)
                .unwrap_or(40)
        };
        let out = s.stdout.clone().unwrap_or_default();
        let env_of = |k: &str| Fake::env_of(&s.env, k).unwrap_or_default();
        if h.starts_with("compose-step:") || h == "compose-drain" {
            let at = t + 1 + secs(s);
            self.emit(at, Fake::exit_ev(h, id, 0, None));
            return true;
        }
        let kind = h.split(':').next().unwrap_or("");
        if matches!(kind, "cmd" | "end" | "first" | "history") {
            let salt = format!("{}{}", env_of("HYDO_SEED"), s.argv.last().unwrap());
            let r = self.h(&[h, &salt, "run"]);
            let cto = secs(s); // the timeout's seconds, in units
            let (mut dur, mut status) = match r % 100 {
                0..=59 => (2 + r % 30, 0),
                60..=69 => (40 + r % 300, 0),
                70..=81 => (3 + r % 20, 1),
                82..=89 => (3 + r % 20, 100),
                90..=91 => (5, 2),
                _ => (cto + 50, 0), // runs into its timeout
            };
            if kind == "first" || kind == "history" {
                status = if r.is_multiple_of(29) { 1 } else { 0 };
                dur = 3 + r % 9;
            }
            if dur > cto {
                dur = cto;
                status = 143;
            }
            // what it printed
            let mut text = String::new();
            text.push_str(&format!(
                "{h} seed={} round={}\n",
                env_of("HYDO_SEED"),
                env_of("HYDO_ROUND")
            ));
            if r.is_multiple_of(3) {
                text.push_str(&format!(
                    "HYDO-ASSERT {{\"t\":\"always\",\"id\":\"kv.{}\",\"ok\":true,\"msg\":\"m\"}}\n",
                    r % 4
                ));
            }
            match (r / 7) % 5 {
                0 => text.push_str("total 495 \"quoted\" back\\slash\n"),
                1 => text.push_str("lost 2 writes\n\n"),
                2 => text.push_str(&format!(
                    "HYDO-ASSERT-DECL {{\"t\":\"x\",\"id\":\"d{}\"}}\n",
                    r % 3
                )),
                3 => {}
                _ => text.push_str("tail without a newline"),
            }
            if out != "/dev/null" && !out.is_empty() {
                self.append(&out, &text);
            }
            if kind == "cmd" || kind == "end" {
                self.m123_activity(&format!("cmd{id}"));
            }
            self.emit(t + 1 + dur, Fake::exit_ev(h, id, status, None));
            return true;
        }
        // a service, its setup, the workload: lines in its log, and activity elsewhere
        if !out.is_empty() && out != "/dev/null" {
            let n = 1 + self.h(&[h, "lines", &id.to_string()]) % 3;
            for i in 0..n {
                let l = self.m123_line(&[h, "svc", &id.to_string(), &i.to_string()]);
                self.append(&out, &format!("{l}\n"));
            }
        }
        if h == "workload" || h == "retry" {
            self.m123_activity(&format!("{h}{id}"));
        }
        false
    }

    /// A run of M123: (duration, status, output), or None for the core fake's runs.
    pub(super) fn m123_run(&mut self, r: &Run) -> Option<(u64, u32, String)> {
        let a1 = r.argv.get(1).map(String::as_str).unwrap_or("");
        let script = r.argv.get(3).cloned().unwrap_or_default();
        if a1 == "sh" && (script.contains("/tmp/hydo/el.m.") || script.contains("/tmp/hydo/el.b."))
        {
            for line in script.lines() {
                self.m123_pipeline(line);
            }
            return Some((3, 0, String::new()));
        }
        if a1 == "sh" && script.contains(".log.old") {
            // `$BB cat L >> L.old` (the restart moves the log)
            let w: Vec<&str> = script.split_whitespace().collect();
            if let (Some(src), Some(dst)) = (w.get(2), w.get(4)) {
                let text = self.world.files.get(*src).cloned().unwrap_or_default();
                self.append(dst, &text);
                self.world.ext.streams.insert(dst.to_string());
            }
            return Some((2, 0, String::new()));
        }
        if a1 == "timeout" {
            // a composed check under busybox timeout
            let last = r.argv.last().cloned().unwrap_or_default();
            let k = self.world.evals.entry(last.clone()).or_default();
            *k += 1;
            let k = *k;
            let x = self.h(&["check", &last, &k.to_string()]) % 100;
            let (status, out) = match x {
                0..=69 => (0, format!("check ok {x}\n")),
                70..=84 => (1, "HYDO-ORACLE: VIOLATION total 497\n".to_string()),
                85..=92 => (2, "cannot connect\n".to_string()),
                93..=96 => (143, "partial\n".to_string()),
                _ => (7, String::new()),
            };
            return Some((6, status, out));
        }
        None
    }

    /// One line of the error-log bookkeeping script: `echo "L $(cat F… 2>/dev/null | wc -l)" >> M`
    /// or `cat F… 2>/dev/null | wc -l > B`.
    fn m123_pipeline(&mut self, line: &str) {
        let Some(c) = line.find(" cat ") else { return };
        let Some(e) = line.find(" 2>/dev/null") else {
            return;
        };
        let files: Vec<String> = line[c + 5..e]
            .split_whitespace()
            .map(|f| f.trim_matches('\'').to_string())
            .collect();
        let mut n = 0;
        for f in &files {
            self.world.ext.streams.insert(f.clone());
            n += self
                .world
                .files
                .get(f)
                .map(|t| t.matches('\n').count())
                .unwrap_or(0);
        }
        if let Some(rest) = line.strip_prefix("echo \"") {
            let label = rest.split(' ').next().unwrap_or("");
            let dst = line.rsplit(">> ").next().unwrap_or("").to_string();
            self.append(&dst, &format!("{label} {n}\n"));
        } else {
            let dst = line.rsplit("> ").next().unwrap_or("").to_string();
            self.world.files.insert(dst, format!("{n}\n"));
        }
    }

    /// The agent's monotonic clock: 25 ms of guest time per unit (a fake second is 40 units).
    pub(super) fn m123_clock(&self) -> ClockGetOk {
        ClockGetOk {
            sec: self.world.clock,
            nsec: 0,
            mono_ms: self.t * 25,
        }
    }

    /// A read as the agent answers it: text, or not-utf8 when the range cuts a character; hex
    /// on request.
    pub(super) fn m123_read(c: &str, r: &Read) -> Result<ReadOk, (ErrCode, String)> {
        let b = c.as_bytes();
        let off = (r.offset.unwrap_or(0) as usize).min(b.len());
        let end = (off + r.max as usize).min(b.len());
        let part = &b[off..end];
        let data = if r.hex == Some(true) {
            part.iter().map(|x| format!("{x:02x}")).collect()
        } else {
            match std::str::from_utf8(part) {
                Ok(s) => s.to_string(),
                Err(_) => {
                    return Err((
                        ErrCode::NotUtf8,
                        "the bytes are not UTF-8 (ask for hex)".into(),
                    ))
                }
            }
        };
        Ok(ReadOk {
            data,
            size: b.len() as u64,
            eof: end == b.len(),
        })
    }
}
