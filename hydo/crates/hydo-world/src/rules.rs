//! One `H=` token, switched on by the init for the token's step window.
//!
//! ```text
//! H=<target>@<from>-<to>:<rule>[,<rule>...]
//! target  <fake|proxy> or <fake|proxy>.<route>
//! request rules: status=<code> [retry-after=<s>] | delay=<ms> | drop | reset
//! webhook rules: dup | reorder | late=<ms> | lost
//! p=<1..100>      share of the matching requests (or webhooks) the rule acts on (default 100)
//! effect=yes|no   whether the outside world still performs the side effect; default no for
//!                 status=, yes for delay/drop/reset (the request arrived, the answer is lost)
//! ```
//!
//! gRPC proxies add `rpc=`, `call=`, `cut=`, `hold=`, `dir=`, `seed=` and `reset=conn`
//! ([`GrpcExt`], the module `grpc`).

use crate::goish::quote;
use crate::rt::Latch;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub struct Rule {
    pub id: i64,
    pub token: String,
    /// endpoint name
    pub target: String,
    /// "" = every route
    pub route: String,
    /// http status or (grpc) status code; 0 = none
    pub status: i64,
    pub retry_after: String,
    pub delay_ms: i64,
    pub drop: bool,
    pub reset: bool,
    pub dup: bool,
    pub reorder: bool,
    pub late_ms: i64,
    pub lost: bool,
    pub p: i64,
    /// -1 default, 0 no, 1 yes
    pub effect: i64,
    /// how many matching requests (or webhooks) the rule has decided on (under the world lock)
    n: AtomicU64,
    pub release: Arc<Latch>,
    /// the gRPC-only keys; present once the token had `reset` or one of them
    pub ext: Option<GrpcExt>,
}

/// The gRPC-only keys of a rule.
#[derive(Clone, Debug)]
pub struct GrpcExt {
    pub rpc: String,
    /// 0 = any
    pub call_lo: i64,
    pub call_hi: i64,
    /// -1 = none
    pub cut: i64,
    pub hold: i64,
    pub dir_req: bool,
    /// reset=conn
    pub conn: bool,
    pub keys: Vec<String>,
}

impl Default for GrpcExt {
    fn default() -> Self {
        GrpcExt {
            rpc: String::new(),
            call_lo: 0,
            call_hi: 0,
            cut: -1,
            hold: -1,
            dir_req: false,
            conn: false,
            keys: Vec::new(),
        }
    }
}

/// The gRPC status names `status=` accepts.
pub fn grpc_code(name: &str) -> Option<i64> {
    const CODES: [&str; 17] = [
        "OK",
        "CANCELLED",
        "UNKNOWN",
        "INVALID_ARGUMENT",
        "DEADLINE_EXCEEDED",
        "NOT_FOUND",
        "ALREADY_EXISTS",
        "PERMISSION_DENIED",
        "RESOURCE_EXHAUSTED",
        "FAILED_PRECONDITION",
        "ABORTED",
        "OUT_OF_RANGE",
        "UNIMPLEMENTED",
        "INTERNAL",
        "UNAVAILABLE",
        "DATA_LOSS",
        "UNAUTHENTICATED",
    ];
    CODES.iter().position(|c| *c == name).map(|i| i as i64)
}

fn num(k: &str, v: &str) -> Result<i64, String> {
    match v.parse::<i64>() {
        Ok(n) if n >= 0 => Ok(n),
        _ => Err(format!("{k}={} is not a number", quote(v))),
    }
}

