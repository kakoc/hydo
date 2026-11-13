//! Seed ranges for budgets and shards (stage 2 of docs/design/ci.md, sections 7.1 and 7.3).
//!
//! The explorer draws each schedule from its seed, so a seed range splits into parts that run
//! anywhere with the same results. `--shard I/N` takes the I-th of N contiguous parts of the
//! range, the first parts one seed longer when the range does not divide evenly; a time budget
//! stops at a seed boundary in seed order. What a run explored is therefore always a list of
//! ranges, written `1-37,101-140`.

/// `I/N` with 1 <= I <= N.
pub fn parse_shard(s: &str) -> Option<(u64, u64)> {
    let (i, n) = s.split_once('/')?;
    let (i, n): (u64, u64) = (i.trim().parse().ok()?, n.trim().parse().ok()?);
    (i >= 1 && i <= n).then_some((i, n))
}

/// The I-th (1-based) of N contiguous parts of `lo..=hi`; None when the part is empty (more
/// shards than seeds).
pub fn shard_of(lo: u64, hi: u64, i: u64, n: u64) -> Option<(u64, u64)> {
    let total = hi.checked_sub(lo)? + 1;
    let (base, rem) = (total / n, total % n);
    let size = |k: u64| base + u64::from(k < rem);
    let start = lo + (0..i - 1).map(size).sum::<u64>();
    let len = size(i - 1);
    (len > 0).then(|| (start, start + len - 1))
}

/// `a-b` ranges, sorted and joined where they touch.
pub fn ranges_text(mut r: Vec<(u64, u64)>) -> String {
    r.sort();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (a, b) in r {
        match out.last_mut() {
            Some(l) if a <= l.1 + 1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out.iter()
        .map(|(a, b)| format!("{a}-{b}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The ranges of a text written by [`ranges_text`] (a single seed `7` reads as `7-7`).
pub fn parse_ranges(s: &str) -> Vec<(u64, u64)> {
    s.split(',')
        .filter(|x| !x.trim().is_empty())
        .filter_map(hydo_manifest::ci::parse_seeds)
        .collect()
}

/// The number of seeds in some ranges.
pub fn count(r: &[(u64, u64)]) -> u64 {
    r.iter().map(|(a, b)| b - a + 1).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shards of a range cover it exactly once, in order, and do not depend on anything but
    /// the range and the shard's number.
    #[test]
    fn shards_cover_the_range_once() {
        let parts: Vec<_> = (1..=3).map(|i| shard_of(1, 200, i, 3).unwrap()).collect();
        assert_eq!(parts, [(1, 67), (68, 134), (135, 200)]);
        assert_eq!(shard_of(5, 6, 2, 2), Some((6, 6)));
        assert_eq!(shard_of(1, 2, 3, 3), None);
        for n in 1..=7 {
            let all: Vec<_> = (1..=n).filter_map(|i| shard_of(11, 60, i, n)).collect();
            assert_eq!(ranges_text(all.clone()), "11-60", "{n} shards");
            assert_eq!(count(&all), 50);
        }
        assert_eq!(parse_shard("2/4"), Some((2, 4)));
        assert_eq!(parse_shard("0/4"), None);
        assert_eq!(parse_shard("5/4"), None);
    }

    #[test]
    fn ranges_join_and_read_back() {
        let t = ranges_text(vec![(101, 140), (1, 37), (38, 40)]);
        assert_eq!(t, "1-40,101-140");
        assert_eq!(parse_ranges(&t), [(1, 40), (101, 140)]);
        assert_eq!(count(&parse_ranges(&t)), 80);
        assert_eq!(ranges_text(vec![]), "");
    }
}
