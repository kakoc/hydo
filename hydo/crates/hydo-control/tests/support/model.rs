//! The shell model: `hydo/onboard/hydo-init.sh` transliterated as plain blocking code, written
//! from the shell text and not from the controller, which checks a conversation's items (the
//! commands the controller sent, its report and log lines, the agent's answers) against what the
//! shell would do in the same order.
//!
//! - The main sequence is checked strictly in order: each expected step is the next item of the
//!   controller's `main` part, built here from the plan and the shell's lines, and every command
//!   the shell waits for must have been answered before the controller's next main item.
//! - The shell's background jobs (`periodic_loop`, `pause_timer`, the stack's `kill_timer` for
//!   X=) are checked each on its own, because their order against the main sequence depends on
//!   guest timing in the shell too. Where the shell reads state the jobs share (whether a service
//!   is alive, which invariant failed first, which records were printed), the model reads it from
//!   the items before the point where the controller's main sequence continues.
//! - The decisions where the shell is ambiguous (HANDOFF.md) are the controller's; the model
//!   checks them as written there (the exit of a killed service before its restart, the end of
//!   a stopped job, commands whose answer the shell ignores not being waited for).
//!
//! The text functions (jesc, inv_detail, the failed ids, the clock text) are written again here,
//! differently, so that the controller's versions are checked too.
//!
//! The extensions M122 drives (liveness, T= and clock_after_cut, BUGGIFY) are modelled in the
//! child module `features` (model_features.rs), from their shell text.

use super::fake::Msg;
use super::Item;
use hydo_agent_proto::msg::*;
use hydo_control::schedule::GuestState;
use hydo_control::{Ending, EngineAction, Plan};
use std::collections::{BTreeMap, BTreeSet};

#[path = "model_features.rs"]
mod features;
mod m123; // history, error logs and the composer (M123)
use features::LiveJob;
// hydo-init-links.sh, hydo-init-freeze.sh, hydo-init-netns.sh and the network faults (M124)
mod net;

/// Why checking stopped early.
#[derive(Debug)]
pub enum Halt {
    /// The conversation ended (die_oracle, finish): not an error.
    Ended,
    Mismatch(String),
}

type R<T> = Result<T, Halt>;

fn bad<T>(m: String) -> R<T> {
    Err(Halt::Mismatch(m))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conv {
    Boot,
    Run,
    Recovery,
    Solo,
}

/// What the run conversation's background jobs were: their labels and cut-off stops.
#[derive(Default, Debug)]
struct Jobs {
    periodic: Vec<usize>,
    /// The environment when start_periodic forked the checkers (`periodic_loop $i &`): each
    /// checker's subshell keeps it.
    periodic_env: Vec<String>,
    /// The stop at which stop_periodic ran (the stop of the next main item).
    periodic_cut: Option<u64>,
    pause: Option<(String, u64, u64)>,
    pause_cut: Option<u64>,
    pause_joined: bool,
    kill: Option<(String, u64)>,
    kill_cut: Option<u64>,
}

pub struct Model<'a> {
    plan: &'a Plan,
    items: &'a [Item],
    main: Vec<usize>,
    k: usize,
    replies: BTreeMap<u64, (usize, Result<Reply, String>)>,
    dones: BTreeMap<u64, (usize, Done)>,
    probes: BTreeMap<u64, (usize, ProbeDone)>,
    exits: BTreeMap<u64, (usize, Exit)>,
    sends: BTreeMap<u64, usize>,
    // the init's state
    env_svc: Option<(String, String)>,
    started: BTreeSet<String>,
    spawn: BTreeMap<String, u64>,
    killed: BTreeSet<String>,
    own_out: BTreeMap<String, String>,
    jobs: Jobs,
    end_stop: u64,
    m: m123::St,
    // hydo-liveness.sh: the anchors' uptime texts, the monitors started, the ones resolved
    live_anchors: BTreeMap<String, String>,
    live_jobs: Vec<LiveJob>,
    live_done: BTreeSet<usize>,
    /// hydo-init-fakes.sh's files and variables (model/fakes.rs).
    pf: fakes::Pf,
    /// The other extensions' shell variables (model/net.rs).
    ext: net::Ext,
}

mod fakes;

const BB: &str = "/bin/busybox";
const PRELUDE: &str = "/etc/hydo/prelude.sh";
const H: &str = "/tmp/hydo";

fn s(x: &str) -> String {
    x.to_string()
}

/// `$BB sh -c ". /etc/hydo/prelude.sh; $1"`
fn run_of(cmd: &str) -> Vec<String> {
    vec![s(BB), s("sh"), s("-c"), format!(". {PRELUDE}; {cmd}")]
}

fn plain(args: &[&str]) -> Vec<String> {
    let mut v = vec![s(BB)];
    v.extend(args.iter().map(|a| s(a)));
    v
}

/// The shell's jesc, written as a filter.
fn jesc(x: &str) -> String {
    x.chars()
        .filter(|c| (*c as u32) >= 32)
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            '"' => vec!['\\', '"'],
            c => vec![c],
        })
        .collect()
}

/// inv_detail, step by step as the pipeline runs.
fn inv_detail(out: &str) -> String {
    let mut ls: Vec<&str> = out.split('\n').collect();
    if ls.last() == Some(&"") {
        ls.pop();
    }
    let mut d = String::new();
    if let Some(l) = ls.iter().filter(|l| l.contains("HYDO-ORACLE:")).next_back() {
        // s/.*HYDO-ORACLE: *// (greedy: the last occurrence)
        let mut t: &str = l;
        while let Some(i) = t.find("HYDO-ORACLE:") {
            t = &t[i + 12..];
        }
        let t = t.trim_start_matches(' ');
        // s/^OK *//; s/^VIOLATION *//
        let t = match t.split_at_checked(2) {
            Some(("OK", r)) => r.trim_start_matches(' '),
            _ => t,
        };
        let t = match t.split_at_checked(9) {
            Some(("VIOLATION", r)) => r.trim_start_matches(' '),
            _ => t,
        };
        d = t.to_string();
    }
    if d.is_empty() {
        d = ls
            .iter()
            .filter(|l| l.chars().any(|c| c != ' '))
            .filter(|l| !l.starts_with("HYDO-ASSERT"))
            .next_back()
            .map(|l| l.to_string())
            .unwrap_or_default();
    }
    d
}

/// failed_ids' awk + sort -n, with a different sort: (number, line).
fn failed_lines(outcomes: &str) -> String {
    let mut last: Vec<(String, String)> = Vec::new();
    for l in outcomes.lines() {
        let mut w = l.split_whitespace();
        let (a, b) = (
            w.next().unwrap_or("").to_string(),
            w.next().unwrap_or("").to_string(),
        );
        last.retain(|(x, _)| *x != a);
        last.push((a, b));
    }
    let mut ids: Vec<(i128, String)> = last
        .into_iter()
        .filter(|(_, o)| o == "failed")
        .map(|(i, _)| {
            let digits: String = i.chars().take_while(|c| c.is_ascii_digit()).collect();
            (digits.parse().unwrap_or(0), i)
        })
        .collect();
    ids.sort();
    ids.into_iter().map(|(_, i)| format!("{i}\n")).collect()
}

