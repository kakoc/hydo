//! The scripted guest's network (M124): the pinned tc's qdiscs per namespace and device (with a
//! netem queue that drains after `qdisc change`, quickly, slowly or not at all), `qlen`, the
//! pinned ip and iptables, the netfilter modules, and the agent's packet trigger (`freeze`,
//! `unfreeze`, the `froze`/`resumed` events and hydo_fc's trace file). Every answer is a pure
//! function of the world's salt, the command and the world's state; rare failures reach the
//! shell's ERROR paths (tc setup failed, freeze rules failed, freeze helper did not start,
//! service network setup failed).

use super::*;

#[derive(Clone, Debug, Default)]
pub struct Dev {
    root: bool,
    netem: bool,
    q0: u64,
    stuck: bool,
    linger: bool,
    /// `qdisc change` refused (a netem the kernel cannot change in place)
    refuse_change: bool,
    changed_at: Option<u64>,
    filters_off_at: Option<u64>,
    sent: u64,
    dropped: u64,
}

#[derive(Clone, Debug)]
struct Armed {
    id: u64,
    fired_at: Option<u64>,
    resumed_at: Option<u64>,
    counted: u64,
}

#[derive(Clone, Debug, Default)]
pub struct NetWorld {
    devs: BTreeMap<(String, String), Dev>,
    freeze: Option<Armed>,
    /// The service a froze event stopped, until its resumed event.
    frozen: Option<String>,
    /// How many tc/ip/iptables commands ran (a salt for their rare failures).
    calls: u64,
}

fn arg_after<'a>(argv: &'a [String], k: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == k)
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
}

impl Dev {
    fn backlog(&self, t: u64) -> u64 {
        match self.filters_off_at {
            // a packet still in its rate window, now and then
            Some(off) if self.linger && t < off + 300 => 1,
            Some(_) => 0,
            None => self.backlog_before(t),
        }
    }

    fn backlog_before(&self, t: u64) -> u64 {
        match self.changed_at {
            None => self.q0,
            Some(_) if self.stuck => self.q0,
            Some(c) => self.q0.saturating_sub((t - c.min(t)) / 4),
        }
    }
}

impl Fake {
    fn net_h(&mut self, parts: &[&str]) -> u64 {
        self.world.net.calls += 1;
        let n = self.world.net.calls.to_string();
        let mut v = parts.to_vec();
        v.push(&n);
        self.h(&v)
    }

    /// `tc -s qdisc show dev <dev>` as kernel 6.6 prints it.
    fn show(&self, ns: &str, dev: &str) -> String {
        let Some(d) = self.world.net.devs.get(&(ns.to_string(), dev.to_string())) else {
            return format!("qdisc noqueue 0: dev {dev} root refcnt 2\n Sent 0 bytes 0 pkt (dropped 0, overlimits 0 requeues 0)\n backlog 0b 0p requeues 0\n");
        };
        let mut s = format!(
            "qdisc prio 1: dev {dev} root refcnt 2 bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1\n Sent {} bytes {} pkt (dropped 0, overlimits 0 requeues 0)\n backlog 0b 0p requeues 0\n",
            d.sent * 90,
            d.sent
        );
        if d.netem {
            let b = d.backlog(self.t);
            s.push_str(&format!(
                "qdisc netem 40: dev {dev} parent 1:4 limit 1000 delay 100ms\n Sent {} bytes {} pkt (dropped {}, overlimits 0 requeues 0)\n backlog {}b {b}p requeues 0\n",
                d.sent * 70,
                d.sent,
                d.dropped,
                b * 66
            ));
        }
        s
    }

    /// The packets queued in the netem qdisc of a device of the guest's own namespace.
    pub(super) fn qlen(&self, dev: &str) -> Option<u64> {
        let d = self
            .world
            .net
            .devs
            .get(&("-".to_string(), dev.to_string()))?;
        d.netem.then(|| d.backlog(self.t))
    }

