//! `hydo rules scan --kind clock` (Milestone 248, tentative number; docs/design/rule-suggestion.md,
//! "The clock audit"): code that measures a duration with the wall clock instead of a monotonic
//! clock, and a clock-step schedule aimed at each place.
//!
//! A lease, a timeout, a throttle, a lag or a delay measured on the wall clock lasts longer by
//! the size of a backward step of the clock, and ends early after a forward one. NTP steps a
//! clock that is far off, so such code fails on real machines. Four bugs that hydo found on
//! 2026-11-14 had this one cause (Valkey, Temporal, River, and a private queue).
//!
//! The audit reads the source files of eight languages (Go, C and C++, Rust, Java and Kotlin,
//! Python, JavaScript and TypeScript, Ruby, C#) and SQL. It works on text, without a compiler:
//!
//! 1. Each line is cleaned of comments and of the contents of strings.
//! 2. Wall-clock readings are found: the calls of each language that read the realtime clock
//!    (`gettimeofday`, `time(NULL)`, `SystemTime::now()`, `System.currentTimeMillis()`,
//!    `time.time()`, `Date.now()`, Go's `time.Now().UTC()` and the other forms that drop Go's
//!    monotonic reading, SQL's `now()`).
//! 3. The readings are followed through the code: a small function whose name speaks of time
//!    and that reads the wall clock (`ustime()`, a `Now()` method that returns `time.Now().UTC()`)
//!    becomes a reading too, and so does a variable or a field that is set from a reading.
//! 4. A line is a match when it measures a duration with a reading: a subtraction whose result is
//!    compared with a limit (`elapsed`), a reading plus a duration stored as a deadline or a lease
//!    (`deadline`), or a reading compared with a deadline (`check`). A reading that is only logged
//!    or stored as a time stamp is not a match.
//!
//! Precision matters more than recall: a match is `high` when the line also compares the
//! duration with a limit or stores a lease, and `medium` when only its words suggest it. Weaker
//! matches are dropped. Tests and vendored code are not read.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// The languages the audit reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Go,
    C,
    Rust,
    Jvm,
    Python,
    Js,
    Ruby,
    CSharp,
    Sql,
}

impl Lang {
    pub fn name(self) -> &'static str {
        match self {
            Lang::Go => "go",
            Lang::C => "c",
            Lang::Rust => "rust",
            Lang::Jvm => "java",
            Lang::Python => "python",
            Lang::Js => "javascript",
            Lang::Ruby => "ruby",
            Lang::CSharp => "csharp",
            Lang::Sql => "sql",
        }
    }
    fn braces(self) -> bool {
        !matches!(self, Lang::Python | Lang::Ruby | Lang::Sql)
    }
}

/// The language of a file, by its extension.
pub fn lang_of(rel: &str) -> Option<Lang> {
    let name = rel.rsplit('/').next()?;
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "go" => Lang::Go,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" => Lang::C,
        "rs" => Lang::Rust,
        "java" | "kt" | "kts" | "scala" => Lang::Jvm,
        "py" => Lang::Python,
        "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "mts" | "cts" => Lang::Js,
        "rb" => Lang::Ruby,
        "cs" => Lang::CSharp,
        "sql" => Lang::Sql,
        _ => return None,
    })
}

/// Whether a file is a test, a test helper, an example or vendored code, by its path (lower
/// case). The audit reads the product's own code only.
pub fn skipped(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let (dirs, name) = parts.split_at(parts.len().saturating_sub(1));
    let name = name.first().copied().unwrap_or("");
    let dir_skip = dirs.iter().any(|d| {
        d.contains("test")
            || matches!(
                *d,
                "mock"
                    | "mocks"
                    | "fixtures"
                    | "fixture"
                    | "deps"
                    | "examples"
                    | "example"
                    | "docs"
                    | "doc"
                    | "bench"
                    | "benches"
                    | "benchmarks"
                    | "e2e"
                    | "spec"
                    | "specs"
                    | "unit"
                    | "scripts"
                    | "script"
                    | "fakes"
            )
    });
    let stem = name.split('.').next().unwrap_or(name);
    dir_skip
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem.ends_with("_spec")
        || stem.ends_with("test")
        || stem.ends_with("tests")
        || stem == "conftest"
        || stem.starts_with("test")
        || stem.ends_with("testing")
        || stem.contains("benchmark")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains("_mock")
        || name.contains(".pb.")
        || name.ends_with("_pb2.py")
        || name.ends_with(".min.js")
        || name.ends_with(".d.ts")
}

/// Whether the first lines of a file say that a tool generated it.
pub(crate) fn generated(text: &str) -> bool {
    text.lines().take(8).any(|l| {
        let l = l.to_ascii_lowercase();
        (l.contains("code generated") && l.contains("do not edit")) || l.contains("@generated")
    })
}

