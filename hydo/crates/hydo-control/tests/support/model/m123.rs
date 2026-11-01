//! The shell model's part for the feature controllers of M123, transliterated from the shell
//! files (not from the controller): `hydo/onboard/hydo-init-history.sh` (hist_declare,
//! hist_final), `hydo/onboard/hydo-init-errlog.sh` (el_mark, el_report with EL_SCAN and EL_SUM, the
//! wrapped mount_data, step_range, retry_workload, failed_ids, final_verdict, die_oracle) and
//! `hydo/onboard/hydo-composer.sh` (setw with parse_w, PLAN_AWK and RES_AWK; first; step; the
//! daemon's rounds, launches and drain; run1; stop_all; verdict, end and check).
//!
//! The awk programs are written again here as awk runs them: EL_SCAN's clean/nums/norm with
//! awk's match()/substr()/gsub() on bytes (gsub's leftmost-longest match found by trying every
//! end, not with a DFA), PLAN_AWK's Park-Miller generator itself (not hydo-composer's), RES_AWK
//! field by field. Where the controller had to choose how a shell construct becomes agent
//! commands (HANDOFF.md, M123), the model checks that choice as written there.
// the if / else if chains are the awk's, as written there
#![allow(clippy::if_same_then_else)]

use super::{bad, plain, run_of, s, Halt, Model, BB, H, R};
use crate::support::fake::Msg;
use crate::support::Item;
use hydo_agent_proto::msg::*;
use std::collections::{BTreeMap, BTreeSet};

/// The model's part of the init's state for M123.
#[derive(Default, Debug)]
pub struct St {
    pub recovery: bool,
    pub hist_declared: bool,
    pub el_reported: bool,
    /// final_verdict's output while it goes to $H/el.verdict.
    pub held: Option<Vec<String>>,
    /// compose.W
    pub w: String,
    /// $C/started
    pub started: bool,
    /// (the item where `step` wrote $C/gate, TO)
    pub gates: Vec<(usize, i64)>,
    /// the daemon's environment
    pub denv: Vec<String>,
    /// main's step calls that came back because the daemon waited at TO: (the item where main
    /// went on, TO)
    pub returned: Vec<(usize, i64)>,
    /// stop_all ran (the item where it began)
    pub stopped: Option<usize>,
    /// main's step calls that came back because the time was up: (the item, TO)
    pub timed_out: Vec<(usize, i64)>,
}

const C: &str = "/tmp/hydo/compose";
const SH: &str = "/bin/busybox sh /etc/hydo/compose.sh";

fn timeout_of(secs: i64, cmd: &str) -> Vec<String> {
    let mut a = plain(&["timeout", &secs.to_string()]);
    a.extend(run_of(cmd));
    a
}

fn spawn_to(h: &str, argv: Vec<String>, env: Vec<String>, out: &str, append: bool) -> Command {
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

fn kill(h: &str) -> Command {
    Command::Signal(Signal {
        h: s(h),
        sig: Sig::Kill,
        tree: true,
        wait_stopped: None,
    })
}

fn jesc(x: &str) -> String {
    super::jesc(x)
}

/// awk's `x + 0`.
fn num(x: &str) -> f64 {
    let t = x.trim_start();
    let mut end = 0;
    let mut seen = false;
    for (i, c) in t.char_indices() {
        let ok = c.is_ascii_digit()
            || (i == 0 && (c == '-' || c == '+'))
            || (c == '.' && !t[..i].contains('.'));
        if !ok {
            break;
        }
        seen |= c.is_ascii_digit();
        end = i + c.len_utf8();
    }
    if !seen {
        return 0.0;
    }
    t[..end].parse().unwrap_or(0.0)
}

// ================================================================== the awk text, again

thread_local! {
    static RX: std::cell::RefCell<BTreeMap<String, regex::bytes::Regex>> = Default::default();
}

/// A regular expression of the awk programs, for the patterns they use: Rust syntax, bytes.
fn rx(p: &str) -> regex::bytes::Regex {
    RX.with(|c| {
        c.borrow_mut()
            .entry(s(p))
            .or_insert_with(|| regex::bytes::Regex::new(&format!("(?-u){p}")).unwrap())
            .clone()
    })
}

/// awk's match(): the leftmost start, then the longest end, every end tried from the longest
/// (up to 512 bytes).
fn awk_match(re: &regex::bytes::Regex, s: &[u8], from: usize) -> Option<(usize, usize)> {
    let m = re.find_at(s, from)?;
    let a = m.start();
    let body = re.as_str().trim_start_matches("(?-u)").to_string();
    let full = rx(&format!("^(?:{body})$"));
    let mut e = s.len().min(a + 512);
    while e > a {
        if full.is_match(&s[a..e]) {
            return Some((a, e));
        }
        e -= 1;
    }
    Some((a, m.end()))
}

/// match() of a pattern whose matches only grow by one byte at a time (`[class]+`, a single
/// byte): once a piece fails every longer one does, so the ends are tried upwards.
fn awk_match_run(re: &regex::bytes::Regex, s: &[u8], from: usize) -> Option<(usize, usize)> {
    let m = re.find_at(s, from)?;
    let a = m.start();
    let body = re.as_str().trim_start_matches("(?-u)").to_string();
    let full = rx(&format!("^(?:{body})$"));
    let mut e = a + 1;
    while e < s.len() && full.is_match(&s[a..e + 1]) {
        e += 1;
    }
    Some((a, e))
}

/// awk's gsub(re, repl, s) for these patterns (none of them matches the empty string).
fn awk_gsub(re: &regex::bytes::Regex, repl: &[u8], s: &[u8]) -> Vec<u8> {
    awk_gsub_by(re, repl, s, awk_match)
}

type Finder = fn(&regex::bytes::Regex, &[u8], usize) -> Option<(usize, usize)>;

fn awk_gsub_by(re: &regex::bytes::Regex, repl: &[u8], s: &[u8], find: Finder) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    if !re.is_match(s) {
        return s.to_vec();
    }
    while let Some((a, e)) = find(re, s, i) {
        out.extend_from_slice(&s[i..a]);
        for &c in repl {
            if c == b'&' {
                out.extend_from_slice(&s[a..e]);
            } else {
                out.push(c);
            }
        }
        i = e;
    }
    out.extend_from_slice(&s[i..]);
    out
}

fn substr(s: &[u8], m: usize, n: usize) -> Vec<u8> {
    let a = (m - 1).min(s.len());
    s[a..(a + n).min(s.len())].to_vec()
}

/// clean(s), one gsub at a time.
fn clean(s: &[u8]) -> Vec<u8> {
    let mut s = awk_gsub_by(&rx(r"\\"), b"/", s, awk_match_run);
    s = awk_gsub_by(&rx("\""), b"'", &s, awk_match_run);
    s = awk_gsub_by(&rx(r"[\x00-\x1f\x7f]"), b" ", &s, awk_match_run);
    s = awk_gsub(&rx("hydo-"), b"hydo_", &s);
    awk_gsub(&rx("HYDO-"), b"HYDO_", &s)
}

fn ishost(t: &[u8], hosts: &[String]) -> bool {
    t == b"localhost" || hosts.iter().any(|h| h.as_bytes() == t)
}

/// nums(s), the while (match(...)) loop as written.
fn nums(s0: &[u8], hosts: &[String]) -> Vec<u8> {
    let tokre = rx("[0-9A-Za-z]+");
    let mut s = s0.to_vec();
    let mut out: Vec<u8> = Vec::new();
    let mut prev: Vec<u8> = Vec::new();
    let m = |re: &str, x: &[u8]| rx(re).is_match(x);
    while let Some((a, e)) = awk_match_run(&tokre, &s, 0) {
        let (rstart, rlength) = (a + 1, e - a);
        let pre = substr(&s, 1, rstart - 1);
        let orig = substr(&s, rstart, rlength);
        let mut tok = orig.clone();
        s = s[(rstart + rlength - 1).min(s.len())..].to_vec();
        out.extend_from_slice(&pre);
        if m("^[0-9]+$", &tok) {
            let mut keep = false;
            let n = num(std::str::from_utf8(&tok).unwrap());
            if pre == b"-" && m("^[A-Z][A-Z]+$", &prev) {
                keep = true;
            } else if m("^[ :=]+$", &pre)
                && m(
                    "^(errno|Errno|ERRNO|code|Code|CODE|error|Error|ERROR|status|Status|STATUS)$",
                    &prev,
                )
            {
                keep = true;
            } else if pre == b":"
                && tok.len() <= 5
                && n < 32768.0
                && (m(r"#\.#\.#\.#:$", &out) || m(r"\]:$", &out) || ishost(&prev, hosts))
            {
                keep = true;
            }
            if !keep {
                tok = b"#".to_vec();
            }
        } else if m("[0-9]", &tok) {
            if tok.len() >= 8 && m("^[0-9a-fA-F]+$", &tok) {
                tok = b"<hex>".to_vec();
            } else if tok.len() >= 16 && m("[A-Za-z]", &tok) {
                tok = b"<id>".to_vec();
            } else if !m("^[A-Z]+[0-9]+$", &tok) {
                tok = awk_gsub_by(&rx("[0-9]+"), b"#", &tok, awk_match_run);
            }
        }
        out.extend_from_slice(&tok);
        prev = orig;
    }
    out.extend_from_slice(&s);
    out
}

