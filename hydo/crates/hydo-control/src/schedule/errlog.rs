//! Error logs as findings (seam `error-logs`; docs/HISTORY.md M94, M123): what
//! `hydo/onboard/hydo-init-errlog.sh` adds to the init for a manifest with `errors:`.
//!
//! Each covered service has log streams (0 = its stdout/stderr log, `<svc>.log.old` and
//! `<svc>.log` together; 1 = its setup log; 2.. = the files of its `errors.files`). The guest
//! keeps the shell's bookkeeping files, written by the same pipelines (`cat … | wc -l`):
//! - [`after_mount`] (mount_data): each data-disk file's line count when this boot mounted the
//!   disk (`$H/el.b.<svc>.<k>`), so older lines are not reported again;
//! - [`mark`] (step_range, retry_workload): every stream's line count when a workload range (label
//!   = its first step) or the end phase (`end`) starts (`$H/el.m.<svc>.<k>`).
//!
//! [`report`] (el_report, once per boot: before the verdict, before die_oracle's verdict, and at a
//! power cut after the services are stopped) reads the streams and those files and computes the
//! records on the host: EL_SCAN's selection and template and EL_SUM's grouping, ported from the
//! awk programs with busybox awk's semantics (POSIX regular expressions with leftmost-longest
//! substitution, bytes in the C locale). hydo-errors then reads the records as it reads the
//! shell's. The guest no longer runs the awk.
//!
//! The verdict stays the last line: the assertion records the final invariants print (the
//! shell's final_verdict output, redirected into `$H/el.verdict`) are held back until the error
//! records are out ([`final_verdict`], [`assert_line`]).

use super::text::{read_bytes, records, shq, text};
use super::{bb, env, log, run, sh, svc_sig, Conversation, Ctx, F};
use crate::flow::{Memo, Step};
use hydo_agent_proto::msg::Sig;
use hydo_manifest::py;
use regex_automata::hybrid::dfa::DFA;
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input, MatchKind};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;

// ------------------------------------------------------------------ the plan (hydo-build's compile_errors)

/// hydo-build's level presets (hydo/crates/hydo-build/src/shell.rs; a test compares the plan
/// with the manifest.sh hydo-build writes).
pub const LEVEL_ERROR: &str = concat!(
    r#"(^|[^A-Za-z0-9_])(ERROR|ERRO|FATAL|FATA|CRITICAL|CRIT|PANIC|SEVERE|EMERG|ALERT)([^A-Za-z0-9_]|$)"#,
    r#"|(^|[^A-Za-z0-9_])(Error|Fatal|Critical|Panic|Severe)(:|\])"#,
    r#"|[Ll][Ee][Vv][Ee][Ll]"?[=:] *"?(error|err|fatal|crit|critical|panic|emerg|alert|ERROR|ERR|FATAL|CRITICAL|PANIC)([^A-Za-z]|$)"#,
    r#"|\[(error|err|fatal|crit|critical|alert|emerg|panic)\]"#,
    r#"|^panic: |^fatal error: "#
);
pub const LEVEL_WARNING: &str = concat!(
    r#"(^|[^A-Za-z0-9_])(WARN|WARNING)([^A-Za-z0-9_]|$)|(^|[^A-Za-z0-9_])Warning(:|\])"#,
    r#"|[Ll][Ee][Vv][Ee][Ll]"?[=:] *"?(warn|warning|WARN|WARNING)([^A-Za-z]|$)|\[(warn|warning)\]"#
);
pub const EXCEPTION_LINE: &str = r#"^[A-Za-z_$][A-Za-z0-9_$.]*(Exception|Error)(:|$)"#;
pub const REDIS_LEVEL: &str = r#"^[0-9]+:[XCSM] [^#]* # "#;

/// One covered service (manifest.sh's HYDO_ERR_<svc>_*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrSvc {
    pub name: String,
    pub level: String,
    pub exc: String,
    pub ignore: String,
    /// errors.files, as the words of HYDO_ERR_<svc>_files.
    pub files: Vec<String>,
    /// HYDO_ERR_<svc>_norm's lines (`<pattern>\t<replacement>`).
    pub norm: Vec<String>,
}

/// The `errors:` part of a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrPlan {
    /// HYDO_ERR_MAX.
    pub max: i64,
    /// HYDO_ERR_SERVICES, in start order.
    pub services: Vec<ErrSvc>,
}

fn s(v: &Value) -> String {
    py::str_of(v)
}

fn words(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a
            .iter()
            .map(py::str_of)
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        other => py::str_of(other)
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    }
}