/// `date -u +%FT%TZ`, by counting days.
fn utc(mut sec: i64) -> String {
    let mut y = 1970;
    let leap = |y: i64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    loop {
        let len = if leap(y) { 366 } else { 365 } * 86400;
        if sec < len {
            break;
        }
        sec -= len;
        y += 1;
    }
    let ml = [
        31,
        if leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while sec >= ml[m] * 86400 {
        sec -= ml[m] * 86400;
        m += 1;
    }
    let d = sec / 86400 + 1;
    let r = sec % 86400;
    format!(
        "{y:04}-{:02}-{d:02}T{:02}:{:02}:{:02}Z",
        m + 1,
        r / 3600,
        r % 3600 / 60,
        r % 60
    )
}

fn secs6(ms: u64) -> String {
    format!("{:.6}", ms as f64 / 1000.0)
}

fn secs3(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

impl<'a> Model<'a> {
    pub fn new(plan: &'a Plan, items: &'a [Item], at: Option<&GuestState>) -> Model<'a> {
        let mut m = Model {
            plan,
            items,
            main: Vec::new(),
            k: 0,
            replies: BTreeMap::new(),
            dones: BTreeMap::new(),
            probes: BTreeMap::new(),
            exits: BTreeMap::new(),
            sends: BTreeMap::new(),
            env_svc: None,
            started: BTreeSet::new(),
            spawn: BTreeMap::new(),
            killed: BTreeSet::new(),
            own_out: BTreeMap::new(),
            jobs: Jobs::default(),
            pf: fakes::Pf::default(),
            end_stop: u64::MAX,
            m: m123::St::default(),
            live_anchors: BTreeMap::new(),
            live_jobs: Vec::new(),
            live_done: BTreeSet::new(),
            ext: net::Ext::default(),
        };
        for (i, it) in items.iter().enumerate() {
            match it {
                Item::Got { msg, .. } => match msg {
                    Msg::Reply(id, r) => {
                        m.replies.insert(*id, (i, Ok(r.clone())));
                    }
                    Msg::Refused(id, _, _, why) => {
                        m.replies.insert(*id, (i, Err(why.clone())));
                    }
                    Msg::Event(Event::Done(d)) => {
                        m.dones.insert(d.id, (i, d.clone()));
                    }
                    Msg::Event(Event::Probe(p)) => {
                        m.probes.insert(p.id, (i, p.clone()));
                    }
                    Msg::Event(Event::Exit(e)) => {
                        m.exits.insert(e.id, (i, e.clone()));
                    }
                    _ => {}
                },
                Item::Send { id, by, .. } => {
                    m.sends.insert(*id, i);
                    if by == "main" {
                        m.main.push(i);
                    }
                }
                Item::End { n, .. } => {
                    m.end_stop = *n;
                    m.main.push(i);
                }
                it if it.by() == Some("main") => m.main.push(i),
                _ => {}
            }
        }
        if let Some(g) = at {
            m.started = g.started.clone();
            m.spawn = g.running.clone();
            if let (Some(a), Some(b)) = (g.env.get("HYDO_SVC"), g.env.get("HYDO_LOG")) {
                m.env_svc = Some((a.clone(), b.clone()));
            }
        }
        m
    }

    // ------------------------------------------------------------------ reading the items

    /// The item index where the controller's main sequence continues.
    fn point(&self) -> usize {
        self.main.get(self.k).copied().unwrap_or(self.items.len())
    }

    fn next_main(&mut self, want: &str) -> R<&'a Item> {
        let Some(&i) = self.main.get(self.k) else {
            return bad(format!(
                "the controller's main part ended; the shell does next: {want}"
            ));
        };
        self.k += 1;
        Ok(&self.items[i])
    }

    fn send(&mut self, want: Command) -> R<u64> {
        let what = format!("{want:?}");
        match self.next_main(&what)? {
            Item::Send { id, cmd, .. } if *cmd == want => Ok(*id),
            other => bad(format!(
                "the shell sends {what}\n  the controller: {other:?}"
            )),
        }
    }

    fn note(&mut self, line: String) -> R<()> {
        let line = if line.starts_with("hydo-stack:") {
            line
        } else {
            format!("hydo-ob: {line}")
        };
        match self.next_main(&line)? {
            Item::Note { line: l, .. } if *l == line => Ok(()),
            other => bad(format!(
                "the shell logs {line:?}\n  the controller: {other:?}"
            )),
        }
    }

    fn report(&mut self, line: String) -> R<()> {
        match self.next_main(&line)? {
            Item::Report { line: l, .. } if *l == line => Ok(()),
            other => bad(format!(
                "the shell prints {line:?}\n  the controller: {other:?}"
            )),
        }
    }

    /// Every command sent before this point was answered before it (the snapshot and end rule).
    fn settled(&self) -> R<()> {
        let p = self.point();
        for (id, si) in &self.sends {
            if *si < p && self.replies.get(id).is_none_or(|(ri, _)| *ri >= p) {
                return bad(format!(
                    "command {id} was not answered before the snapshot or the end"
                ));
            }
        }
        Ok(())
    }

    fn snap(&mut self, name: &str) -> R<()> {
        self.settled()?;
        match self.next_main(name)? {
            Item::Snap { name: n, .. } if n == name => Ok(()),
            other => bad(format!(
                "the shell is at {name}\n  the controller: {other:?}"
            )),
        }
    }

    fn engine(&mut self, a: EngineAction) -> R<()> {
        match self.next_main("go")? {
            Item::Engine { action, .. } if *action == a => Ok(()),
            other => bad(format!(
                "the shell waits for go\n  the controller: {other:?}"
            )),
        }
    }

    fn end<T>(&mut self, e: Ending) -> R<T> {
        self.settled()?;
        match self.next_main("the end")? {
            Item::End { end, .. } if *end == e => Err(Halt::Ended),
            other => bad(format!(
                "the shell ends with {e:?}\n  the controller: {other:?}"
            )),
        }
    }

    /// An answer the shell waits for: it must be there before the controller goes on.
    fn waited(&self, what: &str, at: Option<usize>) -> R<()> {
        match at {
            Some(i) if i < self.point() => Ok(()),
            Some(_) => bad(format!("the controller went on before {what} came")),
            None => bad(format!("{what} never came, but the controller went on")),
        }
    }

    fn reply(&self, id: u64) -> R<Result<Reply, String>> {
        let r = self.replies.get(&id);
        self.waited(&format!("the reply to {id}"), r.map(|x| x.0))?;
        Ok(r.unwrap().1.clone())
    }

    fn done(&self, id: u64) -> R<Done> {
        let d = self.dones.get(&id);
        self.waited(&format!("the end of run {id}"), d.map(|x| x.0))?;
        Ok(d.unwrap().1.clone())
    }

    /// `run` as the shell's synchronous command: (status, output).
    fn run(
        &mut self,
        argv: Vec<String>,
        env: Vec<String>,
        capture: Option<u32>,
    ) -> R<(u32, String)> {
        let id = self.send(Command::Run(Run {
            argv,
            env: Some(env),
            cwd: None,
            uid: None,
            gid: None,
            netns: None,
            kill_after_ms: None,
            capture,
        }))?;
        match self.reply(id)? {
            Err(e) => Ok((127, e)),
            Ok(_) => {
                let d = self.done(id)?;
                Ok((d.status, d.out))
            }
        }
    }

    /// A command with its output in a file: the exit status.
    fn spawn_wait(
        &mut self,
        h: &str,
        argv: Vec<String>,
        env: Vec<String>,
        out: &str,
        append: bool,
    ) -> R<u32> {
        let id = self.send(spawn(h, argv, env, out, append))?;
        if self.reply(id)?.is_err() {
            return Ok(127);
        }
        let e = self.exits.get(&id);
        self.waited(&format!("the exit of {h}"), e.map(|x| x.0))?;
        Ok(e.unwrap().1.status)
    }

    fn read_all(&mut self, path: &str) -> R<String> {
        let mut text = String::new();
        loop {
            let id = self.send(Command::Read(Read {
                path: s(path),
                offset: if text.is_empty() {
                    None
                } else {
                    Some(text.len() as u64)
                },
                max: 3000,
                hex: None,
            }))?;
            match self.reply(id)? {
                Ok(Reply::Read(r)) => {
                    text.push_str(&r.data);
                    if r.eof || r.data.is_empty() {
                        return Ok(text);
                    }
                }
                _ => return Ok(text),
            }
        }
    }

    /// The environment init's children get.
    fn env(&self, extra: &[(&str, String)]) -> Vec<String> {
        let mut v = vec![
            s("HOME=/"),
            s("TERM=linux"),
            format!("HYDO_STEPS={}", self.plan.steps),
            format!("HYDO_ACKED={H}/acked"),
            format!("HYDO_OUTCOMES={H}/outcomes"),
            format!("HYDO_FAILED={H}/failed"),
        ];
        if let Some((a, b)) = &self.env_svc {
            v.push(format!("HYDO_SVC={a}"));
            v.push(format!("HYDO_LOG={b}"));
        }
        if self.ext.xtables {
            // fc_arm: export XTABLES_LOCKFILE=$H/xtables.lock
            v.push(format!("XTABLES_LOCKFILE={H}/xtables.lock"));
        }
        for (k, x) in extra {
            v.retain(|e| !e.starts_with(&format!("{k}=")));
            v.push(format!("{k}={x}"));
        }
        v.sort();
        v
    }

    // ------------------------------------------------------------------ shared state

    fn exited_before(&self, spawn: u64, p: usize) -> bool {
        self.exits.get(&spawn).is_some_and(|(i, _)| *i < p)
            || matches!(self.replies.get(&spawn), Some((i, Err(_))) if *i < p)
    }

    /// svc_alive at the point where the main sequence continues.
    fn alive(&self, svc: &str) -> bool {
        match self.spawn.get(svc) {
            None => false,
            Some(id) => !self.exited_before(*id, self.point()),
        }
    }

    fn reports_before(&self, p: usize) -> impl Iterator<Item = &'a str> + '_ {
        self.items[..p].iter().filter_map(|i| match i {
            Item::Report { line, .. } => Some(line.as_str()),
            _ => None,
        })
    }

    fn asserted(&self, id: &str, ok: bool) -> bool {
        let pat = "HYDO-ASSERT {\"t\":";
        let mid = format!("\"id\":\"{id}\",\"ok\":{ok},");
        self.reports_before(self.point())
            .any(|l| l.starts_with(pat) && l.contains(&mid))
    }

    /// $H/violated: the first invariant with a failed record.
    fn violated(&self) -> Option<usize> {
        // $H/violated is written whether the record went to the console or into el.verdict
        let held = self.m.held.iter().flatten().map(String::as_str);
        for l in self.reports_before(self.point()).chain(held) {
            if !l.starts_with("HYDO-ASSERT {") || !l.contains("\"ok\":false,") {
                continue;
            }
            if let Some(i) = self
                .plan
                .invariants
                .iter()
                .position(|v| l.contains(&format!("\"id\":\"{}\",", v.id)))
            {
                return Some(i);
            }
        }
        None
    }

    // ------------------------------------------------------------------ hydo-init.sh

    fn die<T>(&mut self, line: String) -> R<T> {
        self.el_report()?; // die_oracle() { el_report; _el_orig_die_oracle "$@"; }
        self.stop_periodic()?;
        self.report(line.clone())?;
        self.end(Ending::Verdict(line))
    }

    fn stop_periodic(&mut self) -> R<()> {
        if self.plan.freeze {
            // hydo-init-freeze.sh: stop_periodic() { fc_kill; _fc_orig_stop_periodic "$@"; }
            self.fc_kill()?;
        }
        self.stop_periodic_plain();
        Ok(())
    }

    fn stop_periodic_plain(&mut self) {
        if !self.jobs.periodic.is_empty() && self.jobs.periodic_cut.is_none() {
            self.jobs.periodic_cut = Some(
                self.items
                    .get(self.point())
                    .map(|i| i.stop())
                    .unwrap_or(u64::MAX),
            );
        }
    }

    fn declare_all(&mut self) -> R<()> {
        for i in &self.plan.invariants {
            self.report(format!(
                "HYDO-ASSERT-DECL {{\"t\":\"{}\",\"id\":\"{}\",\"msg\":\"{}\"}}",
                i.kind,
                i.id,
                jesc(&i.msg)
            ))?;
        }
        Ok(())
    }

    /// The lines of hydo-init.sh before `case "$PHASE"`.
    fn boot_start(&mut self, phase: &str) -> R<()> {
        self.send(Command::Mkdir(Mkdir {
            path: s(H),
            mode: None,
        }))?;
        for f in ["acked", "outcomes", "failed"] {
            self.send(write(&format!("{H}/{f}"), "", false))?;
        }
        let e = self.env(&[]);
        self.run(
            plain(&["ifconfig", "lo", "127.0.0.1", "netmask", "255.0.0.0", "up"]),
            e.clone(),
            None,
        )?;
        self.run(plain(&["ip", "link", "set", "lo", "up"]), e, None)?;
        let names: Vec<&str> = self.plan.services.iter().map(|x| x.name.as_str()).collect();
        self.note(format!(
            "phase={phase} services=[{}] steps={}",
            names.join(" "),
            self.plan.steps
        ))?;
        // hydo-init-netns.sh runs where /sbin/init reaches it: before main
        self.ns_init()?;
        self.declare_all()
    }

    fn date_s(&mut self, log: bool) -> R<()> {
        let id = self.send(Command::ClockSet(ClockSet {
            sec: Some(self.plan.clock_start),
            step: None,
        }))?;
        if let Ok(Reply::ClockSet(c)) = self.reply(id)? {
            if log {
                self.note(format!("realtime clock set to {}", utc(c.sec)))?;
            }
        }
        Ok(())
    }

    fn mount_data(&mut self, verdict: &str) -> R<()> {
        let (flags, data) = mount_args(&self.plan.data_opts);
        let id = self.send(Command::Mount(Mount {
            src: s("/dev/vda2"),
            dst: s("/data"),
            fstype: s("ext4"),
            flags,
            data,
        }))?;
        if let Err(m) = self.reply(id)? {
            return self.die(format!("{verdict} ({m})"));
        }
        for d in self.plan.data_dirs.clone() {
            self.send(Command::Mkdir(Mkdir {
                path: format!("/data/{d}"),
                mode: None,
            }))?;
        }
        self.note(format!(
            "mounted /dev/vda2 on /data ({})",
            self.plan.data_opts
        ))?;
        self.el_after_mount()
    }

    fn start_svc(&mut self, name: &str) -> R<bool> {
        let sv = self.plan.service(name).unwrap().clone();
        let l = format!("{H}/{name}.log");
        // hydo-init-netns.sh's start_svc: BB="$BB nsenter -n$NSD/<svc> $BB" for all of it
        let ns = self.ns_of(name);
        if self.started.contains(name) {
            let e = self.env(&[]);
            self.run_ns(
                vec![s(BB), s("sh"), s("-c"), format!("{BB} cat {l} >> {l}.old")],
                e,
                None,
                ns.clone(),
            )?;
        }
        self.started.insert(s(name));
        self.env_svc = Some((s(name), l.clone()));
        if !sv.setup.is_empty() {
            let e = self.env(&[]);
            let st = self.spawn_wait_ns(
                &format!("setup:{name}"),
                run_of(&sv.setup),
                e,
                &format!("{H}/{name}.setup.log"),
                true,
                ns.clone(),
            )?;
            if st != 0 {
                self.note(format!("{name} setup FAILED"))?;
                return Ok(false);
            }
        }
        // the decision: a killed service's exit comes before its new process
        if let Some(old) = self.spawn.get(name).copied() {
            let p = self.point();
            if !self.exited_before(old, p) {
                return bad(format!(
                    "{name} was started again before its old process ({old}) exited"
                ));
            }
        }
        let e = self.env(&[]);
        let mut sp = spawn(
            name,
            vec![
                s(BB),
                s("sh"),
                s("-c"),
                format!(". {PRELUDE}; exec {}", sv.cmd),
            ],
            e.clone(),
            &l,
            false,
        );
        if let Command::Spawn(x) = &mut sp {
            x.netns = ns.clone();
        }
        let id = self.send(sp)?;
        self.spawn.insert(s(name), id);
        self.killed.remove(name);
        if self.reply(id)?.is_err() {
            self.note(format!("{name} DIED"))?;
            return Ok(false);
        }
        let pid = self.send(Command::Probe(Probe {
            argv: run_of(&sv.ready),
            env: Some(e),
            cwd: None,
            uid: None,
            gid: None,
            netns: ns,
            every_ms: 1000,
            deadline_ms: sv.timeout as u64 * 1000,
            alive: Some(s(name)),
        }))?;
        if self.reply(pid)?.is_err() {
            self.note(format!("{name} NOT ready after {}s", sv.timeout))?;
            return Ok(false);
        }
        let pr = self.probes.get(&pid);
        self.waited("the readiness probe", pr.map(|x| x.0))?;
        let pr = pr.unwrap().1.clone();
        match pr.why {
            ProbeEnd::Ok => {
                self.note(format!("{name} ready after {} polls", pr.tries - 1))?;
                Ok(true)
            }
            ProbeEnd::Died => {
                self.note(format!("{name} DIED"))?;
                Ok(false)
            }
            ProbeEnd::Deadline => {
                self.note(format!("{name} NOT ready after {}s", sv.timeout))?;
                Ok(false)
            }
        }
    }

    fn start_all(&mut self, verdict: &str, recovery: bool) -> R<()> {
        self.pf_start_all(recovery)?;
        self.live_before_start_all(recovery)?;
        for sv in self.plan.services.clone() {
            if recovery && !sv.recovery {
                continue;
            }
            if !self.start_svc(&sv.name)? {
                return self.die(format!("{verdict} ({})", sv.name));
            }
        }
        Ok(())
    }

    fn restart_dead(&mut self, verdict: &str) -> R<()> {
        // hydo-init-fakes.sh's wrapper is spliced after hydo-init-freeze.sh's, so it runs first
        self.pf_restart_dead()?;
        if self.plan.freeze {
            // hydo-init-freeze.sh: restart_dead() { fc_heal; _fc_orig_restart_dead "$@"; }
            self.fc_heal()?;
        }
        // the decision: every killed service has reported its exit before the alive checks
        let p = self.point();
        for k in self.killed.clone() {
            if let Some(id) = self.spawn.get(&k) {
                if !self.exited_before(*id, p) {
                    return bad(format!("restart_dead went on before the killed {k} exited"));
                }
            }
        }
        for sv in self.plan.services.clone() {
            if self.alive(&sv.name) {
                continue;
            }
            self.note(format!("{} is down; restarting it", sv.name))?;
            if !self.start_svc(&sv.name)? {
                return self.die(format!("{verdict} ({})", sv.name));
            }
        }
        Ok(())
    }

    fn sig(&mut self, svc: &str, sig: Sig) -> R<()> {
        self.send(Command::Signal(Signal {
            h: s(svc),
            sig,
            tree: true,
            wait_stopped: None,
        }))?;
        if sig == Sig::Kill {
            self.killed.insert(s(svc));
        }
        Ok(())
    }

    fn eval_inv(&mut self, i: usize) -> R<(u32, String)> {
        let inv = self.plan.invariants[i].clone();
        let e = self.env(&[]);
        let (rc, out) = match self.cmp_eval(&inv.cmd)? {
            Some(x) => x,
            None => self.run(run_of(&inv.cmd), e, Some(3000))?,
        };
        self.own_out.insert(inv.key.clone(), out.clone());
        let d = inv_detail(&out);
        let ok = match rc {
            0 => true,
            2 => return Ok((2, out)),
            _ => false,
        };
        if !self.asserted(&inv.id, ok) && !self.held_has(&inv.id, ok) {
            self.assert_out(format!(
                "HYDO-ASSERT {{\"t\":\"{}\",\"id\":\"{}\",\"ok\":{ok},\"msg\":\"{}\",\"details\":{{\"out\":\"{}\"}}}}",
                inv.kind,
                inv.id,
                jesc(&inv.msg),
                jesc(&d)
            ))?;
        }
        Ok((if ok { 0 } else { 1 }, out))
    }

    /// The output of a periodic-only invariant's latest evaluation the controller took in.
    fn periodic_out(&self, i: usize) -> String {
        let inv = &self.plan.invariants[i];
        let label = format!("every:{}", inv.key);
        let cut = self.jobs.periodic_cut.unwrap_or(u64::MAX);
        let want = run_of(&inv.cmd);
        let mut last = String::new();
        for it in self.items {
            if let Item::Send {
                id,
                by,
                cmd: Command::Run(r),
                ..
            } = it
            {
                if *by == label && r.argv == want {
                    if let Some((_, d)) = self.dones.get(id) {
                        let at = self.items[self.dones[id].0].stop();
                        if at <= cut {
                            last = d.out.clone();
                        }
                    }
                }
            }
        }
        last
    }

    fn final_verdict(&mut self) -> R<String> {
        self.pf_final_verdict()?;
        let mut nheld = 0;
        let mut first = String::new();
        let mut error: Option<String> = None;
        for i in 0..self.plan.invariants.len() {
            if !self.plan.invariants[i].final_ {
                continue;
            }
            let (rc, out) = self.eval_inv(i)?;
            let id = self.plan.invariants[i].id.clone();
            if rc == 0 {
                nheld += 1;
                if first.is_empty() {
                    first = format!("{id}: {}", inv_detail(&out));
                }
            } else if rc == 2 && self.violated().is_none() {
                error = Some(format!(
                    "HYDO-ORACLE: ERROR invariant {id} could not be evaluated: {}",
                    inv_detail(&out)
                ));
            }
        }
        Ok(if let Some(v) = self.violated() {
            let inv = &self.plan.invariants[v];
            let out = match self.own_out.get(&inv.key) {
                Some(o) => o.clone(),
                None => self.periodic_out(v),
            };
            format!("HYDO-ORACLE: VIOLATION [{}] {}", inv.id, inv_detail(&out))
        } else if let Some(e) = error {
            e
        } else {
            format!("HYDO-ORACLE: OK invariants={nheld} held ({first})")
        })
    }

    fn finish(&mut self) -> R<()> {
        self.hist_final()?; // history's finish wraps liveness's
        self.live_resolve()?;
        let v = self.el_final_verdict()?;
        self.report(v.clone())?;
        self.end(Ending::Verdict(v))
    }

    /// step_range as the init has it: hydo-init-fakes.sh's wrapper when it is spliced in.
    fn step_range(&mut self, a: i64, b: i64) -> R<()> {
        self.el_mark(&a.to_string())?; // step_range() { el_mark "$1"; ... }
        if self.plan.world.is_some() {
            // hydo-init-fakes.sh's step_range, around freeze's
            return self.pf_step_range(a, b);
        }
        self.step_range_orig(a, b)
    }

    /// The step_range hydo-init-fakes.sh wraps: hydo-init-freeze.sh's, or hydo-init.sh's own.
    fn step_range_orig(&mut self, a: i64, b: i64) -> R<()> {
        if self.plan.freeze {
            // hydo-init-freeze.sh's step_range (the declaration, fc_arm before fc_step)
            return self.fc_step_range(a, b);
        }
        self.step_range_plain(a, b)
    }

    fn step_range_plain(&mut self, a: i64, b: i64) -> R<()> {
        if a < b {
            self.step_range_body(a, b)?;
        }
        self.live_after_step_range(b)
    }

    fn step_range_body(&mut self, a: i64, b: i64) -> R<()> {
        let rc = if self.plan.compose.is_some() {
            self.cmp_step(a, b)?
        } else {
            let e = self.env(&[("HYDO_FROM", a.to_string()), ("HYDO_TO", b.to_string())]);
            let mut argv = plain(&["timeout", &self.plan.workload_timeout.to_string()]);
            argv.extend(run_of(&self.plan.workload_run));
            self.spawn_wait("workload", argv, e, &format!("{H}/workload.log"), true)?
        };
        if rc == 143 {
            self.note(format!(
                "workload steps {a}..{} TIMED OUT after {}s (killed)",
                b - 1,
                self.plan.workload_timeout
            ))?;
        }
        self.note(format!("workload steps {a}..{} rc={rc}", b - 1))
    }

    fn retry_workload(&mut self) -> R<()> {
        self.el_mark("end")?; // retry_workload() { el_mark end; ... }
        if self.plan.workload_retry.is_empty() {
            return Ok(());
        }
        let e = self.env(&[]);
        let mut argv = plain(&["timeout", &self.plan.workload_timeout.to_string()]);
        argv.extend(run_of(&self.plan.workload_retry));
        let rc = self.spawn_wait("retry", argv, e, &format!("{H}/workload.log"), true)?;
        self.note(format!("workload retry rc={rc}"))
    }

    fn failed_ids(&mut self) -> R<String> {
        let o = self.read_all(&format!("{H}/outcomes"))?;
        let f = failed_lines(&o);
        // one write when it fits (the corpus never needs more)
        self.send(write(&format!("{H}/failed"), &f, false))?;
        Ok(f)
    }

    fn provision(&mut self) -> R<()> {
        if !self.plan.provision.is_empty() {
            let rc = match self.cmp_provision()? {
                Some(rc) => rc,
                None => {
                    let e = self.env(&[]);
                    self.spawn_wait(
                        "provision",
                        run_of(&self.plan.provision),
                        e,
                        &format!("{H}/provision.log"),
                        false,
                    )?
                }
            };
            if rc != 0 {
                return self.die(s("HYDO-ORACLE: ERROR provision failed"));
            }
        }
        let id = self.send(Command::Syncfs(Syncfs {}))?;
        let _ = self.reply(id)?;
        self.note(s("provisioning synced"))?;
        self.report(s("HYDO-SETUP-COMPLETE"))
    }

    fn set_knob(&mut self, letter: &str, value: &str) -> R<()> {
        if letter == "W" && self.plan.compose.is_some() {
            return self.cmp_setw(value); // the composer's knob (default '-')
        }
        if self.plan.buggify && letter == "B" {
            return self.buggify_knob(value);
        }
        if self.plan.freeze && letter == "F" {
            // hydo-init-freeze.sh: set_knob() { if [ "$1" = F ]; then fc_parse "$2"; else ...
            return self.fc_parse(value);
        }
        let k = self.plan.knobs.iter().find(|k| k.token == letter).cloned();
        let Some(k) = k.filter(|k| !k.default.is_empty()) else {
            return self.note(format!("knob {letter} not in the manifest, ignored"));
        };
        if value == k.default {
            return Ok(());
        }
        let e = self.env(&[("HYDO_VALUE", s(value))]);
        let (rc, _) = self.run(run_of(&k.set), e, None)?;
        if rc != 0 {
            return self.die(format!("HYDO-ORACLE: ERROR knob {letter}={value} failed"));
        }
        self.note(format!("knob {letter}={value} ({})", k.name))
    }

    fn xassert(&mut self, t: &str, id: &str, msg: &str) -> R<()> {
        self.report(format!(
            "HYDO-ASSERT {{\"t\":\"{t}\",\"id\":\"{id}\",\"ok\":true,\"msg\":\"{msg}\"}}"
        ))
    }

    /// explore_run, from `CMD="run <tokens>"`.
    fn explore_run(&mut self, cmd: &str) -> R<()> {
        self.note(format!("command: {cmd}"))?;
        let (mut kk, mut cc, mut pp, mut xx, mut tt) = (None, None, None, None, None);
        let (mut nn, mut oo): (Option<String>, Option<String>) = (None, None);
        for t in cmd.split(' ').skip(1) {
            let (l, v) = t.split_once('=').unwrap();
            match l {
                "K" => kk = Some(s(v)),
                "C" => cc = Some(s(v)),
                "P" => pp = Some(s(v)),
                "X" => xx = Some(s(v)),
                "N" => nn = Some(s(v)),
                "O" => oo = Some(s(v)),
                "T" => tt = Some(s(v)),
                // hydo-init-fakes.sh's set_knob: `if [ "$1" = H ]; then pf_add "$2"; ...`
                "H" if self.plan.world.is_some() => self.pf_add(v)?,
                _ => self.set_knob(l, v)?,
            }
        }
        for v in [
            &mut self.ext.nfrom,
            &mut self.ext.nto,
            &mut self.ext.ofrom,
            &mut self.ext.oto,
        ] {
            *v = s("-1");
        }
        let decls = [
            "reachable fault.paused a service was paused (SIGSTOP)",
            "sometimes fault.pause_mid_request a pause began while a client request was in flight",
            "reachable fault.clock_back the realtime clock was stepped backwards",
            "reachable fault.clock_fwd the realtime clock was stepped forwards",
            "reachable fault.oneway_req a one-way network fault hit the req direction",
            "reachable fault.oneway_resp a one-way network fault hit the resp direction",
        ];
        let mut ds: Vec<&str> = decls.to_vec();
        if xx.is_some() {
            ds.push("reachable fault.killed_timer a service was killed by a timer while the workload ran");
        }
        for d in ds {
            let mut w = d.splitn(3, ' ');
            let (dt, di, msg) = (w.next().unwrap(), w.next().unwrap(), w.next().unwrap());
            self.report(format!(
                "HYDO-ASSERT-DECL {{\"t\":\"{dt}\",\"id\":\"{di}\",\"msg\":\"{msg}\"}}"
            ))?;
        }
        let known = |m: &Model, x: &str| m.plan.services.iter().any(|v| v.name == x);
        // ${K%@*}, ${K#*@}, ${r%+*}, ${r#*+}
        let before_last = |x: &str, c: char| x.rfind(c).map(|i| s(&x[..i])).unwrap_or(s(x));
        let after_first = |x: &str, c: char| x.find(c).map(|i| s(&x[i + 1..])).unwrap_or(s(x));
        let num = |x: &str| -> i64 { x.parse().unwrap() };
        let (mut ksvc, mut kat, mut kup) = (String::new(), -1, -1);
        if let Some(k) = &kk {
            ksvc = before_last(k, '@');
            let r = after_first(k, '@');
            kat = num(&before_last(&r, '+'));
            kup = kat + num(&after_first(&r, '+'));
            if !known(self, &ksvc) {
                self.note(format!("K: no service {ksvc}, ignored"))?;
                kat = -1;
                kup = -1;
            }
        }
        if let Some(n) = &nn {
            self.read_n(n)?;
        }
        let cat = cc.as_deref().map(num).unwrap_or(-1);
        let (mut psvc, mut pat, mut pup, mut poff, mut pdur) = (String::new(), -1, -1, None, 0u64);
        if let Some(p) = &pp {
            psvc = before_last(p, '@');
            let r = after_first(p, '@');
            if r.contains('~') {
                pat = num(&before_last(&r, '~'));
                let r2 = after_first(&r, '~');
                poff = Some(before_last(&r2, ':').parse::<u64>().unwrap());
                pdur = after_first(&r2, ':').parse().unwrap();
            } else {
                pat = num(&before_last(&r, '+'));
                pup = pat + num(&after_first(&r, '+'));
            }
            if !known(self, &psvc) {
                self.note(format!("P: no service {psvc}, ignored"))?;
                pat = -1;
                pup = -1;
            }
        }
        if let Some(o) = &oo {
            self.read_o(o)?;
        }
        let (mut xsvc, mut xat, mut xoff) = (String::new(), -1, 0u64);
        if let Some(x) = &xx {
            xsvc = before_last(x, '@');
            let r = after_first(x, '@');
            xat = num(&before_last(&r, '~'));
            xoff = after_first(&r, '~').parse().unwrap();
            if !known(self, &xsvc) {
                self.note(format!("X: no service {xsvc}, ignored"))?;
                xat = -1;
            }
        }
        // tat=${T%%:*}; tdelta=${T#*:}
        let (mut tat, mut tdelta) = (-1, s("0"));
        if let Some(t) = &tt {
            let i = t.find(':').unwrap();
            tat = num(&t[..i]);
            tdelta = s(&t[i + 1..]);
        }
        let steps = self.plan.steps;
        let mut pts: Vec<i64> = vec![0, kat, kup, cat, pat, pup, tat, xat, steps];
        pts.extend(self.net_points());
        pts.retain(|p| *p >= 0 && *p <= steps);
        pts.sort();
        pts.dedup();
        for f in ["acked", "outcomes", "workload.log"] {
            self.send(write(&format!("{H}/{f}"), "", false))?;
        }
        self.start_periodic();
        self.live_anchor("start")?;
        self.hist_declare()?; // history's start_periodic wraps liveness's
        let (mut down, mut cut, mut paused) = (false, false, false);
        let mut prev: Option<i64> = None;
        for p in pts {
            if let Some(a) = prev {
                if a < p {
                    self.step_range(a, p)?;
                }
            }
            if p == cat {
                cut = true;
                break;
            }
            self.at_nto(&p.to_string())?;
            if p == kup && down {
                self.note(format!("restarting {ksvc} before step {p}"))?;
                if !self.start_svc(&ksvc.clone())? {
                    return self.die(format!("HYDO-ORACLE: ERROR {ksvc} restart failed"));
                }
                down = false;
            }
            if p == xat {
                self.jobs.kill = Some((xsvc.clone(), xoff));
                self.note(format!(
                    "kill timer for {xsvc} armed before step {p} (after {xoff}ms)"
                ))?;
            }
            if p == kat {
                self.sig(&ksvc.clone(), Sig::Kill)?;
                down = true;
                self.note(format!("KILLED {ksvc} before step {p}"))?;
            }
            self.at_n_o(&p.to_string())?;
            if p == pup && paused {
                self.sig(&psvc.clone(), Sig::Cont)?;
                paused = false;
                self.note(format!("RESUMED {psvc} before step {p}"))?;
            }
            if p == pat {
                if let Some(off) = poff {
                    self.jobs.pause = Some((psvc.clone(), off, pdur));
                    self.note(format!("pause timer for {psvc} armed before step {p} (after {off}ms, for {pdur}ms)"))?;
                } else {
                    self.sig(&psvc.clone(), Sig::Stop)?;
                    paused = true;
                    self.note(format!("PAUSED {psvc} before step {p}"))?;
                    self.xassert(
                        "reachable",
                        "fault.paused",
                        "a service was paused (SIGSTOP)",
                    )?;
                }
            }
            if p == tat {
                self.clock_jump(p, &tdelta.clone())?;
            }
            prev = Some(p);
        }
        if cut {
            let here = self
                .items
                .get(self.point())
                .map(|i| i.stop())
                .unwrap_or(u64::MAX);
            if self.jobs.pause.is_some() {
                self.jobs.pause_cut = Some(here);
            }
            if self.jobs.kill.is_some() {
                self.jobs.kill_cut = Some(here);
            }
            self.stop_periodic()?;
            let f = self.failed_ids()?;
            // hydo-init-clock.sh's failed_ids appends clock=<epoch>; hydo-init-fakes.sh's (which
            // wraps it) then appends the world's dump: the acked file is ids, clock=, records
            let clock = self.clock_at_cut()?;
            let dump = self.pf_dump()?;
            // hydo-init-errlog.sh's failed_ids around the fakes': the services stopped, the records
            self.el_at_cut()?;
            let a = self.read_all(&format!("{H}/acked"))?;
            self.report(format!(
                "hydo-explore-acked: {}",
                format!("{a}{clock}{dump}").replace('\n', " ")
            ))?;
            let a = a + &dump;
            self.report(format!("hydo-explore-failed: {}", f.replace('\n', " ")))?;
            for sv in self.plan.services.clone() {
                self.sig(&sv.name, Sig::Stop)?;
            }
            self.note(format!("POWER CUT before step {cat} (all services frozen)"))?;
            self.report(s("hydo-explore-workload-done cut"))?;
            return self.end(self.pf_hand_over(&a, &f));
        }
        self.net_at_end()?;
        if self.jobs.pause.is_some() {
            // `wait $ptimer`: the timer's last line comes before the main sequence goes on
            self.jobs.pause_joined = true;
            let p = self.point();
            if let Some(last) = self
                .items
                .iter()
                .rposition(|i| i.by() == Some("pause-timer"))
            {
                if last > p {
                    return bad(s("the main part went on before the pause timer ended"));
                }
            }
            self.sig(&psvc.clone(), Sig::Cont)?;
        }
        if paused {
            self.sig(&psvc.clone(), Sig::Cont)?;
            self.note(format!("RESUMED {psvc} at the end"))?;
        }
        if self.jobs.kill.is_some() {
            let p = self.point();
            if let Some(last) = self
                .items
                .iter()
                .rposition(|i| i.by() == Some("kill-timer"))
            {
                if last > p {
                    return bad(s("the main part went on before the kill timer ended"));
                }
            }
        }
        self.restart_dead("HYDO-ORACLE: ERROR restart after the faults")?;
        self.retry_workload()?;
        self.stop_periodic()?;
        let f = self.failed_ids()?;
        let a = self.read_all(&format!("{H}/acked"))?;
        self.report(format!("hydo-explore-acked: {}", a.replace('\n', " ")))?;
        self.report(format!("hydo-explore-failed: {}", f.replace('\n', " ")))?;
        self.report(s("hydo-explore-workload-done"))?;
        self.engine(EngineAction::DiskFaultsOff)?;
        self.note(s("command: go"))?;
        self.restart_dead("HYDO-ORACLE: VIOLATION service cannot recover after the faults")?;
        self.finish()
    }

    fn conversation(&mut self, conv: Conv, cmd: &str, _ids: (&[String], &[String])) -> R<()> {
        match conv {
            Conv::Boot => {
                self.boot_start("serve")?;
                self.snap("S0")?;
                self.note(s("command: provision"))?;
                self.date_s(true)?;
                self.mount_data("HYDO-ORACLE: ERROR mount data")?;
                self.start_all("HYDO-ORACLE: ERROR service start", false)?;
                self.provision()?;
                self.snap("S1")?;
                self.end(Ending::AtSnapshot(s("S1")))
            }
            Conv::Solo => {
                self.boot_start("solo")?;
                self.date_s(false)?;
                self.mount_data("HYDO-ORACLE: ERROR mount data")?;
                self.start_all("HYDO-ORACLE: ERROR service start", false)?;
                self.provision()?;
                self.start_periodic();
                self.live_anchor("start")?;
                self.hist_declare()?;
                self.step_range(0, self.plan.steps)?;
                self.retry_workload()?;
                self.stop_periodic()?;
                self.failed_ids()?;
                self.finish()
            }
            Conv::Run => self.explore_run(cmd),
            Conv::Recovery => {
                self.m.recovery = true;
                self.note(format!("command: {cmd}"))?;
                self.date_s(true)?;
                if self.plan.world.is_some() {
                    // the oracle loop, with hydo-init-fakes.sh's start_all after it
                    self.pf_oracle_loop(cmd)?;
                } else {
                    // `for id in ${CMD#oracle}; do ... echo "$id" >> $dst; done`, each list in one
                    // write; with hydo-init-clock.sh, `sed -i '/^clock=/d'` takes the clock lines out
                    // again (the controller never writes them)
                    let mut lists: [Vec<&str>; 2] = [Vec::new(), Vec::new()];
                    let mut dst = 0;
                    for id in cmd.strip_prefix("oracle").unwrap_or(cmd).split_whitespace() {
                        if id == "failed" {
                            dst = 1;
                        } else if !(self.plan.clock_after_cut.is_some() && id.starts_with("clock="))
                        {
                            lists[dst].push(id);
                        }
                    }
                    for (f, list) in [("acked", &lists[0]), ("failed", &lists[1])] {
                        let text: String = list.iter().map(|i| format!("{i}\n")).collect();
                        if !text.is_empty() {
                            self.send(write(&format!("{H}/{f}"), &text, true))?;
                        }
                    }
                }
                self.clock_recovery(cmd)?;
                self.mount_data("HYDO-ORACLE: VIOLATION data disk unmountable")?;
                self.note(s("hydo-stack: recovering from log"))?;
                self.start_all(
                    "HYDO-ORACLE: VIOLATION service cannot recover (committed data unrecoverable)",
                    true,
                )?;
                self.finish()
            }
        }
    }

    // ------------------------------------------------------------------ the background jobs

    /// A job's items, and the stop after which it takes in no answer and sends nothing.
    fn job_items(&self, label: &str) -> Vec<&'a Item> {
        self.items
            .iter()
            .filter(|i| i.by() == Some(label))
            .collect()
    }
}