    fn tc(&mut self, ns: &str, a: &[String]) -> (u32, String) {
        let w: Vec<&str> = a.iter().map(String::as_str).collect();
        let dev = arg_after(a, "dev").unwrap_or("").to_string();
        let key = (ns.to_string(), dev.clone());
        let t = self.t;
        let fail = self.net_h(&["tc", &w.join(" ")]).is_multiple_of(97);
        let exists = "Error: Exclusivity flag on, cannot modify.\n".to_string();
        let none = "Error: Cannot find specified qdisc on specified device.\n".to_string();
        match w.as_slice() {
            ["-s", "qdisc", "show", ..] => (0, self.show(ns, &dev)),
            ["qdisc", "add", _, _, "root", ..] => {
                let d = self.world.net.devs.entry(key).or_default();
                if d.root {
                    (2, exists)
                } else if fail {
                    (2, "RTNETLINK answers: Operation not permitted\n".into())
                } else {
                    d.root = true;
                    (0, String::new())
                }
            }
            ["qdisc", "add", _, _, "parent", "1:4", "handle", "40:", "netem", rest @ ..] => {
                let delay = rest.contains(&"delay");
                let loss = rest.contains(&"loss");
                let q = self.h(&["queued", &w.join(" ")]);
                let d = self.world.net.devs.entry(key).or_default();
                if !d.root || d.netem {
                    return (2, exists);
                }
                d.netem = true;
                d.q0 = if delay { q % 14 } else { q % 3 };
                d.stuck = q.is_multiple_of(13);
                d.linger = q.is_multiple_of(7);
                d.refuse_change = q % 11 == 5;
                d.sent = q % 50;
                d.dropped = if loss { q % 5 } else { 0 };
                (0, String::new())
            }
            ["filter", "add", ..] => match self.world.net.devs.get(&key) {
                Some(d) if d.root && !fail => (0, String::new()),
                _ => (2, "Error: Parent Qdisc doesn't exists.\n".into()),
            },
            ["qdisc", "change", ..] => match self.world.net.devs.get_mut(&key) {
                Some(d) if d.netem && !d.refuse_change => {
                    d.changed_at = Some(t);
                    (0, String::new())
                }
                _ => (2, none),
            },
            ["filter", "del", ..] => match self.world.net.devs.get_mut(&key) {
                Some(d) if d.root => {
                    d.filters_off_at = Some(t);
                    (0, String::new())
                }
                _ => (2, none),
            },
            ["qdisc", "del", _, _, "root"] => match self.world.net.devs.remove(&key) {
                Some(d) if d.root => (0, String::new()),
                _ => (2, none),
            },
            _ => (0, String::new()),
        }
    }

    /// The network commands of a `run`: (duration, status, output), or None when it is not one.
    pub(super) fn net_run(&mut self, _id: u64, r: &Run) -> Option<(u64, u32, String)> {
        let a0 = r.argv.first().cloned().unwrap_or_default();
        let a1 = r.argv.get(1).cloned().unwrap_or_default();
        let script = r.argv.get(3).cloned().unwrap_or_default();
        let ns = r.netns.clone().unwrap_or_else(|| "-".into());
        if a0.ends_with("/bin/tc") {
            let (st, out) = self.tc(&ns, &r.argv[1..]);
            return Some((2, st, out));
        }
        if a0.ends_with("/bin/ip") {
            let w = r.argv[1..].join(" ");
            // 10.89.255.254 is refused (the tests' way to a failed setup)
            if self.net_h(&["ip", &w]).is_multiple_of(401) || w.contains("10.89.255.254") {
                return Some((2, 2, "RTNETLINK answers: File exists\n".into()));
            }
            return Some((2, 0, String::new()));
        }
        if a0.ends_with("iptables-legacy") {
            let w = r.argv[1..].join(" ");
            let add = r.argv.get(1).is_some_and(|x| x == "-A");
            let h = self.net_h(&["ipt", &w]);
            if (add && h.is_multiple_of(31)) || (!add && h.is_multiple_of(11)) {
                return Some((
                    3,
                    1,
                    "iptables: No chain/target/match by that name.\n".into(),
                ));
            }
            return Some((3, 0, String::new()));
        }
        if a1 == "insmod" {
            let st = if self.net_h(&["insmod", &r.argv[2]]).is_multiple_of(5) {
                1
            } else {
                0
            };
            return Some((2, st, String::new()));
        }
        if a1 == "unshare" {
            return Some((2, 0, String::new()));
        }
        if a1 == "sh" && script.contains("disable_ipv6") {
            if script.contains("link set lo up") {
                let bad = self.net_h(&["nsenter", &script]).is_multiple_of(401);
                return Some((4, if bad { 2 } else { 0 }, String::new()));
            }
            let has = !self.net_h(&["v6", &script]).is_multiple_of(4);
            return Some((1, if has { 0 } else { 1 }, String::new()));
        }
        if a1 == "sh" && script.contains("grep -c ' #'") {
            let path = script
                .split_whitespace()
                .rev()
                .nth(1)
                .unwrap_or("")
                .to_string();
            return Some(match self.world.files.get(&path) {
                None => (2, 2, String::new()),
                Some(t) => {
                    let n = t.split('\n').filter(|l| l.contains(" #")).count();
                    (2, if n > 0 { 0 } else { 1 }, format!("{n}\n"))
                }
            });
        }
        None
    }