impl ErrPlan {
    /// The plan of a validated manifest with `errors:` (None without it); `order` is the start
    /// order of the services.
    pub fn of(d: &Value, order: &[String]) -> Option<ErrPlan> {
        let e = match d.get("errors") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => return None,
            Some(e) => e,
        };
        let svcs: Vec<Value> = d["services"].as_array().cloned().unwrap_or_default();
        let empty = Value::Object(Default::default());
        let cov: Vec<String> = match e.get("services") {
            Some(Value::Array(a)) if !a.is_empty() => a.iter().map(s).collect(),
            _ => svcs.iter().map(|x| s(&x["name"])).collect(),
        };
        let covered = |x: &Value| {
            let off = x
                .get("errors")
                .filter(|e| py::truthy(e))
                .and_then(|e| e.get("off"))
                .is_some_and(py::truthy);
            cov.contains(&s(&x["name"])) && !off
        };
        let max = e
            .get("max")
            .and_then(py::int_of)
            .and_then(|i| i64::try_from(i).ok())
            .unwrap_or(40);
        let mut services = Vec::new();
        for n in order {
            let Some(sv) = svcs.iter().find(|x| s(&x["name"]) == *n) else {
                continue;
            };
            if !covered(sv) {
                continue;
            }
            let se = sv.get("errors").filter(|e| py::truthy(e)).unwrap_or(&empty);
            let fmt = se.get("format").map(s).unwrap_or_else(|| "auto".into());
            let mut alts: Vec<String> = Vec::new();
            if fmt == "auto" {
                alts.push(LEVEL_ERROR.into());
                let top = e.get("level").cloned().unwrap_or("error".into());
                let lv = se.get("level").cloned().unwrap_or(top);
                if lv.as_str() == Some("warning") {
                    alts.push(LEVEL_WARNING.into());
                }
            } else if fmt == "redis" {
                alts.push(REDIS_LEVEL.into());
            }
            if let Some(mt) = se.get("match").filter(|x| py::truthy(x)) {
                alts.push(s(mt));
            }
            let mut norm = Vec::new();
            for r in se
                .get("normalize")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                match &r {
                    Value::String(x) => norm.push(format!("{x}\t*")),
                    other => {
                        let rep = other.get("replace").map(s).unwrap_or_else(|| "*".into());
                        norm.push(format!("{}\t{rep}", s(&other["pattern"])));
                    }
                }
            }
            services.push(ErrSvc {
                name: n.clone(),
                level: alts
                    .iter()
                    .map(|a| format!("({a})"))
                    .collect::<Vec<_>>()
                    .join("|"),
                exc: if fmt == "auto" {
                    EXCEPTION_LINE.into()
                } else {
                    String::new()
                },
                ignore: se
                    .get("ignore")
                    .filter(|x| py::truthy(x))
                    .map(s)
                    .unwrap_or_default(),
                files: se
                    .get("files")
                    .filter(|f| py::truthy(f))
                    .map(words)
                    .unwrap_or_default(),
                norm,
            });
        }
        Some(ErrPlan { max, services })
    }
}

impl ErrSvc {
    /// el_streams: 0, 1, then one per file.
    pub fn streams(&self) -> Vec<usize> {
        (0..self.files.len() + 2).collect()
    }

    /// The stream's source name in the records.
    pub fn src(&self, k: usize) -> String {
        match k {
            0 => "log".into(),
            1 => "setup".into(),
            _ => self.files[k - 2].clone(),
        }
    }
}

// ------------------------------------------------------------------ POSIX regular expressions