/// norm(s): the fixed substitutions as the awk text has them ([-\/] holds a backslash in busybox).
fn norm(s0: &[u8], hosts: &[String]) -> Vec<u8> {
    let subs: [(&str, &[u8]); 7] = [
        (
            r"[0-9]{4}-[0-9]{2}-[0-9]{2}[T ][0-9]{2}:[0-9]{2}(:[0-9]{2}([.,][0-9]+)?)?(Z|[+-][0-9]{2}:?[0-9]{2})?",
            b"<time>",
        ),
        (r"[0-9]{4}[-\\/][0-9]{2}[-\\/][0-9]{2}", b"<date>"),
        (r"[0-9]?[0-9]:[0-9]{2}:[0-9]{2}([.,][0-9]+)?", b"<time>"),
        (
            r"[0-9]?[0-9][ -](Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[ -][0-9]{4}",
            b"<date>",
        ),
        (
            r"((Mon|Tue|Wed|Thu|Fri|Sat|Sun),? +)?(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) +[0-9]?[0-9],?( [0-9]{4})? <time>",
            b"<date> <time>",
        ),
        (
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]+",
            b"<uuid>",
        ),
        (r"0[xX][0-9a-fA-F]+", b"<hex>"),
    ];
    let mut s = s0.to_vec();
    for (p, r) in subs {
        s = awk_gsub(&rx(p), r, &s);
    }
    s = nums(&s, hosts);
    s = awk_gsub_by(&rx("[ \t]+"), b" ", &s, awk_match_run);
    if s.first() == Some(&b' ') {
        s.remove(0);
    }
    if s.last() == Some(&b' ') {
        s.pop();
    }
    substr(&s, 1, 200)
}

/// One el.rec line: svc, src, tpl, lab, ex.
type ElRec = (String, String, Vec<u8>, String, Vec<u8>);

/// EL_SCAN on one stream (the patterns are hydo-build's presets, plain ERE the regex crate reads
/// as written).
#[allow(clippy::too_many_arguments)]
fn el_scan(
    svc: &str,
    src: &str,
    lev: &str,
    exc: &str,
    ign: &str,
    stream: &[u8],
    marks: &[u8],
    base: &[u8],
    first: &str,
    hosts: &[String],
) -> Vec<ElRec> {
    for p in [lev, exc, ign] {
        assert!(
            !p.contains('{') && !p.contains("\\d"),
            "the model reads only the presets' plain ERE: {p}"
        );
    }
    let mut mlab = Vec::new();
    let mut mcnt = Vec::new();
    for ml in String::from_utf8_lossy(marks).lines() {
        let m: Vec<&str> = ml.split_whitespace().collect();
        mlab.push(s(m.first().copied().unwrap_or("")));
        mcnt.push(num(m.get(1).copied().unwrap_or("")));
    }
    let base = num(String::from_utf8_lossy(base).trim_end_matches('\n'));
    let mut lab = s(first);
    let mut mi = 0;
    let mut trace = false;
    let mut out = Vec::new();
    let mut lines: Vec<&[u8]> = stream.split(|c| *c == b'\n').collect();
    if lines.last() == Some(&&b""[..]) {
        lines.pop();
    }
    for (k, line) in lines.iter().enumerate() {
        let nr = (k + 1) as f64;
        while mi < mlab.len() && nr > mcnt[mi] {
            lab = mlab[mi].clone();
            mi += 1;
        }
        if nr <= base {
            continue;
        }
        if line.starts_with(b"HYDO-") || (!ign.is_empty() && rx(ign).is_match(line)) {
            trace = false;
            continue;
        }
        if !lev.is_empty() && rx(lev).is_match(line) {
        } else if !exc.is_empty() && rx(exc).is_match(line) {
            if trace {
                continue;
            }
        } else {
            if trace && (rx("^[ \t]").is_match(line) || line.starts_with(b"Caused by")) {
                continue;
            }
            trace = false;
            continue;
        }
        trace = true;
        let c = clean(line);
        out.push((
            s(svc),
            s(src),
            norm(&c, hosts),
            lab.clone(),
            substr(&c, 1, 240),
        ));
    }
    out
}

/// EL_SUM over el.rec.
fn el_sum(recs: &[ElRec], max: i64, boot: &str, steps: i64) -> Vec<String> {
    let rank = |l: &str| match l {
        "s" => -1.0,
        "end" => 1_000_000.0,
        "r" => 1_000_001.0,
        x => num(x),
    };
    let mut keys: Vec<(String, Vec<u8>)> = Vec::new();
    let mut cnt: BTreeMap<(String, Vec<u8>), usize> = BTreeMap::new();
    let mut src: BTreeMap<(String, Vec<u8>), String> = BTreeMap::new();
    let mut ex: BTreeMap<(String, Vec<u8>), Vec<u8>> = BTreeMap::new();
    let mut lb: BTreeMap<(String, Vec<u8>), Vec<String>> = BTreeMap::new();
    for (sv, sr, tpl, l, e) in recs {
        let k = (sv.clone(), tpl.clone());
        if !cnt.contains_key(&k) {
            keys.push(k.clone());
            src.insert(k.clone(), sr.clone());
            ex.insert(k.clone(), e.clone());
            lb.insert(k.clone(), Vec::new());
            cnt.insert(k.clone(), 0);
        }
        *cnt.get_mut(&k).unwrap() += 1;
        let v = lb.get_mut(&k).unwrap();
        if !v.contains(l) {
            v.push(l.clone());
        }
    }
    let mut out = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        if (i as i64) >= max {
            break;
        }
        let mut t = lb[k].clone();
        t.sort_by(|a, b| rank(a).partial_cmp(&rank(b)).unwrap());
        out.push(format!(
            "HYDO-ERRLOG {{\"svc\":\"{}\",\"src\":\"{}\",\"n\":{},\"at\":\"{}\",\"tpl\":\"{}\",\"ex\":\"{}\"}}",
            k.0,
            src[k],
            cnt[k],
            t.join(","),
            String::from_utf8_lossy(&k.1),
            String::from_utf8_lossy(&ex[k])
        ));
    }
    let nk = keys.len() as i64;
    out.push(format!(
        "HYDO-ERRLOG-SUM {{\"boot\":\"{boot}\",\"steps\":{steps},\"lines\":{},\"kinds\":{nk},\"shown\":{}}}",
        recs.len(),
        nk.min(max)
    ));
    out
}

/// parse_w's variables.
#[derive(Clone, Debug, Default)]
struct Wv {
    w: String,
    ws: i64,
    wp: String,
    wq: String,
    wr: String,
    wn: String,
    wt: String,
    wx: String,
    wo: String,
    wv: String,
}

