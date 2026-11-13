//! Clock steps to a time (`T=<at>:@<point>[<offset>]`, Milestone 225): the realtime clock is set
//! to the next instant of a time of day or of a calendar boundary, in UTC, instead of being moved
//! by a fixed amount.
//!
//! A point names a sequence of instants:
//!
//! - `@HH:MM` or `@HH:MM:SS`: that time of every day;
//! - `@:MM` or `@:MM:SS`: that minute (and second) of every hour;
//! - `@hour`, `@day`, `@month`, `@year`: the start of every hour, day (00:00), month (the first,
//!   00:00) or year (1 January, 00:00).
//!
//! An offset follows the point: a sign and whole units in descending order, each written once and
//! not zero (`+10m`, `-2s`, `+1d2h`, `+9m58s`; `d`, `h`, `m`, `s`; hours below 24, minutes and
//! seconds below 60). The target is the first instant `b` of the sequence with `b + offset`
//! after the clock's current value, plus the offset, so the step always goes forward. With
//! `@month+10m` at 2026-11-30 23:55 the clock goes to 2026-12-01 00:10; at 2026-12-01 00:05 it
//! goes to 00:10 of the same day.
//!
//! The text is canonical: the guest logs it, so a second spelling of one point (`@00:10:00` for
//! `@00:10`, `+600s` for `+10m`) is refused with the canonical one in the message.

use crate::num::err;
use crate::ParseError;
use std::fmt;

/// The instants a [`ClockPoint`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Every {
    /// `@HH:MM[:SS]`: that time of every day.
    Day { h: u8, m: u8, s: u8 },
    /// `@:MM[:SS]`: that minute and second of every hour.
    Hour { m: u8, s: u8 },
    /// `@hour`: every full hour.
    HourStart,
    /// `@day`: every midnight.
    DayStart,
    /// `@month`: the first of every month, 00:00.
    MonthStart,
    /// `@year`: 1 January, 00:00.
    YearStart,
}

/// A clock step's target: a sequence of instants and an offset in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClockPoint {
    pub every: Every,
    pub offset: i64,
}

fn two(s: &str, max: u8, what: &str, name: &str) -> Result<u8, ParseError> {
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(format!("{what}: the {name} is two digits (got {s:?})")));
    }
    let v: u8 = s.parse().expect("two digits");
    if v > max {
        return Err(err(format!("{what}: the {name} {v} is above {max}")));
    }
    Ok(v)
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The civil date (year, month, day) of a day number since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl ClockPoint {
    /// The text after `T=<at>:`, starting with `@`.
    pub fn parse(v: &str, what: &str) -> Result<ClockPoint, ParseError> {
        let body = v
            .strip_prefix('@')
            .ok_or_else(|| err(format!("{what}: a clock point starts with @")))?;
        // the point runs up to the offset's sign
        let cut = body.find(['+', '-']).unwrap_or(body.len());
        let (p, off) = body.split_at(cut);
        let every = match p {
            "hour" => Every::HourStart,
            "day" => Every::DayStart,
            "month" => Every::MonthStart,
            "year" => Every::YearStart,
            _ if p.starts_with(':') => {
                let f: Vec<&str> = p[1..].split(':').collect();
                match f.as_slice() {
                    [m] => Every::Hour {
                        m: two(m, 59, what, "minute")?,
                        s: 0,
                    },
                    [m, s] => Every::Hour {
                        m: two(m, 59, what, "minute")?,
                        s: two(s, 59, what, "second")?,
                    },
                    _ => return Err(err(format!("{what}: @:MM or @:MM:SS"))),
                }
            }
            _ if p.contains(':') => {
                let f: Vec<&str> = p.split(':').collect();
                match f.as_slice() {
                    [h, m] => Every::Day {
                        h: two(h, 23, what, "hour")?,
                        m: two(m, 59, what, "minute")?,
                        s: 0,
                    },
                    [h, m, s] => Every::Day {
                        h: two(h, 23, what, "hour")?,
                        m: two(m, 59, what, "minute")?,
                        s: two(s, 59, what, "second")?,
                    },
                    _ => return Err(err(format!("{what}: @HH:MM or @HH:MM:SS"))),
                }
            }
            _ => {
                return Err(err(format!(
                    "{what}: the clock point {p:?} is not @HH:MM[:SS], @:MM[:SS], @hour, @day, \
                     @month or @year"
                )))
            }
        };
        let offset = if off.is_empty() {
            0
        } else {
            parse_offset(off, what)?
        };
        let cp = ClockPoint { every, offset };
        let canon = cp.to_string();
        if canon != v {
            return Err(err(format!(
                "{what}: {v:?} is not written canonically; write {canon:?}"
            )));
        }
        Ok(cp)
    }

    /// The first boundary of the sequence strictly after `x` (seconds since the epoch, UTC).
    fn boundary_after(&self, x: i64) -> i64 {
        const H: i64 = 3600;
        const D: i64 = 86400;
        match self.every {
            Every::Hour { m, s } => {
                let b = x.div_euclid(H) * H + m as i64 * 60 + s as i64;
                if b > x {
                    b
                } else {
                    b + H
                }
            }
            Every::Day { h, m, s } => {
                let b = x.div_euclid(D) * D + h as i64 * H + m as i64 * 60 + s as i64;
                if b > x {
                    b
                } else {
                    b + D
                }
            }
            Every::HourStart => x.div_euclid(H) * H + H,
            Every::DayStart => x.div_euclid(D) * D + D,
            Every::MonthStart => {
                let (y, mo, _) = civil_from_days(x.div_euclid(D));
                let (y, mo) = if mo == 12 { (y + 1, 1) } else { (y, mo + 1) };
                days_from_civil(y, mo, 1) * D
            }
            Every::YearStart => {
                let (y, _, _) = civil_from_days(x.div_euclid(D));
                days_from_civil(y + 1, 1, 1) * D
            }
        }
    }

    /// The clock's new value when it reads `now`: the first instant of the sequence whose value
    /// plus the offset is after `now`, plus the offset (always after `now`).
    pub fn next_after(&self, now: i64) -> i64 {
        self.boundary_after(now - self.offset) + self.offset
    }
}