/// A POSIX extended regular expression as busybox awk's regcomp reads it, translated for the
/// regex crate (bytes, no Unicode): a backslash inside brackets is a literal, an unmatched `{`,
/// `*`, `+` or `?` at the start of an expression is a literal, `\<` and `\>` are word
/// boundaries, and every other byte is matched as itself.
pub fn ere(p: &str) -> Result<String, String> {
    let b = p.as_bytes();
    let mut out = String::new();
    // whether a repeat may follow, where the last atom starts in `out`, whether it is repeated
    let mut atom = false;
    let mut start = 0usize;
    let mut repeated = false;
    let mut groups: Vec<usize> = Vec::new();
    let mut i = 0;
    let lit = |c: u8| format!("\\x{c:02x}");
    // a repeat of a repeated atom (`a**`, `a+{2}`): POSIX repeats the whole; the regex crate
    // needs a group for that
    let regroup = |out: &mut String, start: usize, repeated: &mut bool| {
        if *repeated {
            out.insert_str(start, "(?:");
            out.push(')');
        }
        *repeated = true;
    };
    while i < b.len() {
        let c = b[i];
        if !matches!(c, b'*' | b'+' | b'?' | b'{') || !atom {
            repeated = false;
            if !matches!(c, b')') {
                start = out.len();
            }
        }
        match c {
            b'\\' => {
                i += 1;
                let Some(&d) = b.get(i) else {
                    return Err("trailing backslash".into());
                };
                out.push_str(&match d {
                    b't' => lit(b'\t'),
                    b'n' => lit(b'\n'),
                    b'r' => lit(b'\r'),
                    b'f' => lit(0x0c),
                    b'e' => lit(0x1b),
                    b'w' | b'W' | b's' | b'S' | b'd' | b'D' | b'b' | b'B' => {
                        format!("\\{}", d as char)
                    }
                    b'<' | b'>' => "\\b".into(),
                    other => lit(other),
                });
                atom = !matches!(d, b'b' | b'B' | b'<' | b'>');
                i += 1;
            }
            b'[' => {
                let (cls, n) = bracket(&b[i..])?;
                out.push_str(&cls);
                i += n;
                atom = true;
            }
            b'*' | b'+' | b'?' => {
                if atom {
                    regroup(&mut out, start, &mut repeated);
                    out.push(c as char);
                } else {
                    out.push_str(&lit(c));
                    atom = true;
                }
                i += 1;
            }
            b'{' => {
                let end = b[i..].iter().position(|x| *x == b'}').map(|e| i + e);
                let body = end.map(|e| &p[i + 1..e]);
                let valid = body.is_some_and(|x| {
                    let (lo, hi) = x.split_once(',').unwrap_or((x, x));
                    !lo.is_empty()
                        && lo.bytes().all(|c| c.is_ascii_digit())
                        && hi.bytes().all(|c| c.is_ascii_digit())
                });
                if !atom {
                    out.push_str(&lit(c));
                    atom = true;
                    i += 1;
                } else if valid {
                    let e = end.unwrap();
                    regroup(&mut out, start, &mut repeated);
                    out.push_str(&p[i..=e]);
                    i = e + 1;
                } else {
                    return Err("invalid repetition count(s)".into());
                }
            }
            b'(' => {
                groups.push(out.len());
                out.push('(');
                atom = false;
                i += 1;
            }
            b')' => {
                start = groups.pop().ok_or("unmatched ( or )")?;
                out.push(')');
                atom = true;
                i += 1;
            }
            b'|' => {
                out.push('|');
                atom = false;
                i += 1;
            }
            b'^' | b'$' => {
                out.push(c as char);
                atom = false;
                i += 1;
            }
            b'.' => {
                out.push('.');
                atom = true;
                i += 1;
            }
            other => {
                out.push_str(&lit(other));
                atom = true;
                i += 1;
            }
        }
    }
    Ok(out)
}

/// A bracket expression at the start of `b`: the class and its length.
fn bracket(b: &[u8]) -> Result<(String, usize), String> {
    let mut i = 1;
    let mut out = String::from("[");
    if b.get(i) == Some(&b'^') {
        out.push('^');
        i += 1;
    }
    let mut first = true;
    loop {
        let Some(&c) = b.get(i) else {
            return Err("unmatched [ or [^".into());
        };
        if c == b']' && !first {
            out.push(']');
            return Ok((out, i + 1));
        }
        first = false;
        if c == b'[' && matches!(b.get(i + 1), Some(b':' | b'=' | b'.')) {
            let kind = b[i + 1];
            let close = b[i + 2..]
                .windows(2)
                .position(|w| w[0] == kind && w[1] == b']')
                .ok_or("unmatched [ or [^")?;
            let name = std::str::from_utf8(&b[i + 2..i + 2 + close]).unwrap_or("");
            match kind {
                b':' => {
                    let known = [
                        "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print",
                        "punct", "space", "upper", "xdigit",
                    ];
                    if !known.contains(&name) {
                        return Err("invalid character class".into());
                    }
                    out.push_str(&format!("[:{name}:]"));
                }
                _ => {
                    for x in name.bytes() {
                        out.push_str(&format!("\\x{x:02x}"));
                    }
                }
            }
            i += 2 + close + 2;
            continue;
        }
        if b.get(i + 1) == Some(&b'-') && b.get(i + 2).is_some_and(|x| *x != b']') {
            let hi = b[i + 2];
            if hi < c {
                return Err("invalid character range".into());
            }
            out.push_str(&format!("\\x{c:02x}-\\x{hi:02x}"));
            i += 3;
            continue;
        }
        out.push_str(&format!("\\x{c:02x}"));
        i += 1;
    }
}

/// A compiled expression: the leftmost match start (regex) and the longest match from it (a lazy
/// DFA that reports every match), which together give POSIX's leftmost-longest match.
pub struct Rx {
    first: regex::bytes::Regex,
    all: Option<DFA>,
}