/// PLAN_AWK's generator and plan, as the awk program writes it (doubles are exact here).
fn plan_awk(kinds: &[String], ids: &[String], v: &Wv, steps: i64) -> Option<Vec<String>> {
    const M: i64 = 2147483647;
    let mut st: i64 = v.ws % (M - 1) + 1;
    let mut nxt = || {
        st = ((st as i128 * 48271) % M as i128) as i64;
        st
    };
    let q = num(&v.wq) as i64;
    let (mut p, mut se, mut sg, mut a) = (vec![], vec![], vec![], vec![]);
    for (i, k) in kinds.iter().enumerate() {
        match k.as_str() {
            "parallel_driver" => p.push(i),
            "serial_driver" => se.push(i),
            "singleton_driver" => sg.push(i),
            "anytime" => a.push(i),
            _ => {}
        }
    }
    for _ in 0..8 {
        nxt();
    }
    let mut es: Vec<(i64, usize, i64)> = Vec::new();
    let mut single = false;
    if !sg.is_empty() {
        if p.len() + se.len() == 0 {
            single = true;
        } else if nxt() % 4 == 0 {
            single = true;
        }
    }
    let mut g = 0;
    if single {
        g = sg[(nxt() % sg.len() as i64) as usize];
    }
    for r in 0..steps {
        if single {
            if r == 0 {
                es.push((r, g, nxt()));
            }
        } else if !se.is_empty() && (p.is_empty() || nxt() % 4 == 0) {
            let c = se[(nxt() % se.len() as i64) as usize];
            es.push((r, c, nxt()));
        } else if !p.is_empty() {
            if q == 0 {
                return None; // awk: division by zero
            }
            let kk = 1 + nxt() % q;
            for _ in 0..kk {
                let c = p[(nxt() % p.len() as i64) as usize];
                es.push((r, c, nxt()));
            }
        }
        if !a.is_empty() && ((p.len() + se.len() + sg.len() == 0) || nxt() % 3 == 0) {
            let c = a[(nxt() % a.len() as i64) as usize];
            es.push((r, c, nxt()));
        }
    }
    let inlist = |lst: &str, x: f64| {
        lst.split('.').any(|it| {
            if it.is_empty() {
                false
            } else if it.contains('-') {
                let b: Vec<&str> = it.split('-').collect();
                x >= num(b[0]) && x <= num(b.get(1).copied().unwrap_or(""))
            } else {
                x == num(it)
            }
        })
    };
    let omitted = |c: usize| {
        if !v.wo.chars().any(|x| x.is_ascii_alphabetic()) {
            inlist(&v.wo, c as f64)
        } else {
            v.wo.split('+').any(|x| x == ids[c])
        }
    };
    let n = num(&v.wn);
    let mut out = Vec::new();
    for (i, (r, c, sd)) in es.iter().enumerate() {
        if inlist(&v.wx, i as f64) || omitted(*c) {
            continue;
        }
        if n > 0.0 && out.len() as f64 >= n {
            break;
        }
        out.push(format!("{i} {r} {c} {} {sd}", kinds[*c]));
    }
    Some(out)
}

/// RES_AWK.
fn res_awk(kinds: &[String], ids: &[String], v: &str, steps: i64) -> Result<Vec<String>, String> {
    let all: String = ids.iter().map(|i| format!(" {i}")).collect();
    let f: Vec<&str> = v[1..].split(',').collect();
    let mut nc: i64 = -1;
    let mut ci: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    let prt = regex::Regex::new("^[prt][0-9]+$").unwrap();
    let ent = regex::Regex::new("^[0-9]+[.][0-9]+[.][0-9]+[.][0-9]+$").unwrap();
    for x in f {
        if prt.is_match(x) {
            continue;
        }
        if x.starts_with('c') && nc < 0 {
            let cs: Vec<&str> = x[1..].split('+').filter(|_| x.len() > 1).collect();
            nc = cs.len() as i64;
            for c in cs {
                let Some(i) = ids.iter().position(|d| d == c) else {
                    return Err(format!("the composition uses command {c}, which this image does not have (commands:{all})"));
                };
                let k = kinds[i].as_str();
                if k != "parallel_driver"
                    && k != "serial_driver"
                    && k != "singleton_driver"
                    && k != "anytime"
                {
                    return Err(format!("command {c} is not a driver or anytime command"));
                }
                ci.push(i);
            }
            continue;
        }
        if ent.is_match(x) {
            let e: Vec<&str> = x.split('.').collect();
            if num(e[2]) >= nc as f64 {
                return Err(format!(
                    "entry {x} names command {} of a table of {}",
                    e[2],
                    if nc < 0 { 0 } else { nc }
                ));
            }
            if num(e[1]) >= steps as f64 {
                return Err(format!(
                    "entry {x} is in round {}, the plan has {steps} rounds",
                    e[1]
                ));
            }
            let c = ci[num(e[2]) as usize];
            out.push(format!(
                "{} {} {} {} {}",
                num(e[0]) as i64,
                num(e[1]) as i64,
                c,
                kinds[c],
                num(e[3]) as i64
            ));
            continue;
        }
        return Err(format!("field {x} is not understood"));
    }
    if out.is_empty() {
        return Err(s("no entry"));
    }
    Ok(out)
}

/// What setw/the daemon's wplan does with W: (console lines, Ok(vars, plan lines) | Err(WERR or
/// None when the shell itself fails)).
#[allow(clippy::type_complexity)]
fn wplan(m: &Model, w0: &str) -> (Vec<String>, Result<(Wv, Vec<String>), Option<String>>) {
    let cp = m.plan.compose.as_ref().unwrap();
    let kinds: Vec<String> = cp.commands.iter().map(|c| c.kind.clone()).collect();
    let ids: Vec<String> = cp.commands.iter().map(|c| c.id.clone()).collect();
    let mut con = Vec::new();
    let mut v = Wv {
        wp: cp.parallel.to_string(),
        wq: cp.per_round.to_string(),
        wr: cp.round_ms.to_string(),
        wn: cp.max_commands.to_string(),
        wt: cp.budget_ms.to_string(),
        ..Wv::default()
    };
    v.w = if w0 == "-" { s("") } else { s(w0) };
    if v.w.is_empty() {
        let p = plan_awk(&kinds, &ids, &v, m.plan.steps);
        return (con, p.map(|p| (v, p)).ok_or(Some(s(""))));
    }
    if let Some(k) = v.w.strip_prefix('@') {
        if k.is_empty()
            || !k
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        {
            return (
                con,
                Err(Some(format!(
                    "W={}: @ takes the key of a saved composition (hex)",
                    v.w
                ))),
            );
        }
        match cp.saved.get(k) {
            Some(x) if !x.is_empty() => v.wv = x.clone(),
            _ => {
                return (con, Err(Some(format!("W={}: this image carries no saved composition {k} (build it with the regression that has it)", v.w))))
            }
        }
    } else if v.w.starts_with(':') {
        v.wv = v.w.clone();
    }
    let fields = |x: &str| -> Vec<String> {
        let mut f: Vec<String> = x.split(',').map(s).collect();
        if f.last().map(String::as_str) == Some("") {
            f.pop();
        }
        f
    };
    if !v.wv.is_empty() {
        v.wn = s("0");
        v.wt = s("0");
        v.wq = s("-");
        for f in fields(&v.wv[1..]) {
            let d = f.chars().nth(1).is_some_and(|c| c.is_ascii_digit());
            if f.starts_with('p') && d {
                v.wp = s(&f[1..]);
            } else if f.starts_with('r') && d {
                v.wr = s(&f[1..]);
            } else if f.starts_with('t') && d {
                v.wt = s(&f[1..]);
            }
        }
        let r = res_awk(&kinds, &ids, &v.wv, m.plan.steps);
        return (
            con,
            r.map(|p| (v.clone(), p))
                .map_err(|e| Some(format!("W={}: {e}", v.w))),
        );
    }
    let fs = fields(&v.w);
    match i64::from_str_radix(&fs[0], 16) {
        Ok(x) if !fs[0].is_empty() && fs[0].len() <= 15 => v.ws = x,
        _ => match u64::from_str_radix(&fs[0], 16) {
            Ok(x) if fs[0].len() <= 16 => v.ws = x as i64,
            _ => return (con, Err(None)),
        },
    }
    for f in &fs[1..] {
        let rest = s(f.get(1..).unwrap_or(""));
        match f.chars().next() {
            Some('p') => v.wp = rest,
            Some('q') => v.wq = rest,
            Some('r') => v.wr = rest,
            Some('n') => v.wn = rest,
            Some('t') => v.wt = rest,
            Some('x') => v.wx = rest,
            Some('o') => v.wo = rest,
            _ => con.push(format!(
                "HYDO-COMPOSE: W={}: unknown field '{f}' ignored",
                v.w
            )),
        }
    }
    let p = plan_awk(&kinds, &ids, &v, m.plan.steps);
    (con, p.map(|p| (v, p)).ok_or(Some(s(""))))
}

// ================================================================== the model's steps

impl<'a> Model<'a> {
    fn send_item(&self, i: usize) -> Option<(u64, &'a Command)> {
        match &self.items[i] {
            Item::Send { id, cmd, .. } => Some((*id, cmd)),
            _ => None,
        }
    }

    fn refused_code(&self, id: u64) -> Option<ErrCode> {
        self.items.iter().find_map(|it| match it {
            Item::Got {
                msg: Msg::Refused(r, _, code, _),
                ..
            } if *r == id => Some(*code),
            _ => None,
        })
    }

    /// The agent's clock: centiseconds of uptime.
    fn clock(&mut self) -> R<i64> {
        let id = self.send(Command::ClockGet(ClockGet {}))?;
        match self.reply(id)? {
            Ok(Reply::ClockGet(c)) => Ok((c.mono_ms / 10) as i64),
            _ => Ok(0),
        }
    }

