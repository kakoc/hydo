//! hydo's schedule language as types (a placeholder until the typed parser lands).
//!
//! API contract other crates rely on (keep these names and signatures):
//! - `Schedule::parse(&str) -> Result<Schedule, ParseError>` and `impl FromStr for Schedule`
//! - `impl Display for Schedule` (the canonical text; `parse(s)?.to_string() == s` for every
//!   schedule the explorer prints)
//! - `Schedule::host_text()` / `Schedule::guest_text()`: the two halves as the engines see them
//! - `Schedule::has_power_cut()`: a `C=` token or a `cf=` host token (a recovery boot follows)

use std::fmt;
use std::str::FromStr;

/// One schedule: a host part (engine-side faults and timing) and a guest part (`run` + tokens).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    host: String,
    guest: String,
}

/// Why a schedule string was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

impl Schedule {
    pub fn parse(text: &str) -> Result<Schedule, ParseError> {
        let (host, guest) = text.split_once('|').ok_or_else(|| ParseError {
            message: "a schedule needs a '|' between its host and guest parts".into(),
        })?;
        Ok(Schedule {
            host: host.trim().to_string(),
            guest: guest.trim().to_string(),
        })
    }

    pub fn host_text(&self) -> String {
        self.host.clone()
    }

    pub fn guest_text(&self) -> String {
        self.guest.clone()
    }

    pub fn has_power_cut(&self) -> bool {
        self.guest.contains(" C=") || self.host.split(' ').any(|t| t.starts_with("cf="))
    }
}

impl FromStr for Schedule {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Schedule::parse(s)
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} | {}", self.host, self.guest)
    }
}
