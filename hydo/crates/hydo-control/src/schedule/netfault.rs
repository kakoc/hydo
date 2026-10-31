//! NET-FAULTS (docs/HISTORY.md M124): `N=` and `O=`, netem on a service's port or on the link
//! between two services, and the heal that drains the netem queue (M71).
//!
//! It does what `hydo/onboard/hydo-init.sh` does in explore_run (reading the tokens, the
//! points, `net_on`, `net_heal`/`net_off`, the heal lines), with `hydo-init-links.sh`'s
//! `known_svc`/`net_on` for a manifest whose services have `addr:` and `hydo-init-netns.sh`'s
//! `net_on`/`ns_heal` with `network: bridge`: the same `tc` invocations in the same order through
//! `run` (each waited for, as the shell runs them one after another), the netem queue read with
//! `qlen` (inside a service's namespace, where `qlen` cannot go, by `tc -s qdisc show` through
//! `run` and the shell's awk on the host), and the same drain waits: `sleep 0.01` of guest time
//! between two polls, at most the largest delay in force + 500 ms, then at most 50 ms after the
//! filters are gone.
//!
//! AMBIGUOUS (HANDOFF.md, M124): `$BB nsenter -n<ns> <cmd>` is the agent's `netns` field on the
//! command; `$tc` is one word (the agent's `qlen` takes it as a program path); the agent's `qlen`
//! reads the backlog of the qdisc with handle 40: as the shell's awk does.

use super::*;
use hydo_agent_proto::msg::Qlen;

const PRIOMAP: [&str; 16] = ["1"; 16];

/// What explore_run knows about the network faults (the shell's nsvc/nfrom/nto/nargs,
/// osvc/ofrom/oto/odir/oargs, netup/oneup, NET_ARGS and NET_DEVS).
#[derive(Debug)]
pub(super) struct Net {
    nsvc: String,
    nfrom: i64,
    nto: i64,
    nargs: String,
    osvc: String,
    ofrom: i64,
    oto: i64,
    odir: String,
    oargs: String,
    netup: bool,
    oneup: bool,
    /// NET_ARGS: the netem arguments of the last fault put on.
    net_args: String,
    /// NET_DEVS (network: bridge): the devices of the faults in force, as (namespace file or
    /// "-" for the guest's own, device).
    devs: Vec<(String, String)>,
}

impl Default for Net {
    fn default() -> Net {
        Net {
            nsvc: String::new(),
            nfrom: -1,
            nto: -1,
            nargs: String::new(),
            osvc: String::new(),
            ofrom: -1,
            oto: -1,
            odir: String::new(),
            oargs: String::new(),
            netup: false,
            oneup: false,
            net_args: String::new(),
            devs: Vec::new(),
        }
    }
}

// ------------------------------------------------------------------ the shell's text operations

/// `${v%%c*}`: before the first c (all of v when there is none).
pub(super) fn before_first(v: &str, c: char) -> &str {
    v.split_once(c).map(|x| x.0).unwrap_or(v)
}

/// `${v#*c}`: after the first c.
pub(super) fn after_first(v: &str, c: char) -> &str {
    v.split_once(c).map(|x| x.1).unwrap_or(v)
}

/// `${v%c*}`: before the last c.
pub(super) fn before_last(v: &str, c: char) -> &str {
    v.rsplit_once(c).map(|x| x.0).unwrap_or(v)
}

/// `${v##*c}`: after the last c.
pub(super) fn after_last(v: &str, c: char) -> &str {
    v.rsplit_once(c).map(|x| x.1).unwrap_or(v)
}

fn num(v: &str) -> i64 {
    v.parse().unwrap_or(-1)
}

/// awk's numeric value of a string (`v + 0`): the longest decimal prefix, 0 when there is none.
pub(super) fn awk_num(v: &str) -> f64 {
    let b = v.trim_start_matches([' ', '\t']).as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let d0 = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == d0 || (i == d0 + 1 && b[d0] == b'.') {
        return 0.0;
    }
    let mut end = i;
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let e0 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > e0 {
            end = j;
        }
    }
    let t = v.trim_start_matches([' ', '\t']);
    t[..end].parse().unwrap_or(0.0)
}