    /// `cat file` as bytes (text, then hex where the text read is refused).
    fn read_bytes(&mut self, path: &str) -> R<Vec<u8>> {
        let mut b: Vec<u8> = Vec::new();
        loop {
            let off = if b.is_empty() {
                None
            } else {
                Some(b.len() as u64)
            };
            let id = self.send(Command::Read(Read {
                path: s(path),
                offset: off,
                max: 3000,
                hex: None,
            }))?;
            let (data, eof) = match self.reply(id)? {
                Ok(Reply::Read(r)) => (r.data.into_bytes(), r.eof),
                Err(_) if self.refused_code(id) == Some(ErrCode::NotUtf8) => {
                    let h = self.send(Command::Read(Read {
                        path: s(path),
                        offset: off,
                        max: 1500,
                        hex: Some(true),
                    }))?;
                    match self.reply(h)? {
                        Ok(Reply::Read(r)) => {
                            let x: Vec<u8> = (0..r.data.len() / 2)
                                .map(|i| u8::from_str_radix(&r.data[2 * i..2 * i + 2], 16).unwrap())
                                .collect();
                            (x, r.eof)
                        }
                        _ => return Ok(b),
                    }
                }
                _ => return Ok(b),
            };
            b.extend_from_slice(&data);
            if eof || data.is_empty() {
                return Ok(b);
            }
        }
    }

    // ------------------------------------------------------------------ hydo-init-history.sh

    /// start_periodic() { _hist_orig_start_periodic "$@"; hist_declare; }
    pub(super) fn hist_declare(&mut self) -> R<()> {
        if self.plan.history.is_empty() || self.m.hist_declared {
            return Ok(());
        }
        self.m.hist_declared = true;
        for c in &self.plan.history {
            self.report(format!("HYDO-CHECK {}", c.json))?;
        }
        Ok(())
    }

