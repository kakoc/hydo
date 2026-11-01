//! A schedule's host part, read as `hvx_explore.c`'s `parse_sched` reads it (`strstr` for the
//! tokens, glibc `strtoull`, the same defaults, refusals and buffer limits), so every schedule,
//! malformed ones included, arms the module exactly as the C engine does.

use crate::abi::*;
use crate::cstr::{atoi, find, strtoul, strtoull, Scan};

#[derive(Clone, Copy)]
pub struct Sched {
    /// The host part (before `|`), at most 1023 bytes (the C buffer).
    pub host: [u8; 1024],
    pub host_len: usize,
    /// The guest part (after `|` and spaces, trailing new lines and spaces cut), at most 511 bytes.
    pub guest: [u8; 512],
    pub guest_len: usize,
    pub classes: u32,
    pub prob: u32,
    pub dseed: u64,
    pub cut: bool,
    pub tj_on: bool,
    pub tj: hvx_tjitter,
    pub rb: u64,
    pub pp: u64,
    pub pptsc: u64,
    pub xt_on: bool,
    pub xt: hvx_xtick,
    pub cf_on: bool,
    pub cf: u64,
    pub q_on: bool,
    pub q: u64,
    pub qs: u64,
    pub tc_on: bool,
    pub tc: hvx_turnctl,
}

impl Sched {
    pub fn empty() -> Sched {
        Sched {
            host: [0; 1024],
            host_len: 0,
            guest: [0; 512],
            guest_len: 0,
            classes: 0,
            prob: 0,
            dseed: 0,
            cut: false,
            tj_on: false,
            tj: hvx_tjitter::default(),
            rb: 0,
            pp: 0,
            pptsc: 0,
            xt_on: false,
            xt: hvx_xtick::default(),
            cf_on: false,
            cf: 0,
            q_on: false,
            q: 0,
            qs: 0,
            tc_on: false,
            tc: hvx_turnctl::default(),
        }
    }

    pub fn host(&self) -> &[u8] {
        &self.host[..self.host_len]
    }

    pub fn guest(&self) -> &[u8] {
        &self.guest[..self.guest_len]
    }

    /// `parse_sched`: None where the C function returns -1.
    pub fn parse(s: &[u8]) -> Option<Sched> {
        let mut sc = Sched::empty();
        // a C string ends at its first NUL
        let s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];
        let bar = s.iter().position(|&c| c == b'|')?;
        let h = &s[..bar.min(1023)];
        sc.host[..h.len()].copy_from_slice(h);
        sc.host_len = h.len();
        let mut p = bar + 1;
        while p < s.len() && s[p] == b' ' {
            p += 1;
        }
        let g = &s[p..];
        let g = &g[..g.len().min(511)];
        sc.guest[..g.len()].copy_from_slice(g);
        sc.guest_len = g.len();
        while sc.guest_len > 0 && matches!(sc.guest[sc.guest_len - 1], b'\n' | b'\r' | b' ') {
            sc.guest_len -= 1;
        }
        let host = sc.host;
        let host = &host[..sc.host_len];
        let mut disk: Vec<u8> = b"none".to_vec();
        if let Some(i) = find(host, b"disk=") {
            let mut w = Scan::new(&host[i + 5..]);
            if let Some(d) = w.word(63) {
                disk = d.to_vec();
            }
        }
        if let Some(i) = find(host, b"dseed=") {
            sc.dseed = strtoull(&host[i + 6..], 16).0;
        }
        if disk == b"none" {
            sc.classes = 0;
        } else if disk.starts_with(b"ioerr:") {
            sc.classes = HVX_FAULT_ERROR;
            sc.prob = atoi(&disk[6..]) as u32;
        } else if disk == b"cut" {
            sc.classes = HVX_FAULT_DROP_UNFLUSHED;
            sc.prob = 100;
        } else if disk.starts_with(b"lying:") {
            sc.classes = HVX_FAULT_DROP;
            sc.prob = atoi(&disk[6..]) as u32;
        } else {
            return None;
        }
        parse_tj(host, &mut sc)?;
        parse_preempt(host, &mut sc)?;
        parse_turns(host, &mut sc)?;
        if let Some(i) = find(host, b"rb=") {
            sc.rb = strtoull(&host[i + 3..], 10).0;
        }
        sc.cut = find(sc.guest(), b" C=").is_some();
        if let Some(i) = find(host, b"cf=") {
            sc.cf = strtoull(&host[i + 3..], 10).0;
            if sc.cf == 0 || sc.classes != HVX_FAULT_DROP_UNFLUSHED || sc.cut {
                return None;
            }
            sc.cf_on = true;
            sc.cut = true;
        } else if find(host, b"cfn").is_some() {
            sc.cf_on = true;
        }
        Some(sc)
    }
}