// ---------------------------------------------------------------- cleaning

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Each line of `text` without its comments and with the contents of its strings removed (the
/// quotes stay). SQL keeps its strings: `datetime('now')` is a reading.
pub fn clean(lang: Lang, text: &str) -> Vec<String> {
    clean_with(lang, text, false)
}

/// Each line of `text` without its comments; `keep` keeps the contents of strings (the route
/// scan reads paths in them, Milestone 344), but not those of Python's triple-quoted strings.
pub(crate) fn clean_with(lang: Lang, text: &str, keep: bool) -> Vec<String> {
    #[derive(PartialEq)]
    enum St {
        Code,
        Block,
        Str(char),
        Raw,
        Triple(char),
    }
    let chars: Vec<char> = text.chars().collect();
    let slash = !matches!(lang, Lang::Python | Lang::Ruby | Lang::Sql);
    let hash = matches!(lang, Lang::Python | Lang::Ruby);
    let sql = lang == Lang::Sql;
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut st = St::Code;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let n = chars.get(i + 1).copied();
        if c == '\n' {
            out.push(std::mem::take(&mut cur));
            if let St::Str(_) = st {
                if lang != Lang::Rust {
                    st = St::Code;
                }
            }
            i += 1;
            continue;
        }
        match st {
            St::Code => {
                if (slash && c == '/' && n == Some('/'))
                    || (hash && c == '#')
                    || (sql && c == '-' && n == Some('-'))
                {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                if (slash || sql) && c == '/' && n == Some('*') {
                    st = St::Block;
                    cur.push(' ');
                    i += 2;
                    continue;
                }
                if sql {
                    cur.push(c);
                    i += 1;
                    continue;
                }
                if lang == Lang::Python
                    && (c == '"' || c == '\'')
                    && n == Some(c)
                    && chars.get(i + 2) == Some(&c)
                {
                    st = St::Triple(c);
                    cur.push_str("\"\"");
                    i += 3;
                    continue;
                }
                if c == '`' && matches!(lang, Lang::Go | Lang::Js) {
                    st = St::Raw;
                    cur.push('`');
                    i += 1;
                    continue;
                }
                let quote = c == '"'
                    || (c == '\''
                        && (lang != Lang::Rust
                            || n == Some('\\')
                            || chars.get(i + 2) == Some(&'\'')));
                if quote {
                    st = St::Str(c);
                    cur.push(c);
                    i += 1;
                    continue;
                }
                cur.push(c);
                i += 1;
            }
            St::Block => {
                if c == '*' && n == Some('/') {
                    st = St::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            St::Str(q) => {
                if c == '\\' && n != Some('\n') {
                    if keep {
                        cur.push(c);
                        cur.extend(n);
                    }
                    i += 2;
                    continue;
                }
                if c == q {
                    st = St::Code;
                    cur.push(q);
                } else if keep {
                    cur.push(c);
                }
                i += 1;
            }
            St::Raw => {
                if c == '`' {
                    st = St::Code;
                    cur.push('`');
                } else if keep {
                    cur.push(c);
                }
                i += 1;
            }
            St::Triple(q) => {
                if c == q && n == Some(q) && chars.get(i + 2) == Some(&q) {
                    st = St::Code;
                    cur.push_str("\"\"");
                    i += 3;
                } else {
                    i += 1;
                }
            }
        }
    }
    out.push(cur);
    out
}

// ---------------------------------------------------------------- readings of the wall clock

/// A call that reads the wall clock: its text, its languages, and whether the character before
/// it may be a dot (false for SQL's `now()`, so that `Date.now()` is not taken for it).
struct Source {
    pat: &'static str,
    langs: &'static [Lang],
    /// `ms`, `s`, `us`, or `-` for a time value whose unit is its type's
    unit: &'static str,
}

const SOURCES: &[Source] = &[
    // Go: these drop the monotonic reading, so a later Sub, Since or comparison uses the wall clock
    Source {
        pat: "time.Now().UTC()",
        langs: &[Lang::Go],
        unit: "-",
    },
    Source {
        pat: "time.Now().Round(0)",
        langs: &[Lang::Go],
        unit: "-",
    },
    Source {
        pat: "time.Now().Truncate(0)",
        langs: &[Lang::Go],
        unit: "-",
    },
    Source {
        pat: "time.Now().Local()",
        langs: &[Lang::Go],
        unit: "-",
    },
    Source {
        pat: "time.Now().In(",
        langs: &[Lang::Go],
        unit: "-",
    },
    Source {
        pat: "time.Now().UnixMilli()",
        langs: &[Lang::Go],
        unit: "ms",
    },
    Source {
        pat: "time.Now().UnixMicro()",
        langs: &[Lang::Go],
        unit: "us",
    },
    Source {
        pat: "time.Now().UnixNano()",
        langs: &[Lang::Go],
        unit: "ns",
    },
    Source {
        pat: "time.Now().Unix()",
        langs: &[Lang::Go],
        unit: "s",
    },
    Source {
        pat: "time.Unix(",
        langs: &[Lang::Go],
        unit: "-",
    },
    Source {
        pat: "time.UnixMilli(",
        langs: &[Lang::Go],
        unit: "-",
    },
    Source {
        pat: "time.UnixMicro(",
        langs: &[Lang::Go],
        unit: "-",
    },
    // C and C++
    Source {
        pat: "gettimeofday(",
        langs: &[Lang::C],
        unit: "us",
    },
    Source {
        pat: "time(NULL)",
        langs: &[Lang::C],
        unit: "s",
    },
    Source {
        pat: "time(0)",
        langs: &[Lang::C],
        unit: "s",
    },
    Source {
        pat: "time(nullptr)",
        langs: &[Lang::C],
        unit: "s",
    },
    Source {
        pat: "time(&",
        langs: &[Lang::C],
        unit: "s",
    },
    Source {
        pat: "clock_gettime(CLOCK_REALTIME",
        langs: &[Lang::C],
        unit: "-",
    },
    Source {
        pat: "system_clock::now()",
        langs: &[Lang::C],
        unit: "-",
    },
    Source {
        pat: "GetSystemTimeAsFileTime(",
        langs: &[Lang::C],
        unit: "-",
    },
    // Rust
    Source {
        pat: "SystemTime::now()",
        langs: &[Lang::Rust],
        unit: "-",
    },
    Source {
        pat: "Utc::now()",
        langs: &[Lang::Rust],
        unit: "-",
    },
    Source {
        pat: "Local::now()",
        langs: &[Lang::Rust],
        unit: "-",
    },
    Source {
        pat: "OffsetDateTime::now_utc()",
        langs: &[Lang::Rust],
        unit: "-",
    },
    // Java, Kotlin, Scala
    Source {
        pat: "System.currentTimeMillis()",
        langs: &[Lang::Jvm],
        unit: "ms",
    },
    Source {
        pat: "Instant.now()",
        langs: &[Lang::Jvm],
        unit: "-",
    },
    Source {
        pat: "new Date()",
        langs: &[Lang::Jvm, Lang::Js],
        unit: "-",
    },
    Source {
        pat: "LocalDateTime.now(",
        langs: &[Lang::Jvm],
        unit: "-",
    },
    Source {
        pat: "ZonedDateTime.now(",
        langs: &[Lang::Jvm],
        unit: "-",
    },
    Source {
        pat: "OffsetDateTime.now(",
        langs: &[Lang::Jvm],
        unit: "-",
    },
    Source {
        pat: "Clock.systemUTC()",
        langs: &[Lang::Jvm],
        unit: "-",
    },
    // Python
    Source {
        pat: "time.time()",
        langs: &[Lang::Python],
        unit: "s",
    },
    Source {
        pat: "time.time_ns()",
        langs: &[Lang::Python],
        unit: "ns",
    },
    Source {
        pat: "datetime.now(",
        langs: &[Lang::Python],
        unit: "-",
    },
    Source {
        pat: "datetime.utcnow()",
        langs: &[Lang::Python],
        unit: "-",
    },
    Source {
        pat: "timezone.now()",
        langs: &[Lang::Python],
        unit: "-",
    },
    // JavaScript, TypeScript
    Source {
        pat: "Date.now()",
        langs: &[Lang::Js],
        unit: "ms",
    },
    // Ruby
    Source {
        pat: "Time.now",
        langs: &[Lang::Ruby],
        unit: "s",
    },
    Source {
        pat: "Time.current",
        langs: &[Lang::Ruby],
        unit: "s",
    },
    Source {
        pat: "Time.zone.now",
        langs: &[Lang::Ruby],
        unit: "s",
    },
    Source {
        pat: "DateTime.now",
        langs: &[Lang::Ruby, Lang::Js],
        unit: "-",
    },
    // C#
    Source {
        pat: "DateTime.UtcNow",
        langs: &[Lang::CSharp],
        unit: "-",
    },
    Source {
        pat: "DateTime.Now",
        langs: &[Lang::CSharp],
        unit: "-",
    },
    Source {
        pat: "DateTimeOffset.UtcNow",
        langs: &[Lang::CSharp],
        unit: "-",
    },
    Source {
        pat: "DateTimeOffset.Now",
        langs: &[Lang::CSharp],
        unit: "-",
    },
];

/// Go's readings that keep the monotonic clock: a wall clock reading only against a value
/// that lost it.
const GO_WEAK: &[&str] = &["time.Now()", "time.Since(", "time.Until("];

/// Calls that read a monotonic clock: an assignment from one makes a field not a reading.
const MONOTONIC: &[&str] = &[
    "CLOCK_MONOTONIC",
    "CLOCK_BOOTTIME",
    "steady_clock",
    "getMonotonic",
    "mach_absolute_time",
    "uv_hrtime",
    "uv_now",
    "Instant::now()",
    "System.nanoTime()",
    "time.monotonic",
    "time.perf_counter",
    "performance.now()",
    "process.hrtime",
    "CLOCK_MONOTONIC_RAW",
    "Stopwatch",
    "Environment.TickCount",
    "monotime",
];

/// SQL's readings of the database server's clock (lower case).
const SQL_NOW: &[&str] = &[
    "now()",
    "current_timestamp",
    "localtimestamp",
    "clock_timestamp()",
    "statement_timestamp()",
    "transaction_timestamp()",
    "sysdate",
    "getdate()",
    "getutcdate()",
    "utc_timestamp",
    "unix_timestamp()",
    "datetime('now'",
    "strftime('%s', 'now')",
    "strftime('%s','now')",
    "julianday('now')",
    "unixepoch()",
];

/// Words of a name that holds a duration or a limit.
const DURATION_WORDS: &[&str] = &[
    "timeout",
    "delay",
    "lag",
    "idle",
    "interval",
    "ttl",
    "lease",
    "expir",
    "deadline",
    "period",
    "threshold",
    "limit",
    "elapsed",
    "since",
    "retry",
    "backoff",
    "wait",
    "stale",
    "grace",
    "window",
    "refresh",
    "throttle",
    "heartbeat",
    "keepalive",
    "cooldown",
    "duration",
];

/// Words of a name that holds a point in time after which something ends.
const EXPIRY_WORDS: &[&str] = &[
    "expir",
    "deadline",
    "lease",
    "until",
    "ttl",
    "timeout",
    "visib",
    "locked",
    "lock_",
    "_lock",
    "retry",
    "not_before",
    "notbefore",
    "run_at",
    "runat",
    "scheduled",
    "heartbeat",
    "leader",
    "horizon",
    "cutoff",
    "stale",
    "available",
];

/// The words that make a deadline check high: a lease, a lock, a timeout.
const LEASE_WORDS: &[&str] = &[
    "lease",
    "leader",
    "locked",
    "lock_",
    "_lock",
    "heartbeat",
    "visib",
    "expir",
    "ttl",
    "timeout",
    "deadline",
    "until",
];

/// Lines that only report a time (logs, metrics, formatted output).
const REPORT_WORDS: &[&str] = &[
    "log.",
    "log(",
    "logger",
    "logf(",
    "serverlog",
    "printf",
    "print(",
    "println",
    "fmt.",
    "addreply",
    "metrics",
    "metric.",
    "histogram",
    "observe(",
    "tag.",
    "span.",
    "trace(",
    "debug(",
    "info(",
    "warn(",
    "console.",
    "puts ",
    "elapsedms(",
    "stats",
];

fn has_any(l: &str, words: &[&str]) -> bool {
    words.iter().any(|w| l.contains(w))
}

/// The positions of `pat` in `code` where the character before is not part of a name (and,
/// with `no_dot`, not a dot).
fn find_all(code: &str, pat: &str, no_dot: bool) -> Vec<usize> {
    code.match_indices(pat)
        .filter(|(i, _)| {
            let before = code[..*i].chars().next_back();
            let first_ident = pat.chars().next().is_some_and(is_ident);
            !(first_ident && before.is_some_and(is_ident)
                || no_dot && matches!(before, Some('.' | ':' | '>' | '$')))
        })
        .map(|(i, _)| i)
        .collect()
}

/// The end of a call that starts at `open` (the index of its `(`), after its `)`.
fn call_end(code: &str, open: usize) -> usize {
    let b = code.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

// ---------------------------------------------------------------- operands

/// The start of the operand that ends just before `p` (spaces skipped): a path of names with
/// `.`, `->` and `::`, calls, index brackets and casts in parentheses.
fn operand_left(code: &str, p: usize) -> usize {
    let b = code.as_bytes();
    let mut i = p;
    while i > 0 && b[i - 1] == b' ' {
        i -= 1;
    }
    let end = i;
    loop {
        if i == 0 {
            break;
        }
        let c = b[i - 1];
        if c == b')' || c == b']' {
            let (open, close) = if c == b')' {
                (b'(', b')')
            } else {
                (b'[', b']')
            };
            let mut depth = 0i32;
            let mut j = i;
            while j > 0 {
                j -= 1;
                if b[j] == close {
                    depth += 1;
                } else if b[j] == open {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            i = j;
            continue;
        }
        if (c as char).is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b':' {
            i -= 1;
            continue;
        }
        if c == b'>' && i >= 2 && b[i - 2] == b'-' {
            i -= 2;
            continue;
        }
        if c == b'?' && i < b.len() && b[i] == b'.' {
            i -= 1;
            continue;
        }
        break;
    }
    if i == end {
        end
    } else {
        i
    }
}

/// The end of the operand that starts after `p` (spaces and a unary sign skipped).
fn operand_right(code: &str, p: usize) -> usize {
    let b = code.as_bytes();
    let mut i = p;
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    while i < b.len() && (b[i] == b'*' || b[i] == b'&' || b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let start = i;
    loop {
        if i >= b.len() {
            break;
        }
        let c = b[i];
        if c == b'(' || c == b'[' {
            let (open, close) = if c == b'(' {
                (b'(', b')')
            } else {
                (b'[', b']')
            };
            let mut depth = 0i32;
            while i < b.len() {
                if b[i] == open {
                    depth += 1;
                } else if b[i] == close {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            continue;
        }
        if (c as char).is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b':' {
            i += 1;
            continue;
        }
        if c == b'-' && i + 1 < b.len() && b[i + 1] == b'>' {
            i += 2;
            continue;
        }
        if c == b'?' && i + 1 < b.len() && b[i + 1] == b'.' {
            i += 1;
            continue;
        }
        break;
    }
    if i == start {
        start
    } else {
        i
    }
}

/// The comparison operators of a line, written with spaces around them (so that generics such
/// as `Vec<String>` and shifts are not taken for them), and the comparing methods.
fn comparisons(code: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for op in [" <= ", " >= ", " < ", " > "] {
        for (i, _) in code.match_indices(op) {
            out.push((i + 1, i + op.len() - 1));
        }
    }
    for m in [
        ".Before(",
        ".After(",
        ".isBefore(",
        ".isAfter(",
        ".compareTo(",
        ".Compare(",
    ] {
        for (i, _) in code.match_indices(m) {
            out.push((i, i + m.len()));
        }
    }
    out
}

fn has_comparison(code: &str) -> bool {
    !comparisons(code).is_empty() || code.contains("max(") || code.contains("min(")
}

// ---------------------------------------------------------------- the files and their scopes

struct Func {
    name: String,
    start: usize,
    end: usize,
    /// the names of its parameters, in order
    params: Vec<String>,
}

/// The parameter names of a function whose header is in `lines` (its first lines).
fn params_of(lang: Lang, lines: &[String]) -> Vec<String> {
    let text = lines.join(" ");
    let Some(open) = text.find('(') else {
        return Vec::new();
    };
    // Go's receiver comes first: take the parentheses after the name
    let open = if lang == Lang::Go && text.trim_start().starts_with("func (") {
        let after = call_end(&text, open);
        match text[after..].find('(') {
            Some(p) => after + p,
            None => return Vec::new(),
        }
    } else {
        open
    };
    let close = call_end(&text, open);
    let inner_end = if text[..close].ends_with(')') {
        close - 1
    } else {
        close
    };
    let inner = &text[open + 1..inner_end.max(open + 1)];
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in inner.chars() {
        match c {
            '(' | '[' | '<' | '{' => depth += 1,
            ')' | ']' | '>' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    parts.push(cur);
    let last_first = matches!(lang, Lang::C | Lang::Jvm | Lang::CSharp);
    parts
        .iter()
        .map(|p| {
            let p = p.split('=').next().unwrap_or("").trim();
            let words: Vec<String> = p
                .split(|c: char| !is_ident(c))
                .filter(|w| {
                    !w.is_empty() && !matches!(*w, "mut" | "self" | "const" | "final" | "ref")
                })
                .map(String::from)
                .collect();
            let w = if last_first {
                words.last()
            } else {
                words.first()
            };
            w.cloned().unwrap_or_default()
        })
        .collect()
}

struct SrcFile {
    rel: String,
    lang: Lang,
    raw: Vec<String>,
    code: Vec<String>,
    /// the function that holds each line (an index into `funcs`, or None at file level)
    scope: Vec<Option<usize>>,
    funcs: Vec<Func>,
    /// the test functions and modules inside the file, whose lines are left out of `code`
    tests: TestItems,
}

const CONTROL: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "else",
    "do",
    "try",
    "return",
    "synchronized",
    "new",
    "sizeof",
    "defined",
    "elif",
    "match",
    "foreach",
    "using",
    "lock",
    "when",
    "with",
];

/// The name of the function whose header is on `code` (one line), if any.
fn func_header(lang: Lang, code: &str) -> Option<String> {
    let t = code.trim();
    let ident_before_paren = |s: &str| -> Option<String> {
        let p = s.find('(')?;
        let head = s[..p].trim_end();
        let name: String = head
            .chars()
            .rev()
            .take_while(|c| is_ident(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        (!name.is_empty() && !CONTROL.contains(&name.as_str())).then_some(name)
    };
    match lang {
        Lang::Go => {
            let rest = t.strip_prefix("func ")?;
            let rest = if rest.starts_with('(') {
                &rest[call_end(rest, 0)..]
            } else {
                rest
            };
            ident_before_paren(rest.trim_start())
        }
        Lang::Rust => {
            let i = t.find("fn ")?;
            if i > 0 && is_ident(t[..i].chars().next_back()?) {
                return None;
            }
            ident_before_paren(&t[i + 3..])
        }
        Lang::Python | Lang::Ruby => {
            let rest = t
                .strip_prefix("def ")
                .or_else(|| t.strip_prefix("async def "))?;
            let name: String = rest
                .chars()
                .take_while(|c| is_ident(*c) || *c == '.')
                .collect();
            let name = name.rsplit('.').next().unwrap_or("").to_string();
            (!name.is_empty()).then_some(name)
        }
        Lang::C => {
            if code.starts_with(|c: char| c.is_whitespace())
                || t.starts_with('#')
                || t.ends_with(';')
                || t.starts_with("typedef")
            {
                return None;
            }
            let p = t.find('(')?;
            if t[..p].contains('=') || t[..p].split_whitespace().count() < 2 {
                return None;
            }
            ident_before_paren(t)
        }
        Lang::Jvm | Lang::Js | Lang::CSharp => {
            if t.ends_with(';') || !t.contains('(') {
                return None;
            }
            if let Some(rest) = t
                .strip_prefix("function ")
                .or_else(|| t.find(" function ").map(|i| &t[i + 10..]))
            {
                return ident_before_paren(rest);
            }
            if t.contains("=>") {
                let p = t.find('=')?;
                let head = t[..p].trim_end();
                let name: String = head
                    .chars()
                    .rev()
                    .take_while(|c| is_ident(*c))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                return (!name.is_empty()).then_some(name);
            }
            let p = t.find('(')?;
            if t[..p].contains('=') || t[..p].contains('.') {
                return None;
            }
            ident_before_paren(t)
        }
        Lang::Sql => None,
    }
}

fn indent_of(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace()).count()
}

/// The functions of a file and the function of each line.
fn scopes(lang: Lang, code: &[String]) -> (Vec<Option<usize>>, Vec<Func>) {
    let mut scope = vec![None; code.len()];
    let mut funcs: Vec<Func> = Vec::new();
    if lang == Lang::Sql {
        return (scope, funcs);
    }
    if !lang.braces() {
        // Python and Ruby: a function holds the lines indented deeper than its `def`
        let mut stack: Vec<(usize, usize)> = Vec::new(); // (func, indent)
        for (i, l) in code.iter().enumerate() {
            let blank = l.trim().is_empty();
            if !blank {
                let ind = indent_of(l);
                while let Some(&(f, fi)) = stack.last() {
                    if ind <= fi && !(lang == Lang::Ruby && l.trim() == "end" && ind == fi) {
                        funcs[f].end = i.saturating_sub(1);
                        stack.pop();
                    } else {
                        break;
                    }
                }
                if let Some(name) = func_header(lang, l) {
                    let params = params_of(lang, &code[i..(i + 6).min(code.len())]);
                    funcs.push(Func {
                        name,
                        start: i,
                        end: code.len().saturating_sub(1),
                        params,
                    });
                    stack.push((funcs.len() - 1, ind));
                }
            }
            scope[i] = stack.last().map(|x| x.0);
        }
        return (scope, funcs);
    }
    let mut depth = 0i32;
    let mut stack: Vec<(usize, i32)> = Vec::new(); // (func, depth at its opening brace)
    let mut pending: Option<(String, usize)> = None;
    for (i, l) in code.iter().enumerate() {
        if let Some(name) = func_header(lang, l) {
            pending = Some((name, i));
        }
        let before = depth;
        let opens = l.contains('{');
        for c in l.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if let Some((name, start)) = pending.take() {
            if opens {
                let params = params_of(lang, &code[start..=i]);
                funcs.push(Func {
                    name,
                    start,
                    end: i,
                    params,
                });
                stack.push((funcs.len() - 1, before));
            } else if !l.trim_end().ends_with(';') && i < start + 6 {
                pending = Some((name, start));
            }
        }
        scope[i] = stack.last().map(|x| x.0);
        while let Some(&(f, d)) = stack.last() {
            if depth <= d {
                funcs[f].end = i;
                stack.pop();
            } else {
                break;
            }
        }
    }
    for f in &funcs {
        for s in scope.iter_mut().take(f.end + 1).skip(f.start) {
            if s.is_none() {
                *s = funcs.iter().position(|g| g.start == f.start);
            }
        }
    }
    (scope, funcs)
}

// ---------------------------------------------------------------- test code inside a source file

/// A source file as the route scan reads it (Milestone 344): its lines without comments and
/// without test code, with the contents of strings, and its functions as (name, first line,
/// last line).
pub(crate) struct Outline {
    pub code: Vec<String>,
    pub funcs: Vec<(String, usize, usize)>,
    /// every line that starts a function, also one whose parameters take many lines
    pub heads: Vec<(String, usize)>,
    pub tests: TestItems,
}

pub(crate) fn outline(lang: Lang, text: &str) -> Outline {
    let bare = clean(lang, text);
    let mut code = clean_with(lang, text, true);
    let (_, funcs) = scopes(lang, &bare);
    let (mask, tests) = test_lines(lang, &bare, &funcs);
    for (i, t) in mask.iter().enumerate() {
        if *t {
            if let Some(l) = code.get_mut(i) {
                l.clear();
            }
        }
    }
    let funcs = funcs
        .into_iter()
        .filter(|f| !mask.get(f.start).copied().unwrap_or(false))
        .map(|f| (f.name, f.start, f.end))
        .collect();
    let heads = bare
        .iter()
        .enumerate()
        .filter(|(i, _)| !mask.get(*i).copied().unwrap_or(false))
        .filter_map(|(i, l)| func_header(lang, l).map(|n| (n, i)))
        .collect();
    Outline {
        code,
        funcs,
        heads,
        tests,
    }
}

/// What the audit left out of one file as test code (Milestone 324): the functions and the
/// modules or classes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TestItems {
    pub functions: usize,
    pub modules: usize,
}

/// The last line of the item that starts at line `start`: where its braces close, or the line
/// that ends it with `;` before any brace opens. `parens` counts parentheses too (a call such
/// as JavaScript's `describe("...", () => { ... })`).
fn item_end(code: &[String], start: usize, parens: bool) -> usize {
    let mut depth = 0i32;
    let mut opened = false;
    for (i, l) in code.iter().enumerate().skip(start) {
        for c in l.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '(' if parens => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                ')' if parens => depth -= 1,
                _ => {}
            }
        }
        if opened && depth <= 0 {
            return i;
        }
        if !opened && l.trim_end().ends_with(';') {
            return i;
        }
    }
    code.len().saturating_sub(1)
}

/// The last line of a Python or Ruby block that starts at line `start`: the lines indented
/// deeper than it.
fn indent_end(code: &[String], start: usize) -> usize {
    let ind = indent_of(&code[start]);
    let mut end = start;
    for (i, l) in code.iter().enumerate().skip(start + 1) {
        if l.trim().is_empty() {
            continue;
        }
        if indent_of(l) <= ind {
            break;
        }
        end = i;
    }
    end
}

/// The path of a Rust attribute (`tokio::test` of `#[tokio::test(flavor = "...")]`).
fn rust_attr(t: &str) -> Option<&str> {
    let inner = t.strip_prefix("#[")?;
    let end = inner.find(['(', ']', ' ']).unwrap_or(inner.len());
    Some(&inner[..end])
}

/// Whether a Rust attribute marks a test: `#[test]` and the test attributes of other crates
/// (`#[tokio::test]`, `#[rstest]`, `#[test_case(...)]`, `#[bench]`), and `#[cfg(test)]` or
/// `#[cfg(all(test, ...))]` (code that only a test build has; `any(test, ...)` is not).
fn rust_test_attr(t: &str) -> bool {
    let Some(path) = rust_attr(t) else {
        return false;
    };
    let last = path.rsplit("::").next().unwrap_or(path);
    if matches!(
        last,
        "test" | "rstest" | "test_case" | "bench" | "quickcheck" | "proptest"
    ) {
        return true;
    }
    let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    path == "cfg" && (compact.starts_with("#[cfg(test)]") || compact.starts_with("#[cfg(all(test,"))
}

/// The lines of test code in a source file that is not itself a test file, by each language's
/// conventions (Milestone 324: the audit ranked a Rust test function in a `#[cfg(test)]` module
/// first):
///
/// - Rust: an item after `#[cfg(test)]` (a `mod tests { ... }`), and a function after `#[test]`
///   or another crate's test attribute;
/// - Go: `func TestX(t *testing.T)`, `BenchmarkX` and `FuzzX`;
/// - Python and Ruby: `def test_...` and `class Test...`;
/// - Java, Kotlin and C#: a method or class after `@Test` (and JUnit's other test
///   annotations), `[Test]`, `[Fact]`, `[Theory]`, `[TestMethod]` or `[TestFixture]`;
/// - JavaScript and TypeScript: `describe(`, `it(` and `test(` blocks, and Vitest's
///   `if (import.meta.vitest)`;
/// - C and C++: googletest's `TEST(`, `TEST_F(` and `TEST_P(`.
fn test_lines(lang: Lang, code: &[String], funcs: &[Func]) -> (Vec<bool>, TestItems) {
    let mut mask = vec![false; code.len()];
    let mut items = TestItems::default();
    let mark = |mask: &mut Vec<bool>, a: usize, b: usize| {
        for m in mask.iter_mut().take(b + 1).skip(a) {
            *m = true;
        }
    };
    // the first line of the item after the attributes that start at line `i`
    let after_attrs = |i: usize, is_attr: &dyn Fn(&str) -> bool| -> Option<usize> {
        (i..code.len().min(i + 12)).find(|&k| {
            let t = code[k].trim();
            !t.is_empty() && !is_attr(t)
        })
    };
    let mut i = 0;
    while i < code.len() {
        if mask[i] {
            i += 1;
            continue;
        }
        let t = code[i].trim();
        // (start, end, what it counts as: a module or class, a function, or neither)
        let mut range: Option<(usize, usize, Option<bool>)> = None;
        match lang {
            Lang::Rust if rust_test_attr(t) => {
                let rust_is_attr = |s: &str| s.starts_with("#[");
                if let Some(s) = after_attrs(i, &rust_is_attr) {
                    let item = code[s].trim();
                    let words: Vec<&str> = item
                        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .filter(|w| !w.is_empty())
                        .take(4)
                        .collect();
                    let kind = if words.iter().any(|w| matches!(*w, "mod" | "impl")) {
                        Some(true)
                    } else if words.contains(&"fn") {
                        Some(false)
                    } else {
                        None
                    };
                    range = Some((i, item_end(code, s, false), kind));
                }
            }
            Lang::Jvm | Lang::CSharp => {
                let word = t
                    .trim_start_matches(['@', '['])
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
                    .next()
                    .unwrap_or("");
                let last = word.rsplit('.').next().unwrap_or(word);
                let annotated = (t.starts_with('@') || t.starts_with('['))
                    && matches!(
                        last,
                        "Test"
                            | "ParameterizedTest"
                            | "RepeatedTest"
                            | "TestFactory"
                            | "TestTemplate"
                            | "TestMethod"
                            | "Fact"
                            | "Theory"
                            | "TestCase"
                            | "TestFixture"
                            | "TestClass"
                    );
                if annotated {
                    let is_attr = |s: &str| s.starts_with('@') || s.starts_with('[');
                    if let Some(s) = after_attrs(i, &is_attr) {
                        let class = code[s].contains("class ");
                        range = Some((i, item_end(code, s, false), Some(class)));
                    }
                }
            }
            Lang::Js => {
                let call = ["describe(", "it(", "test(", "describe.", "it.", "test."]
                    .iter()
                    .any(|p| t.starts_with(p))
                    || t.starts_with("if (import.meta.vitest)");
                if call {
                    let block = t.starts_with("describe") || t.starts_with("if");
                    range = Some((i, item_end(code, i, true), Some(block)));
                }
            }
            Lang::C => {
                if ["TEST(", "TEST_F(", "TEST_P("]
                    .iter()
                    .any(|p| t.starts_with(p))
                {
                    range = Some((i, item_end(code, i, false), Some(false)));
                }
            }
            Lang::Python | Lang::Ruby => {
                let class = t
                    .strip_prefix("class ")
                    .is_some_and(|r| r.starts_with("Test"));
                if class {
                    range = Some((i, indent_end(code, i), Some(true)));
                }
            }
            _ => {}
        }
        if let Some((a, b, kind)) = range {
            mark(&mut mask, a, b);
            match kind {
                Some(true) => items.modules += 1,
                Some(false) => items.functions += 1,
                None => {}
            }
            i = b + 1;
            continue;
        }
        i += 1;
    }
    // functions by their names: Go's TestX(t *testing.T), Python's and Ruby's def test_x
    for f in funcs {
        if mask[f.start] {
            continue;
        }
        let test = match lang {
            Lang::Go => {
                ["Test", "Benchmark", "Fuzz"]
                    .iter()
                    .any(|p| f.name.starts_with(p))
                    && code[f.start].contains("testing.")
            }
            Lang::Python | Lang::Ruby => f.name.starts_with("test_") || f.name == "test",
            _ => false,
        };
        if test {
            mark(&mut mask, f.start, f.end);
            items.functions += 1;
        }
    }
    (mask, items)
}

// ---------------------------------------------------------------- taint: what holds a reading

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    /// a function or method whose result is a reading
    Call(String),
    /// a field of a struct or an object (by its name, in every file)
    Field(String),
    /// a variable of one function (file, function or None for file level, name)
    Local(usize, Option<usize>, String),
}

#[derive(Clone, Debug)]
struct Origin {
    file: usize,
    line: usize,
    /// the reading it was set from: a source pattern, or another key
    from: Result<&'static str, Key>,
}

#[derive(Default)]
struct Taint {
    map: HashMap<Key, Origin>,
    /// fields that are also set from a monotonic clock: not taken for readings
    mixed: HashSet<String>,
}

/// A wall-clock reading on a line: its span, and how it reads the clock.
#[derive(Clone, Debug)]
struct Span {
    start: usize,
    end: usize,
    /// Go's `time.Now()`, which is a wall reading only against a value that lost its
    /// monotonic reading
    weak: bool,
    from: Result<&'static str, Key>,
    unit: &'static str,
}

const TIME_NAME_WORDS: &[&str] = &[
    "time", "now", "clock", "stamp", "epoch", "date", "utc", "millis",
];

fn spans(f: &SrcFile, fi: usize, i: usize, t: &Taint) -> Vec<Span> {
    let code = &f.code[i];
    let mut out = Vec::new();
    for s in SOURCES.iter().filter(|s| s.langs.contains(&f.lang)) {
        for p in find_all(code, s.pat, false) {
            let mut end = p + s.pat.len();
            if s.pat.ends_with('(') {
                end = call_end(code, end - 1);
            }
            out.push(Span {
                start: p,
                end,
                weak: false,
                from: Ok(s.pat),
                unit: s.unit,
            });
        }
    }
    if f.lang == Lang::Go {
        for w in GO_WEAK {
            for p in find_all(code, w, false) {
                if out.iter().any(|s| s.start == p) {
                    continue;
                }
                let end = if w.ends_with('(') {
                    call_end(code, p + w.len() - 1)
                } else {
                    p + w.len()
                };
                out.push(Span {
                    start: p,
                    end,
                    weak: true,
                    from: Ok(w),
                    unit: "-",
                });
            }
        }
    }
    // names: calls, fields and locals that hold a reading
    let b = code.as_bytes();
    let mut j = 0;
    while j < b.len() {
        if !is_ident(b[j] as char) || (j > 0 && is_ident(b[j - 1] as char)) {
            j += 1;
            continue;
        }
        let mut k = j;
        while k < b.len() && is_ident(b[k] as char) {
            k += 1;
        }
        let name = &code[j..k];
        let before = if j > 0 { b[j - 1] } else { b' ' };
        let dotted = before == b'.' || (before == b'>' && j >= 2 && b[j - 2] == b'-');
        let next = code[k..].chars().next();
        let mut hit: Option<(Key, usize, usize)> = None;
        if next == Some('(') {
            let key = Key::Call(name.to_string());
            if t.map.contains_key(&key) {
                let recv_start = if dotted { operand_left(code, j - 1) } else { j };
                let recv = code[recv_start..j].trim_end_matches(['.', '>', '-']);
                let monotonic_recv = matches!(
                    recv,
                    "time" | "performance" | "Instant" | "process" | "loop"
                );
                if !monotonic_recv {
                    hit = Some((key, recv_start, call_end(code, k)));
                }
            }
        } else if dotted {
            let key = Key::Field(name.to_string());
            if t.map.contains_key(&key) && !t.mixed.contains(name) {
                hit = Some((key, operand_left(code, j - 1), k));
            }
        } else {
            for key in [
                Key::Local(fi, f.scope[i], name.to_string()),
                Key::Local(fi, None, name.to_string()),
            ] {
                if t.map.contains_key(&key) {
                    hit = Some((key, j, k));
                    break;
                }
            }
        }
        if let Some((key, s, e)) = hit {
            if !out.iter().any(|x| x.start <= s && e <= x.end) {
                out.push(Span {
                    start: s,
                    end: e,
                    weak: false,
                    from: Err(key),
                    unit: "-",
                });
            }
        }
        j = k;
    }
    out
}

/// The name an assignment on `code` sets, whether it is a field, whether it declares a new
/// variable, and where its right-hand side starts.
fn assignment(lang: Lang, code: &str) -> Option<(String, bool, bool, usize)> {
    let b = code.as_bytes();
    let mut eq = None;
    for i in 0..b.len() {
        if b[i] != b'=' {
            continue;
        }
        let prev = if i > 0 { b[i - 1] } else { b' ' };
        let next = if i + 1 < b.len() { b[i + 1] } else { b' ' };
        if next == b'='
            || next == b'>'
            || matches!(
                prev,
                b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'|' | b'&' | b'^'
            )
        {
            if prev == b':' {
                eq = Some((i - 1, i + 1));
                break;
            }
            continue;
        }
        eq = Some((if prev == b':' { i - 1 } else { i }, i + 1));
        break;
    }
    let (lhs_end, rhs) = match eq {
        Some(x) => x,
        None => {
            // Go and JavaScript keys of a composite literal: `name: value,`
            if matches!(lang, Lang::Go | Lang::Js) {
                let t = code.trim();
                let c = t.find(": ")?;
                let key = &t[..c];
                if !key.is_empty() && key.chars().all(is_ident) && t.ends_with(',') {
                    let off = code.find(key)? + c + 2;
                    return Some((key.to_string(), true, false, off));
                }
            }
            return None;
        }
    };
    let mut lhs = code[..lhs_end].trim_end();
    // `m[key.Id] = v` sets the map `m`, not `Id`
    while lhs.ends_with(']') {
        let start = operand_left(lhs, lhs.len());
        let b = lhs.as_bytes();
        let mut depth = 0i32;
        let mut open = None;
        for j in (start..lhs.len()).rev() {
            match b[j] {
                b']' => depth += 1,
                b'[' => {
                    depth -= 1;
                    if depth == 0 {
                        open = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        lhs = lhs[..open?].trim_end();
    }
    if lhs.ends_with(')') {
        return None;
    }
    let name: String = lhs
        .chars()
        .rev()
        .take_while(|c| is_ident(*c))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if name.is_empty() || CONTROL.contains(&name.as_str()) {
        return None;
    }
    let head = &lhs[..lhs.len() - name.len()];
    let field = head.ends_with('.') || head.ends_with("->");
    let decl = code[lhs_end..].starts_with(":=")
        || has_any(head, &["let ", "var ", "const ", "auto "])
        || !field && !head.trim().is_empty();
    Some((name, field, decl, rhs))
}

/// Whether the right-hand side `rhs` (from `from`) of a line is a reading: it holds a span that
/// is not weak, it is not the difference of two readings (a duration), and it reads no
/// monotonic clock.
fn rhs_reading<'a>(code: &str, from: usize, sp: &'a [Span]) -> Option<&'a Span> {
    let rhs = &code[from..];
    if has_any(rhs, MONOTONIC) {
        return None;
    }
    let inside: Vec<&Span> = sp.iter().filter(|s| s.start >= from && !s.weak).collect();
    // the value is the reading itself, scaled or cast: not a call that only takes it as an
    // argument (`buf = sdscatprintf(buf, "%ld", now)` is no time)
    let first = starts_with_span(code, from, sp)?;
    if !inside.iter().any(|s| s.start == first.start) {
        return None;
    }
    // a duration: a reading minus a reading, or Go's Sub, Since
    if inside.len() >= 2 && rhs.contains(" - ") {
        return None;
    }
    if has_any(
        rhs,
        &[
            ".Sub(",
            "Since(",
            "Until(",
            ".duration_since(",
            ".elapsed()",
            "Duration.between(",
        ],
    ) {
        let epoch = rhs.contains("UNIX_EPOCH");
        if !epoch {
            return None;
        }
    }
    // a comparison is a boolean, not a time
    if !comparisons(rhs).is_empty() {
        return None;
    }
    Some(first)
}

/// Fields too generic to follow through a whole project: a `tv_sec` or a `start` holds a
/// monotonic time as often as a wall-clock one.
const GENERIC_FIELDS: &[&str] = &[
    "tv_sec",
    "tv_usec",
    "tv_nsec",
    "sec",
    "usec",
    "nsec",
    "seconds",
    "time",
    "value",
    "val",
    "data",
    "start",
    "end",
    "now",
    "when",
    "ts",
    "timestamp",
    "Time",
    "Value",
    "Start",
    "End",
];

/// Whether a reading may pass from `from` to the name `target`. A hop from a variable to a
/// variable keeps a time only when one of the two names speaks of time, so `x.BuildId =
/// y.Version` is not followed. One hop from a local that holds a direct reading is kept
/// (`ust = tv.tv_sec * 1000000` after `gettimeofday(&tv, NULL)`).
fn hop_ok(t: &Taint, from: &Result<&'static str, Key>, target: &str) -> bool {
    match from {
        Err(key @ Key::Local(_, _, src)) => {
            let direct = matches!(
                t.map.get(key).map(|o| &o.from),
                Some(Ok(_) | Err(Key::Call(_)))
            );
            direct || timeish(src) || timeish(target)
        }
        Err(Key::Field(src)) => timeish(src) || timeish(target),
        _ => true,
    }
}

/// Whether a name speaks of a point in time: `lastRefresh`, `repl_ack_time`, `expires_at`.
fn timeish(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    has_any(
        &l,
        &[
            "time",
            "stamp",
            "since",
            "last",
            "expir",
            "deadline",
            "until",
            "clock",
            "now",
            "date",
            "epoch",
            "lease",
            "heartbeat",
            "interaction",
            "when",
            "millis",
            "nanos",
            "micros",
            "seen",
            "start",
            "began",
            "created",
            "updated",
            "modified",
            "fire",
            "sched",
            "wake",
            "ping",
            "pong",
            "alive",
            "refresh",
            "ttl",
        ],
    ) || l.ends_with("_at")
        || name.ends_with("At")
        || l.ends_with("_ts")
        || name.ends_with("Ts")
        || l.ends_with("ms")
        || l.ends_with("_us")
        || l.ends_with("_sec")
}

/// Function names too common to follow a reading into their parameters.
const GENERIC_CALLS: &[&str] = &[
    "Sub", "Add", "Before", "After", "Equal", "Compare", "Since", "Until", "String", "New", "new",
    "Get", "get", "Set", "set", "Update", "update", "Run", "run", "Start", "start", "Stop", "stop",
    "max", "min", "Max", "Min", "append", "len", "add", "sub", "plus", "minus", "push", "insert",
    "put", "Put", "call", "apply", "format", "Format", "parse", "Parse",
];

/// Conversions that keep a time a time: `int64(...)`, `Math.floor(...)`.
const CONVERSIONS: &[&str] = &[
    "int",
    "int64",
    "uint64",
    "int32",
    "uint32",
    "float64",
    "long",
    "double",
    "Number",
    "parseInt",
    "Math.floor",
    "Math.round",
    "Math.trunc",
    "BigInt",
    "time.Duration",
    "float",
    "round",
    "Some",
    "Ok",
];

/// The reading that the expression at `from` starts with, after spaces, parentheses, casts
/// (`(long long)`) and conversions (`int64(`).
fn starts_with_span<'a>(code: &str, from: usize, sp: &'a [Span]) -> Option<&'a Span> {
    let b = code.as_bytes();
    let mut i = from;
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if i < b.len() && b[i] == b'(' {
            // a cast: `(long long)`, `(time_t)`
            let close = call_end(code, i);
            let inner = if close > i + 1 && code[..close].ends_with(')') {
                code[i + 1..close - 1].trim()
            } else {
                ""
            };
            let cast = !inner.is_empty()
                && inner
                    .split_whitespace()
                    .all(|w| w.trim_end_matches('*').chars().all(is_ident))
                && sp.iter().all(|s| s.start < i || s.start >= close);
            if cast {
                i = close;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(c) = CONVERSIONS
            .iter()
            .find(|c| code[i..].starts_with(&format!("{c}(")))
        {
            if !sp.iter().any(|s| s.start == i) {
                i += c.len() + 1;
                continue;
            }
        }
        break;
    }
    sp.iter().find(|s| s.start == i && !s.weak)
}

/// Out-parameters that receive a reading: `gettimeofday(&tv, ...)`.
fn out_param(code: &str) -> Option<String> {
    for pat in [
        "gettimeofday(&",
        "clock_gettime(CLOCK_REALTIME, &",
        "clock_gettime(CLOCK_REALTIME,&",
        "time(&",
    ] {
        if let Some(p) = find_all(code, pat, false).first() {
            let rest = &code[p + pat.len()..];
            let name: String = rest.chars().take_while(|c| is_ident(*c)).collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn propagate(files: &[SrcFile], t: &mut Taint) {
    // the functions of the project by name, to follow a reading into a parameter
    let mut defs: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (fi, f) in files.iter().enumerate() {
        for (k, fu) in f.funcs.iter().enumerate() {
            defs.entry(fu.name.as_str()).or_default().push((fi, k));
        }
    }
    let mut monotonic_twin: HashSet<&str> = HashSet::new();
    for (name, ds) in &defs {
        let monotonic_only = |(fi, k): &(usize, usize)| {
            let f = &files[*fi];
            let fu = &f.funcs[*k];
            let body = &f.code[fu.start..=fu.end.min(f.code.len() - 1)];
            body.iter().any(|l| has_any(l, MONOTONIC))
                && !body.iter().any(|l| {
                    SOURCES
                        .iter()
                        .any(|s| s.langs.contains(&f.lang) && l.contains(s.pat))
                })
        };
        if ds.len() >= 2 && ds.iter().any(monotonic_only) {
            monotonic_twin.insert(name);
        }
    }
    for _round in 0..8 {
        let before = t.map.len() + t.mixed.len();
        for (fi, f) in files.iter().enumerate() {
            if f.lang == Lang::Sql {
                continue;
            }
            for i in 0..f.code.len() {
                let code = &f.code[i];
                if code.trim().is_empty() {
                    continue;
                }
                if let Some(name) = out_param(code) {
                    let key = Key::Local(fi, f.scope[i], name);
                    t.map.entry(key).or_insert(Origin {
                        file: fi,
                        line: i,
                        from: Ok("gettimeofday("),
                    });
                }
                // a reading passed as an argument: the parameter holds it in the called function
                let mut sp_cache: Option<Vec<Span>> = None;
                for (p, _) in code.match_indices('(') {
                    let name_start = code[..p].trim_end_matches(is_ident).len();
                    let name = &code[name_start..p];
                    // a method named like a time method (`Sub`, `Add`) takes readings from
                    // every caller of the time method of the same name: not followed
                    if GENERIC_CALLS.contains(&name) {
                        continue;
                    }
                    let Some(ds) = defs.get(name) else { continue };
                    if ds.len() > 3
                        || ds
                            .iter()
                            .any(|(dfi, k)| *dfi == fi && files[fi].funcs[*k].start == i)
                    {
                        continue;
                    }
                    let close = call_end(code, p);
                    let mut args = Vec::new();
                    let (mut depth, mut s0) = (0i32, p + 1);
                    for (q, c) in code[p..close].char_indices() {
                        let q = p + q;
                        match c {
                            '(' | '[' | '{' => depth += 1,
                            ')' | ']' | '}' => depth -= 1,
                            ',' if depth == 1 => {
                                args.push((s0, q));
                                s0 = q + 1;
                            }
                            _ => {}
                        }
                    }
                    let last = if code[..close].ends_with(')') {
                        close - 1
                    } else {
                        close
                    };
                    args.push((s0, last.max(s0)));
                    let sp = sp_cache.get_or_insert_with(|| spans(f, fi, i, t));
                    for (k, (a0, a1)) in args.iter().enumerate() {
                        let Some(s) = starts_with_span(code, *a0, sp) else {
                            continue;
                        };
                        if s.end > *a1 || has_any(&code[*a0..*a1], MONOTONIC) {
                            continue;
                        }
                        for (dfi, dk) in ds {
                            let fu = &files[*dfi].funcs[*dk];
                            if fu.params.len() == args.len()
                                && fu.params[k].len() >= 2
                                && hop_ok(t, &s.from, &fu.params[k])
                            {
                                let key = Key::Local(*dfi, Some(*dk), fu.params[k].clone());
                                t.map.entry(key).or_insert(Origin {
                                    file: fi,
                                    line: i,
                                    from: s.from.clone(),
                                });
                            }
                        }
                    }
                }
                let Some((name, field, decl, rhs)) = assignment(f.lang, code) else {
                    continue;
                };
                if name.len() < 2 {
                    continue;
                }
                if field && has_any(&code[rhs..], MONOTONIC) {
                    t.mixed.insert(name.clone());
                    continue;
                }
                let sp = spans(f, fi, i, t);
                let Some(s) = rhs_reading(code, rhs, &sp) else {
                    continue;
                };
                let origin = Origin {
                    file: fi,
                    line: i,
                    from: s.from.clone(),
                };
                let _ = decl;
                if !hop_ok(t, &s.from, &name) {
                    continue;
                }
                if field {
                    if name.len() >= 3 && !GENERIC_FIELDS.contains(&name.as_str()) {
                        t.map.entry(Key::Field(name)).or_insert(origin);
                    }
                } else {
                    t.map
                        .entry(Key::Local(fi, f.scope[i], name))
                        .or_insert(origin);
                }
            }
            // small functions whose name speaks of time and that return a reading
            for fu in &f.funcs {
                if fu.end < fu.start || fu.end - fu.start > 30 || fu.name.len() < 3 {
                    continue;
                }
                let lname = fu.name.to_ascii_lowercase();
                if !has_any(&lname, TIME_NAME_WORDS)
                    || t.map.contains_key(&Key::Call(fu.name.clone()))
                {
                    continue;
                }
                // another function of the same name reads a monotonic clock (two
                // implementations of one clock interface): the name is not a reading
                if monotonic_twin.contains(fu.name.as_str()) {
                    continue;
                }
                let last = (fu.start..=fu.end.min(f.code.len() - 1)).rev().find(|k| {
                    !f.code[*k]
                        .trim()
                        .trim_matches(['{', '}', ';'])
                        .trim()
                        .is_empty()
                        && f.code[*k].trim() != "end"
                });
                let mut found = None;
                for i in fu.start..=fu.end.min(f.code.len() - 1) {
                    let code = &f.code[i];
                    let sp = spans(f, fi, i, t);
                    // `return <reading>`, or a last expression that is one (Rust, Ruby)
                    let at = match code.find("return ") {
                        Some(p) => Some(p + 7),
                        None if Some(i) == last
                            && i > fu.start
                            && matches!(f.lang, Lang::Rust | Lang::Ruby) =>
                        {
                            Some(code.len() - code.trim_start().len())
                        }
                        None => None,
                    };
                    if let Some(s) = at.and_then(|p| starts_with_span(code, p, &sp)) {
                        if s.from != Err(Key::Call(fu.name.clone())) {
                            found = Some(Origin {
                                file: fi,
                                line: i,
                                from: s.from.clone(),
                            });
                            break;
                        }
                    }
                }
                if let Some(o) = found {
                    t.map.insert(Key::Call(fu.name.clone()), o);
                }
            }
        }
        if t.map.len() + t.mixed.len() == before {
            break;
        }
    }
}

// ---------------------------------------------------------------- matches

/// One place that measures a duration with the wall clock.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockMatch {
    pub file: String,
    pub line: usize,
    pub lang: &'static str,
    /// the line as written
    pub text: String,
    /// `elapsed` (a reading minus an earlier one, compared with a limit), `deadline` (a reading
    /// plus a duration: a deadline, a lease or a horizon) or `check` (a reading compared with a
    /// deadline)
    pub shape: &'static str,
    pub confidence: &'static str,
    /// the function that holds the line
    pub function: Option<String>,
    /// where the reading comes from, nearest first, in plain words
    pub origin: Vec<String>,
    /// the limit the duration is measured against: its name, its value in seconds when the
    /// code states it, and where
    pub limit: Option<Limit>,
    /// whether the file is in one of the repository's main languages (at least a fifth of its
    /// source files, SQL always): a client library in another language ranks lower
    pub main_lang: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub name: String,
    pub seconds: Option<f64>,
    pub at: Option<String>,
}

fn words_of(l: &str) -> String {
    l.to_ascii_lowercase()
}

/// The unit of a reading, from the nearest name in its chain that states one (`mstime`,
/// `unixtime`, `now_ms`), else from the call that reads the clock.
fn reading_unit(t: &Taint, span: &Span) -> &'static str {
    let mut cur = span.from.clone();
    for _ in 0..10 {
        match cur {
            Ok(pat) => {
                return SOURCES
                    .iter()
                    .find(|s| s.pat == pat)
                    .map(|s| s.unit)
                    .unwrap_or("-")
            }
            Err(key) => {
                let n = match &key {
                    Key::Call(n) | Key::Field(n) | Key::Local(_, _, n) => n.to_ascii_lowercase(),
                };
                if n.contains("mstime")
                    || n.ends_with("ms")
                    || n.contains("millis")
                    || n.ends_with("_msec")
                {
                    return "ms";
                }
                if n.contains("ustime")
                    || n.contains("micros")
                    || n.ends_with("_us")
                    || n.ends_with("usec")
                {
                    return "us";
                }
                if n.contains("unixtime")
                    || n.ends_with("sec")
                    || n.ends_with("secs")
                    || n.ends_with("seconds")
                {
                    return "s";
                }
                match t.map.get(&key) {
                    Some(o) => cur = o.from.clone(),
                    None => return span.unit,
                }
            }
        }
    }
    span.unit
}

fn describe_origin(files: &[SrcFile], t: &Taint, from: &Result<&'static str, Key>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = from.clone();
    let mut seen = HashSet::new();
    for _ in 0..10 {
        match cur {
            Ok(pat) => {
                out.push(format!(
                    "{} reads the wall clock",
                    pat.trim_end_matches('(')
                ));
                break;
            }
            Err(key) => {
                if !seen.insert(key.clone()) {
                    break;
                }
                let Some(o) = t.map.get(&key) else { break };
                let at = format!("{}:{}", files[o.file].rel, o.line + 1);
                out.push(match &key {
                    Key::Call(n) => format!("{n}() returns a wall-clock reading ({at})"),
                    Key::Field(n) => {
                        format!("the field {n} is set from a wall-clock reading ({at})")
                    }
                    Key::Local(_, _, n) => format!("{n} is set from a wall-clock reading ({at})"),
                });
                cur = o.from.clone();
            }
        }
    }
    out
}

fn other_names(code: &str, skip: &[(usize, usize)]) -> Vec<String> {
    let mut out = Vec::new();
    let b = code.as_bytes();
    let mut j = 0;
    while j < b.len() {
        if !is_ident(b[j] as char)
            || (j > 0 && is_ident(b[j - 1] as char))
            || skip.iter().any(|(s, e)| *s <= j && j < *e)
        {
            j += 1;
            continue;
        }
        let mut k = j;
        while k < b.len() && is_ident(b[k] as char) {
            k += 1;
        }
        let n = &code[j..k];
        if !n.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            out.push(n.to_string());
        }
        j = k;
    }
    out
}

/// Whether a name holds a duration or a limit: one of its words starts with a duration word
/// (`repl_diskless_sync_delay`, `sessionRefreshMinInternal`) or is a unit (`age`, `ms`).
/// Whole words, so `PageSize` is not an `age` and `maxAttempts` is not a limit.
fn duration_name(n: &str) -> bool {
    n.len() >= 3
        && name_words(n).iter().any(|w| {
            DURATION_WORDS.iter().any(|d| w.starts_with(d))
                || matches!(
                    w.as_str(),
                    "age"
                        | "ms"
                        | "sec"
                        | "secs"
                        | "seconds"
                        | "millis"
                        | "minute"
                        | "minutes"
                        | "hour"
                        | "hours"
                )
        })
}

/// Matches of one line of code (not SQL).
fn code_line(
    files: &[SrcFile],
    fi: usize,
    i: usize,
    t: &Taint,
) -> Option<(&'static str, &'static str, Span, Vec<String>)> {
    let f = &files[fi];
    let code = &f.code[i];
    let sp = spans(f, fi, i, t);
    if sp.iter().all(|s| s.weak) {
        return None;
    }
    let low = words_of(code);
    if has_any(&low, REPORT_WORDS) {
        return None;
    }
    let strong = |a: usize, b: usize| {
        sp.iter()
            .find(|s| !s.weak && s.start < b && a < s.end)
            .cloned()
    };
    let any = |a: usize, b: usize| sp.iter().find(|s| s.start < b && a < s.end).cloned();
    let cmp_here = has_comparison(code) || code.contains("return ") && has_comparison(code);
    // a later comparison of the variable this line sets
    let cmp_later = || -> bool {
        let Some((name, _, _, _)) = assignment(f.lang, code) else {
            return false;
        };
        for k in i + 1..(i + 13).min(f.code.len()) {
            if f.scope[k] != f.scope[i] {
                break;
            }
            let c = &f.code[k];
            if other_names(c, &[]).contains(&name) && has_comparison(c) {
                return true;
            }
        }
        false
    };
    let skip: Vec<(usize, usize)> = sp.iter().map(|s| (s.start, s.end)).collect();
    let names = other_names(code, &skip);
    let duration_words = names.iter().any(|n| duration_name(n));
    let b = code.as_bytes();
    // elapsed: a binary minus with a reading on one side
    for (p, _) in code.match_indices('-') {
        let next = b.get(p + 1).copied().unwrap_or(b' ');
        let prev = if p > 0 { b[p - 1] } else { b' ' };
        if matches!(next, b'>' | b'-' | b'=') || prev == b'-' {
            continue;
        }
        let ls = operand_left(code, p);
        if ls == p || code[ls..p].trim().is_empty() {
            continue;
        }
        let re = operand_right(code, p + 1);
        let (l, r) = (
            strong(ls, p).or_else(|| any(ls, p)),
            strong(p + 1, re).or_else(|| any(p + 1, re)),
        );
        let rtext = &code[p + 1..re];
        let ltext = &code[ls..p];
        // `LLONG_MAX - now`: a guard against an overflow, not a duration
        let limit_const = |s: &str| {
            let s = s.trim();
            (s.contains("MAX") || s.contains("MIN"))
                && s.chars().all(|c| {
                    c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '.' || c == ':'
                })
                || s.contains("MaxInt")
                || s.contains("MinInt")
                || s.contains("MAX_VALUE")
        };
        if limit_const(ltext) || limit_const(rtext) {
            continue;
        }
        let pick = match (&l, &r) {
            (Some(a), Some(b)) if !(a.weak && b.weak) => {
                Some(if a.weak { b.clone() } else { a.clone() })
            }
            (Some(a), None) if !a.weak => {
                // a reading minus a duration: a horizon (`now - ttl`), a deadline in the past
                if duration_name(rtext.trim())
                    || rtext
                        .trim()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit())
                    || rtext.contains("interval")
                {
                    if has_any(&low, EXPIRY_WORDS) || duration_name(rtext.trim()) {
                        return Some((
                            "deadline",
                            if has_any(&low, EXPIRY_WORDS) {
                                "high"
                            } else {
                                "medium"
                            },
                            a.clone(),
                            names,
                        ));
                    }
                    None
                } else {
                    Some(a.clone())
                }
            }
            (None, Some(b)) if !b.weak && !ltext.trim().is_empty() => Some(b.clone()),
            _ => None,
        };
        if let Some(s) = pick {
            let conf = if cmp_here || cmp_later() {
                "high"
            } else if duration_words || duration_name(ltext) {
                "medium"
            } else {
                continue;
            };
            return Some(("elapsed", conf, s, names));
        }
    }
    // Go's Sub, Since, Until; Rust's duration_since, elapsed; Java's Duration.between
    for m in [
        ".Sub(",
        "time.Since(",
        "time.Until(",
        ".duration_since(",
        ".elapsed()",
        ".signed_duration_since(",
        "Duration.between(",
        "ChronoUnit.",
    ] {
        for (p, _) in code.match_indices(m) {
            let open = p + m.len() - 1;
            let (arg_s, arg_e) = if m.ends_with('(') {
                (open, call_end(code, open))
            } else {
                (p, p)
            };
            if code[arg_s..arg_e].contains("UNIX_EPOCH") {
                continue;
            }
            let recv = if m.starts_with('.') {
                strong(operand_left(code, p), p).or_else(|| any(operand_left(code, p), p))
            } else {
                None
            };
            let arg = if arg_e > arg_s {
                strong(arg_s, arg_e).or_else(|| any(arg_s, arg_e))
            } else {
                None
            };
            let pick = match (&recv, &arg) {
                (Some(a), Some(b)) if !(a.weak && b.weak) => {
                    Some(if a.weak { b.clone() } else { a.clone() })
                }
                (Some(a), None) if !a.weak && m.starts_with('.') && m != ".Sub(" => Some(a.clone()),
                (Some(a), None) if !a.weak && m == ".Sub(" => Some(a.clone()),
                (None, Some(b)) if !b.weak => Some(b.clone()),
                _ => None,
            };
            if let Some(s) = pick {
                let conf = if cmp_here || cmp_later() {
                    "high"
                } else if duration_words {
                    "medium"
                } else {
                    continue;
                };
                return Some(("elapsed", conf, s, names));
            }
        }
    }
    // deadline: a reading plus a duration (`now + ttl`, Go's Add, Java's plus)
    let expiry_line = has_any(&low, EXPIRY_WORDS);
    for (p, _) in code.match_indices('+') {
        let next = b.get(p + 1).copied().unwrap_or(b' ');
        let prev = if p > 0 { b[p - 1] } else { b' ' };
        if matches!(next, b'+' | b'=') || prev == b'+' {
            continue;
        }
        let ls = operand_left(code, p);
        if ls == p {
            continue;
        }
        let re = operand_right(code, p + 1);
        let l = strong(ls, p);
        let r = strong(p + 1, re);
        let other = if l.is_some() {
            &code[p + 1..re]
        } else {
            &code[ls..p]
        };
        if let Some(s) = l.or(r) {
            if strong(ls, p).is_some() && strong(p + 1, re).is_some() {
                continue;
            }
            let named = duration_name(other.trim())
                || other.contains("timedelta")
                || other.contains("Duration");
            let number = other
                .trim()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
            // `now + ttl` is a deadline; `now + 1000` only on a line that names one
            if expiry_line && (named || number) {
                return Some(("deadline", "high", s, names));
            }
            if named {
                return Some(("deadline", "medium", s, names));
            }
            continue;
        }
    }
    for m in [
        ".Add(",
        ".plus(",
        ".plusSeconds(",
        ".plusMillis(",
        ".plusMinutes(",
        ".minus(",
        ".minusSeconds(",
        ".minusMillis(",
        ".AddSeconds(",
        ".AddMilliseconds(",
        ".AddMinutes(",
    ] {
        for (p, _) in code.match_indices(m) {
            if let Some(s) = strong(operand_left(code, p), p) {
                if expiry_line {
                    return Some(("deadline", "high", s, names));
                }
                return Some(("deadline", "medium", s, names));
            }
        }
    }
    // check: a reading compared with a deadline
    for (a, e) in comparisons(code) {
        let method = code[a..e].starts_with('.');
        let (ls, le, rs, re) = if method {
            (operand_left(code, a), a, e - 1, call_end(code, e - 1))
        } else {
            (operand_left(code, a), a, e, operand_right(code, e))
        };
        let l = any(ls, le);
        let r = any(rs, re);
        let (s, other) = match (&l, &r) {
            (Some(x), Some(y)) if !(x.weak && y.weak) => {
                (if x.weak { y.clone() } else { x.clone() }, String::new())
            }
            (Some(x), None) if !x.weak => (x.clone(), code[rs..re].to_string()),
            (None, Some(y)) if !y.weak => (y.clone(), code[ls..le].to_string()),
            _ => continue,
        };
        let ol = other.to_ascii_lowercase();
        if !other.is_empty() && !has_any(&ol, EXPIRY_WORDS) {
            continue;
        }
        let conf = if has_any(&ol, LEASE_WORDS) || (other.is_empty() && has_any(&low, LEASE_WORDS))
        {
            "high"
        } else {
            "medium"
        };
        if other.is_empty() && !has_any(&low, EXPIRY_WORDS) {
            continue;
        }
        return Some(("check", conf, s, names));
    }
    None
}

/// The limit of a line of SQL: an `interval '15 seconds'` literal, or a parameter whose name
/// is a duration (`@ttl`, `:lease_seconds`).
fn sql_limit(line: &str) -> Option<Limit> {
    let low = line.to_ascii_lowercase();
    if let Some(p) = low.find("interval '") {
        let lit: String = low[p + 10..].chars().take_while(|c| *c != '\'').collect();
        let mut it = lit.split_whitespace();
        if let (Some(n), Some(u)) = (it.next().and_then(|n| n.parse::<f64>().ok()), it.next()) {
            let k = if u.starts_with("ms") || u.starts_with("milli") {
                1e-3
            } else if u.starts_with("min") {
                60.0
            } else if u.starts_with("hour") {
                3600.0
            } else if u.starts_with("day") {
                86400.0
            } else {
                1.0
            };
            return Some(Limit {
                name: format!("interval '{lit}'"),
                seconds: Some(n * k),
                at: None,
            });
        }
    }
    for (p, _) in line.match_indices(['@', ':', '$']) {
        let name: String = line[p + 1..].chars().take_while(|c| is_ident(*c)).collect();
        if duration_name(&name) {
            return Some(Limit {
                name,
                seconds: None,
                at: None,
            });
        }
    }
    None
}

fn sql_like(low: &str) -> bool {
    has_any(
        low,
        &[
            "where ",
            " set ",
            "values",
            "select ",
            "update ",
            "insert ",
            "delete ",
            " and ",
            "interval",
            "returning",
            " or ",
        ],
    )
}

/// Matches of one line read as SQL: the database server's clock.
fn sql_line(
    raw: &str,
    ctx_before: &[String],
) -> Option<(&'static str, &'static str, &'static str)> {
    let low = raw.to_ascii_lowercase();
    let mut at = None;
    for n in SQL_NOW {
        if let Some(p) = find_all(&low, n, true).first() {
            at = Some((*p, *n));
            break;
        }
    }
    let (p, n) = at?;
    let after = &low[p + n.len()..];
    let before = &low[..p];
    // `now() + interval '15 seconds'`, `now() + make_interval(...)`, `datetime('now', '+15 seconds')`
    let plus_dur = {
        let a = after.trim_start().trim_start_matches(')').trim_start();
        let a2 = after.split(')').next().unwrap_or("");
        (a.starts_with('+') || a.starts_with('-'))
            && has_any(
                a,
                &[
                    "interval", "@", "$", ":", "?", "second", "minute", "hour", "day", "ttl",
                    "lease", "timeout",
                ],
            )
            || (n.starts_with("datetime(")
                && (a2.contains("'+")
                    || a2.contains("'-")
                    || a2.contains("@ttl")
                    || a2.contains("as text")))
            || after.contains("make_interval")
            || has_any(before, &["date_add(", "dateadd(", "timestampadd("])
    };
    let context = || {
        ctx_before
            .iter()
            .any(|l| has_any(&l.to_ascii_lowercase(), EXPIRY_WORDS))
    };
    if plus_dur {
        let conf = if has_any(&low, EXPIRY_WORDS) || context() {
            "high"
        } else {
            "medium"
        };
        return Some(("deadline", conf, n));
    }
    // `expires_at < now()`, `now() > locked_until`
    let cmp = ["<=", ">=", "<", ">"].iter().any(|c| low.contains(c));
    if cmp && has_any(&low, EXPIRY_WORDS) {
        let conf = if has_any(&low, LEASE_WORDS) {
            "high"
        } else {
            "medium"
        };
        return Some(("check", conf, n));
    }
    None
}

// ---------------------------------------------------------------- the limit and its value

/// The value in seconds of a duration written as `expr`, with `unit` for a bare number (`s`,
/// `ms`, `us`, `ns`).
pub fn duration_value(expr: &str, unit: &str) -> Option<f64> {
    let e = expr.trim().trim_end_matches([',', ';', ')']).trim();
    let go_units = [
        ("time.Nanosecond", 1e-9),
        ("time.Microsecond", 1e-6),
        ("time.Millisecond", 1e-3),
        ("time.Second", 1.0),
        ("time.Minute", 60.0),
        ("time.Hour", 3600.0),
    ];
    for (u, s) in go_units {
        if e.contains(u) {
            let rest = e.replace(u, "").replace(['*', '(', ')', ' '], "");
            let n = if rest.is_empty() {
                1.0
            } else {
                rest.replace('_', "").parse::<f64>().ok()?
            };
            return Some(n * s);
        }
    }
    let calls = [
        ("from_secs(", 1.0),
        ("from_millis(", 1e-3),
        ("from_mins(", 60.0),
        ("ofSeconds(", 1.0),
        ("ofMillis(", 1e-3),
        ("ofMinutes(", 60.0),
        ("seconds=", 1.0),
        ("minutes=", 60.0),
        ("hours=", 3600.0),
        ("milliseconds=", 1e-3),
    ];
    for (c, s) in calls {
        if let Some(p) = e.find(c) {
            let n: String = e[p + c.len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '_')
                .collect();
            return n.replace('_', "").parse::<f64>().ok().map(|n| n * s);
        }
    }
    // a product of numbers: `60 * 1000`, `5`
    let mut v = 1.0;
    let mut any = false;
    for part in e.split('*') {
        let p = part
            .trim()
            .trim_end_matches(['L', 'l', 'U', 'u'])
            .replace('_', "");
        if p.is_empty() {
            return None;
        }
        v *= p.parse::<f64>().ok()?;
        any = true;
    }
    if !any {
        return None;
    }
    Some(match unit {
        "ms" => v / 1e3,
        "us" => v / 1e6,
        "ns" => v / 1e9,
        _ => v,
    })
}

fn unit_of_name(name: &str, default: &str) -> &'static str {
    let l = name.to_ascii_lowercase();
    if l.ends_with("ms") || l.contains("millis") || l.ends_with("_msec") {
        "ms"
    } else if l.ends_with("_us") || l.contains("micros") || l.ends_with("usec") {
        "us"
    } else if l.ends_with("_ns") || l.contains("nanos") {
        "ns"
    } else if l.ends_with("sec") || l.ends_with("secs") || l.ends_with("seconds") {
        "s"
    } else {
        match default {
            "ms" => "ms",
            "us" => "us",
            "ns" => "ns",
            _ => "s",
        }
    }
}

/// The value of `name` where the code defines it: an assignment or a constant, a `#define`, or
/// a default in a table of configuration settings.
fn read_limit(files: &[SrcFile], first: usize, name: &str, unit: &str) -> Option<(f64, String)> {
    let unit = unit_of_name(name, unit);
    for f in std::iter::once(&files[first]).chain(files.iter()) {
        for (i, code) in f.code.iter().enumerate() {
            if !code.contains(name) {
                continue;
            }
            for p in find_all(code, name, false) {
                let after = &code[p + name.len()..];
                if after.starts_with(|c: char| is_ident(c)) {
                    continue;
                }
                let t = after.trim_start();
                // `#define NAME 5`
                if code.trim_start().starts_with("#define") {
                    if let Some(v) = duration_value(t, unit) {
                        return Some((v, format!("{}:{}", f.rel, i + 1)));
                    }
                }
                // `name = 5 * time.Second`, `name := ...`, `name time.Duration = ...`, `name: ...,`
                let rhs = if let Some(r) = t.strip_prefix(":=") {
                    Some(r)
                } else if let Some(r) = t.strip_prefix(':') {
                    (!r.starts_with(':'))
                        .then_some(r)
                        .map(|r| r.split_once('=').map(|x| x.1).unwrap_or(r))
                } else if let Some(r) = t.strip_prefix('=') {
                    (!r.starts_with('=')).then_some(r)
                } else if let Some((ty, r)) = t.split_once('=') {
                    (!ty.contains('(')
                        && !ty.contains(',')
                        && !r.starts_with('=')
                        && ty.split_whitespace().count() <= 2)
                        .then_some(r)
                } else {
                    None
                };
                if let Some(r) = rhs {
                    if let Some(v) = duration_value(r.split(" //").next().unwrap_or(r), unit) {
                        return Some((v, format!("{}:{}", f.rel, i + 1)));
                    }
                }
                // a default in a configuration table: `..., server.name, 5, ...`
                let low = code.to_ascii_lowercase();
                if low.contains("config") {
                    if let Some(r) = t.strip_prefix(',') {
                        let n = r.split(',').next().unwrap_or("").trim();
                        if let Ok(v) = n.parse::<f64>() {
                            return Some((
                                match unit {
                                    "ms" => v / 1e3,
                                    "us" => v / 1e6,
                                    _ => v,
                                },
                                format!("{}:{}", f.rel, i + 1),
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

fn find_limit(
    files: &[SrcFile],
    fi: usize,
    i: usize,
    names: &[String],
    unit: &str,
) -> Option<Limit> {
    let f = &files[fi];
    // a variable the function computes (`idle`, `max_idle`) is the measured time, not the limit
    let mut computed: HashSet<String> = HashSet::new();
    if let Some(fu) = f.scope[i].map(|s| &f.funcs[s]) {
        for k in fu.start..=fu.end.min(f.code.len() - 1) {
            if let Some((n, false, _, _)) = assignment(f.lang, &f.code[k]) {
                computed.insert(n);
            }
        }
    }
    let mut cands: Vec<String> = names
        .iter()
        .filter(|n| duration_name(n) && !computed.contains(*n))
        .cloned()
        .collect();
    // the lines of the same function, nearest first
    if let Some(fu) = f.scope[i].map(|s| &f.funcs[s]) {
        let mut ks: Vec<usize> = (fu.start..=fu.end.min(f.code.len() - 1)).collect();
        ks.sort_by_key(|k| (*k as i64 - i as i64).abs());
        for k in ks.into_iter().take(60) {
            if has_comparison(&f.code[k]) || k == i {
                for n in other_names(&f.code[k], &[]) {
                    if duration_name(&n) && !cands.contains(&n) && !computed.contains(&n) {
                        cands.push(n);
                    }
                }
            }
        }
    }
    let mut first = None;
    for n in &cands {
        if let Some((v, at)) = read_limit(files, fi, n, unit) {
            if v > 0.0 {
                return Some(Limit {
                    name: n.clone(),
                    seconds: Some(v),
                    at: Some(at),
                });
            }
        }
        if first.is_none() {
            first = Some(n.clone());
        }
    }
    first.map(|n| Limit {
        name: n,
        seconds: None,
        at: None,
    })
}

// ---------------------------------------------------------------- the audit

/// Audits every source file under `root`: the matches in path order, and how many files were read.
/// What the audit read and found.
#[derive(Clone, Debug, Default)]
pub struct Audit {
    /// The source files read.
    pub read: usize,
    pub matches: Vec<ClockMatch>,
    /// The test code left out of those files (Milestone 324).
    pub tests: TestItems,
}

pub fn audit(root: &Path) -> Audit {
    let mut paths = Vec::new();
    crate::rules::walk(root, &mut paths);
    let mut files: Vec<SrcFile> = Vec::new();
    for p in paths {
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let Some(lang) = lang_of(&rel) else { continue };
        if skipped(&rel.to_ascii_lowercase()) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        if meta.len() > 1_500_000 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if generated(&text) {
            continue;
        }
        files.push(source_file(rel, lang, &text));
    }
    let mut tests = TestItems::default();
    for f in &files {
        tests.functions += f.tests.functions;
        tests.modules += f.tests.modules;
    }
    Audit {
        read: files.len(),
        matches: audit_files(&files),
        tests,
    }
}

fn source_file(rel: String, lang: Lang, text: &str) -> SrcFile {
    let mut code = clean(lang, text);
    let raw: Vec<String> = text
        .split('\n')
        .map(|s| s.trim_end_matches('\r').to_string())
        .collect();
    let (scope, funcs) = scopes(lang, &code);
    // test code inside the file is not read: neither a match nor a reading (Milestone 324)
    let (mask, tests) = test_lines(lang, &code, &funcs);
    for (l, t) in code.iter_mut().zip(&mask) {
        if *t {
            l.clear();
        }
    }
    SrcFile {
        rel,
        lang,
        raw,
        code,
        scope,
        funcs,
        tests,
    }
}

/// Audits files given as (path, text): the unit tests' entry.
#[cfg(test)]
pub fn audit_texts(texts: &[(&str, &str)]) -> Vec<ClockMatch> {
    let files: Vec<SrcFile> = texts
        .iter()
        .filter_map(|(rel, text)| lang_of(rel).map(|l| source_file(rel.to_string(), l, text)))
        .collect();
    audit_files(&files)
}

fn audit_files(files: &[SrcFile]) -> Vec<ClockMatch> {
    let mut t = Taint::default();
    propagate(files, &mut t);
    let mut out = Vec::new();
    // the main languages: at least a fifth of the source files that are not SQL
    let code_files = files.iter().filter(|f| f.lang != Lang::Sql).count().max(1);
    let main: HashSet<Lang> = files
        .iter()
        .map(|f| f.lang)
        .filter(|l| {
            *l != Lang::Sql && files.iter().filter(|f| f.lang == *l).count() * 5 >= code_files
        })
        .collect();
    for (fi, f) in files.iter().enumerate() {
        for i in 0..f.code.len() {
            let raw = f.raw.get(i).cloned().unwrap_or_default();
            let text: String = raw.trim().chars().take(200).collect();
            let function = f.scope[i].map(|s| f.funcs[s].name.clone());
            // SQL: a .sql file, or a line of SQL in another language's string
            let low_raw = raw.to_ascii_lowercase();
            let sql = if f.lang == Lang::Sql {
                Some(f.code[i].clone())
            } else if sql_like(&low_raw)
                && has_any(
                    &low_raw,
                    &[
                        "now()",
                        "current_timestamp",
                        "datetime('now'",
                        "unixepoch()",
                        "clock_timestamp()",
                        "utc_timestamp",
                        "getdate()",
                    ],
                )
            {
                Some(raw.clone())
            } else {
                None
            };
            if let Some(line) = sql {
                let ctx: Vec<String> = (i.saturating_sub(10)..i)
                    .map(|k| f.code[k].clone())
                    .collect();
                if let Some((shape, conf, n)) = sql_line(&line, &ctx) {
                    let limit = sql_limit(&line);
                    out.push(ClockMatch {
                        file: f.rel.clone(),
                        line: i + 1,
                        lang: "sql",
                        text,
                        shape,
                        confidence: conf,
                        function,
                        main_lang: f.lang == Lang::Sql || main.contains(&f.lang),
                        origin: vec![format!(
                            "{} reads the database server's wall clock",
                            n.trim_end_matches('(')
                        )],
                        limit,
                    });
                    continue;
                }
                if f.lang == Lang::Sql {
                    continue;
                }
            }
            if let Some((shape, conf, span, names)) = code_line(files, fi, i, &t) {
                let limit = find_limit(files, fi, i, &names, reading_unit(&t, &span));
                out.push(ClockMatch {
                    file: f.rel.clone(),
                    line: i + 1,
                    lang: f.lang.name(),
                    text,
                    shape,
                    confidence: conf,
                    function,
                    origin: describe_origin(files, &t, &span.from),
                    main_lang: main.contains(&f.lang),
                    limit,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------- the schedules

/// The services of a project, from its hydo.yaml, to name in the schedules.
#[derive(Clone, Debug, Default)]
pub struct Services {
    pub names: Vec<String>,
    /// services whose command makes them a replica (`--replicaof`, `follower`)
    pub replicas: Vec<String>,
    /// services that are a database
    pub databases: Vec<String>,
    /// groups with a `primary:` command
    pub primary_groups: Vec<String>,
    pub steps: Option<u64>,
    /// the words of the services' commands and environments that name a feature they use
    /// (`replicaof` and `repl` for Valkey's replicas, `sql` for a SQL store), to rank the code
    /// that those services run first
    pub words: HashSet<String>,
    /// which code of the tree the services and the workload run (Milestone 324)
    pub map: crate::clockmap::Mapping,
}

/// Words of a command line that name no feature.
const PLAIN_WORDS: &[&str] = &[
    "server",
    "client",
    "data",
    "true",
    "false",
    "enabled",
    "disabled",
    "host",
    "port",
    "bind",
    "config",
    "conf",
    "file",
    "path",
    "name",
    "user",
    "password",
    "level",
    "info",
    "debug",
    "start",
    "exec",
    "bash",
    "always",
    "never",
    "yaml",
    "json",
    "size",
    "addr",
    "address",
    "announce",
    "local",
    "localhost",
    "default",
    "mode",
    "home",
    "root",
    "tmp",
    "etc",
    "usr",
    "bin",
    "opt",
    "var",
    "image",
    "dir",
];

/// Reads the services of a hydo.yaml.
pub fn services_of(text: &str, name: &str) -> Services {
    let mut s = Services::default();
    let Ok(v) = hydo_manifest::yaml::parse_yaml(text, name) else {
        return s;
    };
    // the project's own name is in every path of its code: it names no feature
    let project: HashSet<String> = name_words(v.get("name").and_then(|x| x.as_str()).unwrap_or(""))
        .into_iter()
        .collect();
    for svc in v
        .get("services")
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
    {
        let Some(n) = svc.get("name").and_then(|x| x.as_str()) else {
            continue;
        };
        s.names.push(n.to_string());
        let cmd = svc
            .get("cmd")
            .map(|c| c.to_string())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let image = format!(
            "{} {}",
            svc.get("image_ref")
                .map(|c| c.to_string())
                .unwrap_or_default(),
            svc.get("image").map(|c| c.to_string()).unwrap_or_default()
        )
        .to_ascii_lowercase();
        let env = svc.get("env").map(|c| c.to_string()).unwrap_or_default();
        let image_words: HashSet<String> = name_words(&image).into_iter().collect();
        for w in name_words(&format!("{cmd} {env}")) {
            if w.len() >= 3
                && !w.chars().all(|c| c.is_ascii_digit())
                && !PLAIN_WORDS.contains(&w.as_str())
                && !image_words.contains(&w)
                && !project.contains(&w)
            {
                s.words.insert(w);
            }
        }
        if has_any(
            &cmd,
            &["replicaof", "slaveof", "--replica", "follower", "standby"],
        ) {
            s.replicas.push(n.to_string());
        }
        let ln = n.to_ascii_lowercase();
        if has_any(
            &image,
            &[
                "postgres",
                "mysql",
                "mariadb",
                "mongo",
                "cockroach",
                "yugabyte",
            ],
        ) || has_any(&ln, &["postgres", "mysql", "mariadb", "mongo"])
            || ln == "db"
            || ln == "database"
        {
            s.databases.push(n.to_string());
        }
    }
    for g in v
        .get("groups")
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
    {
        if g.get("primary").is_some() {
            if let Some(n) = g.get("name").and_then(|x| x.as_str()) {
                s.primary_groups.push(n.to_string());
            }
        }
    }
    s.steps = v
        .get("workload")
        .and_then(|w| w.get("steps"))
        .and_then(|x| {
            x.as_u64()
                .or_else(|| x.as_str().and_then(|t| t.parse().ok()))
        });
    s
}

/// `a`, `a and b`, `a, b and c`.
pub fn and_words(v: &[String]) -> String {
    match v {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The services of hydo.yaml that run a place's code, without the workload (None: the map
/// cannot tell).
fn services_running(m: &ClockMatch, svcs: &Services) -> Option<Vec<String>> {
    svcs.map.run_by(&m.file).map(|v| {
        v.into_iter()
            .filter(|w| w != crate::clockmap::WORKLOAD)
            .collect()
    })
}

/// Why a place's schedules keep `<service>` or `<leader>` (Milestone 324), when hydo.yaml was
/// read: the map cannot tell who runs the file, no service runs it, or several do.
fn placeholder_why(m: &ClockMatch, svcs: &Services) -> Option<String> {
    if svcs.names.is_empty() {
        return None;
    }
    let (_, ps) = proposals(m, svcs);
    let ph = ps
        .iter()
        .find_map(|p| {
            let i = p.schedule.find('<')?;
            let j = p.schedule[i..].find('>')?;
            Some(p.schedule[i..=i + j].to_string())
        })
        .filter(|t| t == "<service>" || t == "<leader>")?;
    Some(match services_running(m, svcs) {
        None if svcs.map.programs.is_empty() => format!(
            "{ph}: the audit cannot tell which service runs {} (it maps the programs of Cargo \
             packages and Go modules only); name the service of hydo.yaml that runs it",
            m.file
        ),
        None => format!(
            "{ph}: the audit cannot tell which service runs {}, because no service of hydo.yaml \
             runs a program that these sources build; name the service that runs this code",
            m.file
        ),
        Some(v) if v.is_empty() => {
            format!("{ph}: no service of hydo.yaml runs this code, so no fault of one makes it run")
        }
        Some(v) => format!(
            "{ph}: {} run this code ({}): name one of them",
            and_words(&v),
            svcs.map.programs_of(&m.file).join(", ")
        ),
    })
}

/// One proposed schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub name: &'static str,
    pub schedule: String,
    pub says: String,
}

/// Which fault makes the code run, by the words of its function, file and line: a placeholder
/// or the service it names, the token, and the words for it.
/// The words of names: `shouldStartChildReplication` and `repl_ack_time` give `should`,
/// `start`, `child`, `replication`, `repl`, `ack`, `time`.
fn name_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The roles of the fault that makes a place run, in the order they are tried: a word that
/// starts with one of the prefixes, or equals one of the exact words.
const ROLES: &[(&str, &[&str], &[&str])] = &[
    (
        "leader",
        &[
            "lease",
            "leader",
            "election",
            "elector",
            "elected",
            "reelect",
            "heartbeat",
            "fence",
            "owner",
            "claim",
        ],
        &["lock", "locks", "locked", "elect"],
    ),
    (
        "replica",
        &["replica", "replication", "follower", "slave", "standby"],
        &["repl", "psync", "fullsync", "sync", "primary", "master"],
    ),
    (
        "database",
        &[
            "database",
            "persistence",
            "postgres",
            "mysql",
            "reconnect",
            "connection",
        ],
        &[
            "db", "sql", "conn", "pool", "session", "sessions", "handle", "dbhandle",
        ],
    ),
    (
        "answer",
        &[
            "timeout", "deadline", "request", "retry", "retries", "visib", "inflight", "pending",
            "deliver", "response",
        ],
        &["rpc", "ack", "acked", "acks"],
    ),
];

fn role_of(m: &ClockMatch) -> &'static str {
    let words = name_words(&format!(
        "{} {} {}",
        m.function.clone().unwrap_or_default(),
        m.file,
        m.text
    ));
    for (role, prefixes, exact) in ROLES {
        if words
            .iter()
            .any(|w| prefixes.iter().any(|p| w.starts_with(p)) || exact.contains(&w.as_str()))
        {
            return role;
        }
    }
    "service"
}

fn fault_for(m: &ClockMatch, svcs: &Services) -> (&'static str, String, &'static str) {
    let one = |v: &[String]| v.last().cloned();
    // the one service that runs the place's code (Milestone 324)
    let only = services_running(m, svcs)
        .filter(|v| v.len() == 1)
        .map(|v| v[0].clone());
    match role_of(m) {
        "leader" => {
            let who = svcs.primary_groups.first().map(|g| format!("{g}.primary")).or(only);
            ("leader", who.unwrap_or_else(|| "<leader>".into()), "the process that holds the lease or lock is killed and comes back, so that another one takes over")
        }
        "replica" => ("replica", one(&svcs.replicas).unwrap_or_else(|| "<replica>".into()), "a replica is killed and comes back, so that it connects again and asks for its data"),
        "database" => {
            let db = (svcs.databases.len() == 1).then(|| svcs.databases[0].clone());
            ("database", db.unwrap_or_else(|| "<database>".into()), "the database is killed and comes back, so that its clients connect again")
        }
        "answer" => ("answer", only.unwrap_or_else(|| "<service>".into()), "answers from the service are lost for a while, so that requests time out and are retried"),
        _ => ("service", only.unwrap_or_else(|| "<service>".into()), "the service is killed and comes back, so that this code runs again"),
    }
}

fn first_above(min: f64, steps: &[u64]) -> u64 {
    steps
        .iter()
        .copied()
        .find(|s| *s as f64 > min)
        .unwrap_or(*steps.last().unwrap_or(&300))
}

/// A number of seconds in words: `5 minutes`, `1 hour`, `30 seconds`.
pub fn seconds_words(s: u64) -> String {
    let (n, u) = if s.is_multiple_of(86400) {
        (s / 86400, "day")
    } else if s.is_multiple_of(3600) {
        (s / 3600, "hour")
    } else if s.is_multiple_of(60) {
        (s / 60, "minute")
    } else {
        (s, "second")
    };
    format!("{n} {u}{}", if n == 1 { "" } else { "s" })
}

fn limit_words(s: f64) -> String {
    if s >= 1.0 && s.fract() == 0.0 {
        seconds_words(s as u64)
    } else if s >= 1.0 {
        format!("{s} seconds")
    } else {
        format!("{} ms", (s * 1000.0).round())
    }
}

/// The step sizes of a match: back (longer than the limit, at least 5 minutes) and forward
/// (longer than the limit, so that a lease or timeout ends at once).
pub fn step_sizes(limit: Option<f64>) -> (u64, u64) {
    match limit {
        Some(l) => (
            first_above((10.0 * l).max(299.0), &[300, 3600, 86400, 2_592_000]),
            first_above(1.5 * l, &[30, 300, 3600, 86400, 2_592_000]),
        ),
        None => (300, 300),
    }
}

/// The schedules aimed at a match: a step back before the fault, a step back after it, and a
/// step forward.
pub fn proposals(m: &ClockMatch, svcs: &Services) -> (String, Vec<Proposal>) {
    let (role, who, why) = fault_for(m, svcs);
    let (back, fwd) = step_sizes(m.limit.as_ref().and_then(|l| l.seconds));
    let steps = svcs.steps.unwrap_or(20).max(4);
    let (a, n) = if steps >= 8 { (2, 3) } else { (1, 1) };
    let after = (a + n + 5).min(steps - 1).max(a + n);
    let fault = if role == "answer" {
        format!("N={who}@{a}-{}:loss,30%", a + n)
    } else {
        format!("K={who}@{a}+{n}")
    };
    let fault_words = if role == "answer" {
        format!(
            "answers from {who} are lost (30%) from step {a} to step {}",
            a + n
        )
    } else {
        format!(
            "{who} is killed before step {a} and starts again before step {}",
            a + n
        )
    };
    let mut out = vec![
        Proposal {
            name: "back-before",
            schedule: format!("disk=none | run T=1:-{back} {fault}"),
            says: format!("the clock goes back by {} before step 1; then {fault_words}", seconds_words(back)),
        },
        Proposal {
            name: "back-after",
            schedule: format!("disk=none | run {fault} T={after}:-{back}"),
            says: format!("{fault_words}; the clock goes back by {} before step {after}, while the system recovers", seconds_words(back)),
        },
    ];
    if role == "leader" {
        out.push(Proposal {
            name: "forward",
            schedule: format!("disk=none | run T={a}:{fwd}"),
            says: format!(
                "the clock goes forward by {} before step {a}, longer than the lease",
                seconds_words(fwd)
            ),
        });
    } else {
        out.push(Proposal {
            name: "forward-after",
            schedule: format!("disk=none | run {fault} T={after}:{fwd}"),
            says: format!(
                "{fault_words}; the clock goes forward by {} before step {after}",
                seconds_words(fwd)
            ),
        });
    }
    (why.to_string(), out)
}

/// What a match does, in plain words.
pub fn what_it_does(m: &ClockMatch) -> String {
    let lim = match &m.limit {
        Some(Limit {
            name,
            seconds: Some(s),
            at: Some(at),
        }) => format!("{name}, {} by default ({at})", limit_words(*s)),
        Some(Limit {
            name,
            seconds: Some(s),
            ..
        }) => format!("{name}, {}", limit_words(*s)),
        Some(Limit { name, .. }) => name.clone(),
        None => "a duration".into(),
    };
    match m.shape {
        "elapsed" => {
            format!("It compares the time since an earlier wall-clock reading with {lim}.")
        }
        "deadline" => format!(
            "It adds {lim} to a wall-clock reading and keeps the sum as a deadline or a lease."
        ),
        _ => "It compares a wall-clock reading with a stored deadline or lease.".into(),
    }
}

// ---------------------------------------------------------------- the report

/// The services of the project in `root`: `--manifest FILE`, else `root/hydo/hydo.yaml`, else
/// `root/hydo.yaml`.
pub fn services_near(root: &Path, manifest: Option<&str>) -> Services {
    let cands: Vec<std::path::PathBuf> = match manifest {
        Some(m) => vec![m.into()],
        None => vec![root.join("hydo").join("hydo.yaml"), root.join("hydo.yaml")],
    };
    for c in cands {
        if let Ok(text) = std::fs::read_to_string(&c) {
            let mut s = services_of(&text, &c.to_string_lossy());
            s.map = crate::clockmap::map(root, &c, &text);
            return s;
        }
    }
    Services::default()
}

/// Words of code that reports or caches rather than waits: a place there is ranked last.
const REPORT_CODE: &[&str] = &[
    "cache",
    "caches",
    "stat",
    "stats",
    "metric",
    "metrics",
    "info",
    "log",
    "logs",
    "debug",
    "report",
    "latency",
    "cli",
    "benchmark",
    "bench",
    "sparkline",
    "monitor",
    "progress",
    "histogram",
    "uptime",
    "print",
    "fuzzer",
    "example",
    "dump",
    "emit",
    "trace",
];

/// Words of code that waits, throttles or retries.
const WAIT_CODE: &[&str] = &[
    "throttle",
    "refresh",
    "retry",
    "backoff",
    "reconnect",
    "timeout",
    "lag",
    "idle",
    "delay",
    "lease",
    "lock",
    "expire",
    "expired",
    "heartbeat",
    "sync",
];

/// How much a place is worth a developer's first look: higher is first.
///
/// - A limit the code states, between 50 ms and one hour, adds 4; a step of a few minutes
///   then shows its effect inside a run.
/// - Leases and replication add 5, a database connection or a request timeout 3.
/// - Words of waiting, throttling or retrying add 1.
/// - Code that the services of hydo.yaml run adds 3: a word of the function's or the file's
///   name is a word of their commands or environments, such as `replication` for a service
///   started with `--replication-...` options.
/// - A measured time, not a stored deadline, adds 1.
/// - Code in a minor language of the repository (a client library) loses 5.
/// - Code that reports, logs, caches or benchmarks loses 6.
pub fn rank_score(m: &ClockMatch, svcs: &Services) -> i32 {
    let words: HashSet<String> = name_words(&format!(
        "{} {} {}",
        m.function.clone().unwrap_or_default(),
        m.file,
        m.text
    ))
    .into_iter()
    .collect();
    let mut s = match m.limit.as_ref() {
        Some(Limit {
            seconds: Some(v), ..
        }) if (0.05..=3600.0).contains(v) => 4,
        Some(_) => 1,
        None => 0,
    };
    s += match role_of(m) {
        "leader" | "replica" => 5,
        "database" | "answer" => 3,
        _ => 0,
    };
    if words.iter().any(|w| WAIT_CODE.contains(&w.as_str())) {
        s += 1;
    }
    // code that the services run: a word of the function's or the file's name
    let names: HashSet<String> = name_words(&format!(
        "{} {}",
        m.function.clone().unwrap_or_default(),
        m.file
    ))
    .into_iter()
    .collect();
    if names.iter().any(|w| svcs.words.contains(w)) {
        s += 3;
    }
    if !m.main_lang {
        s -= 5;
    }
    if m.shape == "elapsed" {
        s += 1;
    }
    if words.iter().any(|w| REPORT_CODE.contains(&w.as_str())) {
        s -= 6;
    }
    s
}

/// The places to aim schedules at, best first: one per function (or file), its first high
/// match. Medium matches get no schedules.
fn targets<'a>(matches: &'a [ClockMatch], svcs: &Services) -> Vec<&'a ClockMatch> {
    let mut seen = HashSet::new();
    let mut out: Vec<&ClockMatch> = matches
        .iter()
        .filter(|m| m.confidence == "high")
        .filter(|m| seen.insert((m.file.clone(), m.function.clone().unwrap_or_default())))
        .collect();
    // the sort is stable: equal scores keep the path order. With a map of what the services
    // run, the places in that code come first (Milestone 324)
    out.sort_by_key(|m| {
        let none = svcs.map.run_by(&m.file).is_some_and(|w| w.is_empty());
        (none, -rank_score(m, svcs))
    });
    out
}

/// How many places the text report shows without `--all`.
pub const TOP: usize = 5;

pub fn match_json(m: &ClockMatch) -> serde_json::Value {
    serde_json::json!({
        "file": m.file,
        "line": m.line,
        "lang": m.lang,
        "text": m.text,
        "shape": m.shape,
        "confidence": m.confidence,
        "function": m.function,
        "origin": m.origin,
        "limit": m.limit.as_ref().map(|l| serde_json::json!({"name": l.name, "seconds": l.seconds, "at": l.at})),
    })
}

pub fn json_report(dir: &str, a: &Audit, svcs: &Services) -> String {
    let (read, matches) = (a.read, &a.matches[..]);
    let t: HashMap<(String, usize), usize> = targets(matches, svcs)
        .iter()
        .enumerate()
        .map(|(k, m)| ((m.file.clone(), m.line), k + 1))
        .collect();
    let items: Vec<serde_json::Value> = matches
        .iter()
        .map(|m| {
            let mut v = match_json(m);
            if let Some(who) = svcs.map.run_by(&m.file) {
                v["run_by"] = who.into();
            }
            if let Some(rank) = t.get(&(m.file.clone(), m.line)) {
                v["rank"] = (*rank).into();
                let (why, ps) = proposals(m, svcs);
                v["fault"] = why.into();
                v["schedules"] = ps
                    .iter()
                    .map(|p| serde_json::json!({"name": p.name, "schedule": p.schedule, "says": p.says}))
                    .collect();
            }
            v
        })
        .collect();
    let mut doc = serde_json::json!({"dir": dir, "files_read": read, "clock": items});
    doc["tests_left_out"] =
        serde_json::json!({"functions": a.tests.functions, "modules": a.tests.modules});
    if !svcs.names.is_empty() {
        doc["services_run"] = svcs.map.words().into();
    }
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

pub fn text_report(dir: &str, a: &Audit, svcs: &Services, max: usize, all: bool) -> String {
    use std::fmt::Write;
    let (read, matches) = (a.read, &a.matches[..]);
    let mut o = String::new();
    let ts = targets(matches, svcs);
    let _ = writeln!(
        o,
        "hydo rules scan --kind clock: {} functions in {read} source files of {dir} measure a duration with the wall clock",
        ts.len()
    );
    // what was left out and how hydo.yaml was read (Milestone 324)
    let t = a.tests;
    if t.functions + t.modules > 0 {
        let mut what = Vec::new();
        if t.functions > 0 {
            what.push(format!(
                "{} test function{}",
                t.functions,
                if t.functions == 1 { "" } else { "s" }
            ));
        }
        if t.modules > 0 {
            what.push(format!(
                "{} test module{} or class{}",
                t.modules,
                if t.modules == 1 { "" } else { "s" },
                if t.modules == 1 { "" } else { "es" }
            ));
        }
        let _ = writeln!(
            o,
            "It left out {} inside the source files, besides the test files.",
            and_words(&what)
        );
    }
    if !svcs.names.is_empty() {
        let _ = writeln!(o, "{}", svcs.map.words());
    }
    if ts.is_empty() {
        let _ = writeln!(
            o,
            "\nNo lease, timeout, throttle, lag or delay was found that reads the wall clock."
        );
        return o;
    }
    let _ = writeln!(
        o,
        "\nNTP or an operator can step the wall clock. A duration measured on it then lasts longer\nby the size of a step back, or ends early after a step forward. The most likely places first:"
    );
    let shown = if all { max } else { TOP.min(max) };
    for (k, m) in ts.iter().take(shown).enumerate() {
        let _ = writeln!(
            o,
            "\n{}. {}:{}{} [{}]",
            k + 1,
            m.file,
            m.line,
            m.function
                .as_ref()
                .map(|f| format!(", in {f}"))
                .unwrap_or_default(),
            m.confidence
        );
        let _ = writeln!(o, "     {}", m.text);
        let _ = writeln!(o, "   {}", what_it_does(m));
        match svcs.map.run_by(&m.file) {
            Some(v) if v.is_empty() => {
                let _ = writeln!(
                    o,
                    "   No service of hydo.yaml and not the workload runs this code."
                );
            }
            Some(v) => {
                let _ = writeln!(
                    o,
                    "   Run by {} ({}).",
                    and_words(&v),
                    svcs.map.programs_of(&m.file).join(", ")
                );
            }
            None => {}
        }
        match m.origin.len() {
            0 => {}
            1 | 2 => {
                let _ = writeln!(o, "   Where the time comes from: {}.", m.origin.join("; "));
            }
            n => {
                let _ = writeln!(
                    o,
                    "   Where the time comes from: {}; after {} more step{}, {}.",
                    m.origin[0],
                    n - 2,
                    if n == 3 { "" } else { "s" },
                    m.origin[n - 1]
                );
            }
        }
        let more: Vec<String> = matches
            .iter()
            .filter(|x| x.file == m.file && x.function == m.function && x.line != m.line)
            .map(|x| x.line.to_string())
            .collect();
        if !more.is_empty() {
            let whole = if m.function.is_some() {
                "function"
            } else {
                "file"
            };
            let _ = writeln!(
                o,
                "   The same {whole} also does it at lines {}.",
                more.join(", ")
            );
        }
        let (why, ps) = proposals(m, svcs);
        let _ = writeln!(
            o,
            "   The schedules (the fault that makes this code run: {why}):"
        );
        for p in &ps {
            let _ = writeln!(o, "     {:<14} {}", p.name, p.schedule);
            let _ = writeln!(o, "     {:<14} {}", "", p.says);
        }
        if let Some(w) = placeholder_why(m, svcs) {
            let _ = writeln!(o, "   {w}.");
        }
    }
    let rest: Vec<&ClockMatch> = matches.iter().filter(|m| m.confidence != "high").collect();
    if !all {
        // one per function: the functions not shown, with a high place or only medium ones
        let functions: HashSet<(String, String)> = matches
            .iter()
            .map(|m| (m.file.clone(), m.function.clone().unwrap_or_default()))
            .collect();
        let more = functions.len().saturating_sub(shown.min(ts.len()));
        if more > 0 {
            let _ = writeln!(o, "\nand {more} more: hydo rules scan --kind clock --all");
        }
    } else {
        if ts.len() > shown {
            let _ = writeln!(
                o,
                "\n... {} more places with schedules (--max N, or --json for all)",
                ts.len() - shown
            );
        }
        if !rest.is_empty() {
            let _ = writeln!(o, "\nMedium confidence (the words suggest a deadline; read the code before you aim a schedule at it):");
            for m in rest.iter().take(max) {
                let _ = writeln!(o, "  {}:{}: {}", m.file, m.line, m.text);
            }
            if rest.len() > max {
                let _ = writeln!(o, "  ... {} more", rest.len() - max);
            }
        }
    }
    let ts: Vec<&ClockMatch> = ts.into_iter().take(shown).collect();
    let placeholders = ts.iter().any(|m| {
        proposals(m, svcs)
            .1
            .iter()
            .any(|p| p.schedule.contains('<'))
    });
    let _ = writeln!(o, "\nTo try a schedule: hydo explore --schedule \"<schedule>\" (it runs it, and shrinks a failure).");
    if placeholders {
        let _ = writeln!(o, "Replace each <name> with a service of hydo.yaml: <replica> a replica, <database> the database, <leader> the holder of the lease, <service> the service the code runs in.");
    }
    let _ = writeln!(o, "The steps assume {} workload steps; move them to where the code runs. hydo guide faults, \"Find the durations measured on the wall clock\".", svcs.steps.unwrap_or(20).max(4));
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(rel: &str, text: &str) -> Vec<ClockMatch> {
        audit_texts(&[(rel, text)])
    }

    #[test]
    fn c_finds_a_cached_wall_clock_in_a_lag_and_an_idle_wait() {
        // Valkey's shape: gettimeofday in ustime(), a cached field, and the field in a lag
        let util = "long long ustime(void) {\n    struct timeval tv;\n    long long ust;\n    gettimeofday(&tv, NULL);\n    ust = ((long long)tv.tv_sec) * 1000000;\n    return ust;\n}\n";
        let server = "static void updateCachedTime(void) {\n    server.ustime = ustime();\n    server.mstime = server.ustime / 1000;\n    server.unixtime = server.mstime / 1000;\n}\n";
        let repl = "void refreshGood(void) {\n    time_t lag = server.unixtime - replica->repl_ack_time;\n    if (lag <= server.repl_min_replicas_max_lag) good++;\n}\nint shouldStart(void) {\n    idle = server.unixtime - replica->last_interaction;\n    if (idle > max_idle) max_idle = idle;\n    return max_idle >= server.repl_diskless_sync_delay;\n}\nvoid info(void) {\n    info = sdscatprintf(info, \"ago:%d\", (int)(server.unixtime - server.lastio));\n}\n";
        let config = "standardConfig configs[] = {\n    createIntConfig(\"repl-diskless-sync-delay\", NULL, MODIFIABLE_CONFIG, 0, INT_MAX, server.repl_diskless_sync_delay, 5, INTEGER_CONFIG, NULL, NULL),\n};\n";
        let m = audit_texts(&[
            ("src/util.c", util),
            ("src/server.c", server),
            ("src/replication.c", repl),
            ("src/config.c", config),
        ]);
        let lines: Vec<(&str, usize, &str)> = m
            .iter()
            .map(|x| (x.file.as_str(), x.line, x.confidence))
            .collect();
        assert_eq!(
            lines,
            vec![
                ("src/replication.c", 2, "high"),
                ("src/replication.c", 6, "high")
            ],
            "{m:#?}"
        );
        let idle = &m[1];
        assert_eq!(idle.function.as_deref(), Some("shouldStart"));
        assert_eq!(
            idle.limit.as_ref().unwrap().name,
            "repl_diskless_sync_delay"
        );
        assert_eq!(idle.limit.as_ref().unwrap().seconds, Some(5.0));
        assert!(
            idle.origin.iter().any(|o| o.contains("unixtime")),
            "{:?}",
            idle.origin
        );
        assert!(
            idle.origin.last().unwrap().contains("gettimeofday"),
            "{:?}",
            idle.origin
        );
    }

    #[test]
    fn go_finds_a_throttle_on_a_time_without_its_monotonic_reading() {
        // Temporal's shape: a Now() method that returns time.Now().UTC()
        let ts = "package clock\n\nfunc (ts RealTimeSource) Now() time.Time {\n\treturn time.Now().UTC()\n}\n";
        let h = "package sql\n\nconst (\n\tsessionRefreshMinInternal = 1 * time.Second\n)\n\nfunc (h *DatabaseHandle) reconnect(force bool) {\n\tnow := h.timeSource.Now()\n\tlastRefresh := h.lastRefresh\n\tif now.Sub(lastRefresh) < sessionRefreshMinInternal {\n\t\treturn\n\t}\n\th.lastRefresh = now\n}\n";
        let m = audit_texts(&[
            ("common/clock/time_source.go", ts),
            ("common/sql/db_handle.go", h),
        ]);
        assert_eq!(m.len(), 1, "{m:#?}");
        assert_eq!(
            (m[0].line, m[0].shape, m[0].confidence),
            (10, "elapsed", "high")
        );
        let l = m[0].limit.as_ref().unwrap();
        assert_eq!(
            (l.name.as_str(), l.seconds),
            ("sessionRefreshMinInternal", Some(1.0))
        );
        assert!(
            m[0].origin.iter().any(|o| o.contains("Now() returns")),
            "{:?}",
            m[0].origin
        );
    }

    #[test]
    fn go_keeps_quiet_about_the_monotonic_clock() {
        let src = "package x\n\nfunc wait(start time.Time, timeout time.Duration) bool {\n\tdeadline := time.Now().Add(timeout)\n\tif time.Since(start) > timeout {\n\t\treturn true\n\t}\n\treturn time.Now().After(deadline)\n}\n";
        assert!(one("x.go", src).is_empty());
    }

    #[test]
    fn sql_finds_a_lease_stamped_and_checked_by_the_database_clock() {
        // River's shape
        let sql = "-- name: LeaderAttemptElect :one\nINSERT INTO river_leader (leader_id, elected_at, expires_at)\nVALUES (\n    @leader_id,\n    coalesce(sqlc.narg('now')::timestamptz, now()),\n    coalesce(sqlc.narg('now')::timestamptz, now()) + make_interval(secs => @ttl)\n);\n-- name: LeaderDeleteExpired :execrows\nDELETE FROM river_leader\nWHERE expires_at < coalesce(sqlc.narg('now')::timestamptz, now());\nCREATE TABLE jobs (created_at timestamptz NOT NULL DEFAULT now());\n";
        let m = one("internal/dbsqlc/river_leader.sql", sql);
        let got: Vec<(usize, &str, &str)> =
            m.iter().map(|x| (x.line, x.shape, x.confidence)).collect();
        assert_eq!(
            got,
            vec![(6, "deadline", "high"), (10, "check", "high")],
            "{m:#?}"
        );
    }

    #[test]
    fn each_language_has_its_reading() {
        let cases: &[(&str, &str, usize)] = &[
            ("a.rs", "fn expired(&self) -> bool {\n    let now = SystemTime::now();\n    now.duration_since(self.last_seen).unwrap() > self.lease_timeout\n}\n", 3),
            ("A.java", "class A {\n  boolean expired() {\n    long now = System.currentTimeMillis();\n    return now - lastHeartbeat > heartbeatTimeoutMs;\n  }\n}\n", 4),
            ("a.py", "def expired(self):\n    if time.time() - self.started > self.timeout:\n        return True\n", 2),
            ("a.ts", "function expired(lock) {\n  return Date.now() - lock.acquiredAt > LOCK_TTL_MS;\n}\n", 2),
            ("a.rb", "def stale?\n  Time.now - @last_seen > TIMEOUT\nend\n", 2),
            ("a.cs", "bool Expired() {\n    return DateTime.UtcNow - lastSeen > timeout;\n}\n", 2),
            ("b.c", "int expired(void) {\n    return time(NULL) - last_ping > ping_timeout;\n}\n", 2),
        ];
        for (rel, src, line) in cases {
            let m = one(rel, src);
            assert_eq!(
                m.iter().map(|x| x.line).collect::<Vec<_>>(),
                vec![*line],
                "{rel}: {m:#?}"
            );
            assert_eq!(m[0].confidence, "high", "{rel}");
        }
    }

    /// Milestone 324: the test code inside a source file is left out by each language's
    /// conventions, and the product's code beside it is still read (the audit ranked a Rust test
    /// function in a `#[cfg(test)]` module first).
    #[test]
    fn test_code_inside_a_source_file_is_left_out() {
        let rust = "fn expired(&self) -> bool {\n    let now = SystemTime::now();\n    now.duration_since(self.last_seen).unwrap() > self.lease_timeout\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn test_clean_unpartitioned_table() {\n        let now = SystemTime::now();\n        assert!(now.duration_since(t.last_seen).unwrap() > t.lease_timeout);\n    }\n}\n\n#[tokio::test(flavor = \"multi_thread\")]\nasync fn lease_ends() {\n    let now = SystemTime::now();\n    assert!(now.duration_since(l.last_seen).unwrap() > l.lease_timeout);\n}\n";
        let m = one("src/lease.rs", rust);
        assert_eq!(
            m.iter().map(|x| x.line).collect::<Vec<_>>(),
            vec![3],
            "{m:#?}"
        );
        let cases: &[(&str, &str, usize, (usize, usize))] = &[
            ("a.go", "package a\n\nfunc expired() bool {\n\treturn time.Now().UTC().Sub(last) > timeout\n}\n\nfunc TestExpired(t *testing.T) {\n\tif time.Now().UTC().Sub(start) > timeout {\n\t}\n}\n", 4, (1, 0)),
            ("a.py", "def expired(self):\n    if time.time() - self.started > self.timeout:\n        return True\n\ndef test_expired():\n    if time.time() - started > timeout:\n        pass\n\nclass TestLease:\n    def check(self):\n        if time.time() - self.started > self.timeout:\n            pass\n", 2, (1, 1)),
            ("A.java", "class A {\n  boolean expired() {\n    long now = System.currentTimeMillis();\n    return now - lastHeartbeat > heartbeatTimeoutMs;\n  }\n  @Test\n  void expires() {\n    long now = System.currentTimeMillis();\n    assert now - lastHeartbeat > heartbeatTimeoutMs;\n  }\n}\n", 4, (1, 0)),
            ("a.ts", "function expired(lock) {\n  return Date.now() - lock.acquiredAt > LOCK_TTL_MS;\n}\nif (import.meta.vitest) {\n  it(\"expires\", () => {\n    expect(Date.now() - lock.acquiredAt > LOCK_TTL_MS).toBe(true);\n  });\n}\n", 2, (0, 1)),
            ("a.cs", "bool Expired() {\n    return DateTime.UtcNow - lastSeen > timeout;\n}\n[Fact]\nvoid Expires() {\n    Assert.True(DateTime.UtcNow - lastSeen > timeout);\n}\n", 2, (1, 0)),
            ("b.cc", "int expired(void) {\n    return time(NULL) - last_ping > ping_timeout;\n}\nTEST(Lease, Expires) {\n    EXPECT_TRUE(time(NULL) - last_ping > ping_timeout);\n}\n", 2, (1, 0)),
        ];
        for (rel, src, line, (fns, mods)) in cases {
            let m = one(rel, src);
            assert_eq!(
                m.iter().map(|x| x.line).collect::<Vec<_>>(),
                vec![*line],
                "{rel}: {m:#?}"
            );
            let f = source_file(rel.to_string(), lang_of(rel).unwrap(), src);
            assert_eq!((f.tests.functions, f.tests.modules), (*fns, *mods), "{rel}");
        }
        let f = source_file("src/lease.rs".into(), Lang::Rust, rust);
        assert_eq!((f.tests.functions, f.tests.modules), (1, 1));
        // code only a test build has; `any(test, ...)` is not that
        assert!(rust_test_attr("#[cfg(all(test, feature = \"x\"))]"));
        assert!(!rust_test_attr("#[cfg(any(test, feature = \"x\"))]"));
        assert!(!rust_test_attr("#[derive(Debug)]"));
    }

    #[test]
    fn logs_time_stamps_and_tests_are_not_matches() {
        let src = "int main(void) {\n    printf(\"took %ld\", time(NULL) - start);\n    rec.created = time(NULL);\n    return 0;\n}\n";
        assert!(one("main.c", src).is_empty());
        let js = "function f() {\n  const id = `${Date.now()}-${n}`;\n  console.log(Date.now() - t0);\n}\n";
        assert!(one("f.js", js).is_empty());
        assert!(skipped("pkg/foo_test.go"));
        assert!(skipped("internal/riverdrivertest/queue.go"));
        assert!(skipped("deps/hiredis/net.c"));
        assert!(!skipped("src/replication.c"));
    }

    #[test]
    fn a_deadline_stamped_in_code_is_a_match() {
        let py = "def lease(self):\n    self.lease_expires_at = time.time() + self.lease_seconds\n";
        let m = one("q.py", py);
        assert_eq!(
            m.iter()
                .map(|x| (x.shape, x.confidence))
                .collect::<Vec<_>>(),
            vec![("deadline", "high")]
        );
        let go = "package q\n\nfunc (q *Q) claim(m *Msg) {\n\tm.VisibleAt = time.Now().UTC().Add(q.visibilityTimeout)\n}\n";
        let m = one("q.go", go);
        assert_eq!(
            m.iter()
                .map(|x| (x.shape, x.confidence))
                .collect::<Vec<_>>(),
            vec![("deadline", "high")]
        );
    }

    #[test]
    fn durations_are_read_from_the_code() {
        assert_eq!(duration_value("15 * time.Second", "-"), Some(15.0));
        assert_eq!(duration_value("time.Minute", "-"), Some(60.0));
        assert_eq!(duration_value("500 * time.Millisecond", "-"), Some(0.5));
        assert_eq!(duration_value("Duration::from_secs(30)", "-"), Some(30.0));
        assert_eq!(duration_value("60 * 1000", "ms"), Some(60.0));
        assert_eq!(duration_value("timedelta(seconds=45)", "-"), Some(45.0));
        assert_eq!(duration_value("foo()", "s"), None);
        assert_eq!(step_sizes(Some(5.0)), (300, 30));
        assert_eq!(step_sizes(Some(15.0)), (300, 30));
        assert_eq!(step_sizes(Some(60.0)), (3600, 300));
        assert_eq!(step_sizes(None), (300, 300));
        assert_eq!(seconds_words(300), "5 minutes");
        assert_eq!(seconds_words(3600), "1 hour");
        // whole words: a page size is no age, and a count of attempts is no limit
        for (name, is) in [
            ("repl_diskless_sync_delay", true),
            ("sessionRefreshMinInternal", true),
            ("heartbeatTimeoutMs", true),
            ("max_age", true),
            ("PageSize", false),
            ("maxAttempts", false),
            ("agentSecret", false),
        ] {
            assert_eq!(duration_name(name), is, "{name}");
        }
    }

    #[test]
    fn the_schedules_name_the_service_that_makes_the_code_run() {
        let yaml = "format: 1\nname: kv\nservices:\n  - name: kv1\n    cmd: [server]\n  - name: kv2\n    cmd: [server, --replicaof, kv1]\nworkload:\n  steps: 30\n";
        let s = services_of(yaml, "hydo.yaml");
        assert_eq!(s.replicas, vec!["kv2"]);
        let m = ClockMatch {
            file: "src/replication.c".into(),
            line: 1,
            lang: "c",
            text: "idle = server.unixtime - replica->last_interaction;".into(),
            shape: "elapsed",
            confidence: "high",
            function: Some("shouldStartChildReplication".into()),
            origin: vec![],
            main_lang: true,
            limit: Some(Limit {
                name: "repl_diskless_sync_delay".into(),
                seconds: Some(5.0),
                at: None,
            }),
        };
        let (_, p) = proposals(&m, &s);
        let got: Vec<&str> = p.iter().map(|x| x.schedule.as_str()).collect();
        assert_eq!(
            got,
            vec![
                "disk=none | run T=1:-300 K=kv2@2+3",
                "disk=none | run K=kv2@2+3 T=10:-300",
                "disk=none | run K=kv2@2+3 T=10:30",
            ]
        );
        let (_, p) = proposals(&m, &Services::default());
        assert_eq!(p[0].schedule, "disk=none | run T=1:-300 K=<replica>@2+3");
    }
}
