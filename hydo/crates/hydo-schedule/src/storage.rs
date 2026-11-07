//! The storage faults of one service (docs/design/storage-faults.md): `G=` corrupts files in the
//! service's storage directory, `L=` is a power loss of that service alone.
//!
//! - `G=<svc>@<at>[+<down>]:<kind>[,n=<count>][,len=<bytes>][,range=<from>-<to>][,file=<text>][,seed=<hex>]`: before
//!   step `at` the host controller changes bytes of one file under the service's `storage:`
//!   directory. With `+<down>` the service is killed first and started again before step
//!   `at+down` (its files are changed while it is down); without it the files change under the
//!   running service. Kinds: `flip` (`n` single-bit flips, default 1), `zero` (`len` bytes set to
//!   zero, default 4096), `trunc` (the file shortened by `len` bytes; without `len` by a seeded
//!   number of bytes). `range=` keeps the places within bytes [from, to) of the file (a file no
//!   longer than `from` does not qualify). `file=` keeps only files whose path (relative to the
//!   storage directory) contains the text; `seed=` chooses the file and the places (default 0).
//!   The choice is a pure
//!   function of the token and of the directory's contents at that moment.
//! - `L=<svc>@<at>+<down>`: before step `at` the service is stopped, its own file system (a
//!   service with `storage: {size_mib: ...}`) is shut down so that nothing more reaches its disk,
//!   the service is killed and the file system mounted again (its journal replayed); what the
//!   service had written but the kernel had not yet written back is lost. It starts again before
//!   step `at+down`.

use crate::num::{dec, err, hex};
use crate::ParseError;
use std::fmt;

/// What `G=` does to the chosen file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CorruptKind {
    /// Single-bit flips.
    Flip,
    /// A range set to zero bytes.
    Zero,
    /// The file shortened.
    Trunc,
}

impl CorruptKind {
    pub fn word(self) -> &'static str {
        match self {
            CorruptKind::Flip => "flip",
            CorruptKind::Zero => "zero",
            CorruptKind::Trunc => "trunc",
        }
    }
}

/// The most single-bit flips one token makes.
pub const MAX_FLIPS: u64 = 64;
/// The longest range one token zeroes or cuts off, in bytes.
pub const MAX_LEN: u64 = 1 << 20;

/// The `<kind>[,key=value]...` part of a `G=` token.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Corruption {
    pub kind: CorruptKind,
    /// `n=`: how many bits a flip changes (flip only).
    pub n: Option<u64>,
    /// `len=`: how many bytes are zeroed or cut off (zero and trunc only).
    pub len: Option<u64>,
    /// `range=<from>-<to>`: the places within bytes [from, to) of the file.
    pub range: Option<(u64, u64)>,
    /// `file=`: only files whose relative path contains this text.
    pub file: Option<String>,
    /// `seed=`: chooses the file and the places.
    pub seed: Option<u64>,
}

impl Corruption {
    /// The number of bit flips (flip), 1 by default.
    pub fn flips(&self) -> u64 {
        self.n.unwrap_or(1)
    }

    pub fn seed(&self) -> u64 {
        self.seed.unwrap_or(0)
    }