impl Rx {
    pub fn new(ere_text: &str) -> Result<Rx, String> {
        let t = ere(ere_text)?;
        let first = regex::bytes::RegexBuilder::new(&format!("(?-u){t}"))
            .build()
            .map_err(|e| e.to_string())?;
        let all = DFA::builder()
            .configure(DFA::config().match_kind(MatchKind::All))
            .syntax(syntax::Config::new().unicode(false).utf8(false))
            .build(&t)
            .ok();
        Ok(Rx { first, all })
    }

    pub fn is_match(&self, hay: &[u8]) -> bool {
        self.first.is_match(hay)
    }

    /// The leftmost-longest match at or after `pos`.
    pub fn find_at(&self, hay: &[u8], pos: usize) -> Option<(usize, usize)> {
        let m = self.first.find_at(hay, pos)?;
        let mut end = m.end();
        if let Some(dfa) = &self.all {
            let mut cache = dfa.create_cache();
            let input = Input::new(hay).range(m.start()..).anchored(Anchored::Yes);
            if let Ok(Some(hm)) = dfa.try_search_fwd(&mut cache, &input) {
                end = end.max(hm.offset());
            }
        }
        Some((m.start(), end))
    }

    /// awk's gsub(re, repl, s): `&` in repl is the match, `\&` a literal `&`.
    pub fn gsub(&self, s: &[u8], repl: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(s.len());
        let mut pos = 0;
        let mut prev_end: Option<usize> = None;
        while pos <= s.len() {
            let Some((a, e)) = self.find_at(s, pos) else {
                break;
            };
            if a == e && prev_end == Some(a) {
                // an empty match right after a match is not one
                if a >= s.len() {
                    break;
                }
                out.extend_from_slice(&s[pos..=a]);
                pos = a + 1;
                continue;
            }
            out.extend_from_slice(&s[pos..a]);
            put_repl(&mut out, repl, &s[a..e]);
            prev_end = Some(e);
            if a == e {
                if a < s.len() {
                    out.push(s[a]);
                }
                pos = a + 1;
            } else {
                pos = e;
            }
        }
        if pos < s.len() {
            out.extend_from_slice(&s[pos..]);
        }
        out
    }
}

