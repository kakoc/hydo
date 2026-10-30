//! The test composer's token `W=` (hydo/onboard/hydo_composer.py, hydo-composer.sh; MANIFEST.md
//! "The test composer" and "Stable compositions"). Three forms:
//!
//! - seed form `<seed hex>,p<n>,q<n>,r<ms>[,n<n>][,t<ms>][,x<entries>][,o<commands>]` (or `-`,
//!   the plan's default composition): what the explorer draws and shrinks;
//! - resolved form `:p<n>,r<ms>[,t<ms>],c<id>+<id>...,<entry>.<round>.<k>.<seed>,...`: every
//!   entry spelled out, commands by id (M98);
//! - saved form `@<12 hex>`: a resolved composition the image carries under that key.
//!
//! The fields are kept in the order written, so any text the composer's `render_w` /
//! `render_resolved` prints comes back byte for byte. The text reaches the guest's console, so
//! a non-canonical spelling (leading zeros, an upper-case seed) is refused rather than rewritten.

use crate::num::{dec, err};
use crate::ParseError;
use std::fmt;

/// The command kinds a command id starts with (hydo_composer.WORKLOAD_KINDS).
pub const WORKLOAD_KINDS: [&str; 7] = [
    "first",
    "parallel_driver",
    "serial_driver",
    "singleton_driver",
    "anytime",
    "eventually",
    "finally",
];

/// A command id `<kind>_<name>` (hydo_composer NAME_RE `[A-Za-z0-9_.-]+` and a known kind).
fn check_id(s: &str, what: &str) -> Result<(), ParseError> {
    let ok_chars = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    let ok_kind = WORKLOAD_KINDS
        .iter()
        .any(|k| s.len() > k.len() + 1 && s.starts_with(k) && s.as_bytes()[k.len()] == b'_');
    if !ok_chars || !ok_kind {
        return Err(err(format!(
            "{what}: {s:?} is not a command id <kind>_<name>"
        )));
    }
    Ok(())
}

/// One item of a range list (`_rlist`): `a` or `a-b`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RangeItem {
    pub lo: u64,
    pub hi: u64,
    /// written as `a-b` (also when a == b, which `_rlist` never prints but `_ilist` reads)
    pub span: bool,
}

/// A range list `0.2-5.7` (entries or command indices), in the order written.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RangeList(pub Vec<RangeItem>);

impl RangeList {
    fn parse(s: &str, what: &str) -> Result<RangeList, ParseError> {
        let mut out = Vec::new();
        for it in s.split('.') {
            if let Some((a, b)) = it.split_once('-') {
                let (lo, hi) = (dec(a, what)?, dec(b, what)?);
                out.push(RangeItem { lo, hi, span: true });
            } else {
                let v = dec(it, what)?;
                out.push(RangeItem {
                    lo: v,
                    hi: v,
                    span: false,
                });
            }
        }
        Ok(RangeList(out))
    }

    /// Every number the list names.
    pub fn values(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.0.iter().flat_map(|r| r.lo..=r.hi).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

impl fmt::Display for RangeList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, r) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            if r.span {
                write!(f, "{}-{}", r.lo, r.hi)?;
            } else {
                write!(f, "{}", r.lo)?;
            }
        }
        Ok(())
    }
}

/// `o`: commands never run, by index (the old form, `o0.4-6`) or by id (`oa_x+b_y`, M98).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Omit {
    Indices(RangeList),
    Ids(Vec<String>),
}

/// One field of the seed form after the seed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SeedField {
    /// `p`: at most this many commands at once.
    P(u64),
    /// `q`: at most this many parallel drivers per round.
    Q(u64),
    /// `r`: the round length in ms of guest time.
    R(u64),
    /// `n`: at most this many entries.
    N(u64),
    /// `t`: the time budget in ms.
    T(u64),
    /// `x`: entries left out.
    X(RangeList),
    /// `o`: commands never run.
    O(Omit),
}

/// One field of the resolved form after the `:`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResolvedField {
    P(u64),
    R(u64),
    T(u64),
    /// `c<id>+<id>...`: the command table the entries index.
    Commands(Vec<String>),
    /// `<entry>.<round>.<k>.<seed>`: entry number, round, index into the table, HYDO_SEED.
    Entry {
        entry: u64,
        round: u64,
        command: u64,
        seed: u64,
    },
}

/// The value of `W=`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Compose {
    /// `-`: the plan's default composition (hydo_composer.parse_w).
    Default,
    /// The seed form. The seed is written as exactly 8 lower-case hex digits (`%08x`).
    Seed { seed: u32, fields: Vec<SeedField> },
    /// The resolved form (`:...`).
    Resolved(Vec<ResolvedField>),
    /// The saved form `@<key>`, 12 lower-case hex digits (hydo_composer.key_of).
    Saved(String),
}