    pub(crate) fn parse(s: &str, what: &str) -> Result<Corruption, ParseError> {
        let mut it = s.split(',');
        let kind = match it.next().unwrap_or("") {
            "flip" => CorruptKind::Flip,
            "zero" => CorruptKind::Zero,
            "trunc" => CorruptKind::Trunc,
            k => {
                return Err(err(format!(
                    "{what}: the corruption {k:?} is flip, zero or trunc"
                )))
            }
        };
        let mut c = Corruption {
            kind,
            n: None,
            len: None,
            range: None,
            file: None,
            seed: None,
        };
        // the canonical order is n, len, range, file, seed (each at most once), so the token prints back
        // byte for byte
        let mut last = 0;
        for kv in it {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| err(format!("{what}: {kv:?} is not <key>=<value>")))?;
            let rank = match k {
                "n" => 1,
                "len" => 2,
                "range" => 3,
                "file" => 4,
                "seed" => 5,
                _ => {
                    return Err(err(format!(
                        "{what}: unknown key {k:?} (n, len, range, file, seed)"
                    )))
                }
            };
            if rank <= last {
                return Err(err(format!(
                    "{what}: the keys come once each, in the order n, len, range, file, seed"
                )));
            }
            last = rank;
            match k {
                "n" => {
                    if kind != CorruptKind::Flip {
                        return Err(err(format!("{what}: n= is for flip only")));
                    }
                    let n = dec(v, what)?;
                    if n == 0 || n > MAX_FLIPS {
                        return Err(err(format!("{what}: n= is 1 to {MAX_FLIPS}")));
                    }
                    c.n = Some(n);
                }
                "len" => {
                    if kind == CorruptKind::Flip {
                        return Err(err(format!("{what}: len= is for zero and trunc")));
                    }
                    let n = dec(v, what)?;
                    if n == 0 || n > MAX_LEN {
                        return Err(err(format!("{what}: len= is 1 to {MAX_LEN}")));
                    }
                    c.len = Some(n);
                }
                "range" => {
                    let (a, b) = v
                        .split_once('-')
                        .ok_or_else(|| err(format!("{what}: range=<from>-<to> (bytes)")))?;
                    let (a, b) = (dec(a, what)?, dec(b, what)?);
                    if a >= b {
                        return Err(err(format!("{what}: range=<from>-<to> with from < to")));
                    }
                    c.range = Some((a, b));
                }
                "file" => {
                    if v.is_empty()
                        || !v.bytes().all(|b| {
                            b.is_ascii_alphanumeric()
                                || b == b'.'
                                || b == b'_'
                                || b == b'-'
                                || b == b'/'
                        })
                    {
                        return Err(err(format!("{what}: file= is letters, digits and . _ - /")));
                    }
                    c.file = Some(v.to_string());
                }
                _ => c.seed = Some(hex(v, what)?),
            }
        }
        Ok(c)
    }
}

impl fmt::Display for Corruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.word())?;
        if let Some(n) = self.n {
            write!(f, ",n={n}")?;
        }
        if let Some(n) = self.len {
            write!(f, ",len={n}")?;
        }
        if let Some((a, b)) = self.range {
            write!(f, ",range={a}-{b}")?;
        }
        if let Some(x) = &self.file {
            write!(f, ",file={x}")?;
        }
        if let Some(s) = self.seed {
            write!(f, ",seed={s:x}")?;
        }
        Ok(())
    }
}

/// splitmix64: the places a corruption chooses (the explorer's generator uses the same mix).
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One change a corruption makes to the chosen file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Flip bit `bit` (0 = the lowest) of the byte at `off`.
    Flip { off: u64, bit: u8 },
    /// Set `len` bytes from `off` to zero.
    Zero { off: u64, len: u64 },
    /// Shorten the file to `size` bytes.
    Trunc { size: u64 },
}

impl Corruption {
    /// Which file of `files` (relative paths and sizes, in any order) the corruption changes, and
    /// how: a pure function of the token and the list. None when no file qualifies (after
    /// `file=`; flip and zero need a file that is not empty, trunc one longer than zero bytes).
    pub fn plan(&self, files: &[(String, u64)]) -> Option<(String, Vec<Edit>)> {
        let from = self.range.map_or(0, |r| r.0);
        let mut c: Vec<&(String, u64)> = files
            .iter()
            .filter(|(p, n)| *n > from && self.file.as_ref().is_none_or(|f| p.contains(f.as_str())))
            .collect();
        c.sort();
        if c.is_empty() {
            return None;
        }
        let seed = self.seed();
        let mut k = 0u64;
        let mut next = || {
            k += 1;
            mix(seed ^ mix(k))
        };
        let (path, size) = c[(next() % c.len() as u64) as usize];
        let size = *size;
        // the places: bytes [from, end) of the file
        let end = self.range.map_or(size, |r| r.1.min(size));
        let span = end - from;
        let edits = match self.kind {
            CorruptKind::Flip => {
                let mut v: Vec<Edit> = (0..self.flips())
                    .map(|_| {
                        let r = next();
                        Edit::Flip {
                            off: from + r % span,
                            bit: ((r >> 56) % 8) as u8,
                        }
                    })
                    .collect();
                v.sort_by_key(|e| match e {
                    Edit::Flip { off, bit } => (*off, *bit),
                    _ => (0, 0),
                });
                v
            }
            CorruptKind::Zero => {
                let len = self.len.unwrap_or(4096).min(span);
                let off = from + next() % (span - len + 1);
                vec![Edit::Zero { off, len }]
            }
            CorruptKind::Trunc => {
                // the new end lies in [from, end): len bytes before end, or a seeded place
                let cut = match self.len {
                    Some(n) => n.min(span),
                    None => 1 + next() % span,
                };
                vec![Edit::Trunc { size: end - cut }]
            }
        };
        Some((path.clone(), edits))
    }
}