fn spawn(h: &str, argv: Vec<String>, env: Vec<String>, out: &str, append: bool) -> Command {
    Command::Spawn(Spawn {
        h: s(h),
        argv,
        env: Some(env),
        cwd: None,
        uid: None,
        gid: None,
        netns: None,
        stdout: Some(s(out)),
        stderr: Some(s(out)),
        append: if append { Some(true) } else { None },
        kill_after_ms: None,
    })
}

fn write(path: &str, data: &str, append: bool) -> Command {
    Command::Write(Write {
        path: s(path),
        data: s(data),
        append: if append { Some(true) } else { None },
        mode: None,
    })
}

/// busybox mount -o, for the data disk options the examples use.
fn mount_args(opts: &str) -> (Option<Vec<MountFlag>>, Option<String>) {
    let mut f: Vec<MountFlag> = Vec::new();
    let mut d: Vec<&str> = Vec::new();
    for o in opts.split(',') {
        match o {
            "" | "rw" | "defaults" => f.retain(|x| *x != MountFlag::Ro || o != "rw"),
            "ro" => f.push(MountFlag::Ro),
            "noatime" => f.push(MountFlag::Noatime),
            "nodev" => f.push(MountFlag::Nodev),
            "nosuid" => f.push(MountFlag::Nosuid),
            "noexec" => f.push(MountFlag::Noexec),
            "relatime" => f.push(MountFlag::Relatime),
            "sync" => f.push(MountFlag::Sync),
            other => d.push(other),
        }
    }
    (
        (!f.is_empty()).then_some(f),
        (!d.is_empty()).then(|| d.join(",")),
    )
}