/// net_heal's bound: the largest delay in force (delay + 4 x jitter), in ms, + 500 (its awk).
pub(super) fn bound_of(args: &str) -> i64 {
    fn ms(v: &str) -> f64 {
        let x = awk_num(v);
        if v.contains("us") {
            x / 1000.0
        } else if v.contains("ms") {
            x
        } else if v.contains('s') {
            x * 1000.0
        } else {
            x / 1000.0
        }
    }
    fn time_word(v: &str) -> bool {
        // ^[0-9.]+(us|usec|usecs|ms|msec|msecs|s|sec|secs)?$
        let n = v
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'.')
            .count();
        n > 0
            && matches!(
                &v[n..],
                "" | "us" | "usec" | "usecs" | "ms" | "msec" | "msecs" | "s" | "sec" | "secs"
            )
    }
    let f: Vec<&str> = args.split_whitespace().collect();
    let field = |i: usize| f.get(i).copied().unwrap_or("");
    let mut m = 0.0;
    for (i, w) in f.iter().enumerate() {
        if *w == "delay" {
            m = ms(field(i + 1));
            if time_word(field(i + 2)) {
                m += 4.0 * ms(field(i + 2));
            }
        }
    }
    (m + 500.0).trunc() as i64
}

/// The awk of net_qlen/ns_qsum: the backlog (packets) of the netem qdisc 40:, None if none.
pub(super) fn backlog_of(show: &str) -> Option<u64> {
    let mut f = false;
    for l in show.lines() {
        if l.starts_with("qdisc netem 40:") {
            f = true;
        }
        let w: Vec<&str> = l.split_whitespace().collect();
        if f && w.first() == Some(&"backlog") {
            let v = w.get(2).copied().unwrap_or("");
            let v = v.strip_suffix('p').unwrap_or(v);
            return Some(awk_num(v).max(0.0) as u64);
        }
    }
    None
}

/// The awk of ns_stats: `sent=<packets>,dropped=<packets>` of the netem qdisc 40:.
pub(super) fn sent_of(show: &str) -> String {
    let mut f = false;
    for l in show.lines() {
        if l.starts_with("qdisc netem 40:") {
            f = true;
            continue;
        }
        let w: Vec<&str> = l.split_whitespace().collect();
        if f && w.first() == Some(&"Sent") {
            let d = w.get(6).copied().unwrap_or("");
            let d = d.strip_suffix(',').unwrap_or(d);
            return format!("sent={},dropped={d}", w.get(3).copied().unwrap_or(""));
        }
    }
    String::new()
}

// ------------------------------------------------------------------ services and links

fn svc_port(ctx: &Ctx, s: &str) -> String {
    ctx.plan
        .service(s)
        .map(|v| v.port.clone())
        .unwrap_or_default()
}

pub(super) fn svc_addr(ctx: &Ctx, s: &str) -> String {
    ctx.plan
        .service(s)
        .map(|v| v.addr.clone())
        .unwrap_or_default()
}