impl Rule {
    pub fn parse(id: i64, token: &str) -> Result<Rule, String> {
        let mut r = Rule {
            id,
            token: token.to_string(),
            target: String::new(),
            route: String::new(),
            status: 0,
            retry_after: String::new(),
            delay_ms: 0,
            drop: false,
            reset: false,
            dup: false,
            reorder: false,
            late_ms: 0,
            lost: false,
            p: 100,
            effect: -1,
            n: AtomicU64::new(0),
            release: Latch::new(),
            ext: None,
        };
        let Some((head, rules)) = token.split_once(':') else {
            return Err(format!("no ':' in {}", quote(token)));
        };
        let target = head.split_once('@').map_or(head, |(t, _)| t); // the window is the init's
        let (t, rt) = target.split_once('.').unwrap_or((target, ""));
        r.target = t.to_string();
        r.route = rt.to_string();
        if r.target.is_empty() {
            return Err(format!("no target in {}", quote(token)));
        }
        for kv in rules.split(',') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            match k {
                "status" => {
                    r.status = match grpc_code(&v.to_uppercase()) {
                        Some(c) => c,
                        None => num(k, v)?,
                    }
                }
                "retry-after" => r.retry_after = v.to_string(),
                "delay" => r.delay_ms = num(k, v)?,
                "late" => r.late_ms = num(k, v)?,
                "p" => r.p = num(k, v)?,
                "drop" => r.drop = true,
                "reset" => {
                    r.reset = true;
                    r.parse_ext(k, v)?; // reset=stream|conn
                }
                "dup" => r.dup = true,
                "reorder" => r.reorder = true,
                "lost" => r.lost = true,
                "effect" => {
                    r.effect = match v {
                        "yes" => 1,
                        "no" => 0,
                        _ => return Err(format!("effect={}: yes or no", quote(v))),
                    }
                }
                "" => {}
                _ => r.parse_ext(k, v)?, // the gRPC-only keys; unknown keys are errors there
            }
        }
        Ok(r)
    }

    /// A key parse does not know (and `reset=<v>`).
    fn parse_ext(&mut self, k: &str, v: &str) -> Result<(), String> {
        let x = self.ext.get_or_insert_with(GrpcExt::default);
        x.keys.push(k.to_string());
        match k {
            "rpc" => {
                if v.is_empty() || v.contains([' ', ',']) {
                    return Err(format!("rpc={}: a method name", quote(v)));
                }
                x.rpc = v.to_string();
            }
            "call" => {
                let (a, b, rng) = match v.split_once('-') {
                    Some((a, b)) => (a, b, true),
                    None => (v, "", false),
                };
                let lo = a.parse::<i64>();
                let hi = if rng { b.parse::<i64>() } else { lo.clone() };
                match (lo, hi) {
                    (Ok(lo), Ok(hi)) if lo >= 1 && hi >= lo => {
                        x.call_lo = lo;
                        x.call_hi = hi;
                    }
                    _ => return Err(format!("call={}: <n> or <a>-<b>, counted from 1", quote(v))),
                }
            }
            "cut" => x.cut = num(k, v)?,
            "hold" => x.hold = num(k, v)?,
            "dir" => match v {
                "req" => x.dir_req = true,
                "resp" => {}
                _ => return Err(format!("dir={}: req or resp", quote(v))),
            },
            "seed" => {
                if v.is_empty() {
                    return Err("seed=: a value".into());
                }
            }
            "reset" => match v {
                "" | "stream" => {}
                "conn" => x.conn = true,
                _ => return Err(format!("reset={}: stream or conn", quote(v))),
            },
            _ => return Err(format!("unknown rule {}", quote(k))),
        }
        Ok(())
    }

    pub fn request_rule(&self) -> bool {
        self.status > 0 || self.delay_ms > 0 || self.drop || self.reset
    }

    pub fn hook_rule(&self) -> bool {
        self.dup || self.reorder || self.late_ms > 0 || self.lost
    }

    pub fn matches(&self, ep: &str, route: &str) -> bool {
        self.target == ep && (self.route.is_empty() || self.route == route)
    }

    /// Does the outside world perform the request's side effect under this rule?
    pub fn applied(&self) -> bool {
        if self.effect >= 0 {
            return self.effect == 1;
        }
        self.status == 0 // a plain error answer by default means "rejected before acting"
    }

    /// The rule's decision for the next matching request (or webhook): a pure function of the
    /// token text and how many matching requests the rule has seen. The caller holds the world
    /// lock.
    pub fn fires(&self) -> bool {
        let k = self.n.fetch_add(1, Ordering::Relaxed);
        mix64(fnv64a(self.token.as_bytes()).wrapping_add(k)) % 100 < self.p as u64
    }

    /// What a request rule did, for the record.
    pub fn outcome(&self) -> String {
        let mut p = Vec::new();
        if self.delay_ms > 0 {
            p.push(format!("delay{}", self.delay_ms));
        }
        if self.drop {
            p.push("drop".into());
        } else if self.reset {
            p.push("reset".into());
        } else if self.status > 0 {
            p.push(format!("status{}", self.status));
        }
        p.join("+")
    }
}

impl GrpcExt {
    /// Does the rule use a key only gRPC proxies implement?
    pub fn grpc_only(x: Option<&GrpcExt>) -> bool {
        x.is_some_and(|x| {
            !x.rpc.is_empty() || x.call_lo > 0 || x.cut >= 0 || x.hold >= 0 || x.dir_req || x.conn
        })
    }