    /// finish() { hist_final; _hist_orig_finish "$@"; }
    pub(super) fn hist_final(&mut self) -> R<()> {
        for (i, c) in self.plan.history.clone().iter().enumerate() {
            if c.final_.is_empty() {
                continue;
            }
            let e = self.env(&[]);
            let mut argv = plain(&["timeout", &self.plan.workload_timeout.to_string()]);
            argv.extend(run_of(&c.final_));
            let rc = self.spawn_wait(
                &format!("history:c{i}"),
                argv,
                e,
                &format!("{H}/workload.log"),
                true,
            )?;
            self.note(format!("check {}: final read rc={rc}", c.id))?;
            self.report(format!(
                "HYDO-CHECK-FINAL {{\"id\":\"{}\",\"rc\":{rc}}}",
                c.id
            ))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ hydo-init-errlog.sh

    fn el_cat(&self, svc: &str, k: usize) -> String {
        let sv = self
            .plan
            .errors
            .as_ref()
            .unwrap()
            .services
            .iter()
            .find(|x| x.name == svc)
            .unwrap();
        match k {
            0 => format!("{BB} cat {H}/{svc}.log.old {H}/{svc}.log"),
            1 => format!("{BB} cat {H}/{svc}.setup.log"),
            _ => format!("{BB} cat '{}'", sv.files[k - 2].replace('\'', "'\\''")),
        }
    }

    /// The bookkeeping pipelines run as one `sh -c` of their lines (the controller's choice:
    /// one command per el_mark / mount_data instead of one process per stream).
    fn el_script(&mut self, lines: Vec<String>) -> R<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let e = self.env(&[]);
        self.run(vec![s(BB), s("sh"), s("-c"), lines.join("\n")], e, None)?;
        Ok(())
    }

    /// mount_data(): after the mount, `el_cat $s $k | wc -l > $H/el.b.$s.$k` for k >= 2.
    pub(super) fn el_after_mount(&mut self) -> R<()> {
        let Some(ep) = self.plan.errors.clone() else {
            return Ok(());
        };
        let mut ls = Vec::new();
        for sv in &ep.services {
            for k in 2..sv.files.len() + 2 {
                ls.push(format!(
                    "{} 2>/dev/null | {BB} wc -l > {H}/el.b.{}.{k}",
                    self.el_cat(&sv.name, k),
                    sv.name
                ));
            }
        }
        self.el_script(ls)
    }

    /// el_mark <label>: `echo "$1 $(el_cat $s $k | $BB wc -l)" >> $H/el.m.$s.$k`.
    pub(super) fn el_mark(&mut self, label: &str) -> R<()> {
        let Some(ep) = self.plan.errors.clone() else {
            return Ok(());
        };
        let mut ls = Vec::new();
        for sv in &ep.services {
            for k in 0..sv.files.len() + 2 {
                ls.push(format!(
                    "echo \"{label} $({} 2>/dev/null | {BB} wc -l)\" >> {H}/el.m.{}.{k}",
                    self.el_cat(&sv.name, k),
                    sv.name
                ));
            }
        }
        self.el_script(ls)
    }

    /// el_report, once per boot. The streams, marks and bases are read in the shell's loop
    /// order (stream, then its marks, then its base: the controller's order).
    pub(super) fn el_report(&mut self) -> R<()> {
        let Some(ep) = self.plan.errors.clone() else {
            return Ok(());
        };
        if self.m.el_reported {
            return Ok(());
        }
        self.m.el_reported = true;
        let (first, boot) = if self.m.recovery {
            ("r", "recovery")
        } else {
            ("s", "first")
        };
        let hosts: Vec<String> = self.plan.services.iter().map(|x| x.name.clone()).collect();
        let mut rec = Vec::new();
        for sv in &ep.services {
            for k in 0..sv.files.len() + 2 {
                let (src, text) = match k {
                    0 => {
                        let mut t = self.read_bytes(&format!("{H}/{}.log.old", sv.name))?;
                        t.extend(self.read_bytes(&format!("{H}/{}.log", sv.name))?);
                        (s("log"), t)
                    }
                    1 => (
                        s("setup"),
                        self.read_bytes(&format!("{H}/{}.setup.log", sv.name))?,
                    ),
                    _ => (sv.files[k - 2].clone(), self.read_bytes(&sv.files[k - 2])?),
                };
                let marks = self.read_bytes(&format!("{H}/el.m.{}.{k}", sv.name))?;
                let base = self.read_bytes(&format!("{H}/el.b.{}.{k}", sv.name))?;
                rec.extend(el_scan(
                    &sv.name, &src, &sv.level, &sv.exc, &sv.ignore, &text, &marks, &base, first,
                    &hosts,
                ));
            }
        }
        for l in el_sum(&rec, ep.max, boot, self.plan.steps) {
            self.report(l)?;
        }
        Ok(())
    }

    /// failed_ids() at a cut: `for s in $HYDO_SERVICES; do svc_sig $s STOP; done; el_report`.
    pub(super) fn el_at_cut(&mut self) -> R<()> {
        if self.plan.errors.is_none() {
            return Ok(());
        }
        for sv in self.plan.services.clone() {
            self.sig(&sv.name, Sig::Stop)?;
        }
        self.el_report()
    }

    /// final_verdict() { _el_orig_final_verdict > $H/el.verdict; el_report; cat $H/el.verdict; }
    pub(super) fn el_final_verdict(&mut self) -> R<String> {
        if self.plan.errors.is_none() {
            return self.final_verdict();
        }
        self.m.held = Some(Vec::new());
        let v = self.final_verdict()?;
        let held = self.m.held.take().unwrap_or_default();
        self.el_report()?;
        for l in held {
            self.report(l)?;
        }
        Ok(v)
    }

    /// An assert_once line: on the console, or into $H/el.verdict.
    pub(super) fn assert_out(&mut self, line: String) -> R<()> {
        match &mut self.m.held {
            Some(h) => {
                h.push(line);
                Ok(())
            }
            None => self.report(line),
        }
    }

    /// Whether assert_once already printed (or redirected) this record.
    pub(super) fn held_has(&self, id: &str, ok: bool) -> bool {
        let mid = format!("\"id\":\"{id}\",\"ok\":{ok},");
        self.m.held.iter().flatten().any(|l| l.contains(&mid))
    }

    // ------------------------------------------------------------------ hydo-composer.sh

    /// set_knob W <value>: `HYDO_VALUE=$2 RUN "compose.sh setw \"$HYDO_VALUE\""`.
    pub(super) fn cmp_setw(&mut self, value: &str) -> R<()> {
        // the knob's default is '-'
        if value == "-" {
            return Ok(());
        }
        self.m.w = s(value);
        let (con, r) = wplan(self, value);
        for l in con {
            self.report(l)?;
        }
        match r {
            Ok(_) => self.note(format!("knob W={value} (test composer (W=))")),
            Err(Some(e)) => {
                self.report(format!("HYDO-COMPOSE error: {e}"))?;
                self.die(format!("HYDO-ORACLE: ERROR knob W={value} failed"))
            }
            Err(None) => self.die(format!("HYDO-ORACLE: ERROR knob W={value} failed")),
        }
    }

    /// provision's `( own\n) && compose.sh first`: Some(status) when provisioning is composed.
    pub(super) fn cmp_provision(&mut self) -> R<Option<u32>> {
        let Some(cp) = self.plan.compose.clone() else {
            return Ok(None);
        };
        let tail = format!("\n) && {SH} first");
        let own = if self.plan.provision == format!("{SH} first") {
            None
        } else if let Some(x) = self
            .plan
            .provision
            .strip_prefix("( ")
            .and_then(|x| x.strip_suffix(&tail))
        {
            Some(s(x))
        } else {
            return Ok(None);
        };
        if let Some(own) = own {
            // the controller runs the subshell on its own, and `first` only when it exited 0
            let e = self.env(&[]);
            let rc = self.spawn_wait(
                "provision",
                run_of(&format!("( {own}\n)")),
                e,
                &format!("{H}/provision.log"),
                false,
            )?;
            if rc != 0 {
                return Ok(Some(rc));
            }
        }
        self.send(Command::Mkdir(Mkdir {
            path: s(C),
            mode: None,
        }))?;
        let mut rcall = 0;
        for (ci, c) in cp.commands.iter().enumerate() {
            if c.kind != "first" {
                continue;
            }
            self.report(format!("HYDO-CMD start i=f{ci} kind=first cmd={}", c.id))?;
            let t1 = self.clock()?;
            let e = self.env(&[("HYDO_CMD", c.id.clone())]);
            let rc = self.spawn_wait(
                &format!("first:{ci}"),
                timeout_of(c.timeout, &c.cmd),
                e,
                &format!("{C}/log.f{ci}"),
                false,
            )?;
            let t2 = self.clock()?;
            self.report(format!(
                "HYDO-CMD end i=f{ci} cmd={} rc={rc} ms={}",
                c.id,
                (t2 - t1) * 10
            ))?;
            if rc != 0 {
                rcall = 1;
            }
        }
        Ok(Some(rcall))
    }

    /// step FROM TO (the workload of a composed plan): its exit status.
    pub(super) fn cmp_step(&mut self, a: i64, b: i64) -> R<u32> {
        let steps = self.plan.steps;
        self.send(Command::Mkdir(Mkdir {
            path: s(C),
            mode: None,
        }))?;
        let gate_at = self.main[self.k - 1];
        self.m.gates.push((gate_at, b));
        if !self.m.started {
            self.m.started = true;
            self.m.denv = self.env(&[("HYDO_FROM", a.to_string()), ("HYDO_TO", b.to_string())]);
        }
        let wt = self.plan.workload_timeout;
        let budget = if wt > 10 { wt - 5 } else { wt / 2 + 1 };
        let th = format!("compose-step:{b}");
        let tid = self.send(spawn_to(
            &th,
            plain(&["sleep", &budget.to_string()]),
            self.env(&[]),
            "/dev/null",
            false,
        ))?;
        // what the controller did when it went on: the shell's loop checks the round (or the
        // drain's end), then whether the daemon is gone, then the time
        let q = self.point();
        let before = |i: Option<usize>| i.is_some_and(|i| i < q);
        let done = before(self.composer_report_at("HYDO-COMPOSE done"));
        let timer = before(self.exits.get(&tid).map(|x| x.0));
        let died = before(self.composer_report_at("HYDO-COMPOSE error"));
        let kill_next =
            matches!(self.items.get(q), Some(Item::Send { cmd, .. }) if *cmd == kill(&th));
        let write_after = matches!(self.items.get(q + 1), Some(Item::Send { cmd: Command::Write(_), by, .. }) if by == "main");
        if kill_next && died && write_after && !(b >= steps && done) {
            self.send(kill(&th))?;
            let werr = self.composer_error_text();
            let mut t = s("compose.sh: the composer daemon died\n");
            if !werr.is_empty() {
                t.push_str(&werr);
                t.push('\n');
            }
            self.send(super::write(&format!("{H}/workload.log"), &t, true))?;
            return Ok(1);
        }
        if kill_next {
            if b >= steps {
                if !done {
                    return bad(format!("step {a} {b} came back before the drain's end"));
                }
            } else if !(done && self.m.stopped.is_some_and(|x| x < q)) {
                // (after stop_all the daemon is gone and $C/done exists: the step returns 0)
                // checked against the daemon's arrival at round b (check_composer)
                self.m.returned.push((q, b));
            }
            self.send(kill(&th))?;
            return Ok(0);
        }
        if !timer {
            return bad(format!("step {a} {b}: the controller went on before the round, the daemon's end or the timeout"));
        }
        if b < steps {
            self.m.timed_out.push((q, b));
        }
        if done {
            return Ok(1);
        }
        self.cmp_stop_all(a, b)?;
        Ok(1)
    }

    fn composer_report_at(&self, prefix: &str) -> Option<usize> {
        self.items.iter().position(|i| matches!(i, Item::Report { by, line, .. } if (by == "composer" || (by == "main" && prefix == "HYDO-COMPOSE done")) && line.starts_with(prefix)))
    }

    fn composer_error_text(&self) -> String {
        self.items
            .iter()
            .find_map(|i| match i {
                Item::Report { by, line, .. } if by == "composer" => {
                    line.strip_prefix("HYDO-COMPOSE error: ").map(s)
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The composed entries started before item p and not ended before it: (entry, kind, id).
    fn running_at(&self, p: usize) -> Vec<(i64, String, String)> {
        let mut run: Vec<(i64, String, String)> = Vec::new();
        for it in &self.items[..p] {
            let Item::Report { line, .. } = it else {
                continue;
            };
            if let Some(r) = line.strip_prefix("HYDO-CMD start i=") {
                let w: Vec<&str> = r.split(' ').collect();
                if let Ok(e) = w[0].parse::<i64>() {
                    let kind = w[2].trim_start_matches("kind=");
                    let id = w[3].trim_start_matches("cmd=");
                    run.push((e, s(kind), s(id)));
                }
            } else if let Some(r) = line.strip_prefix("HYDO-CMD end i=") {
                if let Ok(e) = r.split(' ').next().unwrap().parse::<i64>() {
                    run.retain(|x| x.0 != e);
                }
            }
        }
        run
    }

    /// The statuses of the entries ended before item p (status lines and killed lines).
    fn statuses_at(&self, p: usize) -> Vec<String> {
        self.items[..p]
            .iter()
            .filter_map(|it| match it {
                Item::Report { line, .. }
                    if line.starts_with("HYDO-CMD end i=")
                        && !line.starts_with("HYDO-CMD end i=e")
                        && !line.starts_with("HYDO-CMD end i=f") =>
                {
                    line.split(" status=")
                        .nth(1)
                        .map(|x| s(x.split(' ').next().unwrap()))
                }
                _ => None,
            })
            .collect()
    }

    fn started_at(&self, p: usize) -> usize {
        self.items[..p]
            .iter()
            .filter(|it| matches!(it, Item::Report { line, .. } if line.strip_prefix("HYDO-CMD start i=").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))))
            .count()
    }

    /// stop_all FROM TO.
    fn cmp_stop_all(&mut self, a: i64, b: i64) -> R<()> {
        let p = self.point();
        self.m.stopped = Some(p);
        let mut open = self.running_at(p);
        open.sort_by_key(|x| x.0.to_string());
        for (e, _, _) in &open {
            self.send(kill(&format!("cmd:{e}")))?;
        }
        for (e, _, id) in &open {
            self.report(format!(
                "HYDO-CMD end i={e} cmd={id} rc=137 status=killed (workload.timeout)"
            ))?;
        }
        let q = self.point();
        let st = self.statuses_at(q);
        let n = |x: &str| st.iter().filter(|y| *y == x).count();
        let sum = format!(
            "started={} ok={} failed={} timeout={} violation={} killed={} skipped=0",
            self.started_at(q),
            n("ok"),
            n("failed"),
            n("timeout"),
            n("violation"),
            n("killed")
        );
        self.report(format!(
            "HYDO-COMPOSE done {sum} (stopped: steps {a}..{} ran into workload.timeout={}s)",
            b - 1,
            self.plan.workload_timeout
        ))
    }

    /// An invariant command of the composer: Some((status, output)).
    pub(super) fn cmp_eval(&mut self, cmd: &str) -> R<Option<(u32, String)>> {
        let Some(cp) = self.plan.compose.clone() else {
            return Ok(None);
        };
        let Some(rest) = cmd.strip_prefix(&format!("{SH} ")) else {
            return Ok(None);
        };
        let p = self.point();
        if rest == "verdict" {
            // $C/violation (the first violation's entry, id and detail), else $C/summary
            let vio = self.items[..p].iter().find_map(|it| match it {
                Item::Report { by, line, .. }
                    if by.starts_with("cmd:")
                        && line.starts_with("HYDO-CMD end i=")
                        && line.ends_with("status=violation") =>
                {
                    Some((by.clone(), line.clone()))
                }
                _ => None,
            });
            if let Some((by, line)) = vio {
                let e = &by[4..];
                let id = line
                    .split(" cmd=")
                    .nth(1)
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap();
                let detail = self.run1_detail(&by);
                return Ok(Some((
                    1,
                    format!("command {id} (entry {e}) reported a violation (exit 100): {detail}\n"),
                )));
            }
            let sum = self.items[..p].iter().find_map(|it| match it {
                Item::Report { line, .. } => line
                    .strip_prefix("HYDO-COMPOSE done ")
                    .map(|x| s(x.split(" (stopped").next().unwrap())),
                _ => None,
            });
            return Ok(Some(match sum {
                Some(x) => (0, format!("{x}\n")),
                None => (0, s("no composed workload ran in this boot\n")),
            }));
        }
        if let Some(i) = rest
            .strip_prefix("end ")
            .and_then(|x| x.parse::<usize>().ok())
        {
            let c = cp.commands[i].clone();
            self.send(Command::Mkdir(Mkdir {
                path: s(C),
                mode: None,
            }))?;
            if c.kind == "finally" {
                let q = self.point();
                let done = self
                    .composer_report_at("HYDO-COMPOSE done")
                    .is_some_and(|x| x < q);
                let incomplete = self.items[..q].iter().any(|it| matches!(it, Item::Report { line, .. } if line.starts_with("HYDO-COMPOSE done") && (line.contains("(stopped") || !line.contains(" killed=0 ") || !line.contains(" timeout=0 "))));
                if !self.m.started {
                    return Ok(Some((0, s("skipped: no composed workload ran in this boot (recovery boot after a power cut)\n"))));
                }
                if !done || incomplete {
                    return Ok(Some((
                        0,
                        s("skipped: not every started command ran to completion\n"),
                    )));
                }
            }
            self.report(format!(
                "HYDO-CMD start i=e{i} kind={} cmd={}",
                c.kind, c.id
            ))?;
            let t1 = self.clock()?;
            let e = self.env(&[("HYDO_CMD", c.id.clone())]);
            let rc = self.spawn_wait(
                &format!("end:{i}"),
                timeout_of(c.timeout, &c.cmd),
                e,
                &format!("{C}/log.e{i}"),
                false,
            )?;
            let t2 = self.clock()?;
            let ms = (t2 - t1) * 10;
            self.report(format!("HYDO-CMD end i=e{i} cmd={} rc={rc} ms={ms}", c.id))?;
            let log = self.read_bytes(&format!("{C}/log.e{i}"))?;
            let text = String::from_utf8_lossy(&log).into_owned();
            let ls: Vec<&str> = text.split_terminator('\n').collect();
            for l in ls.iter().filter(|l| l.starts_with("HYDO-ASSERT")) {
                self.report(s(l))?;
            }
            if rc == 0 {
                return Ok(Some((0, format!("{} ok ({ms} ms)\n", c.id))));
            }
            let last = ls
                .iter()
                .rev()
                .find(|l| !l.starts_with("HYDO-ASSERT"))
                .copied()
                .unwrap_or("");
            return Ok(Some((1, format!("{} exited rc={rc}: {last}\n", c.id))));
        }
        if let Some(i) = rest
            .strip_prefix("check ")
            .and_then(|x| x.parse::<usize>().ok())
        {
            let c = cp.checks[i].clone();
            let e = self.env(&[("HYDO_CHECK", c.id.clone())]);
            let (rc, out) = self.run(timeout_of(c.timeout, &c.cmd), e, Some(3000))?;
            return Ok(Some(match rc {
                0..=2 => (rc, out),
                143 => (
                    2,
                    format!("{out}check {} timed out after {}s\n", c.id, c.timeout),
                ),
                _ => (1, out),
            }));
        }
        Ok(None)
    }

    /// run1's `grep -av '^HYDO-ASSERT' log | tail -1` for the entry, from the read replies of
    /// its log.
    fn run1_detail(&self, by: &str) -> String {
        let mut b: Vec<u8> = Vec::new();
        for it in self.items {
            if let Item::Send {
                by: x,
                cmd: Command::Read(_),
                id,
                ..
            } = it
            {
                if x == by {
                    if let Some((_, Ok(Reply::Read(r)))) = self.replies.get(id) {
                        if r.data.len() % 2 == 0
                            && r.data.bytes().all(|c| c.is_ascii_hexdigit())
                            && self.refused_code(id - 1) == Some(ErrCode::NotUtf8)
                        {
                            b.extend((0..r.data.len() / 2).map(|i| {
                                u8::from_str_radix(&r.data[2 * i..2 * i + 2], 16).unwrap()
                            }));
                        } else {
                            b.extend_from_slice(r.data.as_bytes());
                        }
                    }
                }
            }
        }
        let t = String::from_utf8_lossy(&b).into_owned();
        t.split_terminator('\n')
            .rev()
            .find(|l| !l.starts_with("HYDO-ASSERT"))
            .map(s)
            .unwrap_or_default()
    }

    /// Whether a composed command runs at item p (pgrep -f "compose.sh run1").
    pub(super) fn cmp_in_flight(&self, p: usize) -> bool {
        !self.running_at(p).is_empty()
    }

    // ------------------------------------------------------------------ the daemon and run1

    /// The daemon's and every run1's items, each on its own, against compose.sh.
    pub(super) fn check_composer(&self) -> Result<(), String> {
        let Some(cp) = self.plan.compose.as_ref() else {
            return Ok(());
        };
        let d: Vec<usize> = (0..self.items.len())
            .filter(|i| self.items[*i].by() == Some("composer"))
            .collect();
        if !self.m.started {
            return if d.is_empty() {
                Ok(())
            } else {
                Err(s("the composer ran without a step"))
            };
        }
        let mut w = Walk {
            m: self,
            idx: d,
            j: 0,
            label: s("composer"),
        };
        // the daemon may be cut off by the end of the conversation or by stop_all
        let mut arrivals: BTreeMap<i64, usize> = BTreeMap::new();
        let r = self.daemon(&mut w, cp, &mut arrivals);
        match r {
            Ok(()) | Err(Halt::Ended) => {}
            Err(Halt::Mismatch(e)) => return Err(format!("composer: {e}")),
        }
        if w.j < w.idx.len() {
            return Err(format!(
                "composer: more than compose.sh does: {:?}",
                self.items[w.idx[w.j]]
            ));
        }
        // a step that came back for round b did so once the daemon waited at b (the gate still
        // at b when it arrived); one that timed out did so while the daemon had not
        for (q, b) in &self.m.returned {
            match arrivals.get(b) {
                Some(a) if a < q && self.gate_at(a + 1) <= *b => {}
                other => return Err(format!("composer: step ..{b} came back at item {q}, the daemon's arrival at round {b}: {other:?}")),
            }
        }
        // (at one stop the main part goes first: when the daemon's arrival and the step's timer
        // are seen at the same stop, the time is up before the daemon takes in its arrival;
        // HANDOFF.md M123 decision 11)
        for (q, b) in &self.m.timed_out {
            if let Some(a) = arrivals.get(b) {
                let earlier = self.items[*a].stop() < self.items[*q].stop();
                if earlier && self.gate_at(a + 1) <= *b {
                    return Err(format!("composer: step ..{b} timed out at item {q} although the daemon waited at round {b} from item {a}"));
                }
            }
        }
        // every run1
        let labels: BTreeSet<String> = self
            .items
            .iter()
            .filter_map(|i| i.by())
            .filter(|b| b.starts_with("cmd:"))
            .map(s)
            .collect();
        for l in labels {
            let idx: Vec<usize> = (0..self.items.len())
                .filter(|i| self.items[*i].by() == Some(l.as_str()))
                .collect();
            let mut w = Walk {
                m: self,
                idx,
                j: 0,
                label: l.clone(),
            };
            match self.run1(&mut w, cp) {
                Ok(()) | Err(Halt::Ended) => {}
                Err(Halt::Mismatch(e)) => return Err(format!("{l}: {e}")),
            }
            if w.j < w.idx.len() {
                return Err(format!(
                    "{l}: more than run1 does: {:?}",
                    self.items[w.idx[w.j]]
                ));
            }
        }
        Ok(())
    }

    fn gate_at(&self, p: usize) -> i64 {
        self.m
            .gates
            .iter()
            .filter(|(i, _)| *i < p)
            .map(|x| x.1)
            .max()
            .unwrap_or(0)
    }

    fn daemon(
        &self,
        w: &mut Walk,
        cp: &hydo_control::schedule::composer::ComposePlan,
        arrivals: &mut BTreeMap<i64, usize>,
    ) -> R<()> {
        let steps = self.plan.steps;
        let (con, r) = wplan(self, &self.m.w);
        for l in con {
            w.report(&l)?;
        }
        let (v, plan) = match r {
            Ok(x) => x,
            Err(e) => {
                if let Some(e) = e {
                    w.report(&format!("HYDO-COMPOSE error: {e}"))?;
                }
                return Ok(());
            }
        };
        let line = if v.wv.is_empty() {
            format!(
                "HYDO-COMPOSE plan W={} seed={} p={} q={} r={} n={} rounds={steps} entries={}",
                if v.w.is_empty() { "-" } else { &v.w },
                v.ws,
                v.wp,
                v.wq,
                v.wr,
                v.wn,
                plan.len()
            )
        } else {
            format!(
                "HYDO-COMPOSE plan W={} resolved p={} r={} t={} rounds={steps} entries={}",
                v.w,
                v.wp,
                v.wr,
                v.wt,
                plan.len()
            )
        };
        w.report(&line)?;
        for c in &cp.commands {
            if [
                "parallel_driver",
                "serial_driver",
                "singleton_driver",
                "anytime",
            ]
            .contains(&c.kind.as_str())
            {
                w.report(&format!("HYDO-ASSERT-DECL {{\"t\":\"sometimes\",\"id\":\"compose.{}.succeeded\",\"msg\":\"{} exited 0 at least once\"}}", c.id, c.id))?;
                w.report(&format!("HYDO-ASSERT-DECL {{\"t\":\"always_or_unreachable\",\"id\":\"compose.{}.no_violation\",\"msg\":\"{} never reports a violation (exit 100)\"}}", c.id, c.id))?;
            }
        }
        let wr = v.wr.parse::<i64>().unwrap_or(0);
        let rc = if wr / 10 < 1 { 1 } else { wr / 10 };
        let wp = v.wp.parse::<i64>().ok();
        let wt = v.wt.parse::<i64>().unwrap_or(0);
        let (t0, mut arrive) = w.clock()?;
        let mut t = t0;
        let mut r: i64 = -1;
        let mut nskip = 0;
        for pl in &plan {
            let f: Vec<&str> = pl.split(' ').collect();
            let (pe, pr, pc, pk) = (
                f[0],
                f[1].parse::<i64>().unwrap(),
                f[2].parse::<usize>().unwrap(),
                f[3],
            );
            while r < pr {
                r += 1;
                arrive = self.round_start(w, r, rc, &mut t, arrive, arrivals)?;
            }
            // launch: can_start at the launch's clock
            let at = w.peek_index()?;
            let run = self.running_at(at);
            let n = |k: &str| run.iter().filter(|x| x.1 == k).count() as i64;
            let all = run.len() as i64;
            let ok = match wp {
                None => false,
                Some(wp) => match pk {
                    "parallel_driver" => {
                        all < wp && n("serial_driver") + n("singleton_driver") == 0
                    }
                    "anytime" => all < wp,
                    _ => {
                        n("parallel_driver") + n("serial_driver") + n("singleton_driver") == 0
                            && all < wp
                    }
                },
            };
            if !ok {
                return bad(format!(
                    "entry {pe} ({pk}) launched while can_start was false ({run:?})"
                ));
            }
            let (now, a2) = w.clock()?;
            arrive = a2;
            let id = &cp.commands[pc].id;
            if wt > 0 && (now - t0) * 10 >= wt {
                w.report(&format!(
                    "HYDO-CMD skip i={pe} r={r} cmd={id} budget_ms={wt}"
                ))?;
                nskip += 1;
            } else {
                w.report(&format!("HYDO-CMD start i={pe} r={r} kind={pk} cmd={id}"))?;
            }
        }
        while r < steps - 1 {
            r += 1;
            arrive = self.round_start(w, r, rc, &mut t, arrive, arrivals)?;
        }
        let _ = arrive;
        // drain
        let tid = w.send(
            spawn_to(
                "compose-drain",
                plain(&["sleep", &cp.drain_timeout.to_string()]),
                self.m.denv.clone(),
                "/dev/null",
                false,
            ),
            true,
        )?;
        let x = w.peek_index()?;
        let fired = self.exits.get(&tid).is_some_and(|e| e.0 < x);
        let mut open = self.running_at(x);
        // the order of $RUNNING: launch order
        let starts: Vec<i64> = self
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Report { by, line, .. } if by == "composer" => line
                    .strip_prefix("HYDO-CMD start i=")
                    .and_then(|r| r.split(' ').next())
                    .and_then(|e| e.parse().ok()),
                _ => None,
            })
            .collect();
        open.sort_by_key(|o| starts.iter().position(|e| *e == o.0));
        if open.is_empty() {
            w.send(kill("compose-drain"), false)?;
        } else if !fired {
            return bad(format!(
                "the drain killed {open:?} before its {}s were up",
                cp.drain_timeout
            ));
        }
        for (e, _, id) in &open {
            w.send(kill(&format!("cmd:{e}")), false)?;
            w.report(&format!("HYDO-CMD end i={e} cmd={id} rc=137 status=killed (still running {}s after the last round)", cp.drain_timeout))?;
        }
        let q = w.peek_index()?;
        let st = self.statuses_at(q);
        let n = |x: &str| st.iter().filter(|y| *y == x).count();
        w.report(&format!(
            "HYDO-COMPOSE done started={} ok={} failed={} timeout={} violation={} killed={} skipped={nskip}",
            self.started_at(q),
            n("ok"),
            n("failed"),
            n("timeout"),
            n("violation"),
            n("killed")
        ))?;
        // a main step for the last range comes back after this line, the earlier ones after the
        // daemon reached their round
        Ok(())
    }

    /// round_start r: the round's time, then the gate. `arrive` is where the daemon got its last
    /// answer; returns the new one.
    fn round_start(
        &self,
        w: &mut Walk,
        r: i64,
        rc: i64,
        t: &mut i64,
        mut arrive: usize,
        arrivals: &mut BTreeMap<i64, usize>,
    ) -> R<usize> {
        if r > 0 {
            *t += rc;
            let (now, a) = w.clock()?;
            arrive = a;
            if now < *t {
                let d = *t - now;
                let id = w.send_run(plain(&["sleep", &format!("{}.{:02}", d / 100, d % 100)]))?;
                arrive = w.waited_done(id)?;
            }
        }
        // the gate: the daemon goes on only once a step opened round r + 1
        arrivals.insert(r, arrive);
        let c = w.peek_index()?;
        if self.gate_at(c) <= r {
            return bad(format!(
                "round {r} began before the gate was open (gate {})",
                self.gate_at(c)
            ));
        }
        let (now, a) = w.clock()?;
        if now > *t {
            *t = now;
        }
        Ok(a)
    }

    fn run1(&self, w: &mut Walk, cp: &hydo_control::schedule::composer::ComposePlan) -> R<()> {
        let e: i64 = w.label[4..].parse().unwrap();
        // the start line names the round and the command; the plan names the seed
        let start = self
            .items
            .iter()
            .find_map(|i| match i {
                Item::Report { by, line, .. }
                    if by == "composer" && line.starts_with(&format!("HYDO-CMD start i={e} ")) =>
                {
                    Some(line.clone())
                }
                _ => None,
            })
            .ok_or_else(|| Halt::Mismatch(s("a run1 without its start line")))?;
        let round = start
            .split(" r=")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .to_string();
        let (_, r) = wplan(self, &self.m.w);
        let plan = r.map_err(|_| Halt::Mismatch(s("no plan")))?.1;
        let pl = plan
            .iter()
            .find(|l| l.split(' ').next() == Some(&e.to_string()))
            .ok_or_else(|| Halt::Mismatch(s("an entry the plan does not have")))?;
        let f: Vec<&str> = pl.split(' ').collect();
        let c = &cp.commands[f[2].parse::<usize>().unwrap()];
        let killed = self.items.iter().any(|i| matches!(i, Item::Report { line, .. } if line.starts_with(&format!("HYDO-CMD end i={e} cmd=")) && line.contains("status=killed")));
        let r = (|| -> R<()> {
            let (t1, _) = w.clock()?;
            let mut env = self.m.denv.clone();
            for (k, x) in [
                ("HYDO_CMD", c.id.clone()),
                ("HYDO_CMD_ID", e.to_string()),
                ("HYDO_ROUND", round.clone()),
                ("HYDO_SEED", s(f[4])),
            ] {
                env.retain(|y| !y.starts_with(&format!("{k}=")));
                env.push(format!("{k}={x}"));
            }
            env.sort();
            let log = format!("{C}/log.{e}");
            let id = w.send(
                spawn_to(
                    &w.label.clone(),
                    timeout_of(c.timeout, &c.cmd),
                    env,
                    &log,
                    false,
                ),
                true,
            )?;
            let rc = match self.replies.get(&id) {
                Some((_, Err(_))) => 127,
                _ => w.waited_exit(id)?,
            };
            let (t2, _) = w.clock()?;
            let st = match rc {
                0 => "ok",
                100 => "violation",
                143 => "timeout",
                _ => "failed",
            };
            let text = w.read_all(&log)?;
            let ls: Vec<&str> = text.split_terminator('\n').collect();
            for l in ls.iter().filter(|l| l.starts_with("HYDO-ASSERT")) {
                w.report(l)?;
            }
            w.report(&format!(
                "HYDO-CMD end i={e} cmd={} rc={rc} ms={} status={st}",
                c.id,
                (t2 - t1) * 10
            ))?;
            let here = w.idx.get(w.j.saturating_sub(1)).copied().unwrap_or(0);
            let earlier = |pat: &str| {
                self.items[..here]
                    .iter()
                    .any(|i| matches!(i, Item::Report { line, .. } if line.starts_with(pat)))
            };
            if st == "ok"
                && !earlier(&format!(
                    "HYDO-ASSERT {{\"t\":\"sometimes\",\"id\":\"compose.{}.succeeded\"",
                    c.id
                ))
            {
                w.report(&format!("HYDO-ASSERT {{\"t\":\"sometimes\",\"id\":\"compose.{}.succeeded\",\"ok\":true,\"msg\":\"{} exited 0 at least once\"}}", c.id, c.id))?;
            }
            if st == "violation" && !earlier(&format!("HYDO-ASSERT {{\"t\":\"always_or_unreachable\",\"id\":\"compose.{}.no_violation\"", c.id)) {
                let d = ls.iter().rev().find(|l| !l.starts_with("HYDO-ASSERT")).copied().unwrap_or("");
                w.report(&format!("HYDO-ASSERT {{\"t\":\"always_or_unreachable\",\"id\":\"compose.{}.no_violation\",\"ok\":false,\"msg\":\"{} never reports a violation (exit 100)\",\"details\":{{\"entry\":{e},\"out\":\"{}\"}}}}", c.id, c.id, jesc(d)))?;
            }
            Ok(())
        })();
        match r {
            Err(Halt::Ended) if killed || self.m.stopped.is_some() || self.end_stop != u64::MAX => {
                Ok(())
            }
            Err(Halt::Ended) => bad(s("run1 stopped before its end line")),
            Ok(()) if killed => bad(s("an entry the drain killed went on to its end line")),
            other => other,
        }
    }
}

/// One background part's items, walked in order.
struct Walk<'m, 'a> {
    m: &'m Model<'a>,
    idx: Vec<usize>,
    j: usize,
    label: String,
}

impl Walk<'_, '_> {
    /// The next item's index (Halt::Ended when the part's items end here).
    fn peek_index(&self) -> R<usize> {
        self.idx.get(self.j).copied().ok_or(Halt::Ended)
    }