    /// The agent's `freeze`: arm the trigger; the trace of the pair's packets is written at once,
    /// the n-th counted packet freezes the service (or warns when it is not running), and the
    /// service is continued dur later.
    pub(super) fn freeze(&mut self, id: u64, at: u64, f: Freeze) {
        let refuse = |me: &mut Fake, code: ErrCode, msg: &str| {
            me.emit(at, Msg::Refused(id, "freeze".into(), code, msg.into()));
        };
        if self.world.net.freeze.is_some() {
            return refuse(self, ErrCode::Busy, "a freeze is already armed");
        }
        let Some(p) = self.world.procs.get(&f.h).cloned() else {
            return refuse(self, ErrCode::NoSuchHandle, "no process");
        };
        if self.h(&["bind", &f.h]).is_multiple_of(43) {
            return refuse(self, ErrCode::OsError, "bind nfqueue 7: EPERM (errno 1)");
        }
        self.emit(at, Msg::Reply(id, Reply::Freeze(FreezeOk {})));
        let want_req = f.dir == Dir::Req;
        // the pair's packets from here to the end: fewer the later the freeze is armed
        let done = self
            .world
            .files
            .get("/tmp/hydo/outcomes")
            .map(|t| t.lines().count() as u64)
            .unwrap_or(0);
        let packets = 2 * 30u64.saturating_sub(done) + self.h(&["packets", &f.h]) % 3;
        let (mut nreq, mut nresp, mut counted) = (0u64, 0u64, 0u64);
        let mut log = String::new();
        let mut fired = None;
        for k in 0..packets {
            let is_req = !self.h(&["dir", &k.to_string()]).is_multiple_of(3);
            let kk = if is_req {
                nreq += 1;
                nreq
            } else {
                nresp += 1;
                nresp
            };
            let matched =
                f.pattern.is_none() || self.h(&["match", &k.to_string()]).is_multiple_of(2);
            let hit = is_req == want_req && matched;
            if hit {
                counted += 1;
            }
            let len = 5 + self.h(&["len", &k.to_string()]) % 60;
            let (sp, dp) = if is_req { (40000, 6379) } else { (6379, 40000) };
            let snippet = if is_req {
                format!("*3..$3..SET..$4..k:{k}..$2..v{k}..")
            } else {
                "+OK..".to_string()
            };
            log.push_str(&format!(
                "{}{kk} {sp}>{dp} len={len}{} t={} {snippet}\n",
                if is_req { "req" } else { "resp" },
                if hit {
                    format!(" #{counted}")
                } else {
                    String::new()
                },
                1000 + k * 7
            ));
            if hit && f.n > 0 && counted == f.n {
                fired = Some((k, is_req, kk, sp, dp, len, snippet));
                break;
            }
        }
        if let Some(l) = &f.log {
            self.world.files.insert(l.clone(), log);
        }
        let t = self.t;
        let mut armed = Armed {
            id,
            fired_at: None,
            resumed_at: None,
            counted,
        };
        if let Some((k, is_req, kk, sport, dport, len, snippet)) = fired {
            let fire = t + 6 + 3 * k;
            let dir = |r: bool| if r { Dir::Req } else { Dir::Resp };
            let running = p.running && !p.dying;
            let ev = if running {
                Event::Froze(Froze {
                    id,
                    h: f.h.clone(),
                    procs: 1 + (self.h(&["procs", &f.h]) % 3) as u32,
                    all_stopped: !self.h(&["stopped", &f.h]).is_multiple_of(11),
                    dir: f.dir,
                    count: f.n,
                    kdir: dir(is_req),
                    k: kk,
                    sport,
                    dport,
                    len: len as u32,
                    snippet,
                })
            } else {
                Event::Warn(Warn {
                    code: WarnCode::FreezeNoProcess,
                    msg: format!(
                        "the n-th packet came, but {} is not running; nothing frozen",
                        f.h
                    ),
                })
            };
            self.emit(fire, Msg::Event(ev));
            // dur_ms of guest time: one unit per 200 ms, and now and then a guest so slow that
            // the init's wait (dur/1000 + 30 sleeps of 1 s) ends first
            let slow = self.h(&["slow", &f.h]).is_multiple_of(8);
            let back = fire + 1 + if slow { f.dur_ms * 2 } else { f.dur_ms / 200 };
            self.emit(
                back,
                Msg::Event(Event::Resumed(Resumed {
                    id,
                    procs: if running { 1 } else { 0 },
                })),
            );
            armed.fired_at = Some(fire);
            armed.resumed_at = Some(back);
        }
        if self.h(&["enobufs", &f.h]).is_multiple_of(17) {
            self.emit(
                t + 4,
                Msg::Event(Event::Warn(Warn {
                    code: WarnCode::NetlinkEnobufs,
                    msg: "netlink ENOBUFS (packets dropped)".into(),
                })),
            );
        }
        self.world.net.freeze = Some(armed);
    }