fn put_repl(out: &mut Vec<u8>, repl: &[u8], m: &[u8]) {
    let mut i = 0;
    while i < repl.len() {
        match (repl[i], repl.get(i + 1)) {
            (b'\\', Some(b'&')) => {
                out.push(b'&');
                i += 2;
            }
            (b'\\', Some(b'\\')) => {
                out.push(b'\\');
                i += 2;
            }
            (b'&', _) => {
                out.extend_from_slice(m);
                i += 1;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
}

// ------------------------------------------------------------------ EL_SCAN and EL_SUM

/// norm()'s fixed substitutions, in order, as the awk text writes them (busybox's lexer keeps
/// `\/` as two characters, so `[-\/]` also holds a backslash).
const NORM_FIXED: [(&str, &str); 7] = [
    (
        r"[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][T ][0-9][0-9]:[0-9][0-9](:[0-9][0-9]([.,][0-9]+)?)?(Z|[+-][0-9][0-9]:?[0-9][0-9])?",
        "<time>",
    ),
    (
        r"[0-9][0-9][0-9][0-9][-\/][0-9][0-9][-\/][0-9][0-9]",
        "<date>",
    ),
    (r"[0-9]?[0-9]:[0-9][0-9]:[0-9][0-9]([.,][0-9]+)?", "<time>"),
    (
        r"[0-9]?[0-9][ -](Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[ -][0-9][0-9][0-9][0-9]",
        "<date>",
    ),
    (
        r"((Mon|Tue|Wed|Thu|Fri|Sat|Sun),? +)?(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) +[0-9]?[0-9],?( [0-9][0-9][0-9][0-9])? <time>",
        "<date> <time>",
    ),
    (
        r"[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F]+",
        "<uuid>",
    ),
    (r"0[xX][0-9a-fA-F]+", "<hex>"),
];

fn fixed() -> &'static [(Rx, &'static [u8]); 7] {
    static F: OnceLock<[(Rx, &'static [u8]); 7]> = OnceLock::new();
    F.get_or_init(|| NORM_FIXED.map(|(p, r)| (Rx::new(p).expect("a fixed pattern"), r.as_bytes())))
}

/// clean(s): no backslash, double quote, control character or harness prefix.
pub fn clean(s: &[u8]) -> Vec<u8> {
    let t: Vec<u8> = s
        .iter()
        .map(|&c| match c {
            b'\\' => b'/',
            b'"' => b'\'',
            0..=0x1f | 0x7f => b' ',
            c => c,
        })
        .collect();
    let t = replace_all(&t, b"hydo-", b"hydo_");
    replace_all(&t, b"HYDO-", b"HYDO_")
}

fn replace_all(s: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

fn is_alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

/// nums(s): numbers become `#` unless they are part of an error code (MY-012592, errno 2) or a
/// service port (below the ephemeral range, after an address or a known host); mixed tokens
/// become <hex>, <id> or lose their digits.
pub fn nums(s: &[u8], hosts: &[String]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut prev: Vec<u8> = Vec::new();
    let mut i = 0;
    let word = |t: &[u8], set: &[&str]| set.iter().any(|w| w.as_bytes() == t);
    while let Some(a) = s[i..].iter().position(|c| is_alnum(*c)).map(|x| i + x) {
        let mut e = a;
        while e < s.len() && is_alnum(s[e]) {
            e += 1;
        }
        let pre = &s[i..a];
        let orig = s[a..e].to_vec();
        let mut tok = orig.clone();
        out.extend_from_slice(pre);
        if tok.iter().all(u8::is_ascii_digit) {
            let n: f64 = std::str::from_utf8(&tok).unwrap().parse().unwrap_or(0.0);
            let upper2 = prev.len() >= 2 && prev.iter().all(u8::is_ascii_uppercase);
            let codeword = word(
                &prev,
                &[
                    "errno", "Errno", "ERRNO", "code", "Code", "CODE", "error", "Error", "ERROR",
                    "status", "Status", "STATUS",
                ],
            );
            let host = prev == b"localhost" || hosts.iter().any(|h| h.as_bytes() == prev);
            let keep = (pre == b"-" && upper2)
                || (!pre.is_empty()
                    && pre.iter().all(|c| matches!(c, b' ' | b':' | b'='))
                    && codeword)
                || (pre == b":"
                    && tok.len() <= 5
                    && n < 32768.0
                    && (out.ends_with(b"#.#.#.#:") || out.ends_with(b"]:") || host));
            if !keep {
                tok = b"#".to_vec();
            }
        } else if tok.iter().any(u8::is_ascii_digit) {
            if tok.len() >= 8 && tok.iter().all(u8::is_ascii_hexdigit) {
                tok = b"<hex>".to_vec();
            } else if tok.len() >= 16 && tok.iter().any(u8::is_ascii_alphabetic) {
                tok = b"<id>".to_vec();
            } else {
                let lead = tok.iter().take_while(|c| c.is_ascii_uppercase()).count();
                let upper_digits = lead > 0 && tok[lead..].iter().all(u8::is_ascii_digit);
                if !upper_digits {
                    let mut t = Vec::new();
                    let mut j = 0;
                    while j < tok.len() {
                        if tok[j].is_ascii_digit() {
                            while j < tok.len() && tok[j].is_ascii_digit() {
                                j += 1;
                            }
                            t.push(b'#');
                        } else {
                            t.push(tok[j]);
                            j += 1;
                        }
                    }
                    tok = t;
                }
            }
        }
        out.extend_from_slice(&tok);
        prev = orig;
        i = e;
    }
    out.extend_from_slice(&s[i..]);
    out
}

/// A user `normalize` substitution: the expression and its replacement.
type UserNorm = (Rx, Vec<u8>);

/// The patterns of one stream, compiled when first used (as awk compiles a dynamic regex).
struct Pats<'a> {
    sv: &'a ErrSvc,
    level: Option<Result<Rx, String>>,
    exc: Option<Result<Rx, String>>,
    ignore: Option<Result<Rx, String>>,
    norm: Option<Result<Vec<UserNorm>, String>>,
}

fn get<'b>(slot: &'b mut Option<Result<Rx, String>>, p: &str) -> Result<&'b Rx, String> {
    slot.get_or_insert_with(|| Rx::new(p))
        .as_ref()
        .map_err(|e| e.clone())
}

impl Pats<'_> {
    fn norm(&mut self) -> Result<&Vec<(Rx, Vec<u8>)>, String> {
        let sv = self.sv;
        self.norm
            .get_or_insert_with(|| {
                sv.norm
                    .iter()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let mut f = l.split('\t');
                        let p = f.next().unwrap_or("");
                        let r = f.next().unwrap_or("");
                        Rx::new(p).map(|rx| (rx, r.as_bytes().to_vec()))
                    })
                    .collect()
            })
            .as_ref()
            .map_err(|e| e.clone())
    }
}