    fn next(&mut self) -> R<&Item> {
        let i = self.peek_index()?;
        self.j += 1;
        Ok(&self.m.items[i])
    }

    /// The next item's index, or the end of the items when the part has no next item.
    fn next_or_end(&self) -> usize {
        self.idx.get(self.j).copied().unwrap_or(self.m.items.len())
    }

    fn report(&mut self, want: &str) -> R<()> {
        let label = self.label.clone();
        match self.next()? {
            Item::Report { line, .. } if line == want => Ok(()),
            other => bad(format!(
                "{label}: compose.sh prints {want:?}\n  the controller: {other:?}"
            )),
        }
    }

    /// A command; `exact`: its environment too (else only that it is the init's).
    fn send(&mut self, want: Command, exact: bool) -> R<u64> {
        let label = self.label.clone();
        let same = |a: &Command| match (a, &want) {
            (Command::Spawn(x), Command::Spawn(y)) if !exact => {
                x.h == y.h
                    && x.argv == y.argv
                    && x.stdout == y.stdout
                    && x.append == y.append
                    && x.env
                        .as_ref()
                        .is_some_and(|e| e.iter().any(|v| v.starts_with("HYDO_STEPS=")))
            }
            _ => *a == want,
        };
        match self.next()? {
            Item::Send { id, cmd, .. } if same(cmd) => Ok(*id),
            other => bad(format!(
                "{label}: compose.sh does {want:?}\n  the controller: {other:?}"
            )),
        }
    }

