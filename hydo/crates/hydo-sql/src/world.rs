//! The fake outside world's record (`/tmp/hydo/world.log`, hydo/onboard/MANIFEST.md "The
//! record") as a source: one record per line of the selected kind, its `k=v` fields as the
//! record's fields (values unescaped), so hydo-check can compare what a fake saw with what a
//! database holds:
//!
//! ```sh
//! hydo-sql check equal --left 'books=sql primary "SELECT intent FROM payment WHERE captured"' \
//!                      --right 'stripe=world stripe.intent_capture --field ref'
//! ```
//!
//! `world <target> [--all] [--log PATH]`: `target` is `<ep>.<route>`, a route of any endpoint,
//! or `*`; without `--all` only the requests that took effect (`fx=1`: effectful, not an
//! idempotent replay; for a proxy, forwarded) and the system's own events, with `--all` every
//! request and event. Webhook deliveries are not requests and are left out.

use crate::json::Val;
use crate::{cannot, rec, usage, Record, Res};

/// `%XX` escapes back to bytes.
pub fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut o = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(x), Some(y)) = (h(b[i + 1]), h(b[i + 2])) {
                o.push((x * 16 + y) as u8);
                i += 3;
                continue;
            }
        }
        o.push(b[i]);
        i += 1;
    }
    o
}

/// The fields of one record line.
pub fn fields(line: &str) -> Vec<(String, Vec<u8>)> {
    line.split_whitespace()
        .filter_map(|t| t.split_once('=').map(|(k, v)| (k.to_string(), unescape(v))))
        .collect()
}

fn matches(f: &[(String, Vec<u8>)], target: &str, all: bool) -> bool {
    let get = |k: &str| {
        f.iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
    };
    let kind = get("kind").unwrap_or_default();
    if kind != "req" && kind != "event" {
        return false;
    }
    if !all && get("fx").as_deref() != Some("1") {
        return false;
    }
    if target == "*" {
        return true;
    }
    let ep = get("ep").unwrap_or_default();
    let route = get("route").unwrap_or_default();
    match target.split_once('.') {
        Some((e, r)) => e == ep && r == route,
        None => target == route,
    }
}

/// The records of `text` (a world log) for `target`.
pub fn select(text: &str, target: &str, all: bool) -> Vec<Record> {
    text.lines()
        .map(fields)
        .filter(|f| matches(f, target, all))
        .map(|f| {
            rec(f
                .iter()
                .map(|(k, v)| (k.as_str(), Val::Str(v.clone())))
                .collect())
        })
        .collect()
}

pub fn read(args: &[String]) -> Res<Vec<Record>> {
    let mut target = None;
    let mut all = false;
    let mut log = "/tmp/hydo/world.log".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => all = true,
            "--log" => {
                log = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| usage("world: --log needs a path"))?;
                i += 1;
            }
            t if target.is_none() => target = Some(t.to_string()),
            t => return Err(usage(format!("world: unexpected {t}"))),
        }
        i += 1;
    }
    let target =
        target.ok_or_else(|| usage("world <ep>.<route>|<route>|* [--all] [--log PATH]"))?;
    let text = std::fs::read_to_string(&log).map_err(|e| cannot(format!("open {log}: {e}")))?;
    Ok(select(&text, &target, all))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str =
        "seq=7 t=1 kind=req ep=stripe route=refund ref=pi_3 idem=r-1 body=x fx=1 out=ok rule=-\n\
seq=8 t=2 kind=req ep=stripe route=refund ref=pi_3 idem=r-1 body=x fx=0 out=replay orig=7\n\
seq=9 t=3 kind=hook ep=stripe route=refund ref=pi_3 src=7 n=1 dup=0 out=200\n\
seq=10 t=4 kind=req ep=stripe route=intent_capture ref=pi%5F4 fx=1 out=ok\n\
seq=11 t=5 kind=event ep=- route=cancelled ref=o2 fx=1\n";

    #[test]
    fn selects_effects() {
        let r = select(LOG, "stripe.refund", false);
        assert_eq!(r.len(), 1);
        let r = select(LOG, "refund", true);
        assert_eq!(r.len(), 2);
        let r = select(LOG, "*", false);
        assert_eq!(r.len(), 3);
        let r = select(LOG, "intent_capture", false);
        let o = r[0].as_obj().unwrap();
        assert_eq!(o.get(&b"ref"[..]), Some(&Val::str("pi_4")));
        assert_eq!(select(LOG, "cancelled", false).len(), 1);
    }
}