/// norm(s): the template of a cleaned line.
fn norm(s: &[u8], user: &[(Rx, Vec<u8>)], hosts: &[String]) -> Vec<u8> {
    let mut s = s.to_vec();
    for (rx, r) in user {
        s = rx.gsub(&s, r);
    }
    for (rx, r) in fixed() {
        s = rx.gsub(&s, r);
    }
    let s = nums(&s, hosts);
    // gsub(/[ \t]+/, " ", s); sub(/^ /, "", s); sub(/ $/, "", s); substr(s, 1, 200)
    let t = collapse(&s);
    let mut t: &[u8] = &t;
    if t.first() == Some(&b' ') {
        t = &t[1..];
    }
    if t.last() == Some(&b' ') {
        t = &t[..t.len() - 1];
    }
    t[..t.len().min(200)].to_vec()
}

fn collapse(s: &[u8]) -> Vec<u8> {
    let mut t: Vec<u8> = Vec::with_capacity(s.len());
    let mut blank = false;
    for &c in s {
        if matches!(c, b' ' | b'\t') {
            if !blank {
                t.push(b' ');
            }
            blank = true;
        } else {
            t.push(c);
            blank = false;
        }
    }
    t
}

/// One error line (a line of $H/el.rec).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rec {
    pub svc: String,
    pub src: String,
    pub tpl: Vec<u8>,
    pub lab: String,
    pub ex: Vec<u8>,
}

/// What one stream gave: its records, and the error that stopped the scan (awk's fatal error on
/// an expression it cannot compile; the records before it stay).
pub struct Scan {
    pub recs: Vec<Rec>,
    pub err: Option<String>,
}

/// EL_SCAN over one stream: its text, its marks file, its base count and the first label.
pub fn scan(
    sv: &ErrSvc,
    k: usize,
    stream: &[u8],
    marks: &[u8],
    base: &[u8],
    first: &str,
    hosts: &[String],
) -> Scan {
    let ms: Vec<(String, f64)> = records(marks)
        .iter()
        .map(|l| {
            let t = text(l);
            let mut w = t.split_whitespace();
            let lab = w.next().unwrap_or("").to_string();
            let n = super::composer::awk_num(w.next().unwrap_or(""));
            (lab, n)
        })
        .collect();
    let base = super::composer::awk_num(&text(base));
    let (svc, src) = (sv.name.clone(), sv.src(k));
    let mut p = Pats {
        sv,
        level: None,
        exc: None,
        ignore: None,
        norm: None,
    };
    let mut lab = first.to_string();
    let mut mi = 0;
    let mut trace = false;
    let mut recs = Vec::new();
    for (n, line) in records(stream).iter().enumerate() {
        let nr = (n + 1) as f64;
        while mi < ms.len() && nr > ms[mi].1 {
            lab = ms[mi].0.clone();
            mi += 1;
        }
        if nr <= base {
            continue;
        }
        let r: Result<bool, String> = (|| {
            let ign = !sv.ignore.is_empty() && get(&mut p.ignore, &sv.ignore)?.is_match(line);
            if line.starts_with(b"HYDO-") || ign {
                trace = false;
                return Ok(false);
            }
            if !sv.level.is_empty() && get(&mut p.level, &sv.level)?.is_match(line) {
                // a hit
            } else if !sv.exc.is_empty() && get(&mut p.exc, &sv.exc)?.is_match(line) {
                if trace {
                    return Ok(false);
                }
            } else {
                let cont = line.starts_with(b" ")
                    || line.starts_with(b"\t")
                    || line.starts_with(b"Caused by");
                if trace && cont {
                    return Ok(false);
                }
                trace = false;
                return Ok(false);
            }
            trace = true;
            Ok(true)
        })();
        match r {
            Ok(false) => {}
            Ok(true) => {
                let c = clean(line);
                let user = match p.norm() {
                    Ok(u) => u,
                    Err(e) => return Scan { recs, err: Some(e) },
                };
                recs.push(Rec {
                    svc: svc.clone(),
                    src: src.clone(),
                    tpl: norm(&c, user, hosts),
                    lab: lab.clone(),
                    ex: c[..c.len().min(240)].to_vec(),
                });
            }
            Err(e) => return Scan { recs, err: Some(e) },
        }
    }
    Scan { recs, err: None }
}

fn rank(l: &str) -> f64 {
    match l {
        "s" => -1.0,
        "end" => 1_000_000.0,
        "r" => 1_000_001.0,
        other => super::composer::awk_num(other),
    }
}