    pub fn stream_rule(x: Option<&GrpcExt>) -> bool {
        x.is_some_and(|x| x.cut >= 0 || x.hold >= 0)
    }
}

pub fn fnv64a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &c in b {
        h ^= c as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rule() {
        let r = Rule::parse(1, "payments.charge@3-6:status=429,retry-after=2,p=50").unwrap();
        assert!(
            r.target == "payments"
                && r.route == "charge"
                && r.status == 429
                && r.retry_after == "2"
                && r.p == 50
                && !r.applied()
        );
        let r = Rule::parse(2, "pay@0-1:status=UNAVAILABLE,effect=yes").unwrap();
        assert!(r.route.is_empty() && r.status == 14 && r.applied());
        let r = Rule::parse(3, "payments.charge@0-1:delay=3000").unwrap();
        assert!(r.request_rule() && !r.hook_rule() && r.applied());
        let r = Rule::parse(4, "payments.charge@0-1:dup,late=500").unwrap();
        assert!(!r.request_rule() && r.hook_rule() && r.dup && r.late_ms == 500);
        for bad in [
            "payments.charge@0-1",
            "payments@0-1:frob",
            "payments@0-1:delay=x",
            "@0-1:drop",
            "a@0-1:effect=maybe",
        ] {
            assert!(Rule::parse(9, bad).is_err(), "{bad}: no error");
        }
        assert_eq!(Rule::parse(9, "payments@0-1:frob").unwrap_err(), "unknown rule \"frob\"");
        assert_eq!(Rule::parse(9, "p@0-1:delay=x").unwrap_err(), "delay=\"x\" is not a number");
        let r = Rule::parse(5, "orders.place@0-30:drop,call=8-9,rpc=Place,reset=conn").unwrap();
        let x = r.ext.as_ref().unwrap();
        assert!(x.call_lo == 8 && x.call_hi == 9 && x.rpc == "Place" && x.conn);
        assert!(GrpcExt::grpc_only(r.ext.as_ref()));
        let r = Rule::parse(6, "o@0-1:reset").unwrap();
        assert!(r.ext.is_some() && !GrpcExt::grpc_only(r.ext.as_ref()));
    }

    /// The decision sequence is a pure function of the token: two rules from the same text
    /// agree, a different token gives a different sequence, and p=100 always fires.
    #[test]
    fn fires_deterministic() {
        let seq = |tok: &str| {
            let r = Rule::parse(1, tok).unwrap();
            (0..64).map(|_| if r.fires() { '1' } else { '0' }).collect::<String>()
        };
        let (a, b, c) = (seq("x@0-1:drop,p=50"), seq("x@0-1:drop,p=50"), seq("x@1-2:drop,p=50"));
        assert!(a == b && a != c && a.contains('1') && a.contains('0'), "a={a} b={b} c={c}");
        assert!(!seq("x@0-1:drop").contains('0'));
    }

    /// The first decisions of a few tokens and the parse errors, as the Go version gave them.
    #[test]
    fn like_go() {
        for (tok, want) in [
            ("x@0-1:drop,p=50", "1011110011000101011010001110010000011110"),
            ("payments.charge@6-9:reset,p=50", "0101010010010010100100001010011010101001"),
            ("orders.place@10-13:drop", "1111111111111111111111111111111111111111"),
            ("a@0-1:status=500,p=7,seed=3", "0000000000100000011100001000000010000000"),
        ] {
            let r = Rule::parse(1, tok).unwrap();
            let got: String = (0..40).map(|_| if r.fires() { '1' } else { '0' }).collect();
            assert_eq!(got, want, "{tok}");
        }
        for (tok, want) in [
            ("p@0-1:delay=x", "delay=\"x\" is not a number"),
            ("p@0-1:call=0", "call=\"0\": <n> or <a>-<b>, counted from 1"),
            ("p@0-1:rpc=a b", "rpc=\"a b\": a method name"),
            ("p@0-1:dir=up", "dir=\"up\": req or resp"),
            ("p@0-1:reset=x", "reset=\"x\": stream or conn"),
            ("p@0-1:effect=maybe", "effect=\"maybe\": yes or no"),
            ("p", "no ':' in \"p\""),
            ("@0:drop", "no target in \"@0:drop\""),
            ("p@0-1:p=-1", "p=\"-1\" is not a number"),
            ("p@0-1:frob=1", "unknown rule \"frob\""),
        ] {
            assert_eq!(Rule::parse(1, tok).unwrap_err(), want, "{tok}");
        }
    }
}
