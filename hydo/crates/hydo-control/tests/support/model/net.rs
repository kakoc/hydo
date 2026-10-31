//! The shell model's network part (M124): hydo-init.sh's net_on/net_qlen/net_heal/net_off and
//! explore_run's N=/O= lines, hydo-init-links.sh (link_ends, known_svc, net_on), hydo-init-freeze.sh
//! (fc_parse, fc_rules, fc_arm, fc_heal, fc_kill and the four wrappers) and hydo-init-netns.sh
//! (ns_setup, start_svc, net_on, ns_heal, ns_stats), transliterated from the shell text as
//! blocking code. The shell's variables stay strings and its parameter expansions are written
//! out as the shell evaluates them.
//!
//! Where an extension starts a program the agent does not have, the controller's documented
//! decision is checked (HANDOFF.md, M124): hydo_fc is the agent's `freeze` (its arguments as the
//! command's fields, the marker files as its reply and events, the FROZE line naming the handle);
//! `nsenter -n<file>` is the command's `netns`; `net_qlen` in the guest's own namespace is the
//! agent's `qlen`, and `ns_tc <file> -s qdisc show` is a `run` whose output the awk reads.

use super::*;

/// The extensions' shell variables.
#[derive(Default, Debug)]
pub struct Ext {
    // explore_run
    pub nsvc: String,
    pub nfrom: String,
    pub nto: String,
    pub nargs: String,
    pub osvc: String,
    pub ofrom: String,
    pub oto: String,
    pub odir: String,
    pub oargs: String,
    pub netup: bool,
    pub oneup: bool,
    // hydo-init.sh / hydo-init-netns.sh
    pub net_args: String,
    pub net_devs: String,
    pub ns_this: String,
    // hydo-init-freeze.sh
    pub fc: String,
    pub fc_svc: String,
    pub fc_dir: String,
    pub fc_tgt: String,
    pub fc_step: String,
    pub fc_n: String,
    pub fc_match: String,
    pub fc_dur: String,
    pub fc_on: u8,
    /// $fc_pid: the id of the freeze command while the helper runs
    pub fc_pid: Option<u64>,
    pub fc_decl: bool,
    /// The freeze command sent in this conversation and its dur (for the helper's lines).
    pub fc_sent: Option<(u64, String)>,
    pub xtables: bool,
}

// ${v%%c*}, ${v#*c}, ${v%c*}, ${v##*c}: the four expansions, by position of the character
fn pct2(v: &str, c: char) -> String {
    match v.find(c) {
        Some(i) => v[..i].to_string(),
        None => v.to_string(),
    }
}
fn hash1(v: &str, c: char) -> String {
    match v.find(c) {
        Some(i) => v[i + 1..].to_string(),
        None => v.to_string(),
    }
}
fn pct1(v: &str, c: char) -> String {
    match v.rfind(c) {
        Some(i) => v[..i].to_string(),
        None => v.to_string(),
    }
}
fn hash2(v: &str, c: char) -> String {
    match v.rfind(c) {
        Some(i) => v[i + 1..].to_string(),
        None => v.to_string(),
    }
}

/// `[ "$x" -lt "$y" ]` on the shell's number strings.
fn lt(x: &str, y: &str) -> bool {
    match (x.parse::<i64>(), y.parse::<i64>()) {
        (Ok(a), Ok(b)) => a < b,
        _ => false,
    }
}

/// awk's `x + 0`.
fn awk_val(v: &str) -> f64 {
    let t = v.trim_start();
    let mut end = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    for (i, ch) in t.char_indices() {
        let ok = match ch {
            '+' | '-' if i == 0 => true,
            '0'..='9' => {
                seen_digit = true;
                true
            }
            '.' if !seen_dot => {
                seen_dot = true;
                true
            }
            _ => false,
        };
        if !ok {
            break;
        }
        end = i + ch.len_utf8();
    }
    if !seen_digit {
        return 0.0;
    }
    let mut s = t[..end].to_string();
    // an exponent
    let rest = &t[end..];
    if let Some(r) = rest.strip_prefix(['e', 'E']) {
        let sign = r.starts_with(['+', '-']);
        let digits: String = r[sign as usize..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            s.push('e');
            if sign {
                s.push_str(&r[..1]);
            }
            s.push_str(&digits);
        }
    }
    s.parse().unwrap_or(0.0)
}

/// net_heal's awk over the netem arguments: printf "%d", m + 500.
fn heal_bound(args: &str) -> i64 {
    let ms = |v: &str| -> f64 {
        let x = awk_val(v);
        if v.contains("us") {
            x / 1000.0
        } else if v.contains("ms") {
            x
        } else if v.contains('s') {
            x * 1000.0
        } else {
            x / 1000.0
        }
    };
    let unit_ok = |v: &str| -> bool {
        let body = v.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let unit = &v[body.len()..];
        !body.is_empty()
            && body.chars().all(|c| c.is_ascii_digit() || c == '.')
            && [
                "", "us", "usec", "usecs", "ms", "msec", "msecs", "s", "sec", "secs",
            ]
            .contains(&unit)
    };
    let fs: Vec<&str> = args.split_whitespace().collect();
    let at = |i: usize| -> &str {
        if i >= 1 && i <= fs.len() {
            fs[i - 1]
        } else {
            ""
        }
    };
    let mut m = 0.0f64;
    for i in 1..=fs.len() {
        if at(i) == "delay" {
            m = ms(at(i + 1));
            if unit_ok(at(i + 2)) {
                m += 4.0 * ms(at(i + 2));
            }
        }
    }
    (m + 500.0) as i64
}

/// net_qlen's awk: `/^qdisc netem 40:/ { f = 1 } f && $1 == "backlog" { sub(/p$/, "", $3); print $3 + 0; exit }`
fn awk_backlog(out: &str) -> String {
    let mut f = false;
    for line in out.split('\n') {
        if line.starts_with("qdisc netem 40:") {
            f = true;
        }
        let fl: Vec<&str> = line.split_whitespace().collect();
        if f && fl.first().copied() == Some("backlog") {
            let mut v = fl.get(2).copied().unwrap_or("").to_string();
            if v.ends_with('p') {
                v.pop();
            }
            return format!("{}", awk_val(&v) as i64);
        }
    }
    String::new()
}