    fn send_run(&mut self, argv: Vec<String>) -> R<u64> {
        let label = self.label.clone();
        let denv = self.m.m.denv.clone();
        match self.next()? {
            Item::Send {
                id,
                cmd: Command::Run(r),
                ..
            } if r.argv == argv && r.capture.is_none() && r.env.as_ref() == Some(&denv) => Ok(*id),
            other => bad(format!(
                "{label}: compose.sh runs {argv:?}\n  the controller: {other:?}"
            )),
        }
    }

    /// An answer the part waits for: before its next item (or the part ends here).
    fn waited(&self, what: &str, at: Option<usize>) -> R<usize> {
        let next = self.next_or_end();
        match at {
            Some(i) if i < next => Ok(i),
            Some(_) if self.j >= self.idx.len() => Err(Halt::Ended),
            None if self.j >= self.idx.len() => Err(Halt::Ended),
            _ => bad(format!("{}: went on before {what}", self.label)),
        }
    }

    fn clock(&mut self) -> R<(i64, usize)> {
        let id = self.send(Command::ClockGet(ClockGet {}), true)?;
        let at = self.waited("the clock", self.m.replies.get(&id).map(|x| x.0))?;
        Ok(match &self.m.replies[&id].1 {
            Ok(Reply::ClockGet(c)) => ((c.mono_ms / 10) as i64, at),
            _ => (0, at),
        })
    }