/// The links extension is in the image: a service has an `addr:` (hydo-build `has_links`).
fn has_links(ctx: &Ctx) -> bool {
    ctx.plan.services.iter().any(|s| !s.addr.is_empty())
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// hydo-init-links.sh's link_ends: `a-b` or `a-*` -> [a, the other ends...].
fn link_ends(ctx: &Ctx, t: &str) -> Option<Vec<String>> {
    if !t.contains('-') {
        return None;
    }
    let (a, b) = (before_first(t, '-'), after_first(t, '-'));
    if !name_ok(a) || svc_addr(ctx, a).is_empty() {
        return None;
    }
    let mut out = vec![a.to_string()];
    if b == "*" {
        for s in &ctx.plan.services {
            if s.name != a && !s.addr.is_empty() {
                out.push(s.name.clone());
            }
        }
        if out.len() == 1 {
            return None;
        }
    } else {
        if !name_ok(b) || b == a || svc_addr(ctx, b).is_empty() {
            return None;
        }
        out.push(b.to_string());
    }
    Some(out)
}

/// known_svc (hydo-init-links.sh's, which also knows links, when the image has it).
fn known(ctx: &Ctx, t: &str) -> bool {
    ctx.plan.service(t).is_some() || (has_links(ctx) && link_ends(ctx, t).is_some())
}

fn tc_words(ctx: &Ctx) -> Vec<String> {
    ctx.plan
        .layout
        .tc
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// `for m in <mods>; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done`
pub(super) fn insmods(fx: &mut F<'_>, ctx: &Ctx, mods: &[&str]) -> Step<()> {
    for m in mods {
        let e = env(fx, ctx, &[]);
        run(
            fx,
            vec![bb(ctx), "insmod".into(), format!("/lib/modules/{m}.ko")],
            e,
            None,
        )?;
    }
    Ok(())
}

/// ns_tc / `$HYDO_TC <args>`: the pinned tc, in a service's namespace unless `ns` is "-".
fn tc(fx: &mut F<'_>, ctx: &Ctx, ns: &str, args: Vec<String>) -> Step<Ran> {
    tc_cap(fx, ctx, ns, args, None)
}

/// `tc` keeping up to `capture` bytes of its output (for the output the shell reads).
fn tc_cap(
    fx: &mut F<'_>,
    ctx: &Ctx,
    ns: &str,
    args: Vec<String>,
    capture: Option<u32>,
) -> Step<Ran> {
    let mut argv = tc_words(ctx);
    argv.extend(args);
    let e = env(fx, ctx, &[]);
    let netns = (ns != "-").then(|| ns.to_string());
    run_in(fx, argv, e, capture, netns)
}

fn tc_ok(fx: &mut F<'_>, ctx: &Ctx, ns: &str, args: Vec<String>) -> Step<bool> {
    Ok(tc(fx, ctx, ns, args)?.status == 0)
}

fn root_args(dev: &str) -> Vec<String> {
    let mut a = strs(&[
        "qdisc", "add", "dev", dev, "root", "handle", "1:", "prio", "bands", "4", "priomap",
    ]);
    a.extend(strs(&PRIOMAP));
    a
}

fn netem_args(dev: &str, args: &str) -> Vec<String> {
    let mut a = strs(&[
        "qdisc", "add", "dev", dev, "parent", "1:4", "handle", "40:", "netem",
    ]);
    a.extend(args.split_whitespace().map(str::to_string));
    a
}

fn filter_args(dev: &str, matches: &[String]) -> Vec<String> {
    let mut a = strs(&[
        "filter", "add", "dev", dev, "parent", "1:0", "protocol", "ip", "prio", "1", "u32",
    ]);
    a.extend(matches.iter().cloned());
    a.extend(strs(&["flowid", "1:4"]));
    a
}

fn sleep_10ms(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let e = env(fx, ctx, &[]);
    run(fx, vec![bb(ctx), "sleep".into(), "0.01".into()], e, None)?;
    Ok(())
}

const TC_FAILED: &str = "HYDO-ORACLE: ERROR tc setup failed";

impl Net {
    /// `if [ -n "$N" ]; then nsvc=${N%%@*}; ... known_svc $nsvc || ...; fi`
    pub(super) fn read_n(&mut self, fx: &mut F<'_>, ctx: &Ctx, n: Option<&str>) {
        let Some(v) = n else { return };
        self.nsvc = before_first(v, '@').to_string();
        let r = after_first(v, '@');
        let rng = before_first(r, ':');
        self.nargs = after_first(r, ':').to_string();
        self.nfrom = num(before_last(rng, '-'));
        self.nto = num(after_first(rng, '-'));
        if !known(ctx, &self.nsvc) {
            log(fx, format!("N: no service {}, ignored", self.nsvc));
            (self.nfrom, self.nto) = (-1, -1);
        }
    }

    /// `if [ -n "$O" ]; then osvc=${O%%@*}; ...; fi`
    pub(super) fn read_o(&mut self, fx: &mut F<'_>, ctx: &Ctx, o: Option<&str>) {
        let Some(v) = o else { return };
        self.osvc = before_first(v, '@').to_string();
        let r = after_first(v, '@');
        let rng = before_first(r, ':');
        let r = after_first(r, ':');
        self.odir = before_first(r, ':').to_string();
        self.oargs = after_first(r, ':').to_string();
        self.ofrom = num(before_last(rng, '-'));
        self.oto = num(after_first(rng, '-'));
        if !known(ctx, &self.osvc) {
            log(fx, format!("O: no service {}, ignored", self.osvc));
            (self.ofrom, self.oto) = (-1, -1);
        }
    }

    /// The fault's points for the step sequence.
    pub(super) fn points(&self) -> [i64; 4] {
        [self.nfrom, self.nto, self.ofrom, self.oto]
    }

    /// Before step p, right after the cut check: `p == nto && netup`.
    pub(super) fn heal_n_at(&mut self, fx: &mut F<'_>, ctx: &Ctx, p: i64) -> Step<()> {
        if p == self.nto && self.netup {
            self.net_off(fx, ctx)?;
            self.netup = false;
            log(fx, format!("network healed before step {p}"));
        }
        Ok(())
    }

    /// Before step p, after the kill: N= on, O= healed, O= on.
    pub(super) fn at(&mut self, fx: &mut F<'_>, ctx: &Ctx, p: i64) -> Step<()> {
        if p == self.nfrom && self.nfrom < self.nto {
            let (t, a) = (self.nsvc.clone(), self.nargs.clone());
            if self.net_on(fx, ctx, &t, "dport", &a)? {
                self.netup = true;
            }
        }
        if p == self.oto && self.oneup {
            self.net_off(fx, ctx)?;
            self.oneup = false;
            log(fx, format!("one-way fault healed before step {p}"));
        }
        if p == self.ofrom && self.ofrom < self.oto {
            let om = match self.odir.as_str() {
                "req" => "dport",
                "resp" => "sport",
                d => {
                    return die_oracle(
                        fx,
                        format!("HYDO-ORACLE: ERROR bad one-way direction '{d}'"),
                    )
                }
            };
            let (t, a) = (self.osvc.clone(), self.oargs.clone());
            if self.net_on(fx, ctx, &t, om, &a)? {
                self.oneup = true;
                let d = self.odir.clone();
                xassert(
                    fx,
                    "reachable",
                    &format!("fault.oneway_{d}"),
                    &format!("a one-way network fault hit the {d} direction"),
                );
            }
        }
        Ok(())
    }

    /// No cut: `[ $netup = 1 ] && ...; [ $oneup = 1 ] && ...`.
    pub(super) fn heal_at_end(&mut self, fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
        if self.netup {
            self.net_off(fx, ctx)?;
            log(fx, "network healed at the end");
        }
        if self.oneup {
            self.net_off(fx, ctx)?;
            log(fx, "one-way fault healed at the end");
        }
        Ok(())
    }

    /// net_on <target> <dport|sport> <netem args with commas>; whether the fault is on.
    fn net_on(&mut self, fx: &mut F<'_>, ctx: &Ctx, t: &str, m: &str, a: &str) -> Step<bool> {
        // args=$(echo "$3" | $BB tr ',' ' ')
        let args = a.replace(',', " ");
        if ctx.plan.bridge {
            return self.net_on_ns(fx, ctx, t, m, &args);
        }
        if has_links(ctx) {
            if let Some(ends) = link_ends(ctx, t) {
                return self.net_on_link(fx, ctx, t, m, &args, &ends);
            }
        }
        self.net_on_port(fx, ctx, t, m, &args)
    }

    /// The direction of a link fault: N= (or O= and N= on the same link) both ways.
    fn link_dir(&self, t: &str, m: &str) -> String {
        if t == self.osvc && t != self.nsvc {
            m.to_string()
        } else {
            "both".into()
        }
    }

    /// hydo-init.sh's net_on (hydo-init-links.sh's net_on_port): netem on lo for one port.
    fn net_on_port(
        &mut self,
        fx: &mut F<'_>,
        ctx: &Ctx,
        t: &str,
        m: &str,
        args: &str,
    ) -> Step<bool> {
        let port = svc_port(ctx, t);
        if port.is_empty() || ctx.plan.layout.tc.is_empty() {
            log(
                fx,
                format!("network fault on {t} skipped (no port or no tc)"),
            );
            return Ok(false);
        }
        insmods(fx, ctx, &["sch_prio", "sch_netem", "cls_u32"])?;
        let mut matches = strs(&["match", "ip", m]);
        matches.extend(port.split_whitespace().map(str::to_string));
        matches.extend(strs(&["0xffff"]));
        let ok = tc_ok(fx, ctx, "-", root_args("lo"))?
            && tc_ok(fx, ctx, "-", netem_args("lo", args))?
            && tc_ok(fx, ctx, "-", filter_args("lo", &matches))?;
        if !ok {
            return die_oracle(fx, TC_FAILED.into());
        }
        self.net_args = args.to_string();
        log(
            fx,
            format!("NET FAULT on {t} (ip {m} {port}): netem {args}"),
        );
        Ok(true)
    }

    /// hydo-init-links.sh's net_on for a link: netem on lo for the packets between addresses.
    fn net_on_link(
        &mut self,
        fx: &mut F<'_>,
        ctx: &Ctx,
        t: &str,
        m: &str,
        args: &str,
        ends: &[String],
    ) -> Step<bool> {
        let dir = self.link_dir(t, m);
        if ctx.plan.layout.tc.is_empty() {
            log(fx, format!("link fault on {t} skipped (no tc)"));
            return Ok(false);
        }
        insmods(fx, ctx, &["sch_prio", "sch_netem", "cls_u32"])?;
        let ok =
            tc_ok(fx, ctx, "-", root_args("lo"))? && tc_ok(fx, ctx, "-", netem_args("lo", args))?;
        if !ok {
            return die_oracle(fx, TC_FAILED.into());
        }
        let a = &ends[0];
        let aa = svc_addr(ctx, a);
        for b in &ends[1..] {
            let ba = svc_addr(ctx, b);
            if dir != "sport" {
                let mt = pair_match(&aa, &ba);
                if !tc_ok(fx, ctx, "-", filter_args("lo", &mt))? {
                    return die_oracle(fx, TC_FAILED.into());
                }
            }
            if dir != "dport" {
                let mt = pair_match(&ba, &aa);
                if !tc_ok(fx, ctx, "-", filter_args("lo", &mt))? {
                    return die_oracle(fx, TC_FAILED.into());
                }
            }
        }
        self.net_args = args.to_string();
        log(
            fx,
            format!(
                "LINK FAULT {a} <-> {} ({dir}): netem {args}",
                ends[1..].join(" ")
            ),
        );
        Ok(true)
    }

    /// hydo-init-netns.sh's net_on: netem on the veth (or eth0) where the packets leave.
    fn net_on_ns(&mut self, fx: &mut F<'_>, ctx: &Ctx, t: &str, m: &str, args: &str) -> Step<bool> {
        if ctx.plan.layout.tc.is_empty() {
            log(fx, format!("network fault on {t} skipped (no tc)"));
            return Ok(false);
        }
        insmods(fx, ctx, &["sch_prio", "sch_netem", "cls_u32"])?;
        let mut this: Vec<String> = Vec::new();
        let ends = if has_links(ctx) {
            link_ends(ctx, t)
        } else {
            None
        };
        if let Some(ends) = ends {
            let dir = self.link_dir(t, m);
            let a = &ends[0];
            let aa = svc_addr(ctx, a);
            for b in &ends[1..] {
                let ba = svc_addr(ctx, b);
                if dir != "sport" {
                    let d = netns::veth(ctx, b);
                    self.fault_dev(fx, ctx, &mut this, "-", &d, args, pair_match(&aa, &ba))?;
                }
                if dir != "dport" {
                    let d = netns::veth(ctx, a);
                    self.fault_dev(fx, ctx, &mut this, "-", &d, args, pair_match(&ba, &aa))?;
                }
            }
            log(
                fx,
                format!(
                    "LINK FAULT {a} <-> {} ({dir}): netem {args} on{}",
                    ends[1..].join(" "),
                    spaced(&this)
                ),
            );
        } else {
            let port = svc_port(ctx, t);
            if port.is_empty() {
                log(fx, format!("network fault on {t} skipped (no port)"));
                return Ok(false);
            }
            let mut mt = strs(&["match", "ip"]);
            if m == "dport" {
                mt.push("dport".into());
                mt.extend(port.split_whitespace().map(str::to_string));
                mt.push("0xffff".into());
                let d = netns::veth(ctx, t);
                self.fault_dev(fx, ctx, &mut this, "-", &d, args, mt)?;
            } else {
                mt.push("sport".into());
                mt.extend(port.split_whitespace().map(str::to_string));
                mt.push("0xffff".into());
                let f = netns::ns_file(ctx, t);
                self.fault_dev(fx, ctx, &mut this, &f, "eth0", args, mt)?;
            }
            log(
                fx,
                format!(
                    "NET FAULT on {t} (ip {m} {port}): netem {args} on{}",
                    spaced(&this)
                ),
            );
        }
        self.net_args = args.to_string();
        Ok(true)
    }

    /// ns_fault_dev: a prio root and the netem band once per device and fault, one filter.
    #[allow(clippy::too_many_arguments)]
    fn fault_dev(
        &mut self,
        fx: &mut F<'_>,
        ctx: &Ctx,
        this: &mut Vec<String>,
        f: &str,
        d: &str,
        args: &str,
        matches: Vec<String>,
    ) -> Step<()> {
        let key = format!("{f}:{d}");
        if !this.contains(&key) {
            let ok = tc_ok(fx, ctx, f, root_args(d))? && tc_ok(fx, ctx, f, netem_args(d, args))?;
            if !ok {
                return die_oracle(fx, TC_FAILED.into());
            }
            this.push(key);
            self.devs.push((f.to_string(), d.to_string()));
        }
        if !tc_ok(fx, ctx, f, filter_args(d, &matches))? {
            return die_oracle(fx, TC_FAILED.into());
        }
        Ok(())
    }

    /// net_off: `log "network fault lifted: $(net_heal ...)"` (ns_heal with network: bridge).
    fn net_off(&mut self, fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
        let line = if ctx.plan.bridge {
            self.ns_heal(fx, ctx)?
        } else {
            self.net_heal(fx, ctx)?
        };
        log(fx, format!("network fault lifted: {line}"));
        Ok(())
    }

    /// net_qlen on lo: the agent's `qlen`.
    fn qlen_lo(fx: &mut F<'_>, ctx: &Ctx) -> Step<Option<u64>> {
        qlen(fx, ctx, "-", "lo")
    }

    /// hydo-init.sh's net_heal: lift the netem fault on lo without dropping its queue.
    fn net_heal(&mut self, fx: &mut F<'_>, ctx: &Ctx) -> Step<String> {
        let bound = bound_of(&self.net_args);
        let q0 = Self::qlen_lo(fx, ctx)?;
        let mut n = q0.unwrap_or(0);
        let mut t = 0;
        let change = strs(&[
            "qdisc", "change", "dev", "lo", "parent", "1:4", "handle", "40:", "netem", "rate",
            "100gbit", "corrupt", "0%",
        ]);
        if !tc_ok(fx, ctx, "-", change)? {
            n = 0;
        }
        while n > 0 && t < bound {
            sleep_10ms(fx, ctx)?;
            t += 10;
            n = Self::qlen_lo(fx, ctx)?.unwrap_or(0);
        }
        tc(
            fx,
            ctx,
            "-",
            strs(&["filter", "del", "dev", "lo", "parent", "1:0"]),
        )?;
        let mut n = Self::qlen_lo(fx, ctx)?.unwrap_or(0);
        let mut w = 0;
        while n > 0 && w < 50 {
            sleep_10ms(fx, ctx)?;
            w += 10;
            n = Self::qlen_lo(fx, ctx)?.unwrap_or(0);
        }
        tc(fx, ctx, "-", strs(&["qdisc", "del", "dev", "lo", "root"]))?;
        Ok(format!(
            "queued={} drained_ms={t} left={n}",
            q0.unwrap_or(0)
        ))
    }

    /// ns_qsum: the packets in the netem queues of every device of the faults in force.
    fn qsum(&self, fx: &mut F<'_>, ctx: &Ctx) -> Step<u64> {
        let mut n = 0;
        for (f, d) in &self.devs {
            n += qlen(fx, ctx, f, d)?.unwrap_or(0);
        }
        Ok(n)
    }

    /// hydo-init-netns.sh's ns_heal: net_heal for every device of the faults in force.
    fn ns_heal(&mut self, fx: &mut F<'_>, ctx: &Ctx) -> Step<String> {
        let bound = bound_of(&self.net_args);
        let (mut t, mut w) = (0, 0);
        let q0 = self.qsum(fx, ctx)?;
        let mut n = q0;
        for (f, d) in self.devs.clone() {
            let change = strs(&[
                "qdisc", "change", "dev", &d, "parent", "1:4", "handle", "40:", "netem", "rate",
                "100gbit", "corrupt", "0%",
            ]);
            tc(fx, ctx, &f, change)?;
        }
        while n > 0 && t < bound {
            sleep_10ms(fx, ctx)?;
            t += 10;
            n = self.qsum(fx, ctx)?;
        }
        for (f, d) in self.devs.clone() {
            tc(
                fx,
                ctx,
                &f,
                strs(&["filter", "del", "dev", &d, "parent", "1:0"]),
            )?;
        }
        n = self.qsum(fx, ctx)?;
        while n > 0 && w < 50 {
            sleep_10ms(fx, ctx)?;
            w += 10;
            n = self.qsum(fx, ctx)?;
        }
        let st = self.stats(fx, ctx)?;
        for (f, d) in self.devs.clone() {
            tc(fx, ctx, &f, strs(&["qdisc", "del", "dev", &d, "root"]))?;
        }
        self.devs.clear();
        Ok(format!("queued={q0} drained_ms={t} left={n} acted on:{st}"))
    }

    /// ns_stats: what each netem of the fault saw, ` <dev>[@<svc>]:sent=<n>,dropped=<n>`.
    fn stats(&self, fx: &mut F<'_>, ctx: &Ctx) -> Step<String> {
        let mut out = String::new();
        for (f, d) in &self.devs {
            let at = if f == "-" {
                String::new()
            } else {
                // echo "@<ns file>" | sed 's#.*/#@#'
                format!("@{}", after_last(f, '/'))
            };
            out.push_str(&format!(" {d}{at}:"));
            let r = tc_cap(
                fx,
                ctx,
                f,
                strs(&["-s", "qdisc", "show", "dev", d]),
                Some(CHUNK as u32),
            )?;
            out.push_str(&sent_of(&r.out));
        }
        Ok(out)
    }
}

/// `match ip src <a>/32 match ip dst <b>/32`
fn pair_match(a: &str, b: &str) -> Vec<String> {
    vec![
        "match".into(),
        "ip".into(),
        "src".into(),
        format!("{a}/32"),
        "match".into(),
        "ip".into(),
        "dst".into(),
        format!("{b}/32"),
    ]
}

/// NS_THIS as the log line shows it: ` <ns>:<dev>` per device.
fn spaced(this: &[String]) -> String {
    this.iter().map(|x| format!(" {x}")).collect()
}

/// The packets queued in the netem qdisc 40: of a device: the agent's `qlen` in the guest's own
/// namespace; in a service's namespace `tc -s qdisc show` there and the shell's awk on the host.
fn qlen(fx: &mut F<'_>, ctx: &Ctx, ns: &str, dev: &str) -> Step<Option<u64>> {
    if ns == "-" {
        let r = call(
            fx,
            Command::Qlen(Qlen {
                tc: ctx.plan.layout.tc.clone(),
                dev: dev.into(),
                handle: "40:".into(),
            }),
        )?;
        return Ok(match r {
            Ok(Reply::Qlen(q)) => q.packets,
            _ => None,
        });
    }
    let show = strs(&["-s", "qdisc", "show", "dev", dev]);
    let r = tc_cap(fx, ctx, ns, show, Some(CHUNK as u32))?;
    Ok(backlog_of(&r.out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_text_operations() {
        assert_eq!(before_first("a-b@3-7:delay,1ms", '@'), "a-b");
        assert_eq!(after_first("a-b@3-7:delay,1ms", '@'), "3-7:delay,1ms");
        assert_eq!(before_last("3-7", '-'), "3");
        assert_eq!(after_first("3-7", '-'), "7");
        assert_eq!(after_last("/tmp/hydo/ns/redis", '/'), "redis");
        assert_eq!(before_first("x", '@'), "x");
        assert_eq!(bound_of("delay 100ms"), 600);
        assert_eq!(bound_of("delay 100ms 20ms loss 5%"), 680);
        assert_eq!(bound_of("loss 5%"), 500);
        assert_eq!(bound_of("delay 1s"), 1500);
        assert_eq!(bound_of("delay 2500us"), 502);
        assert_eq!(bound_of("delay 1.5ms"), 501);
        assert_eq!(bound_of("delay 40"), 500);
        assert_eq!(bound_of("delay"), 500);
        assert_eq!(awk_num("12k"), 12.0);
        assert_eq!(awk_num("x"), 0.0);
        assert_eq!(awk_num("1e3ms"), 1000.0);
        let show = "qdisc prio 1: root refcnt 2 bands 4\n Sent 900 bytes 9 pkt (dropped 0, overlimits 0 requeues 0)\n backlog 0b 0p requeues 0\nqdisc netem 40: parent 1:4 limit 1000 delay 100ms\n Sent 500 bytes 5 pkt (dropped 2, overlimits 0 requeues 0)\n backlog 120b 3p requeues 0\n";
        assert_eq!(backlog_of(show), Some(3));
        assert_eq!(sent_of(show), "sent=5,dropped=2");
        assert_eq!(backlog_of("qdisc noqueue 0: root\n"), None);
        assert_eq!(sent_of(""), "");
    }
}