impl Compose {
    pub fn parse(v: &str) -> Result<Compose, ParseError> {
        if v == "-" {
            return Ok(Compose::Default);
        }
        if let Some(k) = v.strip_prefix('@') {
            if k.len() != 12
                || !k
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(err(format!(
                    "W=@{k}: a saved composition's key is 12 lower-case hex digits"
                )));
            }
            return Ok(Compose::Saved(k.to_string()));
        }
        if let Some(r) = v.strip_prefix(':') {
            return Self::parse_resolved(v, r);
        }
        let mut it = v.split(',');
        let seed = it.next().unwrap_or("");
        if seed.len() != 8
            || !seed
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(err(format!(
                "W={v}: the seed form starts with 8 lower-case hex digits (as render_w prints it)"
            )));
        }
        let seed = u32::from_str_radix(seed, 16).expect("8 hex digits");
        let mut fields = Vec::new();
        for f in it {
            // hydo_composer.parse_w skips an empty field; render_w never writes one: refused
            let (k, rest) = f.split_at(f.chars().next().map_or(0, char::len_utf8));
            let what = format!("W={v}: field {f:?}");
            fields.push(match k {
                "p" => SeedField::P(dec(rest, &what)?),
                "q" => SeedField::Q(dec(rest, &what)?),
                "r" => SeedField::R(dec(rest, &what)?),
                "n" => SeedField::N(dec(rest, &what)?),
                "t" => SeedField::T(dec(rest, &what)?),
                "x" => SeedField::X(RangeList::parse(rest, &what)?),
                "o" => {
                    if rest.bytes().any(|b| b.is_ascii_alphabetic()) {
                        let ids: Vec<String> = rest.split('+').map(str::to_string).collect();
                        for id in &ids {
                            check_id(id, &what)?;
                        }
                        SeedField::O(Omit::Ids(ids))
                    } else {
                        SeedField::O(Omit::Indices(RangeList::parse(rest, &what)?))
                    }
                }
                _ => return Err(err(format!("{what}: unknown field"))),
            });
        }
        Ok(Compose::Seed { seed, fields })
    }

    fn parse_resolved(v: &str, r: &str) -> Result<Compose, ParseError> {
        // hydo_composer.parse_resolved: p, r and at least one entry; the c table before the
        // entries; an entry's k inside the table; entry numbers unique, rounds never go back.
        let mut fields = Vec::new();
        let mut table: Option<usize> = None;
        let (mut p, mut rr, mut entries) = (false, false, Vec::new());
        for f in r.split(',') {
            let what = format!("W={v}: field {f:?}");
            if let Some(ids) = f.strip_prefix('c').filter(|_| table.is_none()) {
                let ids: Vec<String> = ids.split('+').map(str::to_string).collect();
                for id in &ids {
                    check_id(id, &what)?;
                }
                table = Some(ids.len());
                fields.push(ResolvedField::Commands(ids));
            } else if let Some(n) = f.strip_prefix('p').filter(|n| !n.contains('.')) {
                p = true;
                fields.push(ResolvedField::P(dec(n, &what)?));
            } else if let Some(n) = f.strip_prefix('r').filter(|n| !n.contains('.')) {
                rr = true;
                fields.push(ResolvedField::R(dec(n, &what)?));
            } else if let Some(n) = f.strip_prefix('t').filter(|n| !n.contains('.')) {
                fields.push(ResolvedField::T(dec(n, &what)?));
            } else {
                let parts: Vec<&str> = f.split('.').collect();
                if parts.len() != 4 {
                    return Err(err(format!("{what}: unknown field")));
                }
                let (entry, round, command, seed) = (
                    dec(parts[0], &what)?,
                    dec(parts[1], &what)?,
                    dec(parts[2], &what)?,
                    dec(parts[3], &what)?,
                );
                match table {
                    Some(n) if (command as usize) < n => {}
                    _ => {
                        return Err(err(format!(
                            "{what}: names command {command} of a table of {}",
                            table.unwrap_or(0)
                        )))
                    }
                }
                entries.push((entry, round));
                fields.push(ResolvedField::Entry {
                    entry,
                    round,
                    command,
                    seed,
                });
            }
        }
        let zero = fields
            .iter()
            .any(|f| matches!(f, ResolvedField::P(0) | ResolvedField::R(0)));
        if !p || !rr || entries.is_empty() || zero {
            return Err(err(format!(
                "W={v}: a resolved composition has p, r and at least one entry"
            )));
        }
        let mut seen = std::collections::HashSet::new();
        if !entries.iter().all(|(e, _)| seen.insert(*e))
            || entries.windows(2).any(|w| w[0].1 > w[1].1)
        {
            return Err(err(format!(
                "W={v}: entry numbers are unique and rounds never go back"
            )));
        }
        Ok(Compose::Resolved(fields))
    }
}

impl fmt::Display for Compose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Compose::Default => f.write_str("-"),
            Compose::Saved(k) => write!(f, "@{k}"),
            Compose::Seed { seed, fields } => {
                write!(f, "{seed:08x}")?;
                for x in fields {
                    match x {
                        SeedField::P(n) => write!(f, ",p{n}")?,
                        SeedField::Q(n) => write!(f, ",q{n}")?,
                        SeedField::R(n) => write!(f, ",r{n}")?,
                        SeedField::N(n) => write!(f, ",n{n}")?,
                        SeedField::T(n) => write!(f, ",t{n}")?,
                        SeedField::X(l) => write!(f, ",x{l}")?,
                        SeedField::O(Omit::Indices(l)) => write!(f, ",o{l}")?,
                        SeedField::O(Omit::Ids(ids)) => write!(f, ",o{}", ids.join("+"))?,
                    }
                }
                Ok(())
            }
            Compose::Resolved(fields) => {
                f.write_str(":")?;
                for (i, x) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    match x {
                        ResolvedField::P(n) => write!(f, "p{n}")?,
                        ResolvedField::R(n) => write!(f, "r{n}")?,
                        ResolvedField::T(n) => write!(f, "t{n}")?,
                        ResolvedField::Commands(ids) => write!(f, "c{}", ids.join("+"))?,
                        ResolvedField::Entry {
                            entry,
                            round,
                            command,
                            seed,
                        } => write!(f, "{entry}.{round}.{command}.{seed}")?,
                    }
                }
                Ok(())
            }
        }
    }
}
