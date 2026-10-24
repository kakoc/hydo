//! BUGGIFY: code-level fault points (FoundationDB style), the same decisions as the Go SDK.
//!
//! Activation is two-level, as in FoundationDB (`P_BUGGIFIED_SECTION_ACTIVATED = 0.25`,
//! `P_BUGGIFIED_SECTION_FIRES = 0.25`): for a run, each point is ENABLED with probability
//! enable/1000; an enabled point FIRES on each evaluation with probability fire/1000. Both are
//! pure functions of the run's seed, the point's id and the evaluation's index at that point.
//!
//! Off unless configured: `$HYDO_BUGGIFY`, or else the file `$HYDO_BUGGIFY_FILE` (default
//! `/tmp/hydo-buggify`), holds `<seed-hex>[,<enable-permille>[,<fire-permille>]]` -- the
//! schedule token `B=<seed>` puts it there. It is read once, at the first evaluation of any
//! point, and that first evaluation also writes
//! `HYDO-BUGGIFY {"seed":"..","enable_pm":250,"fire_pm":250,"enabled":["id",...]}`.

use crate::assertion::{all_registered, lock, Assertion, Loc, Type};
use crate::json::{write_str, Details};
use crate::sink;
use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::atomic::Ordering::AcqRel;

const GOLDEN: u64 = 0x9e3779b97f4a7c15;

pub(crate) struct BgState {
    loaded: bool,
    on: bool,
    seed: u64,
    enable: u64,
    fire: u64,
}

impl BgState {
    pub(crate) const fn new() -> Self {
        BgState { loaded: false, on: false, seed: 0, enable: 250, fire: 250 }
    }
}

/// splitmix64 finalizer (the Go SDK's bgMix)
pub(crate) fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(GOLDEN);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// FNV-1a over the id's bytes (the Go SDK's bgKey)
pub(crate) fn key(id: &str) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for b in id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// Go's strconv.ParseUint(s, base, 64): digits only (no sign, no prefix, no underscores).
fn parse_u64(s: &str, radix: u32) -> Option<u64> {
    if s.is_empty() || !s.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(s, radix).ok()
}

/// One declared fault point. Its catalog entry is a sometimes assertion `buggify.<id>`
/// ("fault point fired: <msg>"), recorded when it first fires.
#[repr(transparent)]
pub struct Point(pub(crate) Assertion);

impl std::fmt::Debug for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Point({:?})", self.0.bg_id)
    }
}

