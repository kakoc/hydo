//! hydo_fc's command line, run by the agent's packet trigger: the agent binary installed as
//! `/bin/hydo_fc` (or run as `hydo-agent fc ...`) behaves as hydo/onboard/hydo_fc.c does, with
//! the same arguments, console lines, trace file and marker files. This is how step A3 of
//! docs/design/guest-control.md is proven on today's images: the shell init starts it exactly as
//! it starts the C helper, so their FROZE lines can be compared. The agent's own `freeze` command
//! uses the same parts (nfq.rs, tree.rs).
//!
//! usage: hydo_fc <queue> <pidfile> <svc-ip|-> <target-port|s<own-port>|-> <req|resp> <n> <match|-> <dur-ms>
//!                <logfile> <markerdir>

use std::fs::{self, File, OpenOptions};
use std::io::Write;

use crate::nfq::{self, Counter, Nfq};
use crate::{sys, tree};

fn out(s: &str) {
    let b = s.as_bytes();
    // hydo_fc's out(): one write of at most 1,023 bytes
    let _ = sys::write_all(1, &b[..b.len().min(1023)]);
}

fn marker(dir: &str, name: &str) {
    let _ = File::create(format!("{dir}/{name}"));
}

/// atoi/atol: leading blanks, a sign, digits; anything else ends the number (0 if none).
fn atol(s: &str) -> i64 {
    let s = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (neg, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut n: i64 = 0;
    for c in s.bytes() {
        if !c.is_ascii_digit() {
            break;
        }
        n = n.wrapping_mul(10).wrapping_add((c - b'0') as i64);
    }
    if neg {
        -n
    } else {
        n
    }
}

pub fn main(args: &[String]) -> i32 {
    if args.len() != 10 {
        eprintln!(
            "usage: hydo_fc <queue> <pidfile> <svc-ip|-> <target-port|-> <req|resp> <n> <match|-> <dur-ms> <logfile> <markerdir>"
        );
        return 2;
    }
    let queue = atol(&args[0]) as i32;
    let pidfile = &args[1];
    let have_sip = args[2] != "-";
    let own = args[3].starts_with('s');
    let tport = if args[3] != "-" {
        Some(atol(&args[3][own as usize..]) as i32)
    } else {
        None
    };
    let sip = have_sip.then(|| nfq::parse_ipv4(&args[2]));
    let want_req = args[4] == "req";
    let target = atol(&args[5]);
    let pattern = if args[6] != "-" {
        nfq::unescape_match(&args[6])
    } else {
        Vec::new()
    };
    let dur = atol(&args[7]);
    let mdir = &args[9];
    let mut log = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&args[8])
        .ok();

    if let Err(e) = sys::set_fifo(50) {
        out(&format!("hydo-fc: SCHED_FIFO failed: {}\n", strerror(&e)));
    }
    let mut q = match Nfq::bind(queue as u16) {
        Ok(q) => q,
        Err((what, e)) => {
            eprintln!("hydo_fc: {what}: {}", strerror(&e));
            return 1;
        }
    };
    marker(mdir, "fc.bound");

    let mut c = Counter {
        sip,
        own,
        // a port outside 0..65535 never matches, as -1 never does in hydo_fc
        tport: tport.and_then(|p| u16::try_from(p).ok()),
        want_req,
        pattern,
        target: target.max(0) as u64,
        nreq: 0,
        nresp: 0,
        counted: 0,
    };
    let mut nlog = 0u32;
    let (mut fired, mut resumed) = (false, false);
    let mut resume_at: i64 = 0;
    let mut frozen: Vec<i32> = Vec::new();
    loop {
        let mut timeout = -1;
        if fired && !resumed {
            let left = resume_at - sys::now_ms() as i64;
            timeout = left.max(0) as i32;
        }
        let mut pf = [libc::pollfd {
            fd: q.fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        let pr = sys::poll(&mut pf, timeout);
        if fired && !resumed && sys::now_ms() as i64 >= resume_at {
            for &p in &frozen {
                sys::kill(p, libc::SIGCONT);
            }
            resumed = true;
            marker(mdir, "fc.resumed");
            out(&format!(
                "hydo-fc: RESUMED after {dur} ms ({} processes)\n",
                frozen.len()
            ));
        }
        if pr == 0 {
            continue;
        }
        let pkts = match q.recv() {
            Ok(p) => p,
            Err(e) => {
                if sys::io_errno(&e) == libc::ENOBUFS {
                    out("hydo-fc: netlink ENOBUFS (packets dropped)\n");
                }
                continue;
            }
        };
        for p in pkts {
            let t = match nfq::tcp(&p.pkt) {
                Some(t) if !fired => t,
                _ => {
                    verdict(&q, p.id);
                    continue;
                }
            };
            let s = c.see(&t);
            if let Some(f) = log.as_mut() {
                if nlog < 200_000 {
                    let mut line = nfq::log_line(&s, c.counted, &t, sys::now_ms());
                    line.truncate(399);
                    let _ = f.write_all(line.as_bytes());
                    nlog += 1;
                }
            }
            if !s.fire {
                verdict(&q, p.id);
                continue;
            }
            // the n-th event: freeze the service before it can run past it, then let it through
            let root = fs::read(pidfile)
                .ok()
                .map(|b| atol(&String::from_utf8_lossy(&b[..b.len().min(31)])) as i32)
                .unwrap_or(0);
            if root > 1 {
                frozen = tree::collect(root);
                for &pid in &frozen {
                    sys::kill(pid, libc::SIGSTOP);
                }
                let all = tree::wait_stopped(&frozen);
                out(&format!(
                    "hydo-fc: FROZE pid {root} ({} processes{}) at {} #{} ({}{}, {}>{} len={}) for {dur} ms: {}\n",
                    frozen.len(),
                    if all { "" } else { ", NOT all threads stopped" },
                    if want_req { "req" } else { "resp" },
                    c.counted,
                    if s.is_req { "req" } else { "resp" },
                    s.k,
                    t.sport,
                    t.dport,
                    t.payload.len(),
                    nfq::snippet(t.payload, 60)
                ));
                out("HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.freeze_call\",\"ok\":true,\"msg\":\"a service was frozen at a call (F=)\"}\n");
            } else {
                out(&format!(
                    "hydo-fc: n-th event reached but no pid in {pidfile}; nothing frozen\n"
                ));
            }
            fired = true;
            marker(mdir, "fc.fired");
            resume_at = sys::now_ms() as i64 + dur;
            verdict(&q, p.id);
        }
    }
}

fn verdict(q: &Nfq, id: u32) {
    if let Err(e) = q.accept(id) {
        out(&format!("hydo-fc: verdict failed: {}\n", strerror(&e)));
    }
}

/// The C library's text for an error (hydo_fc prints strerror's words).
fn strerror(e: &std::io::Error) -> String {
    let n = sys::io_errno(e);
    let p = unsafe { libc::strerror(n) };
    if p.is_null() {
        return format!("errno {n}");
    }
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn atol_is_c_atol() {
        assert_eq!(super::atol(" 12x"), 12);
        assert_eq!(super::atol("-3"), -3);
        assert_eq!(super::atol("x"), 0);
        assert_eq!(super::atol("6379"), 6379);
    }
}