/// `host_tok`: the value of `key` where it starts a word of `host`.
pub fn host_tok<'a>(host: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut p = 0;
    while let Some(i) = find(&host[p..], key) {
        let at = p + i;
        if at == 0 || host[at - 1] == b' ' {
            return Some(&host[at + key.len()..]);
        }
        p = at + key.len();
    }
    None
}

/// The byte after a parsed number (`*end`, 0 at the end of the string).
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn parse_turns(host: &[u8], sc: &mut Sched) -> Option<()> {
    if let Some(p) = host_tok(host, b"q=") {
        let (v, n) = strtoull(p, 10);
        if n == 0 || v == 0 || (at(p, n) != 0 && at(p, n) != b' ') {
            return None;
        }
        sc.q = v;
        sc.q_on = true;
    }
    if let Some(p) = host_tok(host, b"qs=") {
        let (v, n) = strtoull(p, 16);
        if n == 0 || v == 0 || (at(p, n) != 0 && at(p, n) != b' ') {
            return None;
        }
        sc.qs = v;
        sc.q_on = true;
    }
    if let Some(p) = host_tok(host, b"qb=") {
        let (bl, n) = strtoull(p, 10);
        if n == 0 || at(p, n) != b':' || bl == 0 {
            return None;
        }
        let p = &p[n + 1..];
        let (bq, n) = strtoull(p, 10);
        if n == 0 || at(p, n) != b':' || bq == 0 {
            return None;
        }
        let p = &p[n + 1..];
        let (pm, n) = strtoull(p, 10);
        if n == 0 || pm > 1000 || (at(p, n) != 0 && at(p, n) != b' ') {
            return None;
        }
        sc.tc.burst_len = bl;
        sc.tc.burst_q = bq;
        sc.tc.burst_permille = pm as u32;
        if sc.tc.burst_seed == 0 {
            sc.tc.burst_seed = 1;
        }
        sc.tc_on = true;
        sc.q_on = true;
    }
    if let Some(p) = host_tok(host, b"qbs=") {
        let (v, n) = strtoull(p, 16);
        sc.tc.burst_seed = v;
        if n == 0 || v == 0 || (at(p, n) != 0 && at(p, n) != b' ') {
            return None;
        }
        if sc.tc.burst_len == 0 {
            return None;
        }
    }
    if let Some(p) = host_tok(host, b"qm=") {
        let (v, n) = strtoull(p, 10);
        sc.tc.margin = v;
        if n == 0 || v < 8 || (at(p, n) != 0 && at(p, n) != b' ') {
            return None;
        }
        sc.tc_on = true;
        sc.q_on = true;
    }
    Some(())
}

fn parse_preempt(host: &[u8], sc: &mut Sched) -> Option<()> {
    if let Some(i) = find(host, b"pp=") {
        sc.pp = strtoull(&host[i + 3..], 10).0;
    }
    if let Some(i) = find(host, b"pptsc=") {
        sc.pptsc = strtoull(&host[i + 6..], 10).0;
    }
    if let Some(i) = find(host, b"xtsc=") {
        sc.xt.tsc = strtoull(&host[i + 5..], 10).0;
    }
    if let Some(i) = find(host, b"xtx=") {
        let mut p = i + 4;
        while at(host, p) != 0
            && at(host, p) != b' '
            && (sc.xt.n_explicit as usize) < HVX_XT_MAX_EXPLICIT
        {
            let (v, n) = strtoull(&host[p..], 10);
            sc.xt.explicit_[sc.xt.n_explicit as usize] = v;
            sc.xt.n_explicit += 1;
            if n == 0 {
                return None;
            }
            let end = p + n;
            p = if at(host, end) == b',' { end + 1 } else { end };
        }
        if at(host, p) != 0 && at(host, p) != b' ' {
            return None;
        }
        sc.xt_on = sc.xt.n_explicit > 0;
        return Some(());
    }
    let Some(i) = find(host, b"xt=") else {
        return Some(());
    };
    sc.xt.seed = strtoull(&host[i + 3..], 16).0;
    sc.xt.rate = match find(host, b"xtr=") {
        Some(j) => strtoul(&host[j + 4..], 10).0 as u32,
        None => 4,
    };
    sc.xt.permille = match find(host, b"xtpm=") {
        Some(j) => strtoul(&host[j + 5..], 10).0 as u32,
        None => 500,
    };
    if let Some(j) = find(host, b"xtlo=") {
        sc.xt.lo = strtoull(&host[j + 5..], 10).0;
    }
    if let Some(j) = find(host, b"xthi=") {
        sc.xt.hi = strtoull(&host[j + 5..], 10).0;
    }
    sc.xt_on = sc.xt.rate != 0 && sc.xt.permille != 0;
    Some(())
}