/// `+<n>d<n>h<n>m<n>s` (descending units, each once and not zero) as seconds.
fn parse_offset(t: &str, what: &str) -> Result<i64, ParseError> {
    let (neg, mut rest) = match t.as_bytes()[0] {
        b'-' => (true, &t[1..]),
        _ => (false, &t[1..]),
    };
    if rest.is_empty() {
        return Err(err(format!(
            "{what}: the offset {t:?} needs a number and a unit (s, m, h, d)"
        )));
    }
    let mut total: i64 = 0;
    while !rest.is_empty() {
        let n = rest.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 || n == rest.len() {
            return Err(err(format!(
                "{what}: the offset {t:?} is <number><unit>..., units s, m, h, d"
            )));
        }
        let v: i64 = rest[..n]
            .parse()
            .map_err(|_| err(format!("{what}: the offset {t:?} is out of range")))?;
        let unit = match rest.as_bytes()[n] {
            b'd' => 86400,
            b'h' => 3600,
            b'm' => 60,
            b's' => 1,
            c => {
                return Err(err(format!(
                    "{what}: the offset's unit {:?} is not s, m, h or d",
                    c as char
                )))
            }
        };
        total = v
            .checked_mul(unit)
            .and_then(|x| total.checked_add(x))
            .filter(|x| *x <= 3650 * 86400)
            .ok_or_else(|| err(format!("{what}: the offset {t:?} is above ten years")))?;
        rest = &rest[n + 1..];
    }
    if total == 0 {
        return Err(err(format!(
            "{what}: an offset of 0 is written without the offset"
        )));
    }
    Ok(if neg { -total } else { total })
}

fn write_offset(f: &mut fmt::Formatter<'_>, off: i64) -> fmt::Result {
    if off == 0 {
        return Ok(());
    }
    f.write_str(if off < 0 { "-" } else { "+" })?;
    let mut a = off.unsigned_abs();
    for (u, c) in [(86400u64, 'd'), (3600, 'h'), (60, 'm'), (1, 's')] {
        if a >= u {
            write!(f, "{}{c}", a / u)?;
            a %= u;
        }
    }
    Ok(())
}

impl fmt::Display for ClockPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.every {
            Every::Day { h, m, s } => {
                write!(f, "@{h:02}:{m:02}")?;
                if s != 0 {
                    write!(f, ":{s:02}")?;
                }
            }
            Every::Hour { m, s } => {
                write!(f, "@:{m:02}")?;
                if s != 0 {
                    write!(f, ":{s:02}")?;
                }
            }
            Every::HourStart => f.write_str("@hour")?,
            Every::DayStart => f.write_str("@day")?,
            Every::MonthStart => f.write_str("@month")?,
            Every::YearStart => f.write_str("@year")?,
        }
        write_offset(f, self.offset)
    }
}