fn done_at(m: &Model, id: u64) -> Option<(usize, u64)> {
    m.dones.get(&id).map(|(i, _)| (*i, m.items[*i].stop()))
}

/// Check one conversation; `cmd` is the shell's command line for it (the guest part of a run, or
/// the recovery's oracle line), `at` the guest state it starts from.
pub fn check(
    plan: &Plan,
    items: &[Item],
    conv: Conv,
    cmd: &str,
    ids: (&[String], &[String]),
    at: Option<&GuestState>,
) -> Result<(), String> {
    let mut m = Model::new(plan, items, at);
    // a resumed conversation starts with the runner's resynchronisation
    if let Some(Item::Send { by, cmd: c, .. }) =
        items.iter().find(|i| matches!(i, Item::Send { .. }))
    {
        if at.is_some() && (by != "runner" || !matches!(c, Command::Sync(_))) {
            return Err(format!(
                "a resumed conversation must start with the runner's sync, not {c:?}"
            ));
        }
    }
    match m.conversation(conv, cmd, ids) {
        Ok(()) => return Err("the model ran past the end".into()),
        Err(Halt::Mismatch(e)) => return Err(e),
        Err(Halt::Ended) => {}
    }
    if m.k != m.main.len() {
        return Err(format!(
            "the controller's main part did more than the shell: {:?}",
            &m.main[m.k..].iter().map(|i| &items[*i]).collect::<Vec<_>>()
        ));
    }
    // the background jobs, each on its own
    let env = m.env_at_jobs();
    for &i in &m.jobs.periodic.clone() {
        m.check_periodic(i, &env)?;
    }
    if let Some((svc, off, dur)) = m.jobs.pause.clone() {
        m.check_pause(&svc, off, dur, &env)?;
    }
    if let Some((svc, off)) = m.jobs.kill.clone() {
        m.check_kill(&svc, off, &env)?;
    }
    m.check_composer()?;
    m.live_rest()?;
    m.check_freeze_watch()?;
    for it in items {
        if let Some(by) = it.by() {
            let known = by == "main"
                || by == "runner"
                || (by == "pause-timer" && m.jobs.pause.is_some())
                || (by == "kill-timer" && m.jobs.kill.is_some())
                || (plan.compose.is_some() && (by == "composer" || by.starts_with("cmd:")))
                || m.live_jobs
                    .iter()
                    .any(|j| by == format!("live:{}", plan.invariants[j.i].key))
                || (by == "freeze" && m.ext.fc_sent.is_some())
                || m.jobs
                    .periodic
                    .iter()
                    .any(|i| by == format!("every:{}", plan.invariants[*i].key));
            if !known {
                return Err(format!("an item of a part the shell does not have: {it:?}"));
            }
        }
    }
    Ok(())
}