impl Point {
    #[doc(hidden)]
    pub const fn __new(aid: &'static str, amsg: &'static str, id: &'static str, file: &'static str, line: u32, col: u32, wire: bool) -> Self {
        Point(Assertion::raw(Type::Sometimes, Cow::Borrowed(aid), Cow::Borrowed(amsg), Some(Loc { file, line, col, wire }), Some(Cow::Borrowed(id))))
    }
    #[doc(hidden)]
    pub fn __assertion(&'static self) -> &'static Assertion {
        &self.0
    }
    /// The point's id (without the `buggify.` prefix of its catalog entry).
    pub fn id(&self) -> &str {
        self.0.bg_id.as_deref().unwrap_or("")
    }

    /// Reports whether the point misbehaves on this evaluation. Always false when BUGGIFY is
    /// not configured (and with the `noop` feature); then it has no other effect either.
    pub fn fire(&'static self, details: impl Into<Details>) -> bool {
        if cfg!(feature = "noop") {
            return false;
        }
        let mut g = lock();
        if !g.bg.loaded {
            g.bg.loaded = true;
            load(&mut g);
        }
        let id = self.id();
        if !g.bg.on || !enabled(&g.bg, id) {
            return false;
        }
        let k = self.0.hits.fetch_add(1, AcqRel) + 1;
        let fire = mix(mix(g.bg.seed ^ key(id)).wrapping_add(k.wrapping_mul(GOLDEN))) % 1000 < g.bg.fire;
        drop(g);
        if fire {
            let mut d = details.into();
            d.insert("hit", k);
            self.0.record(true, d);
        }
        fire
    }
}

fn enabled(bg: &BgState, id: &str) -> bool {
    mix(bg.seed ^ key(id)) % 1000 < bg.enable
}

fn load(g: &mut crate::assertion::Global) {
    let mut cfg = std::env::var("HYDO_BUGGIFY").unwrap_or_default();
    if cfg.is_empty() {
        let f = std::env::var_os("HYDO_BUGGIFY_FILE").filter(|v| !v.is_empty()).unwrap_or_else(|| "/tmp/hydo-buggify".into());
        if let Ok(b) = std::fs::read(&f) {
            cfg = String::from_utf8_lossy(&b).into_owned();
        }
    }
    let parts: Vec<&str> = cfg.trim().split(',').collect();
    let seed = match parse_u64(parts[0], 16) {
        Some(s) => s,
        None => return, // no configuration: every point stays off
    };
    g.bg.on = true;
    g.bg.seed = seed;
    if let Some(v) = parts.get(1).and_then(|p| parse_u64(p, 10)) {
        g.bg.enable = v;
    }
    if let Some(v) = parts.get(2).and_then(|p| parse_u64(p, 10)) {
        g.bg.fire = v;
    }
    if !sink::active() {
        return;
    }
    let mut s = String::from("HYDO-BUGGIFY {\"seed\":");
    write_str(&mut s, &format!("{:x}", seed));
    let _ = write!(s, ",\"enable_pm\":{},\"fire_pm\":{},\"enabled\":[", g.bg.enable, g.bg.fire);
    let mut first = true;
    for a in all_registered(g) {
        if let Some(id) = &a.bg_id {
            if enabled(&g.bg, id) {
                if !first {
                    s.push(',');
                }
                first = false;
                write_str(&mut s, id);
            }
        }
    }
    s.push_str("]}\n");
    sink::write_line(s.as_bytes());
}

/// Declares a fault point at run time (in the catalog in registration order).
pub fn buggify(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Point {
    let id = id.into();
    let msg = msg.into();
    let p: &'static Point = Box::leak(Box::new(Point(Assertion::raw(
        Type::Sometimes,
        Cow::Owned(format!("buggify.{}", id)),
        Cow::Owned(format!("fault point fired: {}", msg)),
        None,
        Some(id),
    ))));
    crate::assertion::register(&p.0);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_like_go() {
        assert_eq!(parse_u64("1234abcd", 16), Some(0x1234abcd));
        assert_eq!(parse_u64("+12", 16), None);
        assert_eq!(parse_u64("0x12", 16), None);
        assert_eq!(parse_u64(" 250", 10), None);
        assert_eq!(parse_u64("", 16), None);
        assert_eq!(parse_u64("ffffffffffffffff1", 16), None);
    }

    #[test]
    fn rates() {
        // about 25% of points enabled, about 25% of their evaluations fire (Go's TestBuggify)
        let ids: Vec<String> = "abcdefgh".chars().map(|c| format!("t.{}", c)).collect();
        let (mut en, mut fired, mut hits) = (0u64, 0u64, 0u64);
        for s in 1..=400u64 {
            let bg = BgState { loaded: true, on: true, seed: s, enable: 250, fire: 250 };
            for id in &ids {
                if enabled(&bg, id) {
                    en += 1;
                    for k in 1..=20u64 {
                        hits += 1;
                        if mix(mix(bg.seed ^ key(id)).wrapping_add(k.wrapping_mul(GOLDEN))) % 1000 < bg.fire {
                            fired += 1;
                        }
                    }
                }
            }
        }
        let pe = en as f64 / (400.0 * ids.len() as f64);
        let pf = fired as f64 / hits as f64;
        assert!((0.2..0.3).contains(&pe) && (0.2..0.3).contains(&pf), "enabled {pe} fire {pf}");
    }
}