fn parse_tj(host: &[u8], sc: &mut Sched) -> Option<()> {
    if let Some(i) = find(host, b"tjx=") {
        let mut p = i + 4;
        while at(host, p) != 0
            && at(host, p) != b' '
            && (sc.tj.n_explicit as usize) < HVX_TJ_MAX_EXPLICIT
        {
            let k = sc.tj.n_explicit as usize;
            let (tick, n) = strtoull(&host[p..], 10);
            sc.tj.explicit_[k].tick = tick;
            let end = p + n;
            if at(host, end) != b':' {
                return None;
            }
            let (delay, n2) = strtoull(&host[end + 1..], 10);
            sc.tj.explicit_[k].delay = delay;
            let end = end + 1 + n2;
            sc.tj.n_explicit += 1;
            p = if at(host, end) == b',' { end + 1 } else { end };
        }
        if at(host, p) != 0 && at(host, p) != b' ' {
            return None;
        }
        sc.tj_on = sc.tj.n_explicit > 0;
        return Some(());
    }
    let Some(i) = find(host, b"tj=") else {
        return Some(());
    };
    sc.tj.seed = strtoull(&host[i + 3..], 16).0;
    sc.tj.permille = match find(host, b"tjpm=") {
        Some(j) => strtoul(&host[j + 5..], 10).0 as u32,
        None => 10,
    };
    sc.tj.max_delay = match find(host, b"tjmax=") {
        Some(j) => strtoull(&host[j + 6..], 10).0,
        None => 500000,
    };
    if let Some(j) = find(host, b"tjlo=") {
        sc.tj.tick_lo = strtoull(&host[j + 5..], 10).0;
    }
    if let Some(j) = find(host, b"tjhi=") {
        sc.tj.tick_hi = strtoull(&host[j + 5..], 10).0;
    }
    sc.tj_on = sc.tj.permille != 0 && sc.tj.max_delay != 0;
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_tokens() {
        let s = Sched::parse(
            b"disk=cut dseed=4d5cb825dfaab05b tj=7 tjpm=20 rb=16 | run K=redis@11+1 C=24\n",
        )
        .unwrap();
        assert_eq!(s.classes, HVX_FAULT_DROP_UNFLUSHED);
        assert_eq!(s.prob, 100);
        assert_eq!(s.dseed, 0x4d5cb825dfaab05b);
        assert!(s.tj_on && s.tj.seed == 7 && s.tj.permille == 20 && s.tj.max_delay == 500000);
        assert_eq!(s.rb, 16);
        assert!(s.cut);
        assert_eq!(s.guest(), b"run K=redis@11+1 C=24");
        assert!(Sched::parse(b"disk=bogus | run").is_none());
        assert!(Sched::parse(b"no bar").is_none());
        // cf= needs disk=cut and no C=
        assert!(Sched::parse(b"disk=none cf=3 | run").is_none());
        let c = Sched::parse(b"disk=cut cf=3 | run").unwrap();
        assert!(c.cf_on && c.cut && c.cf == 3);
        // q= is a whole word; qm= below 8 is refused; qb= arms bursts
        let q = Sched::parse(b"disk=none q=5000 qs=ab qb=100:7:250 | run").unwrap();
        assert!(q.q_on && q.q == 5000 && q.qs == 0xab);
        assert!(q.tc_on && q.tc.burst_len == 100 && q.tc.burst_q == 7 && q.tc.burst_seed == 1);
        assert!(Sched::parse(b"disk=none qm=4 | run").is_none());
        assert!(Sched::parse(b"disk=none qbs=3 | run").is_none());
        let x = Sched::parse(b"disk=none tjx=5:100,9:7 xtx=1000,2000 | run").unwrap();
        assert_eq!(x.tj.n_explicit, 2);
        assert_eq!(x.tj.explicit_[1], hvx_tj_explicit { tick: 9, delay: 7 });
        assert_eq!(x.xt.n_explicit, 2);
        assert!(Sched::parse(b"disk=none tjx=5-100 | run").is_none());
        let e = Sched::parse(b"|").unwrap();
        assert_eq!(e.guest_len, 0);
    }
}