impl Model<'_> {
    /// start_periodic: `periodic_loop $i &` for every invariant with `every`; each subshell keeps
    /// the environment of this moment.
    fn start_periodic(&mut self) {
        self.jobs.periodic = (0..self.plan.invariants.len())
            .filter(|i| !self.plan.invariants[*i].every.is_empty())
            .collect();
        self.jobs.periodic_env = self.env(&[]);
    }

    /// The jobs run with the environment the init had when it started them (after the knobs).
    fn env_at_jobs(&self) -> Vec<String> {
        // in a run conversation start_svc changes HYDO_SVC/LOG only at a restart, which can come
        // after a job started; the pause and kill timers build each command when they send it, so
        // their env is checked loosely (the periodic checkers keep the environment of their
        // start: Jobs::periodic_env, checked command by command)
        self.env(&[])
    }

    fn check_periodic(&self, i: usize, _env: &[String]) -> Result<(), String> {
        let inv = &self.plan.invariants[i];
        let label = format!("every:{}", inv.key);
        let items = self.job_items(&label);
        let cut = self.jobs.periodic_cut;
        let cutoff = cut.unwrap_or(u64::MAX);
        let mut j = 0;
        let pos = |it: &Item| self.items.iter().position(|x| std::ptr::eq(x, it)).unwrap();
        loop {
            // sleep $e
            let Some(it) = items.get(j) else {
                return Ok(());
            };
            let Item::Send {
                id,
                cmd: Command::Run(r),
                n,
                ..
            } = it
            else {
                return Err(format!("{label}: expected its sleep, got {it:?}"));
            };
            if r.argv != plain(&["sleep", &inv.every]) || *n > cutoff {
                return Err(format!(
                    "{label}: expected `sleep {}` before stop {cutoff}, got {it:?}",
                    inv.every
                ));
            }
            if r.env.as_ref() != Some(&self.jobs.periodic_env) {
                return Err(format!(
                    "{label}: its sleep must have the environment of start_periodic {:?}, got {it:?}",
                    self.jobs.periodic_env
                ));
            }
            j += 1;
            match done_at(self, *id) {
                Some((di, dn)) if dn <= cutoff && dn < self.end_stop => {
                    if let Some(nx) = items.get(j) {
                        if pos(nx) < di {
                            return Err(format!("{label} went on before its sleep ended"));
                        }
                    }
                }
                _ => {
                    return if j == items.len() {
                        Ok(())
                    } else {
                        Err(format!("{label} went on after it had to stop"))
                    }
                }
            }
            // eval_inv
            let Some(it) = items.get(j) else {
                return if cut.is_some() || self.end_stop != u64::MAX {
                    Ok(())
                } else {
                    Err(format!("{label} stopped"))
                };
            };
            let Item::Send {
                id,
                cmd: Command::Run(r),
                ..
            } = it
            else {
                return Err(format!("{label}: expected its check, got {it:?}"));
            };
            if r.argv != run_of(&inv.cmd) || r.capture != Some(3000) {
                return Err(format!("{label}: expected its check command, got {it:?}"));
            }
            if r.env.as_ref() != Some(&self.jobs.periodic_env) {
                return Err(format!(
                    "{label}: its check must have the environment of start_periodic {:?}, got {it:?}",
                    self.jobs.periodic_env
                ));
            }
            j += 1;
            let (di, d) = match self.dones.get(id) {
                Some((di, d))
                    if self.items[*di].stop() <= cutoff
                        && self.items[*di].stop() < self.end_stop =>
                {
                    (*di, d.clone())
                }
                _ => {
                    return if j == items.len() {
                        Ok(())
                    } else {
                        Err(format!("{label} went on after it had to stop"))
                    }
                }
            };
            let ok = match d.status {
                0 => true,
                2 => continue,
                _ => false,
            };
            let line = format!(
                "HYDO-ASSERT {{\"t\":\"{}\",\"id\":\"{}\",\"ok\":{ok},\"msg\":\"{}\",\"details\":{{\"out\":\"{}\"}}}}",
                inv.kind,
                inv.id,
                jesc(&inv.msg),
                jesc(&inv_detail(&d.out))
            );
            let mid = format!("\"id\":\"{}\",\"ok\":{ok},", inv.id);
            match items.get(j) {
                Some(Item::Report { line: l, .. }) => {
                    if *l != line {
                        return Err(format!("{label}: expected {line:?}\n  got {l:?}"));
                    }
                    let p = pos(items[j]);
                    if self.items[..p].iter().any(|x| matches!(x, Item::Report { line, .. } if line.starts_with("HYDO-ASSERT {") && line.contains(&mid))) {
                        return Err(format!("{label} printed {line:?} twice"));
                    }
                    let _ = di;
                    j += 1;
                }
                next => {
                    let p = next.map(|x| pos(x)).unwrap_or(self.items.len());
                    if !self.items[..p].iter().any(|x| matches!(x, Item::Report { line, .. } if line.starts_with("HYDO-ASSERT {") && line.contains(&mid))) {
                        return Err(format!("{label} did not print {line:?}"));
                    }
                }
            }
        }
    }

    fn timer_env_ok(&self, got: &Command, argv: &[String]) -> bool {
        matches!(got, Command::Run(r) if r.argv == argv && r.capture.is_none()
            && r.env.as_ref().is_some_and(|e| e.iter().any(|x| x.starts_with("HYDO_STEPS="))))
    }

    fn check_pause(&self, svc: &str, off: u64, dur: u64, _env: &[String]) -> Result<(), String> {
        let label = "pause-timer";
        let items = self.job_items(label);
        let cut = self.jobs.pause_cut;
        let cutoff = cut.unwrap_or(u64::MAX);
        let pos = |it: &Item| self.items.iter().position(|x| std::ptr::eq(x, it)).unwrap();
        let mut j = 0;
        let stop_ok = |j: usize| {
            if j == items.len() {
                Ok(())
            } else {
                Err(format!(
                    "{label} went on after it had to stop: {:?}",
                    &items[j..]
                ))
            }
        };
        // one step: a run whose answer the job waits for
        let runs = |j: &mut usize, argv: Vec<String>| -> Result<Option<Done>, String> {
            let Some(it) = items.get(*j) else {
                return if cut.is_some() || self.end_stop != u64::MAX {
                    Ok(None)
                } else {
                    Err(format!("{label} stopped early"))
                };
            };
            let Item::Send { id, cmd, n, .. } = it else {
                return Err(format!("{label}: expected {argv:?}, got {it:?}"));
            };
            if !self.timer_env_ok(cmd, &argv) || *n > cutoff {
                return Err(format!("{label}: expected {argv:?}, got {it:?}"));
            }
            *j += 1;
            match self.dones.get(id) {
                Some((di, d))
                    if self.items[*di].stop() <= cutoff
                        && self.items[*di].stop() < self.end_stop =>
                {
                    if let Some(nx) = items.get(*j) {
                        if pos(nx) < *di {
                            return Err(format!("{label} went on before {argv:?} ended"));
                        }
                    }
                    Ok(Some(d.clone()))
                }
                _ => Ok(None),
            }
        };
        let Some(_) = runs(&mut j, plain(&["sleep", &secs6(off)]))? else {
            return stop_ok(j);
        };
        let mid = if self.plan.compose.is_some() {
            // `pgrep -f "compose.sh run1"`: a composed command running at the STOP
            match items.get(j) {
                Some(it) => self.cmp_in_flight(pos(it)),
                None => return stop_ok(j),
            }
        } else {
            let Some(pg) = runs(&mut j, plain(&["pgrep", "-f", &self.plan.workload_pgrep]))? else {
                return stop_ok(j);
            };
            pg.status == 0
        };
        let sig = |sg: Sig| {
            Command::Signal(Signal {
                h: s(svc),
                sig: sg,
                tree: true,
                wait_stopped: None,
            })
        };
        let want_seq = |j: &mut usize, w: Item| -> Result<bool, String> {
            match items.get(*j) {
                None => {
                    if matches!(w, Item::Send { .. })
                        && (cut.is_some() || self.end_stop != u64::MAX)
                    {
                        Ok(false)
                    } else {
                        Err(format!("{label} stopped before {w:?}"))
                    }
                }
                Some(it) => {
                    let same = match (it, &w) {
                        (Item::Send { cmd: a, .. }, Item::Send { cmd: b, .. }) => a == b,
                        (Item::Note { line: a, .. }, Item::Note { line: b, .. }) => a == b,
                        (Item::Report { line: a, .. }, Item::Report { line: b, .. }) => a == b,
                        _ => false,
                    };
                    if !same {
                        return Err(format!("{label}: expected {w:?}\n  got {it:?}"));
                    }
                    *j += 1;
                    Ok(true)
                }
            }
        };
        let send = |c: Command| Item::Send {
            n: 0,
            id: 0,
            by: s(label),
            cmd: c,
        };
        let note = |l: String| Item::Note {
            n: 0,
            by: s(label),
            line: l,
        };
        let rep = |l: &str| Item::Report {
            n: 0,
            by: s(label),
            line: s(l),
        };
        if !want_seq(&mut j, send(sig(Sig::Stop)))? {
            return stop_ok(j);
        }
        want_seq(
            &mut j,
            note(format!("hydo-ob: PAUSED {svc} (timer, mid_request={mid})")),
        )?;
        want_seq(&mut j, rep("HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.paused\",\"ok\":true,\"msg\":\"a service was paused (SIGSTOP)\"}"))?;
        if mid {
            want_seq(&mut j, rep("HYDO-ASSERT {\"t\":\"sometimes\",\"id\":\"fault.pause_mid_request\",\"ok\":true,\"msg\":\"a pause began while a client request was in flight\"}"))?;
        }
        let Some(_) = runs(&mut j, plain(&["sleep", &secs3(dur)]))? else {
            return stop_ok(j);
        };
        if !want_seq(&mut j, send(sig(Sig::Cont)))? {
            return stop_ok(j);
        }
        want_seq(&mut j, note(format!("hydo-ob: RESUMED {svc} (timer)")))?;
        if !self.jobs.pause_joined && cut.is_none() && self.end_stop == u64::MAX {
            return Err(format!("{label} was neither joined nor stopped"));
        }
        stop_ok(j)
    }

    fn check_kill(&self, svc: &str, off: u64, _env: &[String]) -> Result<(), String> {
        let label = "kill-timer";
        let items = self.job_items(label);
        let cut = self.jobs.kill_cut;
        let cutoff = cut.unwrap_or(u64::MAX);
        let pos = |it: &Item| self.items.iter().position(|x| std::ptr::eq(x, it)).unwrap();
        let mut j = 0;
        let stop_ok = |j: usize| {
            if j == items.len() {
                Ok(())
            } else {
                Err(format!(
                    "{label} went on after it had to stop: {:?}",
                    &items[j..]
                ))
            }
        };
        let runs = |j: &mut usize, argv: Vec<String>| -> Result<Option<Done>, String> {
            let Some(it) = items.get(*j) else {
                return Ok(None);
            };
            let Item::Send { id, cmd, n, .. } = it else {
                return Err(format!("{label}: expected {argv:?}, got {it:?}"));
            };
            if !self.timer_env_ok(cmd, &argv) || *n > cutoff {
                return Err(format!("{label}: expected {argv:?}, got {it:?}"));
            }
            *j += 1;
            match self.dones.get(id) {
                Some((di, d))
                    if self.items[*di].stop() <= cutoff
                        && self.items[*di].stop() < self.end_stop =>
                {
                    if let Some(nx) = items.get(*j) {
                        if pos(nx) < *di {
                            return Err(format!("{label} went on before {argv:?} ended"));
                        }
                    }
                    Ok(Some(d.clone()))
                }
                _ => Ok(None),
            }
        };
        let Some(_) = runs(&mut j, plain(&["sleep", &secs3(off)]))? else {
            return stop_ok(j);
        };
        let mid = if self.plan.compose.is_some() {
            match items.get(j) {
                Some(it) => self.cmp_in_flight(pos(it)),
                None => return stop_ok(j),
            }
        } else {
            let Some(pg) = runs(&mut j, plain(&["pgrep", "-f", &self.plan.workload_pgrep]))? else {
                return stop_ok(j);
            };
            pg.status == 0
        };
        match items.get(j) {
            None => return stop_ok(j),
            Some(Item::Send {
                cmd: Command::Signal(sg),
                ..
            }) if sg.h == svc && sg.sig == Sig::Kill && sg.tree => j += 1,
            Some(other) => return Err(format!("{label}: expected the kill, got {other:?}")),
        }
        let want = [
            format!("hydo-ob: KILLED {svc} (timer, mid_request={mid})"),
            s("HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.killed_timer\",\"ok\":true,\"msg\":\"a service was killed by a timer while the workload ran\"}"),
        ];
        for w in want {
            match items.get(j) {
                Some(Item::Note { line, .. }) | Some(Item::Report { line, .. }) if *line == w => {
                    j += 1
                }
                other => return Err(format!("{label}: expected {w:?}, got {other:?}")),
            }
        }
        stop_ok(j)
    }
}
