//! Number reading and the error type shared by the token parsers.

use crate::ParseError;

pub(crate) fn err(msg: impl Into<String>) -> ParseError {
    ParseError {
        message: msg.into(),
    }
}

/// A decimal number written canonically: digits only, no leading zero (except "0" itself), no sign.
/// The guest side uses this: its text reaches the guest's console verbatim, so a non-canonical
/// spelling cannot be normalised without changing the run's fingerprint; it is refused instead.
pub(crate) fn dec(s: &str, what: &str) -> Result<u64, ParseError> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(format!("{what}: {s:?} is not a decimal number")));
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(err(format!(
            "{what}: {s:?} has a leading zero (the guest logs the token as written, so it is not \
             rewritten; write the number without leading zeros)"
        )));
    }
    s.parse::<u64>()
        .map_err(|_| err(format!("{what}: {s:?} does not fit in 64 bits")))
}

/// A signed decimal (the clock jump's delta): an optional '-', then a canonical decimal; "-0" is
/// refused.
pub(crate) fn sdec(s: &str, what: &str) -> Result<i64, ParseError> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s),
    };
    let v = dec(digits, what)?;
    if neg && v == 0 {
        return Err(err(format!("{what}: write 0, not -0")));
    }
    let v = i64::try_from(v).map_err(|_| err(format!("{what}: {s:?} is out of range")))?;
    Ok(if neg { -v } else { v })
}

/// A lower-case hex number without leading zeros (the guest-side hex fields, e.g. B=).
pub(crate) fn hex(s: &str, what: &str) -> Result<u64, ParseError> {
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(err(format!("{what}: {s:?} is not a lower-case hex number")));
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(err(format!("{what}: {s:?} has a leading zero")));
    }
    u64::from_str_radix(s, 16).map_err(|_| err(format!("{what}: {s:?} does not fit in 64 bits")))
}

/// A host-side decimal: what hvx_explore's strtoull(.., 10) reads, restricted to a whole token
/// (digits only; leading zeros allowed and dropped when printed). The C side stops at the first
/// non-digit and ignores the rest; here trailing garbage is refused.
pub(crate) fn host_dec(s: &str, what: &str) -> Result<u64, ParseError> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(format!("{what}: {s:?} is not a decimal number")));
    }
    s.parse::<u64>()
        .map_err(|_| err(format!("{what}: {s:?} does not fit in 64 bits")))
}

/// A host-side hex number: what strtoull(.., 16) reads (an optional 0x/0X, digits of either
/// case, leading zeros); printed canonically (lower case, no prefix, no leading zeros).
pub(crate) fn host_hex(s: &str, what: &str) -> Result<u64, ParseError> {
    let d = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if d.is_empty() || !d.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(err(format!("{what}: {s:?} is not a hex number")));
    }
    u64::from_str_radix(d, 16).map_err(|_| err(format!("{what}: {s:?} does not fit in 64 bits")))
}

/// A lower-case service name as the guest's tokens use it (`[a-z0-9_]+`, explore.py's FC_RE).
pub(crate) fn name(s: &str, what: &str) -> Result<(), ParseError> {
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(err(format!(
            "{what}: {s:?} is not a service name (lower-case letters, digits and '_')"
        )));
    }
    Ok(())
}
