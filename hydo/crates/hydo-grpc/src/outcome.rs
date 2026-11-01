//! The outcome of one gRPC call: ok, a definite failure, or unknown -- the distinction every
//! check of a system under faults depends on. A definite failure promises the request had no
//! effect; an unknown outcome means it may or may not have taken effect (Jepsen's :fail, :info).

use std::collections::BTreeMap;

/// Kind of an outcome; the strings are the HYDO-HIST record types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// the call succeeded
    Ok,
    /// definitely not applied
    Fail,
    /// may or may not have been applied
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ok => "ok",
            Kind::Fail => "fail",
            Kind::Unknown => "info",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "ok" => Some(Kind::Ok),
            "fail" => Some(Kind::Fail),
            "info" => Some(Kind::Unknown),
            _ => None,
        }
    }

    /// The word of the HYDO_OUTCOMES file (MANIFEST.md, "Variables").
    pub fn word(self) -> &'static str {
        match self {
            Kind::Ok => "ack",
            Kind::Fail => "failed",
            Kind::Unknown => "unknown",
        }
    }
}

pub const OK: u32 = 0;
pub const CANCELED: u32 = 1;
pub const UNKNOWN: u32 = 2;
pub const INVALID_ARGUMENT: u32 = 3;
pub const DEADLINE_EXCEEDED: u32 = 4;
pub const NOT_FOUND: u32 = 5;
pub const ALREADY_EXISTS: u32 = 6;
pub const PERMISSION_DENIED: u32 = 7;
pub const RESOURCE_EXHAUSTED: u32 = 8;
pub const FAILED_PRECONDITION: u32 = 9;
pub const ABORTED: u32 = 10;
pub const OUT_OF_RANGE: u32 = 11;
pub const UNIMPLEMENTED: u32 = 12;
pub const INTERNAL: u32 = 13;
pub const UNAVAILABLE: u32 = 14;
pub const DATA_LOSS: u32 = 15;
pub const UNAUTHENTICATED: u32 = 16;

const NAMES: [&str; 17] = [
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

/// The default kind of every status code.
///
/// Definite failures are the codes a server returns when it rejected a request before doing
/// anything: bad input, a missing or existing entity, a failed precondition, an auth failure, an
/// aborted transaction, an unimplemented method. Everything that can happen after a request left
/// the client is unknown: the deadline passed, the connection failed or was reset, the call was
/// cancelled, the server crashed half-way (INTERNAL, UNKNOWN, DATA_LOSS), or it was throttled.
pub fn default_kind(code: u32) -> Kind {
    match code {
        OK => Kind::Ok,
        INVALID_ARGUMENT | NOT_FOUND | ALREADY_EXISTS | PERMISSION_DENIED | UNAUTHENTICATED
        | FAILED_PRECONDITION | OUT_OF_RANGE | ABORTED | UNIMPLEMENTED => Kind::Fail,
        _ => Kind::Unknown,
    }
}

/// The status code's canonical upper-case name (DEADLINE_EXCEEDED, ...).
pub fn code_name(c: u32) -> String {
    match NAMES.get(c as usize) {
        Some(n) => n.to_string(),
        None => format!("CODE_{c}"),
    }
}

/// Reads a canonical code name.
pub fn parse_code(s: &str) -> Option<u32> {
    NAMES.iter().position(|n| *n == s).map(|i| i as u32)
}

/// The canonical names, sorted.
pub fn code_names() -> Vec<&'static str> {
    let mut v = NAMES.to_vec();
    v.sort();
    v
}

/// The result of one call.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub kind: Kind,
    pub code: u32,
    /// the status message ("" for OK)
    pub msg: String,
}

/// Classifies a call's status. `overrides` maps a code name to "ok", "fail" or "info" (an RPC's
/// `classify:` hint).
pub fn classify(code: u32, msg: &str, overrides: &BTreeMap<String, String>) -> Outcome {
    let mut k = default_kind(code);
    if let Some(o) = overrides.get(&code_name(code)) {
        if let Some(x) = Kind::parse(o) {
            k = x;
        }
    }
    Outcome {
        kind: k,
        code,
        msg: msg.trim().to_string(),
    }
}

/// Checks an override value.
pub fn valid_override(v: &str) -> bool {
    Kind::parse(v).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_codes() {
        let none = BTreeMap::new();
        let want = [
            (OK, Kind::Ok),
            (INVALID_ARGUMENT, Kind::Fail),
            (NOT_FOUND, Kind::Fail),
            (ALREADY_EXISTS, Kind::Fail),
            (PERMISSION_DENIED, Kind::Fail),
            (UNAUTHENTICATED, Kind::Fail),
            (FAILED_PRECONDITION, Kind::Fail),
            (OUT_OF_RANGE, Kind::Fail),
            (ABORTED, Kind::Fail),
            (UNIMPLEMENTED, Kind::Fail),
            (DEADLINE_EXCEEDED, Kind::Unknown),
            (UNAVAILABLE, Kind::Unknown),
            (CANCELED, Kind::Unknown),
            (INTERNAL, Kind::Unknown),
            (UNKNOWN, Kind::Unknown),
            (DATA_LOSS, Kind::Unknown),
            (RESOURCE_EXHAUSTED, Kind::Unknown),
            (99, Kind::Unknown),
        ];
        for (c, k) in want {
            assert_eq!(classify(c, "x", &none).kind, k, "{}", code_name(c));
        }
        for c in 0..=16 {
            assert_eq!(parse_code(&code_name(c)), Some(c));
        }
        assert_eq!(code_name(99), "CODE_99");
    }

    #[test]
    fn overrides() {
        let o: BTreeMap<String, String> = [
            ("UNAVAILABLE".to_string(), "fail".to_string()),
            ("ABORTED".to_string(), "info".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(classify(UNAVAILABLE, "x", &o).kind, Kind::Fail);
        assert_eq!(classify(ABORTED, "x", &o).kind, Kind::Unknown);
        assert_eq!(classify(OK, "", &o).kind, Kind::Ok);
        assert_eq!(
            (Kind::Ok.word(), Kind::Fail.word(), Kind::Unknown.word()),
            ("ack", "failed", "unknown")
        );
    }
}
