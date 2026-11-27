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
//!
//! A second kind of place (Milestone 350): a reading of the clock kept as the order or the
//! version of data, `order`. The reading is the value of a name about an order (`timestamp`,
//! `ts`, `version`), or of a variable that the function then stores in such a field. It is
//! `high` when the code compares two values of that name: a merge (`other.ts > self.ts`), a
//! maximum or a sort key (`.map(|v| v.timestamp).max()`, `fn cmp_key`). A reading kept above
//! the earlier value (`max(prev + 1, now)`) is no match. After a step back, a later write gets
//! an older version than the writes before it and can lose to them, as Garage's CopyObject did.

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
    /// Erlang (Milestone 382): read by the clock audit only, not by the route scan
    Erlang,
    /// Elixir (Milestone 382): read by the clock audit only
    Elixir,
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
            Lang::Erlang => "erlang",
            Lang::Elixir => "elixir",
        }
    }
    fn braces(self) -> bool {
        !matches!(
            self,
            Lang::Python | Lang::Ruby | Lang::Sql | Lang::Erlang | Lang::Elixir
        )
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

/// The language of a file as the clock audit reads it: the languages of `lang_of`, and Erlang
/// and Elixir (Milestone 382), which the route scan does not read.
pub fn clock_lang_of(rel: &str) -> Option<Lang> {
    if let Some(l) = lang_of(rel) {
        return Some(l);
    }
    let name = rel.rsplit('/').next()?;
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "erl" | "hrl" => Some(Lang::Erlang),
        "ex" | "exs" => Some(Lang::Elixir),
        _ => None,
    }
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
    let slash = !matches!(
        lang,
        Lang::Python | Lang::Ruby | Lang::Sql | Lang::Erlang | Lang::Elixir
    );
    let hash = matches!(lang, Lang::Python | Lang::Ruby | Lang::Elixir);
    let sql = lang == Lang::Sql;
    // Erlang (Milestone 382): `%` starts a comment, and `$c` is a character, also `$"`
    let erl = lang == Lang::Erlang;
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
                if erl && c == '$' && n.is_some_and(|x| x != '\n') {
                    // a character literal: `$a`, `$"`, `$\n`
                    cur.push_str("$c");
                    i += if n == Some('\\') { 3 } else { 2 };
                    continue;
                }
                if lang == Lang::Elixir && c == '?' && matches!(n, Some('"' | '\'')) {
                    cur.push_str("?c");
                    i += 2;
                    continue;
                }
                if (slash && c == '/' && n == Some('/'))
                    || (hash && c == '#')
                    || (erl && c == '%')
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
    // Erlang (Milestone 382): the system time follows the OS clock (with time warp, at its next
    // check); `erlang:monotonic_time` does not
    Source {
        pat: "erlang:system_time(",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "os:system_time(",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "erlang:now()",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "os:timestamp()",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "erlang:timestamp()",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "calendar:universal_time()",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "calendar:local_time()",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "erlang:universaltime()",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    Source {
        pat: "erlang:localtime()",
        langs: &[Lang::Erlang],
        unit: "-",
    },
    // Elixir
    Source {
        pat: "System.system_time(",
        langs: &[Lang::Elixir],
        unit: "-",
    },
    Source {
        pat: "System.os_time(",
        langs: &[Lang::Elixir],
        unit: "-",
    },
    Source {
        pat: "DateTime.utc_now(",
        langs: &[Lang::Elixir],
        unit: "-",
    },
    Source {
        pat: "NaiveDateTime.utc_now(",
        langs: &[Lang::Elixir],
        unit: "-",
    },
    Source {
        pat: ":os.system_time(",
        langs: &[Lang::Elixir],
        unit: "-",
    },
    Source {
        pat: ":erlang.system_time(",
        langs: &[Lang::Elixir],
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
    "erlang:monotonic_time",
    "System.monotonic_time",
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
    // Milestone 382: how long data is kept (VictoriaMetrics's `retentionMsecs`)
    "retention",
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
    // Milestone 382: the due time of a timer (brpc's `run_time`, braft's `_next_duetime`) and
    // a retention's horizon
    "run_time",
    "duetime",
    "due_time",
    "due_at",
    "next_run",
    "wake_at",
    "fire_at",
    "retention",
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
    // Milestone 382
    "run_at",
    "runat",
    "run_time",
    "duetime",
    "due_time",
    "due_at",
    "retention",
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
    // Erlang writes `=<` (Milestone 382)
    for op in [" <= ", " >= ", " < ", " > ", " =< "] {
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
    // Milestone 382: a Rust function's parameters follow its name, not `pub(crate)`
    let base = match (lang, text.find("fn ")) {
        (Lang::Rust, Some(p)) => p + 3,
        _ => 0,
    };
    let Some(open) = text[base..].find('(').map(|o| base + o) else {
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
    // Milestone 382: a Rust method's receiver is no argument of its calls (`x.is_available(t)`)
    if lang == Lang::Rust
        && parts.first().is_some_and(|p| {
            let p = p.split(':').next().unwrap_or("").trim();
            matches!(p, "self" | "&self" | "&mut self" | "mut self")
                || p.starts_with("&'") && p.ends_with("self")
        })
    {
        parts.remove(0);
    }
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
    /// Milestone 382: the bundled third-party library the file belongs to
    bundled: Option<String>,
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
            // `fn new(...)` is a constructor's name, not Java's `new X(...)` (Milestone 350)
            if t[i + 3..].trim_start().starts_with("new(")
                || t[i + 3..].trim_start().starts_with("new<")
            {
                return Some("new".into());
            }
            ident_before_paren(&t[i + 3..])
        }
        Lang::Erlang => {
            // a clause's head starts a line: `name(Args) ->`, not an attribute (`-spec`)
            if code.starts_with(|c: char| c.is_whitespace())
                || !t.starts_with(|c: char| c.is_ascii_lowercase())
            {
                return None;
            }
            let name: String = t.chars().take_while(|c| is_ident(*c)).collect();
            (t[name.len()..].starts_with('(') && !CONTROL.contains(&name.as_str())).then_some(name)
        }
        Lang::Python | Lang::Ruby | Lang::Elixir => {
            let rest = t
                .strip_prefix("def ")
                .or_else(|| t.strip_prefix("async def "))
                .or_else(|| t.strip_prefix("defp "))?;
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
    if lang == Lang::Erlang {
        // Milestone 382: a function is its clauses, from the first head to the `.` that ends
        // the form
        let mut cur: Option<usize> = None;
        for (i, l) in code.iter().enumerate() {
            if cur.is_none() {
                if let Some(name) = func_header(lang, l) {
                    let params = params_of(lang, &code[i..(i + 4).min(code.len())]);
                    funcs.push(Func {
                        name,
                        start: i,
                        end: i,
                        params,
                    });
                    cur = Some(funcs.len() - 1);
                }
            }
            scope[i] = cur;
            if let Some(f) = cur {
                let t = l.trim_end();
                if t.ends_with('.') && !t.ends_with("..") {
                    funcs[f].end = i;
                    cur = None;
                } else {
                    funcs[f].end = i;
                }
            }
        }
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
                    if ind <= fi
                        && !(matches!(lang, Lang::Ruby | Lang::Elixir)
                            && l.trim() == "end"
                            && ind == fi)
                    {
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
    /// Milestone 382: Go's `time.Time` with its monotonic reading (a ticker's tick, a
    /// `time.Now()` passed on): a wall-clock reading only after `.Unix()`, `.UTC()` or
    /// `timestamp.FromTime(...)`
    weak: bool,
}

#[derive(Default)]
struct Taint {
    map: HashMap<Key, Origin>,
    /// fields that are also set from a monotonic clock: not taken for readings
    mixed: HashSet<String>,
    /// Milestone 382: functions that return a reading plus a duration, a deadline
    /// (`butil::milliseconds_from_now(ms)`)
    deadline_calls: HashSet<String>,
    /// Milestone 382: cells that an atomic store sets from a reading, a cached clock
    /// (VictoriaMetrics's `fasttime`, Restate's `RECENT_UNIX_TIMESTAMP_US`)
    cells: HashSet<String>,
}

/// Milestone 382: the mark of the names of keys that bundled code sets. The project's code
/// never takes a reading from them, so a library's own `Now()` or `timestamp` field changes no
/// place of the project; bundled code takes readings from both.
const BUNDLED_MARK: char = '\u{1}';

/// The name under which a key set in `f` is kept.
fn group_name(f: &SrcFile, name: &str) -> String {
    if f.bundled.is_some() {
        format!("{BUNDLED_MARK}{name}")
    } else {
        name.to_string()
    }
}

/// The names a line of `f` looks a key up by: the project's, and in bundled code its own too.
fn lookup_names(f: &SrcFile, name: &str) -> Vec<String> {
    let mut v = vec![name.to_string()];
    // an Erlang key of a map or a record is a library's interface: Ra's command meta carries
    // `system_time` into RabbitMQ's own code
    if f.bundled.is_some() || name.starts_with('#') {
        v.push(format!("{BUNDLED_MARK}{name}"));
    }
    v
}

/// A key's name without the mark of bundled code and without the file of a field that only
/// its own file reads.
fn plain(n: &str) -> &str {
    let n = n.trim_start_matches(BUNDLED_MARK);
    n.split(FILE_MARK).next().unwrap_or(n)
}

/// Milestone 382: the mark of a field that only its own file reads: one set from Go's
/// `time.Time` once converted (`w.startTimestamp = timestamp.FromTime(t)`). Its name alone is
/// too common to follow through a whole project.
const FILE_MARK: char = '\u{2}';

fn file_field(name: &str, fi: usize) -> String {
    format!("{name}{FILE_MARK}{fi}")
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
    /// Milestone 382: the span adds a duration to the reading itself, a deadline: Go's
    /// `time.Now().Add(d).UnixMilli()`, a call of a function that returns now plus a duration
    adds: bool,
}

const TIME_NAME_WORDS: &[&str] = &[
    "time", "now", "clock", "stamp", "epoch", "date", "utc", "millis",
];

/// Milestone 355: the start of the C++ qualifiers before `at` (`std::chrono::` of
/// `std::chrono::system_clock::now()`, `util::` of `util::GetTimeStamp()`), so that a reading
/// starts where its expression does.
fn qualified_start(code: &str, at: usize) -> usize {
    let b = code.as_bytes();
    let mut i = at;
    while i >= 2 && b[i - 1] == b':' && b[i - 2] == b':' {
        let mut k = i - 2;
        while k > 0 && is_ident(b[k - 1] as char) {
            k -= 1;
        }
        // `::time(NULL)`: the global namespace
        if k == i - 2 && (k == 0 || !is_ident(b[k - 1] as char)) {
            i = k;
            break;
        }
        if k == i - 2 {
            break;
        }
        i = k;
    }
    i
}

/// Milestone 355: the end of a template's arguments that open at `lt` (the index of its `<`),
/// after its `>`, when they hold only names, `::`, spaces, commas, `*`, `&` and nested
/// arguments; None for a comparison such as `a < b`.
fn template_end(code: &str, lt: usize) -> Option<usize> {
    let b = code.as_bytes();
    let mut depth = 0i32;
    let mut i = lt;
    while i < b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            c if is_ident(c as char) || matches!(c, b':' | b' ' | b',' | b'*' | b'&') => {}
            _ => return None,
        }
        i += 1;
    }
    None
}

fn spans(f: &SrcFile, fi: usize, i: usize, t: &Taint) -> Vec<Span> {
    spans_in(f, fi, i, &f.code[i], t)
}

/// The readings of `code`, a text of the scope of line `i` of `f`: the line itself, or a
/// statement over several lines (Milestone 382).
fn spans_in(f: &SrcFile, fi: usize, i: usize, code: &str, t: &Taint) -> Vec<Span> {
    let mut out = Vec::new();
    for s in SOURCES.iter().filter(|s| s.langs.contains(&f.lang)) {
        for p in find_all(code, s.pat, false) {
            let mut end = p + s.pat.len();
            if s.pat.ends_with('(') {
                end = call_end(code, end - 1);
            }
            // Milestone 382: a Rust path starts at its crates and modules too
            // (`chrono::Utc::now()`, `std::time::SystemTime::now()`)
            let start = if matches!(f.lang, Lang::C | Lang::Rust) {
                qualified_start(code, p)
            } else {
                p
            };
            out.push(Span {
                start,
                end,
                weak: false,
                from: Ok(s.pat),
                unit: s.unit,
                adds: false,
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
                // Milestone 382: `time.Now().Add(d).UnixMilli()` drops the monotonic reading
                // at its end: a wall-clock deadline
                if *w == "time.Now()" {
                    if let Some((e, unit, adds)) = go_wall_chain(code, end) {
                        out.push(Span {
                            start: p,
                            end: e,
                            weak: false,
                            from: Ok("time.Now()"),
                            unit,
                            adds,
                        });
                        continue;
                    }
                }
                out.push(Span {
                    start: p,
                    end,
                    weak: true,
                    from: Ok(w),
                    unit: "-",
                    adds: false,
                });
            }
        }
        // Milestone 382: an atomic load of a cell that holds a reading, `atomic.LoadInt64(&x.ts)`
        for (p, _) in code.match_indices("atomic.Load") {
            let open = match code[p..].find('(') {
                Some(o) => p + o,
                None => continue,
            };
            let close = call_end(code, open);
            let arg = code[open + 1..close.saturating_sub(1).max(open + 1)].trim();
            let cell: String = arg
                .chars()
                .rev()
                .take_while(|c| is_ident(*c))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let found = lookup_names(f, &cell)
                .into_iter()
                .find(|c| t.cells.contains(c));
            if let (true, Some(cell)) = (arg.starts_with('&'), found) {
                out.push(Span {
                    start: p,
                    end: close,
                    weak: false,
                    from: Err(Key::Field(cell)),
                    unit: "-",
                    adds: false,
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
        // where the receiver ends: before the `.`, or before the `-` of `->` (Milestone 382:
        // `loop->_now_millisec` starts at `loop`)
        let dot = if before == b'>' {
            j.saturating_sub(2)
        } else {
            j.saturating_sub(1)
        };
        let mut next = code[k..].chars().next();
        // Milestone 355: a C++ call with template arguments, `GetTimeStamp<std::chrono::milliseconds>()`
        let mut open = k;
        if f.lang == Lang::C && next == Some('<') {
            if let Some(e) = template_end(code, k) {
                if code[e..].starts_with('(') {
                    open = e;
                    next = Some('(');
                }
            }
        }
        let mut hit: Option<(Key, usize, usize)> = None;
        // Milestone 382: a load of a cell that an atomic store set from a reading, a cached
        // clock (`currentTimestamp.Load()`, `RECENT_UNIX_TIMESTAMP_US.load(Ordering::Relaxed)`)
        let load = [".Load()", ".load("]
            .iter()
            .find(|l| code[k..].starts_with(**l))
            .copied();
        let cell = lookup_names(f, name)
            .into_iter()
            .find(|c| t.cells.contains(c));
        if let (Some(l), Some(cell)) = (load, cell) {
            let s0 = if dotted {
                operand_left(code, dot)
            } else {
                qualified_start(code, j)
            };
            let e = call_end(code, k + l.find('(').unwrap_or(0));
            hit = Some((Key::Field(cell), s0, e));
        } else if f.lang == Lang::Erlang && name == "get" && code[..j].ends_with("maps:") {
            // Milestone 382: `maps:get(system_time, Meta)` reads a map's key
            if next == Some('(') {
                let arg: String = code[k + 1..]
                    .trim_start()
                    .chars()
                    .take_while(|c| is_ident(*c))
                    .collect();
                if let Some(key) = lookup_names(f, &format!("#{arg}"))
                    .into_iter()
                    .map(Key::Field)
                    .find(|k| t.map.contains_key(k))
                {
                    hit = Some((key, j - 5, call_end(code, k)));
                }
            }
        } else if next == Some('(') {
            let key = lookup_names(f, name)
                .into_iter()
                .map(Key::Call)
                .find(|k| t.map.contains_key(k));
            if let Some(key) = key {
                let recv_start = if dotted {
                    operand_left(code, dot)
                } else if matches!(f.lang, Lang::C | Lang::Rust) {
                    qualified_start(code, j)
                } else if f.lang == Lang::Erlang {
                    erl_qualified_start(code, j)
                } else {
                    j
                };
                let recv = code[recv_start..j].trim_end_matches(['.', '>', '-']);
                // Milestone 382: a path's last name, `Instant` of `tokio::time::Instant::now()`
                let last = recv
                    .trim_end_matches(':')
                    .rsplit([':', '.'])
                    .next()
                    .unwrap_or(recv);
                let monotonic_recv = matches!(
                    recv,
                    "time" | "performance" | "Instant" | "process" | "loop"
                ) || matches!(last, "Instant" | "steady_clock" | "Stopwatch");
                if !monotonic_recv {
                    hit = Some((key, recv_start, call_end(code, open)));
                }
            }
        } else if dotted {
            // an Erlang record's field: `S#state.last_run`
            let fname = if f.lang == Lang::Erlang {
                format!("#{name}")
            } else {
                name.to_string()
            };
            let key = lookup_names(f, &fname)
                .into_iter()
                .flat_map(|n| [file_field(&n, fi), n])
                .map(Key::Field)
                .find(|k| t.map.contains_key(k));
            if let Some(key) = key.filter(|_| !t.mixed.contains(name)) {
                let s0 = if f.lang == Lang::Erlang {
                    let r = code[..j - 1]
                        .trim_end_matches(is_ident)
                        .trim_end_matches('?');
                    let r = r.strip_suffix('#').unwrap_or(r);
                    r.trim_end_matches(is_ident).len()
                } else {
                    operand_left(code, dot)
                };
                hit = Some((key, s0, k));
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
        if let Some((key, mut s, mut e)) = hit {
            let adds = matches!(&key, Key::Call(n) if t.deadline_calls.contains(n));
            // Milestone 382: Go's `time.Time` with its monotonic reading is a wall-clock
            // reading only once converted: `tm.Unix()`, `timestamp.FromTime(t)`
            if t.map.get(&key).is_some_and(|o| o.weak) {
                match go_weak_converted(code, s, e) {
                    Some((s1, e1)) => {
                        s = s1;
                        e = e1;
                    }
                    None => {
                        j = k;
                        continue;
                    }
                }
            }
            if !out.iter().any(|x| x.start <= s && e <= x.end) {
                out.push(Span {
                    start: s,
                    end: e,
                    weak: false,
                    from: Err(key),
                    unit: "-",
                    adds,
                });
            }
        }
        j = k;
    }
    out
}

/// Milestone 382: the end of a chain after Go's `time.Now()` (from `at`) that drops the
/// monotonic reading, with its unit and whether it adds a duration: `.Add(d).UnixMilli()`,
/// `.Add(d).UTC()`. None for a chain that keeps it, or one that a source of the table covers.
fn go_wall_chain(code: &str, at: usize) -> Option<(usize, &'static str, bool)> {
    let mut e = at;
    let mut adds = false;
    loop {
        let rest = &code[e..];
        if rest.starts_with(".Add(") || rest.starts_with(".Truncate(") {
            adds |= rest.starts_with(".Add(");
            e = call_end(code, e + rest.find('(').unwrap_or(0));
            continue;
        }
        for (m, unit) in [
            (".UnixMilli()", "ms"),
            (".UnixMicro()", "us"),
            (".UnixNano()", "ns"),
            (".Unix()", "s"),
            (".UTC()", "-"),
            (".Round(0)", "-"),
        ] {
            if rest.starts_with(m) {
                // the plain forms are sources of the table
                return adds.then_some((e + m.len(), unit, adds));
            }
        }
        return None;
    }
}

/// Milestone 382: the span of a Go `time.Time` with its monotonic reading at `s..e` once a
/// conversion drops it: `tm.Unix()` and its kin after it, or `FromTime(...)` around it.
fn go_weak_converted(code: &str, s: usize, e: usize) -> Option<(usize, usize)> {
    for m in [
        ".UnixMilli()",
        ".UnixMicro()",
        ".UnixNano()",
        ".Unix()",
        ".UTC()",
        ".Round(0)",
    ] {
        if code[e..].starts_with(m) {
            return Some((s, e + m.len()));
        }
    }
    let before = code[..s].trim_end();
    if let Some(head) = before.strip_suffix("FromTime(") {
        if code[e..].trim_start().starts_with(')') {
            let start = operand_left(code, head.len() + "FromTime".len());
            return Some((start, e + code[e..].find(')').unwrap_or(0) + 1));
        }
    }
    None
}

/// Milestone 382: an Erlang match `Var = Expr`: the variable and where `Expr` starts. Not
/// `=:=`, `=/=`, `==`, `=<`, `>=`, `:=` or `=>`, and not a record's field (`#r{f = V}`).
fn erl_assignment(code: &str) -> Option<(String, bool, bool, usize)> {
    let b = code.as_bytes();
    for i in 0..b.len() {
        if b[i] != b'=' {
            continue;
        }
        let prev = if i > 0 { b[i - 1] } else { b' ' };
        let next = b.get(i + 1).copied().unwrap_or(b' ');
        if matches!(next, b'=' | b':' | b'/' | b'<' | b'>')
            || matches!(prev, b'=' | b':' | b'/' | b'<' | b'>' | b'!')
        {
            continue;
        }
        let lhs = code[..i].trim_end();
        let name: String = lhs
            .chars()
            .rev()
            .take_while(|c| is_ident(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let head = lhs[..lhs.len() - name.len()].trim_end();
        let var = name.starts_with(|c: char| c.is_ascii_uppercase());
        if !var || !(head.is_empty() || head.ends_with([',', '(', '>'])) {
            return None;
        }
        return Some((name, false, true, i + 1));
    }
    None
}

/// Milestone 382: the keys of Erlang maps and records that a line sets, each with where its
/// value starts: `#{ts => Ts}`, `M#{system_time := V}`, `#state{last_run = Now}`. A key
/// is kept as `#<key>`, apart from the fields of other languages.
fn erl_sets(code: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (k, v) in erl_pairs(code) {
        if v.set {
            out.push((format!("#{k}"), v.value));
        }
    }
    out
}

/// Milestone 382: the variables that an Erlang pattern binds from a key: `#{system_time :=
/// Now} = Meta` binds `Now` from `#system_time`, and a fold's clause `fun (ts, V, Acc) ->`
/// binds `V` from `#ts`.
fn erl_binds(code: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, v) in erl_pairs(code) {
        if v.bind {
            out.push((v.var, format!("#{k}")));
        }
    }
    for (p, _) in code.match_indices("fun (") {
        let rest = &code[p + 5..];
        let atom: String = rest.chars().take_while(|c| is_ident(*c)).collect();
        if !atom.starts_with(|c: char| c.is_ascii_lowercase()) {
            continue;
        }
        let rest = rest[atom.len()..].trim_start();
        let Some(rest) = rest.strip_prefix(',') else {
            continue;
        };
        let var: String = rest
            .trim_start()
            .chars()
            .take_while(|c| is_ident(*c))
            .collect();
        if var.starts_with(|c: char| c.is_ascii_uppercase()) {
            out.push((var, format!("#{atom}")));
        }
    }
    out
}

struct ErlPair {
    /// where the value starts
    value: usize,
    /// the value is set under the key
    set: bool,
    /// a pattern that may bind the bare variable `var` from the key
    bind: bool,
    var: String,
}

/// The `key => value`, `key := value` and record `field = value` pairs of an Erlang line.
fn erl_pairs(code: &str) -> Vec<(String, ErlPair)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    // the `#` that opens each brace: `#{` or `#name{`, and whether an expression comes before
    // it (an update) or not (a construction or a pattern)
    let mut opens: Vec<(bool, bool)> = Vec::new(); // (is a map or record, an update)
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => {
                let before = code[..i].trim_end_matches(is_ident).trim_end_matches('?');
                let rec = before.ends_with('#');
                let upd = rec && {
                    let pre = before[..before.len() - 1].trim_end();
                    pre.ends_with(|c: char| is_ident(c) || c == ')')
                };
                opens.push((rec, upd));
            }
            b'}' => {
                opens.pop();
            }
            b'=' | b':' if opens.last().is_some_and(|o| o.0) => {
                let op_len = match (b[i], b.get(i + 1)) {
                    (b'=', Some(b'>')) | (b':', Some(b'=')) => 2,
                    (b'=', _)
                        if !matches!(b.get(i + 1), Some(b'=' | b':' | b'/' | b'<'))
                            && !matches!(
                                code[..i].chars().next_back(),
                                Some('=' | '<' | '>' | '/' | ':' | '!')
                            ) =>
                    {
                        1
                    }
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                let key_text = code[..i].trim_end();
                let key: String = key_text
                    .chars()
                    .rev()
                    .take_while(|c| is_ident(*c))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let head = key_text[..key_text.len() - key.len()].trim_end();
                if key.len() >= 2
                    && key.starts_with(|c: char| c.is_ascii_lowercase())
                    && head.ends_with(['{', ','])
                {
                    let value = i + op_len;
                    let v = code[value..].trim_start();
                    let var: String = v.chars().take_while(|c| is_ident(*c)).collect();
                    let bare = var.starts_with(|c: char| c.is_ascii_uppercase())
                        && v[var.len()..].trim_start().starts_with([',', '}']);
                    // a construction or an update sets the key; a pattern (`#{k := V} = M`, a
                    // record in a head) binds a bare variable, and `#r{f = V}` may be either
                    let upd = opens.last().is_some_and(|o| o.1);
                    let arrow = b[i] == b'=' && op_len == 2;
                    let bind = bare && !upd && !arrow;
                    let set = arrow || upd || b[i] == b'=';
                    out.push((
                        key,
                        ErlPair {
                            value,
                            set,
                            bind,
                            var,
                        },
                    ));
                }
                i += op_len;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Milestone 382: the start of an Erlang call's module before `at`: `erlang:` of
/// `erlang:system_time(...)`, `?MODULE:`.
fn erl_qualified_start(code: &str, at: usize) -> usize {
    let b = code.as_bytes();
    if at >= 2 && b[at - 1] == b':' && b[at - 2] != b':' {
        let mut k = at - 1;
        while k > 0 && (is_ident(b[k - 1] as char) || b[k - 1] == b'?') {
            k -= 1;
        }
        if k < at - 1 {
            return k;
        }
    }
    at
}

/// The name an assignment on `code` sets, whether it is a field, whether it declares a new
/// variable, and where its right-hand side starts.
fn assignment(lang: Lang, code: &str) -> Option<(String, bool, bool, usize)> {
    if lang == Lang::Erlang {
        return erl_assignment(code);
    }
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

/// C++ casts that keep a time a time (Milestone 355): `std::chrono::duration_cast<Duration>(...)`
/// of a reading's `time_since_epoch()`, as a project's own wrapper of the wall clock returns it
/// (Kvrocks's `util::GetTimeStamp`).
const CPP_CASTS: &[&str] = &[
    "std::chrono::duration_cast<",
    "chrono::duration_cast<",
    "duration_cast<",
    "std::chrono::time_point_cast<",
    "time_point_cast<",
    "static_cast<",
];

/// The reading that the expression at `from` starts with, after spaces, parentheses, casts
/// (`(long long)`, C++'s `duration_cast<...>(`) and conversions (`int64(`).
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
        // Milestone 355: a C++ cast of one reading, not of a difference of two
        if let Some(c) = CPP_CASTS.iter().find(|c| code[i..].starts_with(**c)) {
            let lt = i + c.len() - 1;
            if let Some(gt) = template_end(code, lt) {
                if code[gt..].starts_with('(') {
                    let close = call_end(code, gt);
                    if !code[gt..close].contains(" - ") {
                        i = gt + 1;
                        continue;
                    }
                }
            }
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
        // Milestone 382: a time's own type made from a reading, as Restate's
        // `MillisSinceEpoch::new(SystemTime::now()...)`
        if let Some(n) = time_constructor(code, i) {
            if !sp.iter().any(|s| s.start == i) {
                i += n;
                continue;
            }
        }
        break;
    }
    sp.iter().find(|s| s.start == i && !s.weak)
}

/// Milestone 382: the length of a constructor of a time's own type at `at`, up to its `(`:
/// `MillisSinceEpoch::new(`, `Timestamp::from_millis(`, a type whose name speaks of time.
fn time_constructor(code: &str, at: usize) -> Option<usize> {
    let rest = &code[at..];
    let ty: String = rest.chars().take_while(|c| is_ident(*c)).collect();
    if ty.is_empty() || !ty.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let after = rest[ty.len()..].strip_prefix("::")?;
    let ctor: String = after.chars().take_while(|c| is_ident(*c)).collect();
    if !(ctor == "new" || ctor == "from" || ctor.starts_with("from_")) {
        return None;
    }
    if !after[ctor.len()..].starts_with('(') {
        return None;
    }
    let lty = ty.to_ascii_lowercase();
    has_any(&lty, TIME_NAME_WORDS).then_some(ty.len() + 2 + ctor.len() + 1)
}

/// Out-parameters that receive a reading: `gettimeofday(&tv, ...)`. Each is a name and whether
/// it is a field: `gettimeofday(&loop->_tv_at, NULL)` also sets the field `_tv_at` (Milestone
/// 382, h2o's event loop).
fn out_param(code: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
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
                out.push((name.clone(), false));
                let path: String = rest
                    .chars()
                    .take_while(|c| is_ident(*c) || matches!(c, '.' | '-' | '>'))
                    .collect();
                let last = path.rsplit(['.', '>']).next().unwrap_or("");
                if path.len() > name.len() && !last.is_empty() && last != name {
                    out.push((last.to_string(), true));
                }
                return out;
            }
        }
    }
    out
}

/// Milestone 382: the cell and the value of an atomic store on a line: Go's
/// `atomic.StoreInt64(&x.ts, v)` and `x.Store(v)`, Rust's and C++'s `x.store(v, ...)`.
fn atomic_store(code: &str) -> Option<(String, usize)> {
    if let Some(p) = code.find("atomic.Store") {
        let open = p + code[p..].find('(')?;
        let rest = &code[open + 1..];
        let comma = rest.find(',')?;
        let path = rest[..comma].trim();
        let cell: String = path
            .chars()
            .rev()
            .take_while(|c| is_ident(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if path.starts_with('&') && !cell.is_empty() {
            return Some((cell, open + 1 + comma + 1));
        }
    }
    for m in [".Store(", ".store("] {
        if let Some(p) = code.find(m) {
            let cell: String = code[..p]
                .chars()
                .rev()
                .take_while(|c| is_ident(*c))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !cell.is_empty() {
                return Some((cell, p + m.len()));
            }
        }
    }
    None
}

/// Milestone 382: the variable that a Go loop or receive takes from a ticker's or a timer's
/// channel: `for tm := range ticker.C`, `case now := <-t.C:`. The tick is `time.Now()` with
/// its monotonic reading.
fn go_tick(code: &str) -> Option<String> {
    let t = code.trim();
    let (lhs, rhs) = if let Some(r) = t.strip_prefix("for ") {
        let (l, r) = r.split_once(":= range ")?;
        (l, r)
    } else {
        let t = t.strip_prefix("case ").unwrap_or(t);
        let (l, r) = t.split_once(":= <-")?;
        (l, r)
    };
    let var = lhs.trim();
    let chan = rhs.trim_end_matches(['{', ':', ' ']).trim();
    let owner = chan.strip_suffix(".C")?;
    let last = owner
        .rsplit('.')
        .next()
        .unwrap_or(owner)
        .to_ascii_lowercase();
    (var.chars().all(is_ident)
        && !var.is_empty()
        && var != "_"
        && (last.contains("tick") || last.contains("timer")))
    .then(|| var.to_string())
}

/// Words of a function's name that say it formats, logs or tests a time rather than reading
/// the clock: such a function is followed only when its name speaks of time.
const NOT_CLOCK_FUNCTION_WORDS: &[&str] = &[
    "format",
    "string",
    "str",
    "display",
    "fmt",
    "log",
    "print",
    "debug",
    "serialize",
    "encode",
    "test",
    "mock",
    "fake",
    "main",
    "new",
    "default",
];

/// Milestone 382: whether a reading comes from Go's `time.Time` with its monotonic reading, once
/// converted: a ticker's tick, or `time.Now()` given to a function.
fn weak_chain(t: &Taint, from: &Result<&'static str, Key>) -> bool {
    let mut cur = from.clone();
    for _ in 0..4 {
        match cur {
            Err(ref k) => match t.map.get(k) {
                Some(o) if o.weak => return true,
                Some(o) => cur = o.from.clone(),
                None => return false,
            },
            Ok(_) => return false,
        }
    }
    false
}

/// Milestone 382: whether a reading comes from inside function `fk` of file `fi` without a hop
/// through another name: the clock, a cached clock's cell, or a variable of the function set
/// from one of them.
fn direct_reading(
    t: &Taint,
    from: &Result<&'static str, Key>,
    fi: usize,
    fk: usize,
    params: &[String],
) -> bool {
    let mut cur = from.clone();
    for _ in 0..4 {
        match cur {
            Ok(_) => return true,
            Err(Key::Field(c)) => return t.cells.contains(&c),
            // a parameter holds the caller's reading: the function converts, it reads no clock
            Err(Key::Local(_, _, ref n)) if params.contains(n) => return false,
            Err(ref key @ Key::Local(lfi, Some(lk), _)) if lfi == fi && lk == fk => {
                match t.map.get(key) {
                    Some(o) if !o.weak => cur = o.from.clone(),
                    _ => return false,
                }
            }
            Err(_) => return false,
        }
    }
    false
}

/// Words of a call that adds a duration to a time it is given (`nanoseconds_from(t, ns)`,
/// `timespec_add`, `plus`).
const ADDS_WORDS: &[&str] = &["from", "add", "plus", "after"];

fn propagate(files: &[SrcFile], t: &mut Taint) {
    // the functions of the project by name, to follow a reading into a parameter
    // Milestone 382: the project's own functions and the bundled code's apart, so that bundled
    // code changes neither the calls nor the twins of the project's code
    let mut groups: [HashMap<&str, Vec<(usize, usize)>>; 2] = [HashMap::new(), HashMap::new()];
    for (fi, f) in files.iter().enumerate() {
        for (k, fu) in f.funcs.iter().enumerate() {
            groups[usize::from(f.bundled.is_some())]
                .entry(fu.name.as_str())
                .or_default()
                .push((fi, k));
        }
    }
    let mut twins: [HashSet<&str>; 2] = [HashSet::new(), HashSet::new()];
    for (g, defs) in groups.iter().enumerate() {
        let monotonic_twin = &mut twins[g];
        for (name, ds) in defs {
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
    }
    for _round in 0..8 {
        let before = t.map.len() + t.mixed.len() + t.cells.len();
        for (fi, f) in files.iter().enumerate() {
            if f.lang == Lang::Sql {
                continue;
            }
            let group = usize::from(f.bundled.is_some());
            let defs = &groups[group];
            let monotonic_twin = &twins[group];
            for i in 0..f.code.len() {
                let code = &f.code[i];
                if code.trim().is_empty() {
                    continue;
                }
                for (name, field) in out_param(code) {
                    let key = if field {
                        if name.len() < 3 || GENERIC_FIELDS.contains(&name.as_str()) {
                            continue;
                        }
                        Key::Field(group_name(f, &name))
                    } else {
                        Key::Local(fi, f.scope[i], name)
                    };
                    t.map.entry(key).or_insert(Origin {
                        file: fi,
                        line: i,
                        from: Ok("gettimeofday("),
                        weak: false,
                    });
                }
                // Milestone 382: a Go ticker's tick, a reading once converted
                if f.lang == Lang::Go {
                    if let Some(v) = go_tick(code) {
                        t.map
                            .entry(Key::Local(fi, f.scope[i], v))
                            .or_insert(Origin {
                                file: fi,
                                line: i,
                                from: Ok("time.Now()"),
                                weak: true,
                            });
                    }
                }
                // Milestone 382: Erlang's maps and records carry a reading under a key (Ra's
                // command meta `#{ts => Ts}`), and a pattern takes it out again
                if f.lang == Lang::Erlang {
                    for (var, key) in erl_binds(code) {
                        let found = lookup_names(f, &key)
                            .into_iter()
                            .map(Key::Field)
                            .find(|k| t.map.contains_key(k));
                        if let Some(k) = found {
                            let weak = t.map.get(&k).is_some_and(|o| o.weak);
                            t.map
                                .entry(Key::Local(fi, f.scope[i], var))
                                .or_insert(Origin {
                                    file: fi,
                                    line: i,
                                    from: Err(k),
                                    weak,
                                });
                        }
                    }
                    let sets = erl_sets(code);
                    if !sets.is_empty() {
                        let sp = spans(f, fi, i, t);
                        for (key, at) in sets {
                            let Some(s) = starts_with_span(code, at, &sp) else {
                                continue;
                            };
                            if has_any(&code[at..s.end], MONOTONIC) || !hop_ok(t, &s.from, &key) {
                                continue;
                            }
                            t.map
                                .entry(Key::Field(group_name(f, &key)))
                                .or_insert(Origin {
                                    file: fi,
                                    line: i,
                                    from: s.from.clone(),
                                    weak: false,
                                });
                        }
                    }
                }
                // Milestone 382: an atomic store of a reading makes a cached clock
                if let Some((cell0, at)) = atomic_store(code) {
                    let cell = group_name(f, &cell0);
                    if cell0.len() >= 3
                        && !GENERIC_FIELDS.contains(&cell0.as_str())
                        && !t.cells.contains(&cell)
                    {
                        let sp = spans(f, fi, i, t);
                        if let Some(s) = starts_with_span(code, at, &sp) {
                            if !has_any(&code[at..], MONOTONIC) && hop_ok(t, &s.from, &cell0) {
                                let from = s.from.clone();
                                t.cells.insert(cell.clone());
                                t.map.entry(Key::Field(cell)).or_insert(Origin {
                                    file: fi,
                                    line: i,
                                    from,
                                    weak: false,
                                });
                            }
                        }
                    }
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
                        // Milestone 382: Go's `time.Now()` passed on keeps its monotonic
                        // reading until the function converts it; only into a function whose
                        // name speaks of time (`SetStartTime`), since most functions that take
                        // the time of a call stamp data with it
                        let go_now = f.lang == Lang::Go
                            && code[*a0..*a1].trim() == "time.Now()"
                            && timeish(name);
                        let (from, weak) = match starts_with_span(code, *a0, sp) {
                            Some(s) if s.end <= *a1 && !has_any(&code[*a0..*a1], MONOTONIC) => {
                                (s.from.clone(), false)
                            }
                            _ if go_now => (Ok("time.Now()"), true),
                            _ => continue,
                        };
                        for (dfi, dk) in ds {
                            let fu = &files[*dfi].funcs[*dk];
                            if fu.params.len() == args.len()
                                && (fu.params[k].len() >= 2 || weak && !fu.params[k].is_empty())
                                && hop_ok(t, &from, &fu.params[k])
                            {
                                let key = Key::Local(*dfi, Some(*dk), fu.params[k].clone());
                                t.map.entry(key).or_insert(Origin {
                                    file: fi,
                                    line: i,
                                    from: from.clone(),
                                    weak,
                                });
                            }
                        }
                    }
                }
                let Some((name, field, decl, rhs)) = assignment(f.lang, code) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                if field && name.len() >= 2 && has_any(&code[rhs..], MONOTONIC) {
                    t.mixed.insert(name.clone());
                    continue;
                }
                let sp = spans(f, fi, i, t);
                let Some(s) = rhs_reading(code, rhs, &sp) else {
                    continue;
                };
                // a name of one letter is too common to follow, but for a Go ticker's tick once
                // converted (Milestone 382: `t := uint64(tm.Unix())` of VictoriaMetrics's
                // `fasttime`)
                let tick = matches!(&s.from, Err(k) if t.map.get(k).is_some_and(|o| o.weak));
                if name.len() < 2 && (field || !tick) {
                    continue;
                }
                let origin = Origin {
                    file: fi,
                    line: i,
                    from: s.from.clone(),
                    weak: false,
                };
                let _ = decl;
                if !hop_ok(t, &s.from, &name) {
                    continue;
                }
                if field {
                    if name.len() >= 3 && !GENERIC_FIELDS.contains(&name.as_str()) {
                        let g = group_name(f, &name);
                        let key = if weak_chain(t, &s.from) {
                            file_field(&g, fi)
                        } else {
                            g
                        };
                        t.map.entry(Key::Field(key)).or_insert(origin);
                    }
                } else {
                    t.map
                        .entry(Key::Local(fi, f.scope[i], name))
                        .or_insert(origin);
                }
            }
            // small functions that return a reading: by data flow inside the function, and
            // with a looser reading when their name speaks of time
            for (fk, fu) in f.funcs.iter().enumerate() {
                if fu.end < fu.start || fu.end - fu.start > 30 || fu.name.len() < 3 {
                    continue;
                }
                let lname = fu.name.to_ascii_lowercase();
                let words = name_words(&fu.name);
                let time_named = has_any(&lname, TIME_NAME_WORDS);
                if lookup_names(f, &fu.name)
                    .into_iter()
                    .any(|n| t.map.contains_key(&Key::Call(n)))
                {
                    continue;
                }
                // Milestone 382: a function whose name does not speak of time is a wrapper of
                // the clock when the value it returns comes from a reading inside it; its name
                // must not say that it formats, logs or tests
                if !time_named
                    && (words
                        .iter()
                        .any(|w| NOT_CLOCK_FUNCTION_WORDS.contains(&w.as_str()))
                        || fu.name.len() < 4
                        || GENERIC_CALLS.contains(&fu.name.as_str())
                        || !matches!(
                            f.lang,
                            Lang::Go | Lang::Rust | Lang::C | Lang::Erlang | Lang::Elixir
                        ))
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
                        .trim_matches(['{', '}', ';', '.'])
                        .trim()
                        .is_empty()
                        && f.code[*k].trim() != "end"
                });
                // a tail expression that goes on over lines starts where its chain starts
                // (Milestone 350): `SystemTime::now()` over `.duration_since(UNIX_EPOCH)` and
                // `.as_millis() as u64`
                let tail = last.and_then(|l| {
                    let mut k = l;
                    while k > fu.start + 1 && f.code[k].trim_start().starts_with(['.', '?']) {
                        k -= 1;
                    }
                    // a chain that measures a duration is no reading
                    let chain = f.code[k..=l].join(" ");
                    let duration = has_any(&chain, &[".elapsed()", ".Sub(", "Since("])
                        || chain.contains(".duration_since(") && !chain.contains("UNIX_EPOCH");
                    (!duration).then_some(k)
                });
                // Erlang's and Elixir's last expression is the value, as Rust's and Ruby's
                let tail_lang = matches!(
                    f.lang,
                    Lang::Rust | Lang::Ruby | Lang::Erlang | Lang::Elixir
                );
                let mut found = None;
                let mut adds = false;
                for i in fu.start..=fu.end.min(f.code.len() - 1) {
                    let code = &f.code[i];
                    // `return <reading>`, or a last expression that is one (Rust, Ruby,
                    // Erlang, Elixir)
                    let at = match code.find("return ") {
                        Some(p) => Some(p + 7),
                        None if (Some(i) == last || Some(i) == tail)
                            && (i > fu.start || f.lang == Lang::Erlang)
                            && tail_lang =>
                        {
                            // an Erlang one-line function: the body after its `->`
                            match code.find("->") {
                                Some(p) if f.lang == Lang::Erlang && i == fu.start => Some(
                                    p + 2 + code[p + 2..].len() - code[p + 2..].trim_start().len(),
                                ),
                                _ => Some(code.len() - code.trim_start().len()),
                            }
                        }
                        None => None,
                    };
                    let Some(at) = at else { continue };
                    let mut sp = spans(f, fi, i, t);
                    // Milestone 382: a tail that closes a call opened on earlier lines
                    // (`MillisSinceEpoch::new(` over `SystemTime::now()` and `)`) is read as
                    // one statement, from its first line
                    let joined;
                    let mut code = code;
                    let mut at = at;
                    if f.lang == Lang::Rust
                        && Some(i) == last
                        && code.trim().starts_with(')')
                        && !code.contains("return ")
                    {
                        let mut k = i;
                        while k > fu.start + 1 {
                            let prev = f.code[k - 1].trim_end();
                            if prev.ends_with([';', '{', '}'])
                                || prev.is_empty() && k - 1 == fu.start
                            {
                                break;
                            }
                            k -= 1;
                        }
                        if k < i {
                            joined = f.code[k..=i]
                                .iter()
                                .map(|l| l.trim())
                                .collect::<Vec<_>>()
                                .join(" ");
                            sp = spans_in(f, fi, k, &joined, t);
                            code = &joined;
                            at = 0;
                        }
                    }
                    if let Some(s) = starts_with_span(code, at, &sp) {
                        if matches!(&s.from, Err(Key::Call(n)) if plain(n) == fu.name) {
                            continue;
                        }
                        // without a name about time, the reading must come from inside the
                        // function: the clock itself, a cached clock, or a variable of this
                        // function that holds one of them
                        let inside = direct_reading(t, &s.from, fi, fk, &fu.params);
                        // Milestone 382: Go's `time.Time` given to the function and converted
                        // there (`FromTime(t)` returns `t.UnixMilli()`) makes no clock of it
                        let weak_param = matches!(&s.from, Err(k @ Key::Local(..))
                            if t.map.get(k).is_some_and(|o| o.weak));
                        if (time_named || inside) && !weak_param {
                            adds = s.adds;
                            found = Some(Origin {
                                file: fi,
                                line: i,
                                from: s.from.clone(),
                                weak: false,
                            });
                            break;
                        }
                    }
                    // Milestone 382: a function named for time that returns a call adding a
                    // duration to a reading, `return nanoseconds_from(time, ns);`
                    if time_named {
                        let rest = &code[at..];
                        let callee: String = rest.chars().take_while(|c| is_ident(*c)).collect();
                        let cw = name_words(&callee);
                        if !callee.is_empty()
                            && rest[callee.len()..].starts_with('(')
                            && cw.iter().any(|w| ADDS_WORDS.contains(&w.as_str()))
                        {
                            let a0 = at + callee.len() + 1;
                            if let Some(s) = starts_with_span(code, a0, &sp) {
                                adds = true;
                                found = Some(Origin {
                                    file: fi,
                                    line: i,
                                    from: s.from.clone(),
                                    weak: false,
                                });
                                break;
                            }
                        }
                    }
                }
                if let Some(o) = found {
                    // a function that returns now plus a duration: `milliseconds_from_now(ms)`
                    let from_now = (words.iter().any(|w| w == "from" || w == "after")
                        && words.iter().any(|w| w == "now"))
                        || adds;
                    if from_now && f.lang != Lang::Sql {
                        t.deadline_calls.insert(group_name(f, &fu.name));
                    }
                    t.map.insert(Key::Call(group_name(f, &fu.name)), o);
                }
            }
        }
        if t.map.len() + t.mixed.len() + t.cells.len() == before {
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
    /// for a place of the shape `order` (Milestone 350): the name that keeps the reading as an
    /// order or a version of data, and where the code compares two values of it
    pub order: Option<Order>,
    /// Milestone 382: for a place of the shape `rewind`, the name of the earlier reading that the
    /// code stored and compares
    pub stored: Option<String>,
    /// Milestone 382: the bundled third-party library that holds the place (`h2o`, a Go module
    /// of `vendor/`), None for the project's own code
    pub bundled: Option<String>,
}

/// How a wall-clock reading orders data: the field or variable that keeps it, and the line
/// that compares two of its values (a merge, a maximum, a sort key).
#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    pub name: String,
    pub compared_at: Option<String>,
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
                    Key::Call(n) | Key::Field(n) | Key::Local(_, _, n) => {
                        plain(n).to_ascii_lowercase()
                    }
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
                    Key::Call(n) => format!("{}() returns a wall-clock reading ({at})", plain(n)),
                    Key::Field(n) => {
                        let n = plain(n).trim_start_matches('#');
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
    // Milestone 382: a reading that adds a duration itself, a deadline: Go's
    // `time.Now().Add(d).UnixMilli()`, braft's `butil::milliseconds_from_now(timeout)`
    if let Some(s) = sp.iter().find(|s| s.adds && !s.weak) {
        let conf = if has_any(&low, EXPIRY_WORDS) {
            "high"
        } else {
            "medium"
        };
        return Some(("deadline", conf, s.clone(), names));
    }
    // Milestone 382: a time compared with a wall-clock reading that the code stored earlier
    rewind_line(f, i, t, &sp).map(|s| ("rewind", "high", s, names))
}

/// Words of a timer's code: a timer wheel, a timer thread, an event loop, a scheduler.
const TIMER_WORDS: &[&str] = &[
    "timer",
    "timers",
    "timerwheel",
    "wheel",
    "tick",
    "ticker",
    "ticks",
    "evloop",
    "loop",
    "schedule",
    "scheduler",
    "sched",
    "cron",
    "clock",
    "expire",
    "expired",
    "timeout",
    "timeouts",
    "wake",
    "alarm",
    "deadline",
    "due",
];

/// Words of a stored reading that marks where accepted data starts: `startTimestamp`,
/// `minTime`, a watermark.
const HORIZON_WORDS: &[&str] = &[
    "start",
    "started",
    "min",
    "oldest",
    "horizon",
    "cutoff",
    "watermark",
    "since",
    "boundary",
    "begin",
];

/// Milestone 382: the stored side of a comparison of a time with a wall-clock reading that the
/// code stored earlier, a field set from a reading in another line:
///
/// - a new reading against it, in a timer's code: h2o's `if (now < ctx->last_run)`, which
///   finds no expired timer after a step back until the clock catches up;
/// - a timestamp of data against a stored reading that marks where accepted data starts:
///   Prometheus's `if s.T > w.startTimestamp`.
fn rewind_line(f: &SrcFile, i: usize, t: &Taint, sp: &[Span]) -> Option<Span> {
    let code = &f.code[i];
    let stored = |s: &Span| match &s.from {
        Err(Key::Field(n)) if !t.cells.contains(n) && !s.weak => {
            Some(plain(n).trim_start_matches('#').to_string())
        }
        _ => None,
    };
    let fresh = |s: &Span| {
        !s.weak
            && match &s.from {
                Ok(_) | Err(Key::Call(_)) | Err(Key::Local(..)) => true,
                Err(Key::Field(c)) => t.cells.contains(c),
            }
    };
    let any = |a: usize, b: usize| sp.iter().find(|s| s.start < b && a < s.end).cloned();
    let context: HashSet<String> = name_words(&format!(
        "{} {}",
        f.scope[i]
            .map(|s| f.funcs[s].name.clone())
            .unwrap_or_default(),
        f.rel
    ))
    .into_iter()
    .collect();
    let timer = context.iter().any(|w| TIMER_WORDS.contains(&w.as_str()));
    for (a, e) in comparisons(code) {
        if code[a..e].starts_with('.') {
            continue;
        }
        let (ls, le, rs, re) = (operand_left(code, a), a, e, operand_right(code, e));
        match (any(ls, le), any(rs, re)) {
            (Some(x), Some(y)) if timer => {
                if stored(&x).is_some() && fresh(&y) {
                    return Some(x);
                }
                if stored(&y).is_some() && fresh(&x) {
                    return Some(y);
                }
            }
            (Some(x), None) | (None, Some(x)) => {
                let Some(n) = stored(&x) else { continue };
                let other = if x.start >= rs {
                    &code[ls..le]
                } else {
                    &code[rs..re]
                };
                let last: String = other
                    .trim()
                    .chars()
                    .rev()
                    .take_while(|c| is_ident(*c))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let data_time = other.contains('.') && (last == "T" || order_name(&last));
                let horizon = name_words(&n)
                    .iter()
                    .any(|w| HORIZON_WORDS.contains(&w.as_str()));
                if data_time && horizon {
                    return Some(x);
                }
            }
            _ => {}
        }
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
    // a plain word (`timeout`, `delay`) of another file is another value of that name: only a
    // name of a constant or a setting is looked up in the other files (Milestone 350)
    let others = distinctive(name);
    for f in std::iter::once(&files[first]).chain(files.iter().filter(|_| others)) {
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

/// The names of the types of time and duration: a type is no limit (`Duration` of
/// `Duration::hours(24)` named Garage's `Duration` constant of another file, Milestone 350).
const TIME_TYPES: &[&str] = &[
    "Duration",
    "TimeDelta",
    "TimeSpan",
    "Instant",
    "SystemTime",
    "DateTime",
    "NaiveDateTime",
    "ChronoUnit",
    "TimeUnit",
    "Timeout",
];

/// Whether a name is a constant's or a setting's, so that another file's definition of it is
/// the same value: it has two words (`refreshMin`, `repl_min_replicas_max_lag`) or is written in
/// capitals (`TIMEOUT`).
fn distinctive(name: &str) -> bool {
    name.contains('_')
        || name.len() >= 4 && name.chars().all(|c| !c.is_ascii_lowercase())
        || name
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
}

/// A duration written as a literal on a Rust line, as the limit when no name holds one:
/// `Duration::hours(24)`, `Duration::from_secs(600)`.
fn literal_limit(code: &str) -> Option<Limit> {
    const UNITS: &[(&str, f64)] = &[
        ("::days(", 86400.0),
        ("::hours(", 3600.0),
        ("::minutes(", 60.0),
        ("::seconds(", 1.0),
        ("::milliseconds(", 1e-3),
        ("::from_secs(", 1.0),
        ("::from_millis(", 1e-3),
        ("::from_mins(", 60.0),
        ("::from_hours(", 3600.0),
    ];
    for (pat, unit) in UNITS {
        for (p, _) in code.match_indices(pat) {
            let n: String = code[p + pat.len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '_')
                .collect();
            let rest = &code[p + pat.len() + n.len()..];
            let Ok(v) = n.replace('_', "").parse::<f64>() else {
                continue;
            };
            if !rest.starts_with(')') || v <= 0.0 {
                continue;
            }
            let ty: String = code[..p]
                .chars()
                .rev()
                .take_while(|c| is_ident(*c))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Some(Limit {
                name: format!("{ty}{pat}{n})"),
                seconds: Some(v * unit),
                at: None,
            });
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
    t: &Taint,
) -> Option<Limit> {
    let f = &files[fi];
    // Milestone 355: a function that returns a wall-clock reading (`GetTimeStampMS`) is no limit
    let reader = |n: &str| t.map.contains_key(&Key::Call(n.to_string()));
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
        .filter(|n| {
            duration_name(n)
                && !computed.contains(*n)
                && !TIME_TYPES.contains(&n.as_str())
                && !reader(n)
        })
        .cloned()
        .collect();
    // the lines of the same function, nearest first
    if let Some(fu) = f.scope[i].map(|s| &f.funcs[s]) {
        let mut ks: Vec<usize> = (fu.start..=fu.end.min(f.code.len() - 1)).collect();
        ks.sort_by_key(|k| (*k as i64 - i as i64).abs());
        for k in ks.into_iter().take(60) {
            if has_comparison(&f.code[k]) || k == i {
                for n in other_names(&f.code[k], &[]) {
                    if duration_name(&n)
                        && !cands.contains(&n)
                        && !computed.contains(&n)
                        && !TIME_TYPES.contains(&n.as_str())
                        && !reader(&n)
                    {
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
    if f.lang == Lang::Rust {
        if let Some(l) = literal_limit(&f.code[i]) {
            return Some(l);
        }
    }
    first.map(|n| Limit {
        name: n,
        seconds: None,
        at: None,
    })
}

// ---------------------------------------------------------------- orders and versions

/// Words of a name that keeps an order or a version of data (Milestone 350).
const ORDER_WORDS: &[&str] = &[
    "timestamp",
    "ts",
    "version",
    "revision",
    "rev",
    "seq",
    "sequence",
    "lamport",
    "hlc",
    "mtime",
    "modified",
    "updated",
    "order",
];

/// Words of a name that keeps a time for a duration or a record, not an order: the duration
/// audit and nobody's merge read those.
const NOT_ORDER_WORDS: &[&str] = &[
    "created",
    "creation",
    "expire",
    "expires",
    "expiry",
    "expiration",
    "deadline",
    "ttl",
    "timeout",
    "lease",
    "heartbeat",
    "start",
    "started",
    "next",
    "retry",
    "backoff",
    "tombstone",
];

/// Whether a name keeps an order or a version: `timestamp`, `new_timestamp`, `ts`, `version`.
fn order_name(n: &str) -> bool {
    let ws = name_words(n);
    ws.iter().any(|w| ORDER_WORDS.contains(&w.as_str()))
        && !ws.iter().any(|w| NOT_ORDER_WORDS.contains(&w.as_str()))
}

/// The fields `.name` on a line that are not calls.
fn dotted_fields(code: &str) -> Vec<String> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for (p, _) in code.match_indices('.') {
        let mut k = p + 1;
        while k < b.len() && is_ident(b[k] as char) {
            k += 1;
        }
        if k == p + 1 || b[p + 1].is_ascii_digit() || code[k..].starts_with('(') {
            continue;
        }
        out.push(code[p + 1..k].to_string());
    }
    out
}

/// The names whose values the code compares with each other to order data, with the first
/// line that does: two values of one field compared or given to a maximum (`other.ts >
/// self.ts`, `max(a.version, b.version)`), a field that a sort or a maximum keys on
/// (`.map(|v| v.timestamp).max()`, `sort_by_key(|v| v.ts)`), and the fields of `self` in a
/// function named for an order or a sort key (`fn cmp_key(&self) { (self.timestamp, self.uuid)
/// }`).
fn ordered_names<'a>(files: impl Iterator<Item = &'a SrcFile>) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let ordering = [
        "max(",
        "min(",
        "cmp(",
        "partial_cmp(",
        "compare(",
        "Compare(",
        "compareTo(",
        "Before(",
        "After(",
    ];
    let keyed = [
        "sort_by_key(",
        "max_by_key(",
        "min_by_key(",
        ".max()",
        ".min()",
        "sortBy(",
        "orderBy(",
        "order_by(",
        "sorted(",
        "comparing(",
    ];
    for f in files {
        // a fuzz target or a benchmark compares what the product's code orders, so it is no
        // place to show
        let low = f.rel.to_ascii_lowercase();
        if f.lang == Lang::Sql
            || low
                .split('/')
                .any(|d| d.starts_with("fuzz") || d == "benches")
        {
            continue;
        }
        for (i, code) in f.code.iter().enumerate() {
            if !code.contains('.') {
                continue;
            }
            let fields = dotted_fields(code);
            if fields.is_empty() {
                continue;
            }
            let at = format!("{}:{}", f.rel, i + 1);
            let compares = has_comparison(code) || has_any(code, &ordering);
            let keys = has_any(code, &keyed);
            let in_key_fn = f.scope[i].is_some_and(|s| {
                let ws = name_words(&f.funcs[s].name);
                ws.iter()
                    .any(|w| matches!(w.as_str(), "cmp" | "compare" | "order" | "sort"))
                    && ws.iter().any(|w| w == "key")
            }) && code.contains("self.");
            for (k, fd) in fields.iter().enumerate() {
                if fd.len() < 2 || !order_name(fd) {
                    continue;
                }
                let twice = fields[k + 1..].contains(fd);
                if (twice && compares) || keys || in_key_fn {
                    out.entry(fd.clone()).or_insert_with(|| at.clone());
                }
            }
        }
    }
    out
}

/// Whether the reading at `at` is kept increasing over an earlier value (`max(prev + 1,
/// now_msec())`): the order that a step back cannot break on one node.
fn guarded(code: &str, at: usize) -> bool {
    for (p, _) in code.match_indices("max(") {
        let open = p + 3;
        let close = call_end(code, open);
        if open < at && at < close && code[open..close].contains(',') {
            return true;
        }
    }
    false
}

/// The name a line stores a value under, and where the value starts: an assignment
/// (`let new_timestamp = ...`, `x.version = ...`, Python's `ts=...`) or a field of a struct
/// literal (`timestamp: now_msec(),`).
fn stored_as(lang: Lang, code: &str) -> Option<(String, bool, usize)> {
    if let Some((name, field, _, rhs)) = assignment(lang, code) {
        return Some((name, field, rhs));
    }
    let t = code.trim_start();
    let c = t.find(':')?;
    if t[c..].starts_with("::") || !t[c + 1..].starts_with(' ') {
        return None;
    }
    let key = &t[..c];
    if key.is_empty() || !key.chars().all(is_ident) {
        return None;
    }
    Some((key.to_string(), true, code.len() - t.len() + c + 1))
}

/// A place that keeps a wall-clock reading as the order or the version of data: the reading is
/// the value of a name about an order (`timestamp`, `version`), or of a variable that the same
/// function then stores in such a field, and it is not kept increasing over the earlier value.
/// The place is `high` when the code compares two values of the name.
fn order_line(
    files: &[SrcFile],
    fi: usize,
    i: usize,
    t: &Taint,
    ordered: &HashMap<String, String>,
) -> Option<(Span, Order)> {
    let f = &files[fi];
    if f.lang == Lang::Sql {
        return None;
    }
    let code = &f.code[i];
    if has_any(&words_of(code), REPORT_WORDS) {
        return None;
    }
    let (name, field, rhs) = stored_as(f.lang, code)?;
    // Go's `time.Now()` is a wall-clock reading once stored: its monotonic part is not
    let sp: Vec<Span> = spans(f, fi, i, t)
        .into_iter()
        .map(|mut s| {
            s.weak = false;
            s
        })
        .collect();
    let s = rhs_reading(code, rhs, &sp)?.clone();
    // the clock read here, or through a function that returns its reading: a field or a
    // variable that holds one is followed by the duration audit's guesses, too loose to name
    // an order (`Version: row.Version` after one `Version` set from a clock somewhere)
    if guarded(code, s.start) || matches!(s.from, Err(Key::Field(_) | Key::Local(..))) {
        return None;
    }
    // the clock now: not a time made from a stored value (`time.Unix(0, 0)`), not a getter
    // the duration audit follows (`GetVisibilityTime()`), and no text for a log
    let now = match &s.from {
        Ok(pat) => !pat.starts_with("time.Unix") && !has_any(pat, &["from", "parse", "Parse"]),
        Err(Key::Call(n)) => name_words(n)
            .iter()
            .any(|w| matches!(w.as_str(), "now" | "clock" | "timestamp" | "current")),
        Err(_) => false,
    };
    let formatted = has_any(
        &code[s.start..],
        &[
            ".Format(",
            ".format(",
            "strftime",
            "isoformat",
            "to_rfc3339",
            "toISOString",
            ".String()",
            "to_string()",
        ],
    );
    if !now || formatted {
        return None;
    }
    // a variable: the fields the same function stores it in (`timestamp: new_timestamp,`)
    let mut names = Vec::new();
    if !field {
        if let Some(sc) = f.scope[i] {
            let end = f.funcs[sc].end.min(f.code.len() - 1);
            for k in i + 1..=end {
                let c = &f.code[k];
                for (p, _) in c.match_indices(name.as_str()) {
                    let e = p + name.len();
                    let whole = (p == 0 || !is_ident(c.as_bytes()[p - 1] as char))
                        && c[e..].chars().next().is_none_or(|x| !is_ident(x));
                    if !whole || !c[e..].trim_start().starts_with([',', '}', ';', ')']) {
                        continue;
                    }
                    if let Some((n, _, r)) = stored_as(f.lang, c) {
                        if r <= p && c[r..p].trim().is_empty() && !names.contains(&n) {
                            names.push(n);
                        }
                    }
                }
            }
        }
    }
    names.push(name);
    let pick = names
        .iter()
        .find(|n| order_name(n) && ordered.contains_key(*n))
        .or_else(|| names.iter().find(|n| order_name(n)))?;
    Some((
        s,
        Order {
            name: pick.clone(),
            compared_at: ordered.get(pick).cloned(),
        },
    ))
}

// ---------------------------------------------------------------- the audit

/// Audits every source file under `root`: the matches in path order, and how many files were read.
/// What the audit read and found.
#[derive(Clone, Debug, Default)]
pub struct Audit {
    /// The source files read.
    pub read: usize,
    pub matches: Vec<ClockMatch>,
    /// The places that keep a reading as an order or a version of data (Milestone 350).
    pub orders: Vec<ClockMatch>,
    /// The test code left out of those files (Milestone 324).
    pub tests: TestItems,
    /// Milestone 382: the places of the bundled third-party code, both kinds
    pub bundled: Vec<ClockMatch>,
    /// the files of bundled code read, and their libraries
    pub bundled_read: usize,
    pub bundled_libs: Vec<String>,
}

/// Folders of bundled third-party code (Milestone 382): Go's and Rust's `vendor/`, C and C++'s
/// `third_party/`, and `deps/` (Valkey's libraries, the dependencies that erlang.mk fetches).
const BUNDLED_DIRS: &[&str] = &[
    "vendor",
    "third_party",
    "third-party",
    "thirdparty",
    "3rdparty",
    "deps",
];

/// Folders that the clock audit never reads: a build's outputs and caches.
const CLOCK_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".hydo",
    "hydo-out",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    "coverage",
    ".cache",
];

/// One file the clock audit reads: its path from the root, the path that decides whether it is
/// a test or an example (from the library's root for bundled code), and its library.
struct ClockFile {
    path: std::path::PathBuf,
    rel: String,
    judged: String,
    bundled: Option<String>,
}

/// The folders under `deps/` that the root's `.gitignore` keeps although it ignores `deps/*`:
/// RabbitMQ's own applications beside the ones that erlang.mk fetches.
fn kept_deps(root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(text) = std::fs::read_to_string(root.join(".gitignore")) else {
        return out;
    };
    let ignores = text
        .lines()
        .any(|l| matches!(l.trim(), "/deps/*" | "deps/*" | "/deps/" | "deps/"));
    if !ignores {
        return out;
    }
    for l in text.lines() {
        if let Some(rest) = l
            .trim()
            .strip_prefix("!/deps/")
            .or_else(|| l.trim().strip_prefix("!deps/"))
        {
            let name = rest.trim_end_matches('/');
            if !name.is_empty() && !name.contains('/') {
                out.insert(name.to_string());
            }
        }
    }
    out
}

fn sorted_entries(dir: &Path) -> Vec<std::fs::DirEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    ents.sort_by_key(|e| e.file_name());
    ents
}

/// Every file the clock audit reads under `root`, in path order: the project's own code, and
/// (Milestone 382) the bundled code of `vendor/`, `third_party/`, `deps/` and of Bazel's
/// `bazel-<name>/external/`, each library by its name.
fn clock_files(root: &Path) -> Vec<ClockFile> {
    let kept = kept_deps(root);
    let mut out = Vec::new();
    clock_walk(root, root, root, None, &kept, &mut out);
    out
}

fn clock_walk(
    root: &Path,
    dir: &Path,
    judge_root: &Path,
    lib: Option<&str>,
    kept: &HashSet<String>,
    out: &mut Vec<ClockFile>,
) {
    for e in sorted_entries(dir) {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_file() {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            let judged = p
                .strip_prefix(judge_root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(ClockFile {
                path: p,
                rel,
                judged,
                bundled: lib.map(String::from),
            });
            continue;
        }
        // Bazel's output folder at the root is a link: its `external/` holds the fetched
        // repositories, each a library
        if dir == root && lib.is_none() && name.starts_with("bazel-") {
            let ext = p.join("external");
            if ext.is_dir() {
                for c in sorted_entries(&ext) {
                    let cp = c.path();
                    if cp.is_dir() {
                        let cname = c.file_name().to_string_lossy().into_owned();
                        clock_walk(root, &cp, &cp, Some(&cname), kept, out);
                    }
                }
            }
            continue;
        }
        if !ft.is_dir() || CLOCK_SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        if lib.is_none() && BUNDLED_DIRS.contains(&name.as_str()) {
            for c in sorted_entries(&p) {
                let cp = c.path();
                if !c.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let cname = c.file_name().to_string_lossy().into_owned();
                if name == "deps" && kept.contains(&cname) {
                    // the project's own application under `deps/`
                    clock_walk(root, &cp, &p, None, kept, out);
                    continue;
                }
                // Go's `vendor/github.com/org/module`: the module's path names the library
                if cname.contains('.') {
                    for org in sorted_entries(&cp) {
                        if !org.file_type().is_ok_and(|t| t.is_dir()) {
                            continue;
                        }
                        for m in sorted_entries(&org.path()) {
                            if !m.file_type().is_ok_and(|t| t.is_dir()) {
                                continue;
                            }
                            let lname = format!(
                                "{cname}/{}/{}",
                                org.file_name().to_string_lossy(),
                                m.file_name().to_string_lossy()
                            );
                            let mp = m.path();
                            clock_walk(root, &mp, &mp, Some(&lname), kept, out);
                        }
                    }
                    continue;
                }
                clock_walk(root, &cp, &cp, Some(&cname), kept, out);
            }
            continue;
        }
        clock_walk(root, &p, judge_root, lib, kept, out);
    }
}

pub fn audit(root: &Path) -> Audit {
    let mut own: Vec<SrcFile> = Vec::new();
    let mut bundled: Vec<SrcFile> = Vec::new();
    for cf in clock_files(root) {
        let Some(lang) = clock_lang_of(&cf.rel) else {
            continue;
        };
        if skipped(&cf.judged.to_ascii_lowercase()) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&cf.path) else {
            continue;
        };
        if meta.len() > 1_500_000 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&cf.path) else {
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if generated(&text) {
            continue;
        }
        let mut sf = source_file(cf.rel, lang, &text);
        if cf.bundled.is_some() {
            sf.bundled = cf.bundled;
            bundled.push(sf);
        } else {
            own.push(sf);
        }
    }
    let mut tests = TestItems::default();
    for f in &own {
        tests.functions += f.tests.functions;
        tests.modules += f.tests.modules;
    }
    let read = own.len();
    let bundled_read = bundled.len();
    let mut bundled_libs: Vec<String> = Vec::new();
    for f in &bundled {
        if let Some(l) = &f.bundled {
            if !bundled_libs.contains(l) {
                bundled_libs.push(l.clone());
            }
        }
    }
    // the project's own files first, so that their readings keep their origins
    let mut files = own;
    files.extend(bundled);
    let (all, all_orders) = audit_files(&files);
    let (mut matches, mut orders, mut bundled) = (Vec::new(), Vec::new(), Vec::new());
    for m in all {
        if m.bundled.is_some() {
            bundled.push(m);
        } else {
            matches.push(m);
        }
    }
    for m in all_orders {
        if m.bundled.is_some() {
            bundled.push(m);
        } else {
            orders.push(m);
        }
    }
    Audit {
        read,
        matches,
        orders,
        tests,
        bundled,
        bundled_read,
        bundled_libs,
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
        bundled: None,
    }
}

/// Audits files given as (path, text): the unit tests' entry.
#[cfg(test)]
pub fn audit_texts(texts: &[(&str, &str)]) -> Vec<ClockMatch> {
    audit_both(texts).0
}

/// Audits files given as (path, text): the durations and the orders.
#[cfg(test)]
pub fn audit_both(texts: &[(&str, &str)]) -> (Vec<ClockMatch>, Vec<ClockMatch>) {
    let files: Vec<SrcFile> = texts
        .iter()
        .filter_map(|(rel, text)| clock_lang_of(rel).map(|l| source_file(rel.to_string(), l, text)))
        .collect();
    audit_files(&files)
}

/// The places of both kinds: the durations, and the orders and versions (Milestone 350).
fn audit_files(files: &[SrcFile]) -> (Vec<ClockMatch>, Vec<ClockMatch>) {
    let mut t = Taint::default();
    propagate(files, &mut t);
    // the project's own files decide its main languages and the names it orders by; bundled
    // code (Milestone 382) changes neither
    let own: Vec<&SrcFile> = files.iter().filter(|f| f.bundled.is_none()).collect();
    let ordered = ordered_names(files.iter().filter(|f| f.bundled.is_none()));
    let ordered_all = ordered_names(files.iter());
    let mut out = Vec::new();
    let mut orders = Vec::new();
    // the main languages: at least a fifth of the source files that are not SQL
    let code_files = own.iter().filter(|f| f.lang != Lang::Sql).count().max(1);
    let main: HashSet<Lang> = own
        .iter()
        .map(|f| f.lang)
        .filter(|l| {
            *l != Lang::Sql && own.iter().filter(|f| f.lang == *l).count() * 5 >= code_files
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
                        order: None,
                        stored: None,
                        bundled: f.bundled.clone(),
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
                let limit = find_limit(files, fi, i, &names, reading_unit(&t, &span), &t);
                let stored = match (&span.from, shape) {
                    (Err(Key::Field(n)), "rewind") => {
                        Some(plain(n).trim_start_matches('#').to_string())
                    }
                    _ => None,
                };
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
                    order: None,
                    stored,
                    bundled: f.bundled.clone(),
                });
            } else if let Some((span, order)) = order_line(
                files,
                fi,
                i,
                &t,
                if f.bundled.is_some() {
                    &ordered_all
                } else {
                    &ordered
                },
            ) {
                orders.push(ClockMatch {
                    file: f.rel.clone(),
                    line: i + 1,
                    lang: f.lang.name(),
                    text,
                    shape: "order",
                    confidence: if order.compared_at.is_some() {
                        "high"
                    } else {
                        "medium"
                    },
                    function,
                    origin: describe_origin(files, &t, &span.from),
                    main_lang: main.contains(&f.lang),
                    limit: None,
                    order: Some(order),
                    stored: None,
                    bundled: f.bundled.clone(),
                });
            }
        }
    }
    (out, orders)
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
            match (db, only) {
                (Some(db), _) => ("database", db, "the database is killed and comes back, so that its clients connect again"),
                // Milestone 363: hydo.yaml has no database, but names the one service that runs
                // the place (NSQ's nsqlookupd in the 0.6.0 trial), so the schedule names it
                (None, Some(svc)) => ("service", svc, "the service is killed and comes back, so that its connections are made again"),
                (None, None) => ("database", "<database>".into(), "the database is killed and comes back, so that its clients connect again"),
            }
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
    if m.shape == "order" {
        return order_proposals(m, svcs);
    }
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
/// The schedules aimed at a place that orders data by the wall clock (Milestone 350): the
/// writes run in every workload step, so a step back alone makes a later write older than the
/// writes before it. A short step, a long one, and a step while the system recovers from a
/// fault.
fn order_proposals(m: &ClockMatch, svcs: &Services) -> (String, Vec<Proposal>) {
    // the fault restarts the service that writes, whatever the words of the code say
    let who = services_running(m, svcs)
        .filter(|v| v.len() == 1)
        .map(|v| v[0].clone())
        .unwrap_or_else(|| "<service>".into());
    let steps = svcs.steps.unwrap_or(20).max(4);
    let mid = steps / 2;
    let (a, n) = if steps >= 8 { (2, 3) } else { (1, 1) };
    let after = (a + n + 5).min(steps - 1).max(a + n);
    let fault = format!("K={who}@{a}+{n}");
    let fault_words = format!(
        "{who} is killed before step {a} and starts again before step {}",
        a + n
    );
    let older = "a write after it gets an older version than the writes before it";
    (
        "the code runs at every write, so the step back alone makes a later write older".into(),
        vec![
            Proposal {
                name: "back",
                schedule: format!("disk=none | run T={mid}:-300"),
                says: format!("the clock goes back by 5 minutes before step {mid}; {older}"),
            },
            Proposal {
                name: "back-long",
                schedule: format!("disk=none | run T={mid}:-3600"),
                says: format!(
                    "the clock goes back by 1 hour before step {mid}, longer than the time between two writes of one key; {older}"
                ),
            },
            Proposal {
                name: "back-after",
                schedule: format!("disk=none | run {fault} T={after}:-300"),
                says: format!(
                    "{fault_words}; the clock goes back by 5 minutes before step {after}, while the system recovers and writes again"
                ),
            },
        ],
    )
}

pub fn what_it_does(m: &ClockMatch) -> String {
    if let Some(o) = &m.order {
        return match &o.compared_at {
            Some(at) => format!(
                "It keeps a wall-clock reading as {}, and the code orders data by comparing two values of it ({at}). After a step back, a later write gets an older {} and can lose to the writes before it.",
                o.name, o.name
            ),
            None => format!(
                "It keeps a wall-clock reading as {}, a name of an order or a version; no comparison of two of its values was found.",
                o.name
            ),
        };
    }
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
        // Milestone 382
        "rewind" => format!(
            "It compares a time with {}, a wall-clock reading that the code stored earlier. After a step back, new readings and new data are older than it, so the comparison goes the other way until the clock catches up.",
            m.stored.as_deref().unwrap_or("an earlier reading")
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
        if m.bundled.is_some() {
            return (false, -rank_score(m, svcs) - timer_bonus(m));
        }
        let none = svcs.map.run_by(&m.file).is_some_and(|w| w.is_empty());
        (none, -rank_score(m, svcs))
    });
    out
}

/// Milestone 382: among the places of bundled code, a library's timers come first: a timer
/// wheel, a timer thread, an event loop or a scheduler, whose every timer a step moves.
fn timer_bonus(m: &ClockMatch) -> i32 {
    let words = name_words(&format!(
        "{} {}",
        m.function.clone().unwrap_or_default(),
        m.file
    ));
    if words.iter().any(|w| TIMER_WORDS.contains(&w.as_str())) {
        4
    } else {
        0
    }
}

/// How many places the text report shows without `--all`.
pub const TOP: usize = 5;

pub fn match_json(m: &ClockMatch) -> serde_json::Value {
    let mut v = serde_json::json!({
        "file": m.file,
        "line": m.line,
        "lang": m.lang,
        "text": m.text,
        "shape": m.shape,
        "confidence": m.confidence,
        "function": m.function,
        "origin": m.origin,
        "limit": m.limit.as_ref().map(|l| serde_json::json!({"name": l.name, "seconds": l.seconds, "at": l.at})),
    });
    if let Some(o) = &m.order {
        v["order"] = serde_json::json!({"name": o.name, "compared_at": o.compared_at});
    }
    if let Some(n) = &m.stored {
        v["stored"] = n.clone().into();
    }
    if let Some(l) = &m.bundled {
        v["bundled"] = l.clone().into();
    }
    v
}

/// The places of one kind as JSON, each target with its rank and schedules.
fn places_json(matches: &[ClockMatch], svcs: &Services) -> Vec<serde_json::Value> {
    let t: HashMap<(String, usize), usize> = targets(matches, svcs)
        .iter()
        .enumerate()
        .map(|(k, m)| ((m.file.clone(), m.line), k + 1))
        .collect();
    matches
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
        .collect()
}

pub fn json_report(dir: &str, a: &Audit, svcs: &Services) -> String {
    let mut doc = serde_json::json!({
        "dir": dir,
        "files_read": a.read,
        "clock": places_json(&a.matches, svcs),
        "clock_orders": places_json(&a.orders, svcs),
    });
    doc["tests_left_out"] =
        serde_json::json!({"functions": a.tests.functions, "modules": a.tests.modules});
    if !svcs.names.is_empty() {
        doc["services_run"] = svcs.map.words().into();
    }
    // Milestone 382: the bundled code, and the fallback schedules
    if a.bundled_read > 0 {
        doc["bundled_files_read"] = a.bundled_read.into();
        doc["bundled_libraries"] = a.bundled_libs.clone().into();
        doc["clock_bundled"] = places_json(&a.bundled, svcs).into();
    }
    if targets(&a.matches, svcs).is_empty() && targets(&a.orders, svcs).is_empty() {
        doc["fallback"] = fallback(svcs)
            .iter()
            .map(|p| serde_json::json!({"name": p.name, "schedule": p.schedule, "says": p.says}))
            .collect();
    }
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// Milestone 382: the schedules for a project in whose own code the audit found no place.
/// They aim at no place: at the paths that most systems time with the wall clock, a restart's
/// retries and leases, data kept for a time, sessions and timeouts, and an election.
pub fn fallback(svcs: &Services) -> Vec<Proposal> {
    let steps = svcs.steps.unwrap_or(20).max(4);
    let (a, n) = if steps >= 8 { (2, 3) } else { (1, 1) };
    let after = (a + n + 5).min(steps - 1).max(a + n);
    let mid = steps / 2;
    let mut out = Vec::new();
    let names: Vec<String> = if svcs.names.is_empty() {
        vec!["<service>".into()]
    } else {
        svcs.names.iter().take(8).cloned().collect()
    };
    for svc in &names {
        out.push(Proposal {
            name: "restart-back",
            schedule: format!("disk=none | run K={svc}@{a}+{n} T={after}:-300"),
            says: format!(
                "{svc} is killed before step {a} and starts again before step {}; the clock goes back by 5 minutes before step {after}, while its retries, leases and timers run",
                a + n
            ),
        });
    }
    for g in &svcs.primary_groups {
        out.push(Proposal {
            name: "back-failover",
            schedule: format!("disk=none | run T={after}:-300 K={g}.primary@{}+{n}", after + 1),
            says: format!(
                "the clock goes back by 5 minutes before step {after}; then the primary of {g} is killed, so that an election timer on the wall clock waits the step"
            ),
        });
    }
    out.push(Proposal {
        name: "back-long",
        schedule: format!("disk=none | run T={mid}:-86400"),
        says: format!(
            "the clock goes back by 1 day before step {mid}: data kept for a time, expiries, and work scheduled at a time"
        ),
    });
    out.push(Proposal {
        name: "forward",
        schedule: format!("disk=none | run T={a}:3600"),
        says: format!(
            "the clock goes forward by 1 hour before step {a}: leases, sessions and timeouts end at once"
        ),
    });
    out
}

/// One place of the text report, with its schedules; `same` holds the places of its kind.
fn place_text(o: &mut String, k: usize, m: &ClockMatch, same: &[ClockMatch], svcs: &Services) {
    use std::fmt::Write;
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
    let run_by = if let Some(l) = &m.bundled {
        let _ = writeln!(o, "   Bundled code of {l}.");
        None
    } else {
        svcs.map.run_by(&m.file)
    };
    match run_by {
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
    let more: Vec<String> = same
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

/// The places of one kind: the targets shown, one line for the rest without `--all`, and with
/// `--all` the medium places. Returns the targets shown.
fn kind_text<'a>(
    o: &mut String,
    places: &'a [ClockMatch],
    svcs: &Services,
    max: usize,
    all: bool,
    medium_words: &str,
) -> Vec<&'a ClockMatch> {
    use std::fmt::Write;
    let ts = targets(places, svcs);
    let shown = if all { max } else { TOP.min(max) };
    for (k, m) in ts.iter().take(shown).enumerate() {
        place_text(o, k, m, places, svcs);
    }
    let rest: Vec<&ClockMatch> = places.iter().filter(|m| m.confidence != "high").collect();
    if !all {
        // one per function: the functions not shown, with a high place or only medium ones
        let functions: HashSet<(String, String)> = places
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
            let _ = writeln!(o, "\nMedium confidence ({medium_words}; read the code before you aim a schedule at it):");
            for m in rest.iter().take(max) {
                let _ = writeln!(o, "  {}:{}: {}", m.file, m.line, m.text);
            }
            if rest.len() > max {
                let _ = writeln!(o, "  ... {} more", rest.len() - max);
            }
        }
    }
    ts.into_iter().take(shown).collect()
}

pub fn text_report(dir: &str, a: &Audit, svcs: &Services, max: usize, all: bool) -> String {
    use std::fmt::Write;
    let (read, matches) = (a.read, &a.matches[..]);
    let mut o = String::new();
    let ts = targets(matches, svcs);
    let ords = targets(&a.orders, svcs);
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
    let mut shown: Vec<&ClockMatch> = Vec::new();
    if ts.is_empty() {
        let _ = writeln!(
            o,
            "\nNo lease, timeout, throttle, lag or delay was found that reads the wall clock."
        );
    } else {
        let _ = writeln!(
            o,
            "\nNTP or an operator can step the wall clock. A duration measured on it then lasts longer\nby the size of a step back, or ends early after a step forward. The most likely places first:"
        );
        shown.extend(kind_text(
            &mut o,
            matches,
            svcs,
            max,
            all,
            "the words suggest a deadline",
        ));
    }
    // the orders and versions of data (Milestone 350)
    if !ords.is_empty() {
        let _ = writeln!(
            o,
            "\n{} {} a wall-clock reading as the order or the version of stored data.\nAfter a step back, a later write gets an older version than the writes before it, so it can\nlose to them. The most likely places first:",
            ords.len(),
            if ords.len() == 1 { "function keeps" } else { "functions keep" }
        );
        shown.extend(kind_text(
            &mut o,
            &a.orders,
            svcs,
            max,
            all,
            "the name speaks of a version, but no comparison of two of its values was found",
        ));
    }
    // Milestone 382: the bundled third-party code, after the project's own places
    let bts = targets(&a.bundled, svcs);
    if a.bundled_read > 0 {
        let libs = and_words(&a.bundled_libs.iter().take(6).cloned().collect::<Vec<_>>());
        let more = a.bundled_libs.len().saturating_sub(6);
        let _ = writeln!(
            o,
            "\nThe audit also read {} files of bundled code: {libs}{}.",
            a.bundled_read,
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        );
        if !bts.is_empty() {
            let _ = writeln!(
                o,
                "{} {} of the bundled code {} the wall clock. The services run this code too; its places\ncome after the project's own, a library's timers first:",
                bts.len(),
                if bts.len() == 1 { "function" } else { "functions" },
                if bts.len() == 1 { "times with" } else { "time with" }
            );
            shown.extend(kind_text(
                &mut o,
                &a.bundled,
                svcs,
                max,
                all,
                "the words suggest a deadline",
            ));
        }
    }
    if ts.is_empty() && ords.is_empty() {
        let fb = fallback(svcs);
        let _ = writeln!(
            o,
            "\nThe audit found no place in the project's own code. These schedules are a fallback, aimed at\nno place: at the paths that most systems time with the wall clock."
        );
        for p in &fb {
            let _ = writeln!(o, "     {:<14} {}", p.name, p.schedule);
            let _ = writeln!(o, "     {:<14} {}", "", p.says);
        }
        if fb.iter().any(|p| p.schedule.contains('<')) {
            let _ = writeln!(
                o,
                "Replace <service> with a service of hydo.yaml, or pass --manifest FILE."
            );
        }
    }
    if shown.is_empty() {
        return o;
    }
    let placeholders = shown.iter().any(|m| {
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

    /// Milestone 355: a C++ project's own wrapper of the wall clock, as Kvrocks's
    /// `util::GetTimeStamp` (a template that casts `system_clock::now()`, and its milliseconds
    /// twin), makes its callers' readings: finding 2 of the Kvrocks example's place is found,
    /// and the wrapper is no limit.
    #[test]
    fn cpp_follows_a_projects_own_wrapper_of_the_wall_clock() {
        let util = "#pragma once\n#include <chrono>\nnamespace util {\n\
            template <typename Duration = std::chrono::seconds>\n\
            auto GetTimeStamp() {\n  return std::chrono::duration_cast<Duration>(std::chrono::system_clock::now().time_since_epoch()).count();\n}\n\
            inline uint64_t GetTimeStampMS() { return GetTimeStamp<std::chrono::milliseconds>(); }\n\
            }  // namespace util\n";
        let repl = "void ReplicationThread::sendReplConfAck(bufferevent *bev, bool force) {\n\
            \x20 int64_t now = util::GetTimeStamp();\n\
            \x20 if (force || (now - last_ack_time_secs_) >= 1) {\n\
            \x20   last_ack_time_secs_ = now;\n  }\n}\n\
            Status ReplicationThread::Start() {\n\
            \x20 if (util::GetTimeStampMS() - last_connect_timestamp < 1000) {\n    return 1;\n  }\n  return 0;\n}\n";
        let m = audit_texts(&[
            ("src/common/time_util.h", util),
            ("src/cluster/replication.cc", repl),
        ]);
        let lines: Vec<(&str, usize, &str)> = m
            .iter()
            .map(|x| (x.file.as_str(), x.line, x.shape))
            .collect();
        assert!(
            lines.contains(&("src/cluster/replication.cc", 3, "elapsed")),
            "{m:#?}"
        );
        assert!(
            lines.contains(&("src/cluster/replication.cc", 8, "elapsed")),
            "{m:#?}"
        );
        let start = m.iter().find(|x| x.line == 8).unwrap();
        assert!(
            start
                .limit
                .as_ref()
                .is_none_or(|l| l.name != "GetTimeStampMS"),
            "{start:#?}"
        );
        // a cast of a difference is a duration (its own line measures it), not a reading that
        // its callers keep
        let diff = "namespace util {\nauto ElapsedTime() {\n  return std::chrono::duration_cast<std::chrono::seconds>(std::chrono::system_clock::now() - start).count();\n}\n}\n\
            void f() {\n  int64_t t = util::ElapsedTime();\n  if (t - last > 5) {}\n}\n";
        let d = one("src/x.cc", diff);
        assert!(d.iter().all(|x| x.line == 3), "{d:#?}");
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
            order: None,
            stored: None,
            bundled: None,
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

    /// Milestone 350: a wall-clock reading kept as the version of data that the code orders by
    /// (an object store's shape: a copy takes `now_msec()` as its version's timestamp, while a
    /// put keeps it above the newest version). The reading comes through a function whose tail
    /// expression goes on over three lines.
    #[test]
    fn a_reading_kept_as_a_version_is_an_order() {
        let time = "pub fn now_msec() -> u64 {\n\tSystemTime::now()\n\t\t.duration_since(UNIX_EPOCH)\n\t\t.expect(\"Fix your clock :o\")\n\t\t.as_millis() as u64\n}\n";
        let object = "impl ObjectVersion {\n\tfn cmp_key(&self) -> (u64, Uuid) {\n\t\t(self.timestamp, self.uuid)\n\t}\n}\n";
        let put = "pub(crate) fn next_timestamp(existing: Option<&Object>) -> u64 {\n\texisting\n\t\t.and_then(|obj| obj.versions().iter().map(|v| v.timestamp).max())\n\t\t.map(|t| std::cmp::max(t + 1, now_msec()))\n\t\t.unwrap_or_else(now_msec)\n}\n";
        let copy = "async fn handle_copy_metaonly(ctx: ReqCtx) -> Result<SaveStreamResult, Error> {\n\tlet new_timestamp = now_msec();\n\tlet res = SaveStreamResult {\n\t\tversion_timestamp: new_timestamp,\n\t};\n\tlet v = ObjectVersion {\n\t\tuuid: dest_info.uuid,\n\t\ttimestamp: new_timestamp,\n\t};\n\tOk(res)\n}\n";
        let key = "fn new_key() -> Key {\n\tKey {\n\t\tcreated: Some(now_msec()),\n\t\tlast_seen: now_msec(),\n\t}\n}\n";
        let (durations, orders) = audit_both(&[
            ("src/util/time.rs", time),
            ("src/model/object_table.rs", object),
            ("src/api/put.rs", put),
            ("src/api/copy.rs", copy),
            ("src/model/key.rs", key),
        ]);
        assert!(durations.is_empty(), "{durations:#?}");
        // the copy's reading, not the put's increasing one, not a creation time
        let at: Vec<(&str, usize, &str)> = orders
            .iter()
            .map(|m| (m.file.as_str(), m.line, m.confidence))
            .collect();
        assert_eq!(at, [("src/api/copy.rs", 2, "high")], "{orders:#?}");
        let m = &orders[0];
        assert_eq!(m.shape, "order");
        assert_eq!(
            m.order,
            Some(Order {
                name: "timestamp".into(),
                compared_at: Some("src/model/object_table.rs:3".into()),
            })
        );
        assert!(
            m.origin[0].starts_with("now_msec() returns a wall-clock reading"),
            "{:?}",
            m.origin
        );
        let (_, p) = proposals(m, &Services::default());
        let got: Vec<&str> = p.iter().map(|x| x.schedule.as_str()).collect();
        assert_eq!(
            got,
            [
                "disk=none | run T=10:-300",
                "disk=none | run T=10:-3600",
                "disk=none | run K=<service>@2+3 T=10:-300",
            ]
        );
        // Go: a field merged by its newer value, and a stamp for a log that is not one
        let merge = "package kv\n\nfunc (s *Store) Put(k string) {\n\ts.items[k] = Item{\n\t\tVersion: time.Now().UnixNano(),\n\t}\n}\n\nfunc newer(a, b Item) bool {\n\treturn a.Version > b.Version\n}\n\nfunc logLine() Line {\n\treturn Line{\n\t\tTimestamp: time.Now().UTC().Format(time.RFC3339),\n\t}\n}\n";
        let (_, orders) = audit_both(&[("kv/store.go", merge)]);
        assert_eq!(orders.len(), 1, "{orders:#?}");
        assert_eq!(orders[0].line, 5);
        assert_eq!(orders[0].confidence, "high");
    }

    /// Milestone 350: a limit is a name of the line's code, not a type (`Duration`), and a plain
    /// word of another file is another value; a literal duration of Rust gives the limit.
    #[test]
    fn a_limit_is_no_type_and_no_plain_word_of_another_file() {
        let other = "pub(crate) const RESYNC_DELAY: Duration = Duration::from_secs(600);\nconst timeout = 7;\n";
        let sig = "fn check(date: DateTime<Utc>) -> Result<(), Error> {\n    if Utc::now() - date > Duration::hours(24) {\n        return Err(Error::bad_request(\"too old\"));\n    }\n    Ok(())\n}\n";
        let m = audit_texts(&[("src/block/manager.rs", other), ("src/api/sig.rs", sig)]);
        assert_eq!(m.len(), 1, "{m:#?}");
        let l = m[0].limit.as_ref().unwrap();
        assert_eq!(
            (l.name.as_str(), l.seconds, l.at.as_deref()),
            ("Duration::hours(24)", Some(86400.0), None)
        );
        assert!(distinctive("repl_min_replicas_max_lag"));
        assert!(distinctive("refreshMin"));
        assert!(distinctive("TIMEOUT"));
        assert!(!distinctive("timeout"));
        assert!(!distinctive("idle"));
    }

    fn shapes(m: &[ClockMatch]) -> Vec<(&str, usize, &str, &str)> {
        m.iter()
            .map(|x| (x.file.as_str(), x.line, x.shape, x.confidence))
            .collect()
    }

    /// Milestone 382: the timers of C libraries that Typesense bundles. h2o's event loop reads
    /// `gettimeofday` into a field of its loop, and its timer wheel finds no expired timer
    /// while the clock is before its last run; brpc's `butil` returns now plus a duration, and
    /// its timer thread compares a task's run time with `gettimeofday_us()`.
    #[test]
    fn c_library_timers_on_the_wall_clock_are_places() {
        let evloop = "void update_now(h2o_evloop_t *loop)\n{\n    gettimeofday(&loop->_tv_at, NULL);\n    loop->_now_nanosec = ((uint64_t)loop->_tv_at.tv_sec * 1000000 + loop->_tv_at.tv_usec) * 1000;\n    loop->_now_millisec = loop->_now_nanosec / 1000000;\n}\n\nint run_timers(h2o_evloop_t *loop)\n{\n    h2o_timerwheel_get_expired(loop->_timeouts, loop->_now_millisec, &expired);\n    return 0;\n}\n";
        let wheel = "void h2o_timerwheel_get_expired(h2o_timerwheel_t *ctx, uint64_t now, h2o_linklist_t *expired)\n{\n    if (now < ctx->last_run) {\n        return;\n    }\n    ctx->last_run = now;\n}\n";
        let time_h = "inline timespec nanoseconds_from_now(int64_t nanoseconds) {\n    timespec time;\n    clock_gettime(CLOCK_REALTIME, &time);\n    return nanoseconds_from(time, nanoseconds);\n}\n\ninline timespec milliseconds_from_now(int64_t milliseconds) {\n    return nanoseconds_from_now(milliseconds * 1000000L);\n}\n\ninline int64_t gettimeofday_us() {\n    timeval now;\n    gettimeofday(&now, NULL);\n    return now.tv_sec * 1000000L + now.tv_usec;\n}\n";
        let task = "void RepeatedTimerTask::schedule(std::unique_lock<raft_mutex_t>& lck) {\n    _next_duetime =\n            butil::milliseconds_from_now(adjust_timeout_ms(_timeout_ms));\n}\n";
        let thread = "void TimerThread::run() {\n    while (!tasks.empty()) {\n        Task* task1 = tasks[0];\n        if (butil::gettimeofday_us() < task1->run_time) {\n            break;\n        }\n    }\n}\n";
        let m = audit_texts(&[
            ("lib/common/socket/evloop.c.h", evloop),
            ("lib/common/timerwheel.c", wheel),
            ("src/butil/time.h", time_h),
            ("src/braft/repeated_timer_task.cpp", task),
            ("src/bthread/timer_thread.cpp", thread),
        ]);
        let s = shapes(&m);
        for want in [
            ("lib/common/timerwheel.c", 3, "rewind", "high"),
            ("src/braft/repeated_timer_task.cpp", 3, "deadline", "high"),
            ("src/bthread/timer_thread.cpp", 4, "check", "high"),
        ] {
            assert!(s.contains(&want), "{want:?} in {m:#?}");
        }
        let rewind = m.iter().find(|x| x.shape == "rewind").unwrap();
        assert_eq!(rewind.stored.as_deref(), Some("last_run"));
        assert!(
            what_it_does(rewind).contains("last_run"),
            "{}",
            what_it_does(rewind)
        );
    }

    /// Milestone 382: VictoriaMetrics's places. `fasttime` caches a ticker's tick in an atomic,
    /// a flush deadline is `time.Now().Add(d).UnixMilli()`, the retention watcher keeps
    /// partitions between now minus and plus a retention, and a connection's deadline is
    /// compared with the cached clock in a function of its own.
    #[test]
    fn go_cached_clocks_ticks_and_deadline_chains_are_followed() {
        let fasttime = "package fasttime\n\nfunc init() {\n\tgo func() {\n\t\tticker := time.NewTicker(time.Second)\n\t\tfor tm := range ticker.C {\n\t\t\tt := uint64(tm.Unix())\n\t\t\tcurrentTimestamp.Store(t)\n\t\t}\n\t}()\n}\n\nfunc UnixTimestamp() uint64 {\n\treturn currentTimestamp.Load()\n}\n";
        let conn = "package handshake\n\nfunc (bc *BufferedConn) Read(p []byte) (int, error) {\n\tstartTime := fasttime.UnixTimestamp()\n\tif deadlineExceeded(bc.readDeadline, startTime) {\n\t\treturn 0, os.ErrDeadlineExceeded\n\t}\n\treturn bc.br.Read(p)\n}\n\nfunc deadlineExceeded(deadline time.Time, currentTimestamp uint64) bool {\n\tif deadline.IsZero() {\n\t\treturn false\n\t}\n\treturn currentTimestamp > uint64(deadline.Unix())\n}\n";
        let rows = "package storage\n\nfunc (rrs *rawRowsShard) updateFlushDeadline() {\n\trrs.flushDeadlineMs.Store(time.Now().Add(pendingRowsFlushInterval).UnixMilli())\n}\n";
        let table = "package storage\n\nfunc (tb *table) retentionWatcher() {\n\tfor {\n\t\tnowMsecs := int64(fasttime.UnixTimestamp() * 1000)\n\t\tminTimestamp := nowMsecs - tb.s.retentionMsecs\n\t\tmaxTimestamp := nowMsecs + tb.s.futureRetentionMsecs\n\t\tif ptw.pt.tr.MaxTimestamp < minTimestamp || ptw.pt.tr.MinTimestamp > maxTimestamp {\n\t\t\tdrop(ptw)\n\t\t}\n\t}\n}\n";
        let m = audit_texts(&[
            ("lib/fasttime/fasttime_normal.go", fasttime),
            ("lib/handshake/buffered_conn.go", conn),
            ("lib/storage/raw_row.go", rows),
            ("lib/storage/table.go", table),
        ]);
        let s = shapes(&m);
        for want in [
            ("lib/handshake/buffered_conn.go", 15, "check", "high"),
            ("lib/storage/raw_row.go", 4, "deadline", "high"),
            ("lib/storage/table.go", 6, "deadline", "high"),
            ("lib/storage/table.go", 7, "deadline", "high"),
        ] {
            assert!(s.contains(&want), "{want:?} in {m:#?}");
        }
        let conn = m.iter().find(|x| x.line == 15).unwrap();
        assert!(
            conn.origin.iter().any(|o| o.contains("UnixTimestamp()")),
            "{:?}",
            conn.origin
        );
        // a plain `time.Now()` stays Go's monotonic reading
        let plain = "package x\n\nfunc f() {\n\tdeadline := time.Now().Add(timeout)\n\tif time.Now().After(deadline) {\n\t\treturn\n\t}\n}\n";
        assert!(one("x/x.go", plain).is_empty());
    }

    /// Milestone 382: Restate's wall clock is a cached atomic behind functions whose names do not
    /// all speak of time (`recent_us`), and GreptimeDB's is `chrono::Utc::now()` behind
    /// `current_time_millis()`; a Rust path is read from its first segment, and a method's
    /// receiver is no argument.
    #[test]
    fn rust_wrappers_of_the_clock_are_followed_by_data_flow() {
        let wall = "static RECENT_UNIX_TIMESTAMP_US: AtomicU64 = AtomicU64::new(0);\npub struct WallClock;\nimpl WallClock {\n    pub(crate) fn update_recent() {\n        RECENT_UNIX_TIMESTAMP_US.store(Self::now_us(), Ordering::Relaxed);\n    }\n    pub fn now_ms() -> MillisSinceEpoch {\n        MillisSinceEpoch::new(\n            SystemTime::now()\n                .duration_since(SystemTime::UNIX_EPOCH)\n                .unwrap()\n                .as_millis() as u64,\n        )\n    }\n    pub fn now_us() -> u64 {\n        SystemTime::now()\n            .duration_since(SystemTime::UNIX_EPOCH)\n            .unwrap()\n            .as_micros() as u64\n    }\n    pub fn recent_us() -> u64 {\n        RECENT_UNIX_TIMESTAMP_US.load(Ordering::Relaxed)\n    }\n}\n";
        let clock = "impl SchedulerClock {\n    pub fn now_millis(&self) -> MillisSinceEpoch {\n        let recent_us = WallClock::recent_us();\n        if recent_us > 0 {\n            MillisSinceEpoch::new(recent_us / 1_000)\n        } else {\n            WallClock::now_ms()\n        }\n    }\n}\n";
        let queue = "impl VQueueState {\n    fn eligibility(&self) -> Eligibility {\n        if inbox_head_key.run_at() > SchedulerClock.now_millis() {\n            return Eligibility::Scheduled;\n        }\n        Eligibility::Inbox\n    }\n}\n";
        let util =
            "pub fn current_time_millis() -> i64 {\n    chrono::Utc::now().timestamp_millis()\n}\n";
        let detector = "impl PhiAccrualFailureDetector {\n    pub(crate) fn is_available(&self, ts_millis: i64) -> bool {\n        self.phi(ts_millis) < self.threshold as _\n    }\n    pub(crate) fn phi(&self, ts_millis: i64) -> f64 {\n        if let Some(last_heartbeat_millis) = self.last_heartbeat_millis {\n            let time_diff = ts_millis - last_heartbeat_millis;\n            time_diff as f64\n        } else {\n            0.0\n        }\n    }\n}\nfn supervise(e: &Entry) -> bool {\n    e.failure_detector().is_available(current_time_millis())\n}\n";
        let m = audit_texts(&[
            ("crates/clock/src/wall_clock.rs", wall),
            ("crates/vqueues/src/scheduler/clock.rs", clock),
            ("crates/vqueues/src/scheduler/vqueue_state.rs", queue),
            ("src/common/time/src/util.rs", util),
            ("src/meta-srv/src/failure_detector.rs", detector),
        ]);
        let s = shapes(&m);
        assert!(
            s.contains(&(
                "crates/vqueues/src/scheduler/vqueue_state.rs",
                3,
                "check",
                "high"
            )),
            "{m:#?}"
        );
        assert!(
            s.iter()
                .any(|x| x.0 == "src/meta-srv/src/failure_detector.rs" && x.1 == 7),
            "{m:#?}"
        );
        // `Instant::now()` stays monotonic, also behind a path, when a `now` reads the wall clock
        let mono = "pub fn now() -> u64 {\n    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()\n}\nfn wait() {\n    let deadline = tokio::time::Instant::now() + timeout;\n}\n";
        assert!(
            one("src/m.rs", mono).is_empty(),
            "{:#?}",
            one("src/m.rs", mono)
        );
    }

    /// Milestone 382: Erlang. Ra stamps each command with `erlang:system_time(millisecond)`
    /// under `ts`, renames it `system_time` in the command's meta, and RabbitMQ's stream
    /// coordinator parks a retry at that time plus a backoff and arms a timer for the
    /// difference.
    #[test]
    fn erlang_follows_a_timestamp_through_ras_command_meta() {
        let proc_ = "make_command(Type, {call, From}, Data, Mode) ->\n    Ts = erlang:system_time(millisecond),\n    {Type, #{from => From, ts => Ts}, Data, Mode};\nmake_command(Type, _, Data, Mode) ->\n    Ts = erlang:system_time(millisecond),\n    {Type, #{ts => Ts}, Data, Mode}.\n";
        let server = "augment_command_meta(Idx, Term, MacVer, CmdMeta) ->\n    maps:fold(fun (ts, V, Acc) ->\n                      %% rename from compact key name\n                      Acc#{system_time => V};\n                  (K, V, Acc) ->\n                      Acc#{K => V}\n              end, #{index => Idx, term => Term}, CmdMeta).\n";
        let coord = "retry_reconcile_delay(Parked, Now) ->\n    {{RetryAt, _StreamId, _Node}, _} = gb_trees:smallest(Parked),\n    max(0, RetryAt - Now).\n\npark_action(short, Node, StreamId, M, Members, Meta, Stream1, Parked) ->\n    RetryAt = maps:get(system_time, Meta) + backoff_ms(short)\n              + retry_jitter(StreamId, Node),\n    Parked1 = gb_trees:enter({RetryAt, StreamId, Node}, [], Parked),\n    Effs = [{timer, retry_reconcile,\n             retry_reconcile_delay(Parked1, maps:get(system_time, Meta))}],\n    {Stream1, Parked1, Effs}.\n";
        let m = audit_texts(&[
            ("deps/ra/src/ra_server_proc.erl", proc_),
            ("deps/ra/src/ra_server.erl", server),
            ("deps/rabbit/src/rabbit_stream_coordinator.erl", coord),
        ]);
        let s = shapes(&m);
        for want in [
            (
                "deps/rabbit/src/rabbit_stream_coordinator.erl",
                3,
                "elapsed",
                "high",
            ),
            (
                "deps/rabbit/src/rabbit_stream_coordinator.erl",
                6,
                "deadline",
                "high",
            ),
        ] {
            assert!(s.contains(&want), "{want:?} in {m:#?}");
        }
        let park = m.iter().find(|x| x.line == 6).unwrap();
        assert_eq!(park.lang, "erlang");
        assert_eq!(park.function.as_deref(), Some("park_action"));
        assert!(
            park.origin.iter().any(|o| o.contains("system_time")),
            "{:?}",
            park.origin
        );
        // a monotonic time and a comment are no readings
        let mono = "f() ->\n    %% erlang:system_time(millisecond)\n    T0 = erlang:monotonic_time(millisecond),\n    Deadline = T0 + timeout_ms(),\n    Deadline.\n";
        assert!(one("src/m.erl", mono).is_empty());
    }

    /// Milestone 382: Prometheus's WAL watcher keeps the start time it was given, `time.Now()`,
    /// as a timestamp, and sends only the samples after it: the comparison of a sample's time
    /// with a stored reading that marks where accepted data starts.
    #[test]
    fn go_a_stored_start_time_compared_with_datas_timestamps() {
        let watcher = "package wlog\n\nfunc (w *Watcher) loop() {\n\tw.SetStartTime(time.Now())\n}\n\nfunc (w *Watcher) SetStartTime(t time.Time) {\n\tw.startTime = t\n\tw.startTimestamp = timestamp.FromTime(t)\n}\n\nfunc (w *Watcher) readSegment() {\n\tfor _, s := range samples {\n\t\tif s.T > w.startTimestamp {\n\t\t\tsend(s)\n\t\t}\n\t}\n}\n";
        let m = one("tsdb/wlog/watcher.go", watcher);
        assert_eq!(
            shapes(&m),
            vec![("tsdb/wlog/watcher.go", 14, "rewind", "high")],
            "{m:#?}"
        );
        assert_eq!(m[0].stored.as_deref(), Some("startTimestamp"));
    }

    fn tree(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-clockscan-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (rel, text) in files {
            let p = d.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        d
    }

    /// Milestone 382: bundled code is read apart from the project's own: Go's `vendor/`, C's
    /// `third_party/`, Bazel's `bazel-<name>/external/`, and `deps/`, where a folder that the
    /// root's `.gitignore` keeps is the project's own (RabbitMQ's applications). Its places
    /// have a section of their own after the project's, and its names make no reading of the
    /// project's code.
    #[test]
    fn bundled_code_is_read_after_the_projects_own() {
        let lease = "package store\n\nfunc (s *Store) renew() {\n\tif Now() > s.leaseUntil {\n\t\treturn\n\t}\n}\n";
        let lib = "package wheel\n\nfunc Now() int64 {\n\treturn time.Now().UnixMilli()\n}\n\nfunc (w *Wheel) expire() {\n\tif Now() > w.timerDeadline {\n\t\tw.fire()\n\t}\n}\n";
        let c_lib = "void h2o_timer_link(h2o_loop_t *loop, uint64_t delay_ticks, h2o_timer_t *timer)\n{\n    struct timeval tv;\n    gettimeofday(&tv, NULL);\n    uint64_t now = tv.tv_sec * 1000;\n    timer->expire_at = now + delay_ticks;\n}\n";
        let own_erl = "now_ms() ->\n    erlang:system_time(millisecond).\nrenew(D) ->\n    Until = now_ms() + lease_timeout_ms(),\n    D#lease{until = Until}.\n";
        let d = tree(
            "bundled",
            &[
                ("src/store.go", lease),
                ("vendor/github.com/acme/wheel/wheel.go", lib),
                ("third_party/h2o/lib/timer.c", c_lib),
                ("bazel-proj/external/h2o2/lib/timer.c", c_lib),
                (".gitignore", "/deps/*\n!/deps/app/\n"),
                ("deps/app/src/lease.erl", own_erl),
                ("deps/app/src/coord.erl", "park(Meta) ->\n    RetryAt = maps:get(system_time, Meta) + backoff_ms(short),\n    RetryAt.\n"),
                ("deps/ra/src/lease.erl", own_erl),
                ("deps/ra/src/ra_proc.erl", "make_command(Type, Data) ->\n    Ts = erlang:system_time(millisecond),\n    {Type, #{system_time => Ts}, Data}.\n"),
                ("deps/ra/test/lease_SUITE.erl", own_erl),
            ],
        );
        let a = audit(&d);
        let _ = std::fs::remove_dir_all(&d);
        // the vendored `Now()` is no reading of the project's code
        assert!(
            a.matches.iter().all(|m| m.file != "src/store.go"),
            "{:#?}",
            a.matches
        );
        assert!(a.matches.iter().all(|m| m.bundled.is_none()));
        assert!(
            a.matches.iter().any(|m| m.file == "deps/app/src/lease.erl"),
            "{:#?}",
            a.matches
        );
        // a key of Ra's command meta reaches the project's own Erlang
        assert!(
            a.matches
                .iter()
                .any(|m| m.file == "deps/app/src/coord.erl" && m.line == 2),
            "{:#?}",
            a.matches
        );
        assert_eq!(a.read, 3, "src/store.go and deps/app/src/");
        assert_eq!(
            a.bundled_libs,
            vec!["h2o2", "ra", "h2o", "github.com/acme/wheel"],
            "{:?}",
            a.bundled_libs
        );
        assert_eq!(a.bundled_read, 5, "the test suite of ra is left out");
        let libs: HashSet<&str> = a
            .bundled
            .iter()
            .filter_map(|m| m.bundled.as_deref())
            .collect();
        for l in ["h2o", "h2o2", "ra", "github.com/acme/wheel"] {
            assert!(libs.contains(l), "{l} in {:#?}", a.bundled);
        }
        let text = text_report("proj", &a, &Services::default(), 5, false);
        assert!(
            text.contains("\nThe audit also read 5 files of bundled code: h2o2, ra, h2o and github.com/acme/wheel.\n"),
            "{text}"
        );
        assert!(
            text.contains("   Bundled code of github.com/acme/wheel.\n"),
            "{text}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&json_report("proj", &a, &Services::default())).unwrap();
        assert_eq!(json["bundled_files_read"], 5);
        assert!(json["clock_bundled"]
            .as_array()
            .is_some_and(|v| !v.is_empty()));
        assert!(json["clock_bundled"][0]["bundled"].is_string());
    }

    /// Milestone 382: without a place in the project's own code the report says so and gives
    /// fallback schedules for each service, aimed at no place.
    #[test]
    fn no_place_gives_fallback_schedules() {
        let svcs = services_of(
            "format: 1\nname: q\nservices:\n  - name: broker\n    cmd: \"/bin/broker\"\n  - name: db\n    image_ref: postgres:16\ngroups:\n  - name: nodes\n    members: [broker]\n    primary: \"cat /tmp/p\"\nworkload:\n  steps: 20\n  run: \"true\"\n",
            "hydo.yaml",
        );
        let a = Audit::default();
        let text = text_report("q", &a, &svcs, 5, false);
        assert!(
            text.contains("The audit found no place in the project's own code. These schedules are a fallback"),
            "{text}"
        );
        for s in [
            "disk=none | run K=broker@2+3 T=10:-300",
            "disk=none | run K=db@2+3 T=10:-300",
            "disk=none | run T=10:-300 K=nodes.primary@11+3",
            "disk=none | run T=10:-86400",
            "disk=none | run T=2:3600",
        ] {
            assert!(text.contains(s), "{s} in {text}");
        }
        let json: serde_json::Value = serde_json::from_str(&json_report("q", &a, &svcs)).unwrap();
        assert_eq!(json["fallback"].as_array().map(|v| v.len()), Some(5));
        // a project with a place gets none
        let m = one(
            "src/a.go",
            "package a\n\nfunc f() {\n\tif time.Now().UTC().Sub(last) > leaseTimeout {\n\t}\n}\n",
        );
        let b = Audit {
            matches: m,
            ..Default::default()
        };
        assert!(!text_report("q", &b, &svcs, 5, false).contains("fallback"));
    }
}