/// EL_SUM: one HYDO-ERRLOG line per (service, template), in first-seen order, at most `max`, then
/// the HYDO-ERRLOG-SUM line.
pub fn summarize(recs: &[Rec], max: i64, boot: &str, steps: i64) -> Vec<String> {
    struct K {
        svc: String,
        src: String,
        tpl: Vec<u8>,
        ex: Vec<u8>,
        n: usize,
        labs: Vec<String>,
    }
    let mut ks: Vec<K> = Vec::new();
    let mut at: BTreeMap<(String, Vec<u8>), usize> = BTreeMap::new();
    for r in recs {
        let key = (r.svc.clone(), r.tpl.clone());
        let i = *at.entry(key).or_insert_with(|| {
            ks.push(K {
                svc: r.svc.clone(),
                src: r.src.clone(),
                tpl: r.tpl.clone(),
                ex: r.ex.clone(),
                n: 0,
                labs: Vec::new(),
            });
            ks.len() - 1
        });
        ks[i].n += 1;
        if !ks[i].labs.contains(&r.lab) {
            ks[i].labs.push(r.lab.clone());
        }
    }
    let mut out = Vec::new();
    for k in ks.iter().take(max.max(0) as usize) {
        let mut labs = k.labs.clone();
        // the awk's insertion sort by rank (stable)
        for a in 1..labs.len() {
            let v = labs[a].clone();
            let mut b = a;
            while b >= 1 && rank(&labs[b - 1]) > rank(&v) {
                labs[b] = labs[b - 1].clone();
                b -= 1;
            }
            labs[b] = v;
        }
        out.push(format!(
            "HYDO-ERRLOG {{\"svc\":\"{}\",\"src\":\"{}\",\"n\":{},\"at\":\"{}\",\"tpl\":\"{}\",\"ex\":\"{}\"}}",
            k.svc,
            k.src,
            k.n,
            labs.join(","),
            text(&k.tpl),
            text(&k.ex)
        ));
    }
    let nk = ks.len() as i64;
    out.push(format!(
        "HYDO-ERRLOG-SUM {{\"boot\":\"{boot}\",\"steps\":{steps},\"lines\":{},\"kinds\":{nk},\"shown\":{}}}",
        recs.len(),
        if nk < max { nk } else { max }
    ));
    out
}

// ------------------------------------------------------------------ the controller's part

/// `$H/el.reported`, and final_verdict's redirected output while it is held back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub reported: bool,
    pub held: Option<Vec<String>>,
}

fn plan(ctx: &Ctx) -> Option<&ErrPlan> {
    ctx.plan.errors.as_ref()
}

/// el_cat <svc> <k>, as shell text.
fn cat(ctx: &Ctx, sv: &ErrSvc, k: usize) -> String {
    let (b, h) = (bb(ctx), &ctx.plan.layout.dir);
    match k {
        0 => format!("{b} cat {h}/{n}.log.old {h}/{n}.log", n = sv.name),
        1 => format!("{b} cat {h}/{}.setup.log", sv.name),
        _ => format!("{b} cat {}", shq(&sv.files[k - 2])),
    }
}

/// A shell script run to its end (the bookkeeping pipelines).
fn script(fx: &mut F<'_>, ctx: &Ctx, lines: Vec<String>) -> Step<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let e = env(fx, ctx, &[]);
    run(fx, sh(ctx, lines.join("\n")), e, None)?;
    Ok(())
}

/// mount_data's wrapper: each data-disk file's line count now (`$H/el.b.<svc>.<k>`).
pub(super) fn after_mount(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let Some(ep) = plan(ctx) else { return Ok(()) };
    let h = &ctx.plan.layout.dir;
    let mut ls = Vec::new();
    for sv in &ep.services {
        for k in sv.streams().into_iter().filter(|k| *k >= 2) {
            ls.push(format!(
                "{} 2>/dev/null | {} wc -l > {h}/el.b.{}.{k}",
                cat(ctx, sv, k),
                bb(ctx),
                sv.name
            ));
        }
    }
    script(fx, ctx, ls)
}

/// el_mark <label>: every stream's line count, under the label of what starts now.
pub(super) fn mark(fx: &mut F<'_>, ctx: &Ctx, label: &str) -> Step<()> {
    let Some(ep) = plan(ctx) else { return Ok(()) };
    let h = &ctx.plan.layout.dir;
    let mut ls = Vec::new();
    for sv in &ep.services {
        for k in sv.streams() {
            ls.push(format!(
                "echo \"{label} $({} 2>/dev/null | {} wc -l)\" >> {h}/el.m.{}.{k}",
                cat(ctx, sv, k),
                bb(ctx),
                sv.name
            ));
        }
    }
    script(fx, ctx, ls)
}

