//! Answers in another order (Milestone 393, the request rule `order=<k>[,wait=<ms>]`).
//!
//! A page that asks twice for one list (a search box that asks at every key) shows the answer
//! that came last. When the answers come in the order of the requests, the last answer is the
//! newest. On a real network an older request can be answered later: a slower query, another
//! connection, a retry. A rule `order=<k>` holds the answers of the matching requests in a pen.
//! When k answers are in it, it gives them in another order, drawn from the token: a permutation
//! of their requests' order of arrival that is never the order itself (for two, the second
//! request's answer before the first's). The answers go out [`GAP_MS`] apart, so the client reads
//! them in that order. An answer that waits `wait=` ms (default 3000) for the others goes out
//! with the ones that came, in the drawn order of those.
//!
//! The pen is a pure function of the token and of the requests' order of arrival, so a schedule
//! gives the same order on every run.

use crate::rt::Latch;
use crate::rules::{fnv64a, mix64, Rule};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The time between two answers that a pen gives, ms of guest time.
pub const GAP_MS: u64 = 30;

#[derive(Default)]
struct Pen {
    /// how many groups of answers the pen gave so far
    batch: u64,
    /// (the request's place in the order of arrival, the latch that lets its answer go)
    held: Vec<(u64, Arc<Latch>)>,
}

/// The pens of every `order=` rule, by the rule's id.
#[derive(Default)]
pub struct Pens {
    m: Mutex<BTreeMap<i64, Pen>>,
}

/// The order in which a group of held answers goes out: the places `ns` (sorted, the order of
/// arrival) in a permutation drawn from the token and the group's number, never the order of
/// arrival itself when the group has two or more.
pub fn release_order(token: &str, batch: u64, mut ns: Vec<u64>) -> Vec<u64> {
    ns.sort_unstable();
    if ns.len() < 2 {
        return ns;
    }
    let mut x = mix64(fnv64a(token.as_bytes()) ^ batch.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut out = ns.clone();
    for i in (1..out.len()).rev() {
        x = mix64(x);
        out.swap(i, (x % (i as u64 + 1)) as usize);
    }
    if out == ns {
        out.reverse();
    }
    out
}

impl Pens {
    /// The answer of the `n`-th request of `r` is ready: it waits in the rule's pen until the
    /// returned latch fires.
    pub fn enter(self: &Arc<Self>, r: &Arc<Rule>, n: u64) -> Arc<Latch> {
        let l = Latch::new();
        let mut m = self.m.lock().unwrap_or_else(|e| e.into_inner());
        let pen = m.entry(r.id).or_default();
        pen.held.push((n, l.clone()));
        if pen.held.len() as i64 >= r.order.max(2) {
            let group = std::mem::take(&mut pen.held);
            let b = pen.batch;
            pen.batch += 1;
            tokio::spawn(give(r.token.clone(), b, group));
        } else if pen.held.len() == 1 {
            // the first answer of a group: the others have wait= ms to come
            let (pens, rule, b) = (self.clone(), r.clone(), pen.batch);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(rule.wait_ms.max(0) as u64)).await;
                let group = {
                    let mut m = pens.m.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(pen) = m.get_mut(&rule.id) else {
                        return;
                    };
                    if pen.batch != b || pen.held.is_empty() {
                        return; // the group was full before the time was up
                    }
                    pen.batch += 1;
                    std::mem::take(&mut pen.held)
                };
                give(rule.token.clone(), b, group).await;
            });
        }
        l
    }
}

/// Let a group's answers go, in the drawn order, [`GAP_MS`] apart.
async fn give(token: String, batch: u64, group: Vec<(u64, Arc<Latch>)>) {
    let order = release_order(&token, batch, group.iter().map(|(n, _)| *n).collect());
    for (i, n) in order.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
        }
        if let Some((_, l)) = group.iter().find(|(m, _)| m == n) {
            l.fire();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_the_order_of_arrival() {
        assert_eq!(release_order("t", 0, vec![4, 3]), vec![4, 3]);
        assert_eq!(release_order("t", 0, vec![7]), vec![7]);
        for tok in [
            "api.search@0-3:order=3",
            "api.search@0-3:order=4",
            "x@1-2:order=5",
        ] {
            for b in 0..20 {
                let ns: Vec<u64> = (0..5).collect();
                let o = release_order(tok, b, ns.clone());
                assert_ne!(o, ns, "{tok} {b}");
                let mut s = o.clone();
                s.sort_unstable();
                assert_eq!(s, ns, "a permutation: {o:?}");
                assert_eq!(o, release_order(tok, b, ns.clone()), "the same every time");
            }
        }
    }

    /// Two held answers go out in the other order, the second one first; a lone answer goes
    /// out after wait=.
    #[test]
    fn a_pen_of_two() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let pens = Arc::new(Pens::default());
            let r = Arc::new(Rule::parse(1, "api.search@0-9:order=2,wait=200").unwrap());
            let first = pens.enter(&r, 0);
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(!first.fired());
            let second = pens.enter(&r, 1);
            second.wait().await;
            assert!(!first.fired(), "the second request's answer goes first");
            first.wait().await;
            let lone = pens.enter(&r, 2);
            let t1 = std::time::Instant::now();
            lone.wait().await;
            assert!(t1.elapsed() >= Duration::from_millis(200));
        });
    }
}