/// ns_stats's awk: `/^qdisc netem 40:/ { f = 1; next } f && $1 == "Sent" { ... printf "sent=%s,dropped=%s", $4, d; exit }`
fn awk_sent(out: &str) -> String {
    let mut f = false;
    for line in out.split('\n') {
        if line.starts_with("qdisc netem 40:") {
            f = true;
            continue;
        }
        let fl: Vec<&str> = line.split_whitespace().collect();
        if f && fl.first().copied() == Some("Sent") {
            let mut d = fl.get(6).copied().unwrap_or("").to_string();
            if d.ends_with(',') {
                d.pop();
            }
            return format!("sent={},dropped={d}", fl.get(3).copied().unwrap_or(""));
        }
    }
    String::new()
}

/// ns_mac: `awk -F. -v p="$2" '{ printf "%s:%02x:%02x:%02x:%02x\n", p, $1, $2, $3, $4 }'`
fn ns_mac(a: &str, p: &str) -> String {
    let f: Vec<&str> = a.split('.').collect();
    let x = |i: usize| awk_val(f.get(i).copied().unwrap_or("")) as i64;
    format!("{p}:{:02x}:{:02x}:{:02x}:{:02x}", x(0), x(1), x(2), x(3))
}

/// The trace awk of fc_heal, field by field.
fn fc_trace(log: &str) -> Vec<String> {
    let max = 600;
    let mut recs: Vec<&str> = log.split('\n').collect();
    if log.ends_with('\n') {
        recs.pop();
    }
    if log.is_empty() {
        recs.clear();
    }
    let mut out = Vec::new();
    for (nr, rec) in recs.iter().enumerate() {
        if nr + 1 > max {
            continue;
        }
        let fld: Vec<&str> = rec.split_whitespace().collect();
        let d = |i: usize| -> String {
            if i == 0 || i > fld.len() {
                String::new()
            } else {
                fld[i - 1].to_string()
            }
        };
        let h = d(4).starts_with('#');
        let mut w = String::new();
        let mut c = 0;
        let mut i = if h { 6 } else { 5 };
        while i <= fld.len() && c < 2 {
            let s: String = d(i)
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || "_:-".contains(ch) {
                        ch
                    } else {
                        ' '
                    }
                })
                .collect();
            let a: Vec<&str> = s.split(' ').filter(|x| !x.is_empty()).collect();
            let mut j = 0;
            while j < a.len() && c < 2 {
                if a[j].len() >= 4 && a[j] != "def" {
                    w = format!("{w} {}", &a[j][..a[j].len().min(32)]);
                    c += 1;
                }
                j += 1;
            }
            i += 1;
        }
        let mut line = format!("hydo-fc-trace: {} {}", d(1), d(3));
        if h {
            line = format!("{line} {}", d(4));
        }
        line = format!("{line} {}{w}", d(if h { 5 } else { 4 }));
        out.push(line);
    }
    if recs.len() > max {
        out.push(format!(
            "hydo-fc-trace: ... {} more packets not shown",
            recs.len() - max
        ));
    }
    out
}

fn atol(s: &str) -> i64 {
    let d: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    d.parse().unwrap_or(0)
}

impl<'a> Model<'a> {
    fn sv_addr(&self, s: &str) -> String {
        self.plan
            .services
            .iter()
            .find(|x| x.name == s)
            .map(|x| x.addr.clone())
            .unwrap_or_default()
    }

    fn sv_port(&self, s: &str) -> String {
        self.plan
            .services
            .iter()
            .find(|x| x.name == s)
            .map(|x| x.port.clone())
            .unwrap_or_default()
    }

    fn links_on(&self) -> bool {
        self.plan.services.iter().any(|x| !x.addr.is_empty())
    }