    /// The agent's `unfreeze`: the trigger is off; nothing of it comes any more.
    pub(super) fn unfreeze(&mut self, id: u64, at: u64) {
        let Some(a) = self.world.net.freeze.take() else {
            self.emit(
                at,
                Msg::Refused(
                    id,
                    "unfreeze".into(),
                    ErrCode::NotFound,
                    "no freeze is armed".into(),
                ),
            );
            return;
        };
        let t = self.t;
        let fid = a.id;
        self.pending.retain(|p| {
            !(p.at > t
                && match &p.msg {
                    Msg::Event(Event::Froze(f)) => f.id == fid,
                    Msg::Event(Event::Resumed(r)) => r.id == fid,
                    Msg::Event(Event::Warn(_)) => true,
                    _ => false,
                })
        });
        self.emit(
            at,
            Msg::Reply(
                id,
                Reply::Unfreeze(UnfreezeOk {
                    fired: a.fired_at.is_some_and(|x| x <= t),
                    resumed: a.resumed_at.is_some_and(|x| x <= t),
                    counted: a.counted,
                }),
            ),
        );
    }

    /// A delivered froze/resumed event stops or continues the service in the world.
    pub(super) fn net_deliver(&mut self, m: &Msg) {
        match m {
            Msg::Event(Event::Froze(f)) => {
                if let Some(p) = self.world.procs.get_mut(&f.h) {
                    p.stopped = true;
                }
                self.world.net.frozen = Some(f.h.clone());
            }
            Msg::Event(Event::Resumed(_)) => {
                if let Some(h) = self.world.net.frozen.take() {
                    if let Some(p) = self.world.procs.get_mut(&h) {
                        p.stopped = false;
                    }
                }
            }
            _ => {}
        }
    }
}