/// el_report: once per boot, every covered stream's error lines, grouped, as report lines.
pub(super) fn report(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let Some(ep) = plan(ctx) else { return Ok(()) };
    if fx.effect_bool(|h, _| std::mem::replace(&mut h.errlog.reported, true)) {
        return Ok(());
    }
    let (first, boot) = match ctx.conv {
        Conversation::Recovery { .. } => ("r", "recovery"),
        _ => ("s", "first"),
    };
    let h = ctx.plan.layout.dir.clone();
    let hosts: Vec<String> = ctx.plan.services.iter().map(|s| s.name.clone()).collect();
    let mut recs = Vec::new();
    let mut errs = String::new();
    for sv in &ep.services {
        for k in sv.streams() {
            let mut stream = Vec::new();
            match k {
                0 => {
                    stream.extend(read_bytes(fx, &format!("{h}/{}.log.old", sv.name))?);
                    stream.extend(read_bytes(fx, &format!("{h}/{}.log", sv.name))?);
                }
                1 => stream.extend(read_bytes(fx, &format!("{h}/{}.setup.log", sv.name))?),
                _ => stream.extend(read_bytes(fx, &sv.files[k - 2])?),
            }
            let marks = read_bytes(fx, &format!("{h}/el.m.{}.{k}", sv.name))?;
            let base = read_bytes(fx, &format!("{h}/el.b.{}.{k}", sv.name))?;
            let sc = scan(sv, k, &stream, &marks, &base, first, &hosts);
            recs.extend(sc.recs);
            if let Some(e) = sc.err {
                // AMBIGUOUS: busybox awk's own message (regerror's text) is not reproduced
                errs.push_str(&format!("awk: {}: {e}\n", sv.name));
            }
        }
    }
    if !errs.is_empty() {
        let e: String = errs.chars().take(300).collect();
        log(fx, format!("error-log scan: {e}"));
    }
    for l in summarize(&recs, ep.max, boot, ctx.plan.steps) {
        fx.report(l);
    }
    Ok(())
}

/// failed_ids' wrapper at a power cut: every service stopped, then the report.
pub(super) fn at_cut(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if plan(ctx).is_none() {
        return Ok(());
    }
    for s in &ctx.plan.services {
        svc_sig(fx, &s.name, Sig::Stop)?;
    }
    report(fx, ctx)
}

/// final_verdict's wrapper: its output (the assertion records and the verdict) after the error
/// records.
pub(super) fn final_verdict(fx: &mut F<'_>, ctx: &Ctx) -> Step<String> {
    if plan(ctx).is_none() {
        return super::final_verdict(fx, ctx);
    }
    fx.effect(|h, _| {
        h.errlog.held = Some(Vec::new());
        Memo::Unit
    });
    let v = super::final_verdict(fx, ctx)?;
    let held = fx.effect_text(|h, _| h.errlog.held.take().unwrap_or_default().join("\n"));
    report(fx, ctx)?;
    for l in held.lines() {
        fx.report(l);
    }
    Ok(v)
}

/// assert_once's record: printed now, or held while final_verdict's output is redirected.
pub(super) fn assert_line(fx: &mut F<'_>, line: String) {
    let l = line.clone();
    let held = fx.effect_bool(|h, _| match &mut h.errlog.held {
        Some(v) => {
            v.push(l);
            true
        }
        None => false,
    });
    if !held {
        fx.report(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_expressions() {
        for p in [LEVEL_ERROR, LEVEL_WARNING, EXCEPTION_LINE, REDIS_LEVEL] {
            Rx::new(p).unwrap();
        }
        let rx = Rx::new("a|aa").unwrap();
        assert_eq!(rx.gsub(b"aaa", b"<&>"), b"<aa><a>".to_vec());
        let rx = Rx::new(r"[-\/]").unwrap();
        assert_eq!(rx.gsub(b"a-b\\c/d", b"X"), b"aXbXcXd".to_vec());
        let rx = Rx::new("x*").unwrap();
        assert_eq!(rx.gsub(b"abc", b"-"), b"-a-b-c-".to_vec());
        assert_eq!(Rx::new("{1}a").unwrap().gsub(b"{1}a", b"Z"), b"Z".to_vec());
        assert!(Rx::new("a{x").is_err());
        assert!(Rx::new("[[:bogus:]]").is_err());
        assert!(Rx::new(r"\d+").unwrap().is_match(b"x12"));
        assert_eq!(
            norm(
                &clean(b"2026-01-01T00:00:12.345678Z 0 [ERROR] [MY-012592] [InnoDB] number 2 in"),
                &[],
                &[]
            ),
            b"<time> # [ERROR] [MY-012592] [InnoDB] number # in".to_vec()
        );
        assert_eq!(
            nums(
                b"dial tcp 127.0.0.1:54321->127.0.0.1:3306 errno 2 thread12 HY000 deadbeef42",
                &[]
            ),
            b"dial tcp #.#.#.#:#->#.#.#.#:3306 errno 2 thread# HY000 <hex>".to_vec()
        );
    }
}