    fn tcw(&self) -> Vec<String> {
        self.plan
            .layout
            .tc
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    /// `run` with a namespace.
    pub(super) fn run_ns(
        &mut self,
        argv: Vec<String>,
        env: Vec<String>,
        capture: Option<u32>,
        netns: Option<String>,
    ) -> R<(u32, String)> {
        let id = self.send(Command::Run(Run {
            argv,
            env: Some(env),
            cwd: None,
            uid: None,
            gid: None,
            netns,
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

    /// `spawn_wait` with a namespace.
    pub(super) fn spawn_wait_ns(
        &mut self,
        h: &str,
        argv: Vec<String>,
        env: Vec<String>,
        out: &str,
        append: bool,
        netns: Option<String>,
    ) -> R<u32> {
        let mut c = spawn(h, argv, env, out, append);
        if let Command::Spawn(x) = &mut c {
            x.netns = netns;
        }
        let id = self.send(c)?;
        if self.reply(id)?.is_err() {
            return Ok(127);
        }
        let e = self.exits.get(&id);
        self.waited(&format!("the exit of {h}"), e.map(|x| x.0))?;
        Ok(e.unwrap().1.status)
    }

    /// The namespace file of a service's commands with network: bridge.
    pub(super) fn ns_of(&self, svc: &str) -> Option<String> {
        if self.plan.bridge {
            Some(format!("{H}/ns/{svc}"))
        } else {
            None
        }
    }

    fn insmod(&mut self, mods: &str) -> R<()> {
        for m in mods.split(' ') {
            let e = self.env(&[]);
            self.run(plain(&["insmod", &format!("/lib/modules/{m}.ko")]), e, None)?;
        }
        Ok(())
    }

    /// `$HYDO_TC <args>` (with `nsenter -n<f>` unless f is -): the exit status.
    fn tc_at(&mut self, f: &str, args: &[&str]) -> R<u32> {
        let mut argv = self.tcw();
        argv.extend(args.iter().map(|x| s(x)));
        let e = self.env(&[]);
        let ns = if f == "-" { None } else { Some(s(f)) };
        Ok(self.run_ns(argv, e, None, ns)?.0)
    }

    // ------------------------------------------------------------------ explore_run's lines

    /// `if [ -n "$N" ]; then nsvc=${N%%@*}; r=${N#*@}; rng=${r%%:*}; nargs=${r#*:}; ...`
    pub(super) fn read_n(&mut self, n: &str) -> R<()> {
        self.ext.nsvc = pct2(n, '@');
        let r = hash1(n, '@');
        let rng = pct2(&r, ':');
        self.ext.nargs = hash1(&r, ':');
        self.ext.nfrom = pct1(&rng, '-');
        self.ext.nto = hash1(&rng, '-');
        if !self.known_svc(&self.ext.nsvc.clone()) {
            self.note(format!("N: no service {}, ignored", self.ext.nsvc))?;
            self.ext.nfrom = s("-1");
            self.ext.nto = s("-1");
        }
        Ok(())
    }

    /// `if [ -n "$O" ]; then osvc=${O%%@*}; r=${O#*@}; rng=${r%%:*}; r=${r#*:}; odir=...`
    pub(super) fn read_o(&mut self, o: &str) -> R<()> {
        self.ext.osvc = pct2(o, '@');
        let r = hash1(o, '@');
        let rng = pct2(&r, ':');
        let r = hash1(&r, ':');
        self.ext.odir = pct2(&r, ':');
        self.ext.oargs = hash1(&r, ':');
        self.ext.ofrom = pct1(&rng, '-');
        self.ext.oto = hash1(&rng, '-');
        if !self.known_svc(&self.ext.osvc.clone()) {
            self.note(format!("O: no service {}, ignored", self.ext.osvc))?;
            self.ext.ofrom = s("-1");
            self.ext.oto = s("-1");
        }
        Ok(())
    }

    pub(super) fn net_points(&self) -> Vec<i64> {
        [
            &self.ext.nfrom,
            &self.ext.nto,
            &self.ext.ofrom,
            &self.ext.oto,
        ]
        .iter()
        .filter_map(|x| x.parse().ok())
        .collect()
    }

    /// `if [ "$p" = "$nto" ] && [ $netup = 1 ]; then net_off; netup=0; log ...; fi`
    pub(super) fn at_nto(&mut self, p: &str) -> R<()> {
        if p == self.ext.nto && self.ext.netup {
            self.net_off()?;
            self.ext.netup = false;
            self.note(format!("network healed before step {p}"))?;
        }
        Ok(())
    }

    /// The N= on, O= off and O= on lines of the step loop.
    pub(super) fn at_n_o(&mut self, p: &str) -> R<()> {
        if p == self.ext.nfrom && lt(&self.ext.nfrom, &self.ext.nto) {
            let (a, b) = (self.ext.nsvc.clone(), self.ext.nargs.clone());
            if self.net_on(&a, "dport", &b)? {
                self.ext.netup = true;
            }
        }
        if p == self.ext.oto && self.ext.oneup {
            self.net_off()?;
            self.ext.oneup = false;
            self.note(format!("one-way fault healed before step {p}"))?;
        }
        if p == self.ext.ofrom && lt(&self.ext.ofrom, &self.ext.oto) {
            let om = match self.ext.odir.as_str() {
                "req" => "dport",
                "resp" => "sport",
                _ => {
                    let d = self.ext.odir.clone();
                    return self.die(format!("HYDO-ORACLE: ERROR bad one-way direction '{d}'"));
                }
            };
            let (a, b) = (self.ext.osvc.clone(), self.ext.oargs.clone());
            if self.net_on(&a, om, &b)? {
                self.ext.oneup = true;
                let d = self.ext.odir.clone();
                self.xassert(
                    "reachable",
                    &format!("fault.oneway_{d}"),
                    &format!("a one-way network fault hit the {d} direction"),
                )?;
            }
        }
        Ok(())
    }

    /// `[ $netup = 1 ] && { net_off; log ...; }`, `[ $oneup = 1 ] && { ... }`
    pub(super) fn net_at_end(&mut self) -> R<()> {
        if self.ext.netup {
            self.net_off()?;
            self.note(s("network healed at the end"))?;
        }
        if self.ext.oneup {
            self.net_off()?;
            self.note(s("one-way fault healed at the end"))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ hydo-init-links.sh

    fn link_ends(&self, l: &str) -> Option<Vec<String>> {
        if !l.contains('-') {
            return None;
        }
        let a = pct2(l, '-');
        let b = hash1(l, '-');
        let bad = |x: &str| {
            x.is_empty()
                || x.chars()
                    .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        };
        if bad(&a) || self.sv_addr(&a).is_empty() {
            return None;
        }
        let mut bs: Vec<String> = Vec::new();
        if b == "*" {
            for x in &self.plan.services {
                if x.name != a && !x.addr.is_empty() {
                    bs.push(x.name.clone());
                }
            }
            if bs.is_empty() {
                return None;
            }
        } else {
            if bad(&b) || b == a || self.sv_addr(&b).is_empty() {
                return None;
            }
            bs.push(b);
        }
        let mut v = vec![a];
        v.extend(bs);
        Some(v)
    }

    fn known_svc(&self, x: &str) -> bool {
        if self.plan.services.iter().any(|v| v.name == x) {
            return true;
        }
        self.links_on() && self.link_ends(x).is_some()
    }

    // ------------------------------------------------------------------ net_on

    fn net_on(&mut self, t: &str, m: &str, a3: &str) -> R<bool> {
        if self.plan.bridge {
            return self.net_on_netns(t, m, a3);
        }
        if self.links_on() {
            match self.link_ends(t) {
                Some(ends) => return self.net_on_links(t, m, a3, ends),
                None => return self.net_on_port(t, m, a3),
            }
        }
        self.net_on_port(t, m, a3)
    }

    fn prio(dev: &str) -> Vec<String> {
        let mut v: Vec<String> = format!(
            "qdisc add dev {dev} root handle 1: prio bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1"
        )
        .split(' ')
        .map(s)
        .collect();
        v.retain(|x| !x.is_empty());
        v
    }

    fn netem(dev: &str, args: &str) -> Vec<String> {
        format!("qdisc add dev {dev} parent 1:4 handle 40: netem {args}")
            .split_whitespace()
            .map(s)
            .collect()
    }

    fn tc_v(&mut self, f: &str, v: Vec<String>) -> R<u32> {
        let r: Vec<&str> = v.iter().map(String::as_str).collect();
        self.tc_at(f, &r)
    }

    /// hydo-init.sh's net_on.
    fn net_on_port(&mut self, t: &str, m: &str, a3: &str) -> R<bool> {
        let port = self.sv_port(t);
        let args = a3.replace(',', " ");
        if port.is_empty() || self.plan.layout.tc.is_empty() {
            self.note(format!("network fault on {t} skipped (no port or no tc)"))?;
            return Ok(false);
        }
        self.insmod("sch_prio sch_netem cls_u32")?;
        let filt: Vec<String> = format!(
            "filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip {m} {port} 0xffff flowid 1:4"
        )
        .split_whitespace()
        .map(s)
        .collect();
        let ok = self.tc_v("-", Self::prio("lo"))? == 0
            && self.tc_v("-", Self::netem("lo", &args))? == 0
            && self.tc_v("-", filt)? == 0;
        if !ok {
            return self.die(s("HYDO-ORACLE: ERROR tc setup failed"));
        }
        self.ext.net_args = args.clone();
        self.note(format!("NET FAULT on {t} (ip {m} {port}): netem {args}"))?;
        Ok(true)
    }

    fn dir_for(&self, t: &str, m: &str) -> String {
        if t == self.ext.osvc && t != self.ext.nsvc {
            s(m)
        } else {
            s("both")
        }
    }

    /// hydo-init-links.sh's net_on on a link.
    fn net_on_links(&mut self, t: &str, m: &str, a3: &str, ends: Vec<String>) -> R<bool> {
        let args = a3.replace(',', " ");
        let dir = self.dir_for(t, m);
        if self.plan.layout.tc.is_empty() {
            self.note(format!("link fault on {t} skipped (no tc)"))?;
            return Ok(false);
        }
        self.insmod("sch_prio sch_netem cls_u32")?;
        let ok = self.tc_v("-", Self::prio("lo"))? == 0
            && self.tc_v("-", Self::netem("lo", &args))? == 0;
        if !ok {
            return self.die(s("HYDO-ORACLE: ERROR tc setup failed"));
        }
        let a = ends[0].clone();
        let aa = self.sv_addr(&a);
        let rest: Vec<String> = ends[1..].to_vec();
        for b in &rest {
            let ba = self.sv_addr(b);
            if dir != "sport" {
                let v: Vec<String> = format!("filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip src {aa}/32 match ip dst {ba}/32 flowid 1:4").split(' ').map(s).collect();
                if self.tc_v("-", v)? != 0 {
                    return self.die(s("HYDO-ORACLE: ERROR tc setup failed"));
                }
            }
            if dir != "dport" {
                let v: Vec<String> = format!("filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip src {ba}/32 match ip dst {aa}/32 flowid 1:4").split(' ').map(s).collect();
                if self.tc_v("-", v)? != 0 {
                    return self.die(s("HYDO-ORACLE: ERROR tc setup failed"));
                }
            }
        }
        self.ext.net_args = args.clone();
        self.note(format!(
            "LINK FAULT {a} <-> {} ({dir}): netem {args}",
            rest.join(" ")
        ))?;
        Ok(true)
    }

    fn ns_veth(&self, x: &str) -> String {
        let i = self
            .plan
            .services
            .iter()
            .position(|v| v.name == x)
            .map(|i| i + 1)
            .unwrap_or(0);
        format!("vh{i}")
    }

    /// ns_fault_dev <ns|-> <dev> <netem args> <u32 match...>
    fn ns_fault_dev(&mut self, f: &str, d: &str, a: &str, mat: &str) -> R<()> {
        let fd = format!("{f}:{d}");
        if !format!(" {} ", self.ext.ns_this).contains(&format!(" {fd} ")) {
            let ok = self.tc_v(f, Self::prio(d))? == 0 && self.tc_v(f, Self::netem(d, a))? == 0;
            if !ok {
                return self.die(s("HYDO-ORACLE: ERROR tc setup failed"));
            }
            self.ext.ns_this = format!("{} {fd}", self.ext.ns_this);
            self.ext.net_devs = format!("{} {fd}", self.ext.net_devs);
        }
        let v: Vec<String> =
            format!("filter add dev {d} parent 1:0 protocol ip prio 1 u32 {mat} flowid 1:4")
                .split_whitespace()
                .map(s)
                .collect();
        if self.tc_v(f, v)? != 0 {
            return self.die(s("HYDO-ORACLE: ERROR tc setup failed"));
        }
        Ok(())
    }

    /// hydo-init-netns.sh's net_on.
    fn net_on_netns(&mut self, t: &str, m: &str, a3: &str) -> R<bool> {
        let args = a3.replace(',', " ");
        if self.plan.layout.tc.is_empty() {
            self.note(format!("network fault on {t} skipped (no tc)"))?;
            return Ok(false);
        }
        self.insmod("sch_prio sch_netem cls_u32")?;
        self.ext.ns_this = String::new();
        let ends = if self.links_on() {
            self.link_ends(t)
        } else {
            None
        };
        if let Some(ends) = ends {
            let dir = self.dir_for(t, m);
            let a = ends[0].clone();
            let aa = self.sv_addr(&a);
            let rest: Vec<String> = ends[1..].to_vec();
            for b in &rest {
                let ba = self.sv_addr(b);
                if dir != "sport" {
                    let d = self.ns_veth(b);
                    self.ns_fault_dev(
                        "-",
                        &d,
                        &args,
                        &format!("match ip src {aa}/32 match ip dst {ba}/32"),
                    )?;
                }
                if dir != "dport" {
                    let d = self.ns_veth(&a);
                    self.ns_fault_dev(
                        "-",
                        &d,
                        &args,
                        &format!("match ip src {ba}/32 match ip dst {aa}/32"),
                    )?;
                }
            }
            self.note(format!(
                "LINK FAULT {a} <-> {} ({dir}): netem {args} on{}",
                rest.join(" "),
                self.ext.ns_this
            ))?;
        } else {
            let port = self.sv_port(t);
            if port.is_empty() {
                self.note(format!("network fault on {t} skipped (no port)"))?;
                return Ok(false);
            }
            if m == "dport" {
                let d = self.ns_veth(t);
                self.ns_fault_dev("-", &d, &args, &format!("match ip dport {port} 0xffff"))?;
            } else {
                self.ns_fault_dev(
                    &format!("{H}/ns/{t}"),
                    "eth0",
                    &args,
                    &format!("match ip sport {port} 0xffff"),
                )?;
            }
            self.note(format!(
                "NET FAULT on {t} (ip {m} {port}): netem {args} on{}",
                self.ext.ns_this
            ))?;
        }
        self.ext.net_args = args;
        Ok(true)
    }

    // ------------------------------------------------------------------ the heal

    /// net_qlen / one device of ns_qsum: the agent's qlen in the guest's own namespace, else
    /// `tc -s qdisc show` in the service's namespace and the awk; "" when there is no netem.
    fn qlen_at(&mut self, f: &str, d: &str) -> R<String> {
        if f == "-" {
            let id = self.send(Command::Qlen(Qlen {
                tc: self.plan.layout.tc.clone(),
                dev: s(d),
                handle: s("40:"),
            }))?;
            return Ok(match self.reply(id)? {
                Ok(Reply::Qlen(q)) => q.packets.map(|p| p.to_string()).unwrap_or_default(),
                _ => String::new(),
            });
        }
        let mut argv = self.tcw();
        argv.extend(["-s", "qdisc", "show", "dev", d].map(s));
        let e = self.env(&[]);
        let (_, out) = self.run_ns(argv, e, Some(3000), Some(s(f)))?;
        Ok(awk_backlog(&out))
    }

    fn sleep10(&mut self) -> R<()> {
        let e = self.env(&[]);
        self.run(plain(&["sleep", "0.01"]), e, None)?;
        Ok(())
    }

    fn net_off(&mut self) -> R<()> {
        let line = if self.plan.bridge {
            self.ns_heal()?
        } else {
            self.net_heal()?
        };
        self.note(format!("network fault lifted: {line}"))
    }

    /// hydo-init.sh's net_heal "$HYDO_TC" "$NET_ARGS".
    fn net_heal(&mut self) -> R<String> {
        let bound = heal_bound(&self.ext.net_args.replace(',', " "));
        let q0 = self.qlen_at("-", "lo")?;
        let mut n = if q0.is_empty() { s("0") } else { q0.clone() };
        let mut t = 0;
        if self.tc_at(
            "-",
            &[
                "qdisc", "change", "dev", "lo", "parent", "1:4", "handle", "40:", "netem", "rate",
                "100gbit", "corrupt", "0%",
            ],
        )? != 0
        {
            n = s("0");
        }
        let gt0 = |n: &str| atol(if n.is_empty() { "0" } else { n }) > 0;
        while gt0(&n) && t < bound {
            self.sleep10()?;
            t += 10;
            n = self.qlen_at("-", "lo")?;
        }
        self.tc_at("-", &["filter", "del", "dev", "lo", "parent", "1:0"])?;
        n = self.qlen_at("-", "lo")?;
        let mut w = 0;
        while gt0(&n) && w < 50 {
            self.sleep10()?;
            w += 10;
            n = self.qlen_at("-", "lo")?;
        }
        self.tc_at("-", &["qdisc", "del", "dev", "lo", "root"])?;
        let or0 = |x: &str| if x.is_empty() { s("0") } else { s(x) };
        Ok(format!(
            "queued={} drained_ms={t} left={}",
            or0(&q0),
            or0(&n)
        ))
    }

    fn devs(&self) -> Vec<(String, String)> {
        self.ext
            .net_devs
            .split_whitespace()
            .map(|x| (pct1(x, ':'), hash2(x, ':')))
            .collect()
    }

    fn ns_qsum(&mut self) -> R<i64> {
        let mut n = 0;
        for (f, d) in self.devs() {
            let q = self.qlen_at(&f, &d)?;
            n += if q.is_empty() { 0 } else { atol(&q) };
        }
        Ok(n)
    }

    fn ns_stats(&mut self) -> R<String> {
        let mut out = String::new();
        for (f, d) in self.devs() {
            let at = if f == "-" {
                String::new()
            } else {
                // echo "@<f>" | sed 's#.*/#@#'
                let x = format!("@{f}");
                format!("@{}", &x[x.rfind('/').map(|i| i + 1).unwrap_or(0)..])
            };
            out.push_str(&format!(" {d}{at}:"));
            let mut argv = self.tcw();
            argv.extend(["-s", "qdisc", "show", "dev", &d].map(s));
            let e = self.env(&[]);
            let ns = if f == "-" { None } else { Some(f.clone()) };
            let (_, o) = self.run_ns(argv, e, Some(3000), ns)?;
            out.push_str(&awk_sent(&o));
        }
        Ok(out)
    }

    /// hydo-init-netns.sh's ns_heal.
    fn ns_heal(&mut self) -> R<String> {
        let bound = heal_bound(&self.ext.net_args.replace(',', " "));
        let (mut t, mut w) = (0, 0);
        let q0 = self.ns_qsum()?;
        let mut n = q0;
        for (f, d) in self.devs() {
            self.tc_at(
                &f,
                &[
                    "qdisc", "change", "dev", &d, "parent", "1:4", "handle", "40:", "netem",
                    "rate", "100gbit", "corrupt", "0%",
                ],
            )?;
        }
        while n > 0 && t < bound {
            self.sleep10()?;
            t += 10;
            n = self.ns_qsum()?;
        }
        for (f, d) in self.devs() {
            self.tc_at(&f, &["filter", "del", "dev", &d, "parent", "1:0"])?;
        }
        n = self.ns_qsum()?;
        while n > 0 && w < 50 {
            self.sleep10()?;
            w += 10;
            n = self.ns_qsum()?;
        }
        let st = self.ns_stats()?;
        for (f, d) in self.devs() {
            self.tc_at(&f, &["qdisc", "del", "dev", &d, "root"])?;
        }
        self.ext.net_devs = String::new();
        Ok(format!("queued={q0} drained_ms={t} left={n} acted on:{st}"))
    }

    // ------------------------------------------------------------------ hydo-init-netns.sh

    /// The service network before declare_all: `if ns_setup > $H/netns.log 2>&1; then log ...`
    pub(super) fn ns_init(&mut self) -> R<()> {
        if !self.plan.bridge {
            return Ok(());
        }
        let mut logf = String::new();
        let ok = self.ns_setup(&mut logf)?;
        if ok {
            let ex = self.plan.net_extra.join(" ");
            let plus = if ex.is_empty() {
                String::new()
            } else {
                format!(" + {ex}")
            };
            return self.note(format!(
                "service network (network: bridge): hydo0 10.89.0.1/16{plus}; {}",
                logf.replace('\n', " ")
            ));
        }
        let mut ls: Vec<&str> = logf.split('\n').collect();
        if logf.ends_with('\n') {
            ls.pop();
        }
        if logf.is_empty() {
            ls.clear();
        }
        let from = ls.len().saturating_sub(8);
        for l in ls[from..].iter().copied() {
            self.note_raw(l)?;
        }
        self.die(s(
            "HYDO-ORACLE: ERROR service network setup failed (network: bridge)",
        ))
    }

    fn hydo_ip(&mut self, args: &str, logf: &mut String) -> R<bool> {
        let mut argv: Vec<String> = self.plan.layout.ip.split_whitespace().map(s).collect();
        argv.extend(args.split(' ').map(s));
        let e = self.env(&[]);
        let (rc, out) = self.run(argv, e, None)?;
        logf.push_str(&out);
        Ok(rc == 0)
    }

    fn v6off(&mut self, dev: &str, logf: &mut String) -> R<()> {
        let e = self.env(&[]);
        let (_, out) = self.run(
            vec![
                s(BB),
                s("sh"),
                s("-c"),
                format!("[ -d /proc/sys/net/ipv6/conf/{dev} ] && echo 1 > /proc/sys/net/ipv6/conf/{dev}/disable_ipv6"),
            ],
            e,
            None,
        )?;
        logf.push_str(&out);
        Ok(())
    }

    fn ns_setup(&mut self, logf: &mut String) -> R<bool> {
        let gw = "10.89.0.1";
        self.insmod("llc stp bridge veth")?;
        self.send(Command::Mkdir(Mkdir {
            path: format!("{H}/ns"),
            mode: None,
        }))?;
        if !self.hydo_ip(
            &format!(
                "link add hydo0 address {} type bridge forward_delay 0 stp_state 0",
                ns_mac(gw, "02:42")
            ),
            logf,
        )? {
            return Ok(false);
        }
        self.v6off("hydo0", logf)?;
        if !self.hydo_ip(&format!("addr add {gw}/16 dev hydo0"), logf)? {
            return Ok(false);
        }
        for a in self.plan.net_extra.clone() {
            if !self.hydo_ip(&format!("addr add {a}/32 dev hydo0"), logf)? {
                return Ok(false);
            }
        }
        if !self.hydo_ip("link set hydo0 up", logf)? {
            return Ok(false);
        }
        let mut i = 0;
        for sv in self.plan.services.clone() {
            i += 1;
            let a = sv.addr.clone();
            let f = format!("{H}/ns/{}", sv.name);
            // `: > $f && $BB unshare --net=$f true || return 1`
            let id = self.send(write(&f, "", false))?;
            if self.reply(id)?.is_err() {
                return Ok(false);
            }
            let e = self.env(&[]);
            let (rc, out) =
                self.run(plain(&["unshare", &format!("--net={f}"), "true"]), e, None)?;
            logf.push_str(&out);
            if rc != 0 {
                return Ok(false);
            }
            if !self.hydo_ip(
                &format!(
                    "link add vh{i} address {} type veth peer name eth0 address {} netns {f}",
                    ns_mac(&a, "02:43"),
                    ns_mac(&a, "02:42")
                ),
                logf,
            )? {
                return Ok(false);
            }
            self.v6off(&format!("vh{i}"), logf)?;
            if !self.hydo_ip(&format!("link set vh{i} master hydo0 up"), logf)? {
                return Ok(false);
            }
            let ipp = self.plan.layout.ip.clone();
            let e = self.env(&[]);
            let (rc, out) = self.run_ns(
                vec![
                    s(BB),
                    s("sh"),
                    s("-c"),
                    format!("[ -d /proc/sys/net/ipv6/conf/eth0 ] && echo 1 > /proc/sys/net/ipv6/conf/eth0/disable_ipv6\n      {ipp} link set lo up && {ipp} addr add {a}/16 dev eth0 && {ipp} link set eth0 up"),
                ],
                e,
                None,
                Some(f.clone()),
            )?;
            logf.push_str(&out);
            if rc != 0 {
                return Ok(false);
            }
            logf.push_str(&format!("{}={a}@vh{i}\n", sv.name));
        }
        Ok(true)
    }

    // ------------------------------------------------------------------ hydo-init-freeze.sh

    fn fc_bad<T>(&mut self, why: &str) -> R<T> {
        let fc = self.ext.fc.clone();
        self.die(format!(
            "HYDO-ORACLE: ERROR bad freeze token F={fc} ({why})"
        ))
    }

    fn fc_known(&self, x: &str) -> bool {
        self.plan.services.iter().any(|v| v.name == x)
    }

    /// fc_parse <value of F=>
    pub(super) fn fc_parse(&mut self, v: &str) -> R<()> {
        self.ext.fc = s(v);
        let fc = s(v);
        let head = pct2(&fc, '@');
        let mut rest = hash1(&fc, '@');
        if head == fc {
            return self.fc_bad("no @");
        }
        if head.contains('>') {
            self.ext.fc_dir = s("req");
            self.ext.fc_svc = pct2(&head, '>');
            self.ext.fc_tgt = hash1(&head, '>');
        } else if head.contains('<') {
            self.ext.fc_dir = s("resp");
            self.ext.fc_svc = pct2(&head, '<');
            self.ext.fc_tgt = hash1(&head, '<');
        } else {
            return self.fc_bad("no > or <");
        }
        self.ext.fc_dur = hash2(&rest, ':');
        rest = pct1(&rest, ':');
        if rest == hash1(&fc, '@') {
            return self.fc_bad("no :<dur>");
        }
        if rest.contains(':') {
            self.ext.fc_match = hash1(&rest, ':');
            rest = pct2(&rest, ':');
        } else {
            self.ext.fc_match = s("-");
        }
        if self.ext.fc_match.is_empty() {
            self.ext.fc_match = s("-");
        }
        if rest.contains('#') {
            self.ext.fc_step = pct2(&rest, '#');
            self.ext.fc_n = hash1(&rest, '#');
        } else {
            self.ext.fc_step = rest.clone();
            self.ext.fc_n = s("1");
        }
        for x in [
            self.ext.fc_step.clone(),
            self.ext.fc_n.clone(),
            self.ext.fc_dur.clone(),
        ] {
            if x.is_empty() || x.chars().any(|c| !c.is_ascii_digit()) {
                return self.fc_bad("step, n and dur must be numbers");
            }
        }
        let (fs, ft) = (self.ext.fc_svc.clone(), self.ext.fc_tgt.clone());
        if !self.fc_known(&fs) || (ft != "workload" && !self.fc_known(&ft)) || fs == ft {
            self.note(format!("F: no services {fs} and {ft}, ignored"))?;
            self.ext.fc = String::new();
            return Ok(());
        }
        let e = &self.ext;
        let line = format!(
            "freeze at a call: {} ({} with {}) from step {}, event #{}, match '{}', for {}ms",
            e.fc_svc, e.fc_dir, e.fc_tgt, e.fc_step, e.fc_n, e.fc_match, e.fc_dur
        );
        self.note(line)
    }

    /// step_range's wrapper.
    pub(super) fn fc_step_range(&mut self, a: i64, b: i64) -> R<()> {
        if !self.ext.fc_decl {
            self.ext.fc_decl = true;
            self.report(s("HYDO-ASSERT-DECL {\"t\":\"reachable\",\"id\":\"fault.freeze_call\",\"msg\":\"a service was frozen at a call (F=)\"}"))?;
        }
        let st: i64 = self.ext.fc_step.parse().unwrap_or(-1);
        if !self.ext.fc.is_empty() && self.ext.fc_on == 0 && st >= a && st < b {
            self.step_range_plain(a, st)?;
            self.fc_arm()?;
            self.step_range_plain(st, b)
        } else {
            self.step_range_plain(a, b)
        }
    }

    /// fc_rules <-A|-D>: the iptables runs, (status, output); 3 without addresses or a port.
    fn fc_rules(&mut self, op: &str) -> R<(u32, String)> {
        let sa = self.sv_addr(&self.ext.fc_svc.clone());
        let ta = self.sv_addr(&self.ext.fc_tgt.clone());
        let mut tp = self.sv_port(&self.ext.fc_tgt.clone());
        let q = "-j NFQUEUE --queue-num 7 --queue-bypass";
        let (r1, r2) = if self.ext.fc_tgt == "workload" {
            tp = self.sv_port(&self.ext.fc_svc.clone());
            if tp.is_empty() {
                return Ok((3, String::new()));
            }
            let d = if sa.is_empty() {
                String::new()
            } else {
                format!(" -d {sa}")
            };
            let s_ = if sa.is_empty() {
                String::new()
            } else {
                format!(" -s {sa}")
            };
            (
                format!("{op} OUTPUT -s 127.0.0.1{d} -p tcp --dport {tp} {q}"),
                format!("{op} OUTPUT -d 127.0.0.1{s_} -p tcp --sport {tp} {q}"),
            )
        } else if !sa.is_empty() && !ta.is_empty() {
            (
                format!("{op} OUTPUT -s {sa} -d {ta} {q}"),
                format!("{op} OUTPUT -s {ta} -d {sa} {q}"),
            )
        } else if !tp.is_empty() {
            let (s1, s2) = if sa.is_empty() {
                (String::new(), String::new())
            } else {
                (format!(" -s {sa}"), format!(" -d {sa}"))
            };
            (
                format!("{op} OUTPUT{s1} -p tcp --dport {tp} {q}"),
                format!("{op} OUTPUT{s2} -p tcp --sport {tp} {q}"),
            )
        } else {
            return Ok((3, String::new()));
        };
        let cap = if op == "-A" { Some(3000) } else { None };
        let mut out = String::new();
        let mut rc = 0;
        for r in [r1, r2] {
            let mut argv: Vec<String> = self
                .plan
                .layout
                .iptables
                .split_whitespace()
                .map(s)
                .collect();
            argv.extend(r.split(' ').map(s));
            let e = self.env(&[]);
            let (st, o) = self.run(argv, e, cap)?;
            out.push_str(&o);
            rc = st;
            if st != 0 {
                break;
            }
        }
        Ok((rc, out))
    }

    /// fc_arm, before step $fc_step.
    fn fc_arm(&mut self) -> R<()> {
        let mut sa = self.sv_addr(&self.ext.fc_svc.clone());
        let mut tp = self.sv_port(&self.ext.fc_tgt.clone());
        if self.ext.fc_tgt == "workload" {
            tp = format!("s{}", self.sv_port(&self.ext.fc_svc.clone()));
            if !sa.is_empty() {
                sa = String::new();
            }
        }
        self.ext.fc_on = 1;
        self.ext.xtables = true;
        self.insmod(
            "x_tables ip_tables iptable_filter xt_tcpudp xt_NFQUEUE nfnetlink nfnetlink_queue",
        )?;
        // $HYDO_FC 7 $H/pid.$fc_svc "${sa:--}" "${tp:--}" $fc_dir $fc_n "$fc_match" $fc_dur $H/fc.log $H &
        let a2 = if sa.is_empty() { s("-") } else { sa };
        let a3 = if tp.is_empty() { s("-") } else { tp };
        let sip = if a2 != "-" { Some(a2.clone()) } else { None };
        let (port, own_port) = if a3 == "-" {
            (None, None)
        } else if let Some(o) = a3.strip_prefix('s') {
            (None, Some(atol(o) as u32))
        } else {
            (Some(atol(&a3) as u32), None)
        };
        if sip.is_none() && port.is_none() && own_port.is_none() {
            // the decision: not sent; the helper's (empty) log only
            self.send(write(&format!("{H}/fc.log"), "", false))?;
        } else {
            let id = self.send(Command::Freeze(Freeze {
                h: self.ext.fc_svc.clone(),
                queue: 7,
                sip,
                port,
                own_port,
                dir: if self.ext.fc_dir == "req" {
                    Dir::Req
                } else {
                    Dir::Resp
                },
                n: atol(&self.ext.fc_n) as u64,
                pattern: if self.ext.fc_match != "-" {
                    Some(self.ext.fc_match.clone())
                } else {
                    None
                },
                dur_ms: atol(&self.ext.fc_dur) as u64,
                log: Some(format!("{H}/fc.log")),
            }))?;
            self.ext.fc_sent = Some((id, self.ext.fc_dur.clone()));
            // the wait for $H/fc.bound: the agent's reply. The decision: a freeze the agent
            // refused is not armed, so $fc_pid stays empty and fc_kill sends nothing
            if self.reply(id)?.is_err() {
                return self.die(s("HYDO-ORACLE: ERROR freeze helper did not start"));
            }
            self.ext.fc_pid = Some(id);
        }
        let (rc, out) = self.fc_rules("-A")?;
        if rc == 3 {
            let line = format!(
                "freeze: {} and {} have neither addresses nor a port, skipped",
                self.ext.fc_svc, self.ext.fc_tgt
            );
            self.note(line)?;
            self.fc_stop_helper()?;
            return Ok(());
        }
        if rc != 0 {
            let mut ls: Vec<&str> = out.split('\n').collect();
            if out.ends_with('\n') {
                ls.pop();
            }
            if out.is_empty() {
                ls.clear();
            }
            for l in ls {
                self.note_raw(l)?;
            }
            return self.die(s("HYDO-ORACLE: ERROR freeze rules failed"));
        }
        let e = &self.ext;
        let line = format!(
            "FREEZE ARMED before step {}: {} at {} #{} with {}",
            e.fc_step, e.fc_svc, e.fc_dir, e.fc_n, e.fc_tgt
        );
        self.note(line)
    }

    /// fc_stop_helper: `kill $fc_pid; wait $fc_pid` = the agent's unfreeze; its answer.
    fn fc_stop_helper(&mut self) -> R<Option<UnfreezeOk>> {
        let Some(_) = self.ext.fc_pid else {
            return Ok(None);
        };
        let id = self.send(Command::Unfreeze(Unfreeze {}))?;
        self.ext.fc_pid = None;
        Ok(match self.reply(id)? {
            Ok(Reply::Unfreeze(u)) => Some(u),
            _ => None,
        })
    }

    /// fc_kill: stop_periodic's wrapper.
    pub(super) fn fc_kill(&mut self) -> R<()> {
        if self.ext.fc_pid.is_some() {
            self.fc_stop_helper()?;
        }
        Ok(())
    }

    /// $H/fc.fired and $H/fc.resumed as far as the shell sees them: the helper's events seen
    /// before the point where the controller's main sequence continues.
    fn markers(&self) -> (bool, bool) {
        let p = self.point();
        let (mut f, mut r) = (false, false);
        for it in &self.items[..p] {
            if let Item::Got {
                msg: Msg::Event(e), ..
            } = it
            {
                match e {
                    Event::Froze(_) => f = true,
                    Event::Warn(w) if w.code == WarnCode::FreezeNoProcess => f = true,
                    Event::Resumed(_) => r = true,
                    _ => {}
                }
            }
        }
        (f, r)
    }

    /// fc_heal: restart_dead's wrapper.
    pub(super) fn fc_heal(&mut self) -> R<()> {
        if self.ext.fc_on != 1 {
            return Ok(());
        }
        self.ext.fc_on = 2;
        let (f, r) = self.markers();
        if f && !r {
            let lim = atol(&self.ext.fc_dur) / 1000 + 30;
            let mut n = 0;
            while !self.markers().1 && n < lim {
                let e = self.env(&[]);
                self.run(plain(&["sleep", "1"]), e, None)?;
                n += 1;
            }
        }
        self.fc_rules("-D")?;
        let (f, r) = match self.fc_stop_helper()? {
            Some(u) => (u.fired, u.resumed),
            None => self.markers(),
        };
        let svc = self.ext.fc_svc.clone();
        if f && !r {
            self.sig(&svc, Sig::Cont)?;
            self.note(format!("freeze of {svc} ended by the init"))?;
        }
        if f {
            return self.note(s("freeze healed"));
        }
        let e = self.env(&[]);
        let (_, count) = self.run(
            vec![
                s(BB),
                s("sh"),
                s("-c"),
                format!("{BB} grep -c ' #' {H}/fc.log 2>/dev/null"),
            ],
            e,
            None,
        )?;
        let count = count.trim_end_matches('\n').to_string();
        let line = format!(
            "freeze never fired: {count} of {} matching events after step {}",
            self.ext.fc_n, self.ext.fc_step
        );
        self.note(line)?;
        if self.ext.fc_n == "0" {
            let t = self.read_all(&format!("{H}/fc.log"))?;
            for l in fc_trace(&t) {
                self.report(l)?;
            }
        }
        Ok(())
    }

    /// The helper's console lines, checked on their own: one FROZE line and its record per
    /// froze event, a RESUMED line per resumed event, its warnings; each at or after the event,
    /// all of them for the events seen before the end.
    pub(super) fn check_freeze_watch(&self) -> Result<(), String> {
        let label = "freeze";
        let items: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| i.by() == Some(label))
            .collect();
        let Some((id, dur)) = self.ext.fc_sent.clone() else {
            return if items.is_empty() {
                Ok(())
            } else {
                Err(format!("helper lines without a freeze: {items:?}"))
            };
        };
        let dur = atol(&dur);
        let dname = |d: Dir| if d == Dir::Req { "req" } else { "resp" };
        let mut want: Vec<(u64, String)> = Vec::new();
        for it in self.items {
            let Item::Got {
                n,
                msg: Msg::Event(e),
            } = it
            else {
                continue;
            };
            match e {
                Event::Froze(f) if f.id == id => {
                    let not_all = if f.all_stopped {
                        ""
                    } else {
                        ", NOT all threads stopped"
                    };
                    want.push((*n, format!("hydo-fc: FROZE {} ({} processes{not_all}) at {} #{} ({}{}, {}>{} len={}) for {dur} ms: {}", f.h, f.procs, dname(f.dir), f.count, dname(f.kdir), f.k, f.sport, f.dport, f.len, f.snippet)));
                    want.push((*n, s("HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.freeze_call\",\"ok\":true,\"msg\":\"a service was frozen at a call (F=)\"}")));
                }
                Event::Resumed(r) if r.id == id => {
                    want.push((
                        *n,
                        format!("hydo-fc: RESUMED after {dur} ms ({} processes)", r.procs),
                    ));
                }
                Event::Warn(w)
                    if matches!(
                        w.code,
                        WarnCode::FreezeNoProcess
                            | WarnCode::NetlinkEnobufs
                            | WarnCode::VerdictFailed
                    ) =>
                {
                    want.push((*n, format!("hydo-fc: {}", w.msg)));
                }
                _ => {}
            }
        }
        for (k, it) in items.iter().enumerate() {
            let Item::Report { n, line, .. } = it else {
                return Err(format!("{label}: expected a report line, got {it:?}"));
            };
            let Some((at, w)) = want.get(k) else {
                return Err(format!(
                    "{label}: a line the helper does not print: {line:?}"
                ));
            };
            if line != w || n < at {
                return Err(format!(
                    "{label}: expected {w:?} (seen at stop {at})\n  got {line:?} at stop {n}"
                ));
            }
        }
        if let Some((at, w)) = want.get(items.len()) {
            if *at < self.end_stop {
                return Err(format!("{label} did not print {w:?} (seen at stop {at})"));
            }
        }
        Ok(())
    }
}