    fn waited_done(&self, id: u64) -> R<usize> {
        self.waited("its sleep", self.m.dones.get(&id).map(|x| x.0))
    }

    fn waited_exit(&self, id: u64) -> R<u32> {
        let at = self.waited("the command's exit", self.m.exits.get(&id).map(|x| x.0))?;
        let _ = at;
        Ok(self.m.exits[&id].1.status)
    }

    fn read_all(&mut self, path: &str) -> R<String> {
        let mut b: Vec<u8> = Vec::new();
        loop {
            let off = if b.is_empty() {
                None
            } else {
                Some(b.len() as u64)
            };
            let id = self.send(
                Command::Read(Read {
                    path: s(path),
                    offset: off,
                    max: 3000,
                    hex: None,
                }),
                true,
            )?;
            self.waited("the read", self.m.replies.get(&id).map(|x| x.0))?;
            let (data, eof) = match &self.m.replies[&id].1 {
                Ok(Reply::Read(r)) => (r.data.clone().into_bytes(), r.eof),
                Err(_) if self.m.refused_code(id) == Some(ErrCode::NotUtf8) => {
                    let h = self.send(
                        Command::Read(Read {
                            path: s(path),
                            offset: off,
                            max: 1500,
                            hex: Some(true),
                        }),
                        true,
                    )?;
                    self.waited("the read", self.m.replies.get(&h).map(|x| x.0))?;
                    match &self.m.replies[&h].1 {
                        Ok(Reply::Read(r)) => (
                            (0..r.data.len() / 2)
                                .map(|i| u8::from_str_radix(&r.data[2 * i..2 * i + 2], 16).unwrap())
                                .collect(),
                            r.eof,
                        ),
                        _ => break,
                    }
                }
                _ => break,
            };
            b.extend_from_slice(&data);
            if eof || data.is_empty() {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&b).into_owned())
    }
}
