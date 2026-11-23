//! `hydo rules scan --kind writes` (Milestone 344, tentative number; docs/design/rule-suggestion.md,
//! "The write routes"): every route of a project's API that writes, with its handler.
//!
//! An onboarding writes one workload command per client operation. An operation that nobody
//! lists is never exercised, and its bugs stay hidden: on Meilisearch the index swap, the dump
//! and the payload of a document addition held the three bugs that hydo found, and a first
//! workload had none of them. So the scan lists what the API can write:
//!
//! - HTTP routes with the methods POST, PUT, PATCH and DELETE, from the router code of Rust
//!   (axum, actix-web, and route tables of a macro), Go (net/http, gorilla/mux, chi, gin, labstack's router,
//!   httprouter, Prometheus's route package), JavaScript and TypeScript (express, fastify, koa,
//!   hapi, NestJS), Python (FastAPI, Flask, Django), and Java and Kotlin (Spring, JAX-RS);
//! - RPC methods whose names do not say that they read, from `.proto` files, from gRPC servers
//!   in Go (a type that embeds `Unimplemented<Service>Server`) and Rust (tonic's
//!   `#[async_trait] impl Service for T`);
//! - the operations of OpenAPI 3 and Swagger 2 files, also across `$ref` files.
//!
//! The scan reads text, without a compiler, as the clock audit does: comments and test code are
//! left out (Milestone 324's rule), and so are test files, vendored and generated code, and
//! examples. A POST route whose name says that it reads (`/query`, `search_with_post`) is left
//! out and counted. Each route gets a class by its words: `data` (it changes the system's data),
//! `admin` (an operation on the stored data as a whole: a snapshot, a dump, a compaction),
//! `session` (a sign-in or a token) and `runtime` (health, metrics, profiling, a reload). The
//! list is grouped by resource and ranked by class, so a durable write comes before a login or a
//! metric.

use crate::clockscan::{self, Lang};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

// ---------------------------------------------------------------- the result

/// One route that writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    /// `POST`, `PUT`, `PATCH`, `DELETE`, or `RPC` for a gRPC method without an HTTP mapping.
    pub method: String,
    /// The path, or `/<package>.<Service>/<Method>` for an RPC. A part that the scan could not
    /// read is `{name}`.
    pub path: String,
    /// Other names of the same route: an RPC's HTTP path.
    pub also: Vec<String>,
    /// The handler's name, `<inline>` for a function written in the route itself.
    pub handler: String,
    /// Where the handler is defined, when the scan found it.
    pub handler_at: Option<(String, usize)>,
    /// Where the route is declared.
    pub file: String,
    pub line: usize,
    /// What declares it, as a short name (`axum`, `OpenAPI`, `protobuf`).
    pub source: String,
    /// The other places that declare it (`file:line (source)`).
    pub elsewhere: Vec<String>,
    pub resource: String,
    /// `data`, `admin`, `session` or `runtime`.
    pub class: &'static str,
    pub why: String,
    pub score: i32,
}

/// What the scan found.
pub struct Writes {
    /// The source and description files read.
    pub read: usize,
    /// The routes that write, ranked.
    pub routes: Vec<Route>,
    /// The POST routes and RPC methods whose names say that they read.
    pub reads: Vec<Route>,
    /// The test code left out of the source files.
    pub tests: clockscan::TestItems,
}

// ---------------------------------------------------------------- words

fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
            cur.push(c.to_ascii_lowercase());
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Verbs that change something. A name with one of them is never taken for a read.
const WRITE_VERBS: &[&str] = &[
    "create",
    "add",
    "insert",
    "update",
    "upsert",
    "delete",
    "del",
    "remove",
    "set",
    "put",
    "write",
    "push",
    "ingest",
    "import",
    "upload",
    "save",
    "commit",
    "append",
    "publish",
    "send",
    "enqueue",
    "cancel",
    "swap",
    "replace",
    "edit",
    "patch",
    "merge",
    "move",
    "rename",
    "register",
    "reset",
    "clear",
    "purge",
    "drop",
    "truncate",
    "compact",
    "snapshot",
    "dump",
    "backup",
    "restore",
    "migrate",
    "trigger",
    "schedule",
    "retry",
    "replay",
    "ack",
    "approve",
    "reject",
    "archive",
    "toggle",
    "enable",
    "disable",
    "activate",
    "deactivate",
    "store",
    "submit",
    "assign",
    "unassign",
    "pause",
    "resume",
    "rotate",
    "revoke",
    "grant",
    "invite",
    "accept",
    "decline",
    "close",
    "open",
    "lock",
    "unlock",
    "release",
    "transfer",
    "refund",
    "charge",
    "pay",
    "capture",
    "void",
    "post",
    "flush",
    "change",
    "modify",
    "mark",
    "pin",
    "unpin",
    "star",
    "unstar",
    "follow",
    "unfollow",
    "like",
    "unlike",
    "join",
    "leave",
    "apply",
    "attach",
    "detach",
    "link",
    "unlink",
    "upgrade",
    "downgrade",
    "install",
    "uninstall",
    "start",
    "stop",
    "restart",
    "kill",
    "complete",
    "finish",
    "fail",
    "emit",
    "record",
    "log",
    "track",
    "report",
    "seed",
    "init",
    "initialize",
    "generate",
    "build",
    "deploy",
    "sync",
];

/// Words that say that a POST route or an RPC method reads.
const READ_WORDS: &[&str] = &[
    "get",
    "list",
    "read",
    "query",
    "search",
    "fetch",
    "find",
    "lookup",
    "count",
    "describe",
    "explain",
    "render",
    "format",
    "parse",
    "validate",
    "preview",
    "watch",
    "subscribe",
    "listen",
    "poll",
    "browse",
    "select",
    "show",
    "view",
    "similar",
    "suggest",
    "autocomplete",
    "match",
    "scan",
    "peek",
    "estimate",
    "cost",
    "check",
    "exists",
    "health",
    "ping",
    "stats",
    "info",
    "status",
    "version",
    "heartbeat",
    "probe",
    "verify",
    "lint",
    "dry",
    "inspect",
    "compare",
    "diff",
    "download",
    "export",
    "stream",
    "tail",
    "range",
    "exemplars",
    "analyze",
    "test",
    "echo",
    "static",
    "serve",
    "dryrun",
];

const RUNTIME_WORDS: &[&str] = &[
    "health",
    "healthz",
    "ready",
    "readyz",
    "readiness",
    "live",
    "livez",
    "liveness",
    "ping",
    "stderr",
    "stdout",
    "debug",
    "pprof",
    "prof",
    "profile",
    "profiling",
    "heap",
    "gdump",
    "trace",
    "tracing",
    "level",
    "reload",
    "quit",
    "shutdown",
    "version",
    "gc",
    "heartbeat",
];

const SESSION_WORDS: &[&str] = &[
    "login",
    "logout",
    "signin",
    "signout",
    "refresh",
    "session",
    "sessions",
    "oauth",
    "authenticate",
    "handshake",
    "sso",
    "saml",
    "otp",
    "mfa",
    "2fa",
    "oidc",
    "callback",
    "csrf",
];

const ADMIN_WORDS: &[&str] = &[
    "admin",
    "snapshot",
    "snapshots",
    "dump",
    "dumps",
    "backup",
    "backups",
    "restore",
    "compact",
    "compaction",
    "flush",
    "migrate",
    "migration",
    "migrations",
    "reindex",
    "tombstones",
    "vacuum",
    "purge",
    "truncate",
    "rebuild",
    "repair",
    "recover",
    "failover",
    "rebalance",
    "split",
    "maintenance",
    "cluster",
    "replication",
    "export",
];

fn has(ws: &[String], list: &[&str]) -> Option<String> {
    ws.iter().find(|w| list.contains(&w.as_str())).cloned()
}

/// The last literal segment of a path, without parameters.
fn last_segment(path: &str) -> String {
    path.split(['/', ':'])
        .rfind(|s| !s.is_empty() && !is_param(s))
        .unwrap_or("")
        .to_string()
}

fn is_param(seg: &str) -> bool {
    seg.starts_with('{')
        || seg.starts_with('<')
        || seg.starts_with('*')
        || seg.starts_with('$')
        || seg.starts_with('(')
        || seg.starts_with('^')
        || seg.contains('{')
        || seg.contains('<')
}

/// Whether a POST route (or an RPC method) reads, by its name: a read word in the last
/// segment of its path, or as the first word of its handler or method, and no write verb in
/// either.
fn reads_by_name(path: &str, handler: &str) -> bool {
    let seg = words(&last_segment(path));
    let h = verb_words(handler);
    // the handler's own first verb decides first (`get_snapshot` on `POST /snapshot` reads)
    if h.first().is_some_and(|w| WRITE_VERBS.contains(&w.as_str())) {
        return false;
    }
    if h.first().is_some_and(|w| READ_WORDS.contains(&w.as_str())) {
        return true;
    }
    if has(&seg, WRITE_VERBS).is_some() {
        return false;
    }
    if has(&seg, READ_WORDS).is_some() {
        return true;
    }
    // `labels_query`, `documents_by_query_post`: a word of querying anywhere, without a verb of
    // change anywhere
    has(&h, STRONG_READS).is_some() && has(&h, WRITE_VERBS).is_none()
}

/// Words that say that a name reads wherever they stand in it.
const STRONG_READS: &[&str] = &["query", "search", "get", "list", "fetch", "lookup", "find"];

/// The words of a handler's name without a leading word that names no action (`do_get`,
/// `handle_x`, `BatchGetUsers`, `bulk_delete`).
fn verb_words(name: &str) -> Vec<String> {
    let mut w = words(name);
    while w.len() > 1
        && matches!(
            w[0].as_str(),
            "do" | "handle" | "on" | "batch" | "bulk" | "try"
        )
    {
        w.remove(0);
    }
    w
}

/// The class of a write and the word that decided it.
fn classify(method: &str, path: &str, handler: &str) -> (&'static str, Option<String>) {
    // an RPC's path names its package and service; only its method's name says what it does
    let mut ws = if method == "RPC" {
        Vec::new()
    } else {
        words(path)
    };
    ws.extend(verb_words(handler));
    if let Some(w) = has(&ws, RUNTIME_WORDS) {
        return ("runtime", Some(w));
    }
    // `/metrics` is the process's own; a POST to `/otlp/v1/metrics` is data
    let segs: Vec<&str> = path
        .split('/')
        .filter(|x| !x.is_empty() && !is_param(x))
        .collect();
    if segs == ["metrics"] {
        return ("runtime", Some("metrics".into()));
    }
    if let Some(w) = has(&ws, SESSION_WORDS) {
        return ("session", Some(w));
    }
    if let Some(w) = has(&ws, ADMIN_WORDS) {
        return ("admin", Some(w));
    }
    ("data", has(&ws, WRITE_VERBS))
}

fn class_rank(c: &str) -> i32 {
    match c {
        "data" => 40,
        "admin" => 30,
        "session" => 20,
        _ => 10,
    }
}

const VERSION_SEGMENTS: &[&str] = &["api", "apis", "rest", "rpc", "v1", "v2", "v3", "v4"];

/// The resource of a path: its first two segments that are neither parameters nor versions.
fn resource_of(method: &str, path: &str) -> String {
    if method == "RPC" {
        // /<package>.<Service>/<Method>
        let svc = path.trim_start_matches('/').split('/').next().unwrap_or("");
        return svc.rsplit('.').next().unwrap_or(svc).to_string();
    }
    let segs: Vec<&str> = path
        .split('/')
        .filter(|s| {
            let l = s.to_ascii_lowercase();
            // a version: v1, v2, v1beta1, v1alpha
            let version = l
                .strip_prefix('v')
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
            let skip = s.is_empty()
                || is_param(s)
                || s.starts_with(':')
                || VERSION_SEGMENTS.contains(&l.as_str())
                || version;
            !skip
        })
        .take(2)
        .collect();
    if segs.is_empty() {
        if path.contains('{') {
            return "(path not read)".into();
        }
        return "/".into();
    }
    // a segment with a colon action (`memos:batchDelete`) groups by its resource
    segs.iter()
        .map(|s| s.split(':').next().unwrap_or(s))
        .collect::<Vec<_>>()
        .join("/")
}

/// The path with each parameter as `{}`, for matching one route declared in two places.
fn path_key(method: &str, path: &str) -> String {
    let p: Vec<String> = path
        .trim_end_matches('/')
        .split('/')
        .map(|s| {
            if is_param(s) || s.starts_with(':') {
                "{}".to_string()
            } else {
                s.to_string()
            }
        })
        .collect();
    format!("{method} {}", p.join("/"))
}

fn normalize_path(p: &str) -> String {
    let mut s = p.replace("${", "{");
    if s.is_empty() {
        s = "/".into();
    }
    if !s.starts_with('/') && !s.starts_with('{') {
        s = format!("/{s}");
    }
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    s
}

fn join_path(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        return normalize_path(path);
    }
    let p = path.trim_start_matches('/');
    if p.is_empty() {
        return normalize_path(prefix);
    }
    normalize_path(&format!("{}/{p}", prefix.trim_end_matches('/')))
}

// ---------------------------------------------------------------- a source file as flat text

#[derive(Clone, Debug)]
struct Call {
    name: String,
    /// where the name starts
    at: usize,
    open: usize,
    close: usize,
    /// the identifier before a `.` in front of the name
    recv: String,
    dot: bool,
    /// the character before the receiver chain (`@` for a decorator, `[` for an attribute)
    pre: char,
    args: Vec<(usize, usize)>,
}

struct Flat {
    s: String,
    starts: Vec<usize>,
    instr: Vec<bool>,
    calls: Vec<Call>,
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

impl Flat {
    fn new(lang: Lang, lines: &[String]) -> Flat {
        let s = lines.join("\n");
        let b = s.as_bytes();
        let mut starts = vec![0];
        for (i, c) in b.iter().enumerate() {
            if *c == b'\n' {
                starts.push(i + 1);
            }
        }
        // strings
        let mut instr = vec![false; b.len()];
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            let quote = match c {
                b'"' => true,
                b'`' => matches!(lang, Lang::Go | Lang::Js),
                b'\'' => match lang {
                    Lang::Rust => {
                        b.get(i + 1) == Some(&b'\\') || (i + 2 < b.len() && b[i + 2] == b'\'')
                    }
                    _ => true,
                },
                _ => false,
            };
            if !quote {
                i += 1;
                continue;
            }
            let q = c;
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == q {
                    break;
                }
                if b[i] == b'\n' && q != b'`' && lang != Lang::Rust {
                    break;
                }
                i += 1;
            }
            let end = i.min(b.len().saturating_sub(1));
            for x in instr.iter_mut().take(end + 1).skip(start) {
                *x = true;
            }
            i += 1;
        }
        // brackets
        let mut matched: HashMap<usize, usize> = HashMap::new();
        let mut stack: Vec<usize> = Vec::new();
        for (i, c) in b.iter().enumerate() {
            if instr[i] {
                continue;
            }
            match c {
                b'(' | b'[' | b'{' => stack.push(i),
                b')' | b']' | b'}' => {
                    if let Some(o) = stack.pop() {
                        matched.insert(o, i);
                    }
                }
                _ => {}
            }
        }
        let mut calls = Vec::new();
        for (i, c) in b.iter().enumerate() {
            if *c != b'(' || instr[i] {
                continue;
            }
            let mut e = i;
            if e > 0 && b[e - 1] == b'!' {
                e -= 1; // a Rust macro
            }
            let mut a = e;
            while a > 0 && is_ident_char(b[a - 1]) {
                a -= 1;
            }
            if a == e {
                continue;
            }
            let name = s[a..e].to_string();
            let close = matched
                .get(&i)
                .copied()
                .unwrap_or(b.len().saturating_sub(1));
            let dot = a > 0 && b[a - 1] == b'.';
            let mut recv = String::new();
            let mut p = a;
            if dot {
                let mut r = a - 1;
                while r > 0 && is_ident_char(b[r - 1]) {
                    r -= 1;
                }
                recv = s[r..a - 1].to_string();
            }
            // the receiver chain: identifiers, dots and `::`
            while p > 0 && (is_ident_char(b[p - 1]) || b[p - 1] == b'.' || b[p - 1] == b':') {
                p -= 1;
            }
            let pre = match p {
                0 => '\n',
                _ if b[p - 1] == b'[' && !(p >= 2 && b[p - 2] == b'#') => '\0',
                _ => b[p - 1] as char,
            };
            let mut args = Vec::new();
            let mut st = i + 1;
            let mut depth = 0i32;
            for k in i + 1..close {
                if instr[k] {
                    continue;
                }
                match b[k] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth -= 1,
                    b',' if depth == 0 => {
                        args.push((st, k));
                        st = k + 1;
                    }
                    _ => {}
                }
            }
            if close > st && !s[st..close].trim().is_empty() {
                args.push((st, close));
            }
            calls.push(Call {
                name,
                at: a,
                open: i,
                close,
                recv,
                dot,
                pre,
                args,
            });
        }
        Flat {
            s,
            starts,
            instr,
            calls,
        }
    }

    /// The line (from 0) of a byte offset.
    fn line(&self, at: usize) -> usize {
        match self.starts.binary_search(&at) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    fn text(&self, a: (usize, usize)) -> &str {
        self.s[a.0..a.1].trim()
    }

    /// The string literals inside a range, as (start, contents).
    fn literals(&self, a: (usize, usize)) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let mut i = a.0;
        while i < a.1 {
            if self.instr[i] {
                let st = i;
                while i < a.1 && self.instr[i] {
                    i += 1;
                }
                let raw = &self.s[st..i];
                let inner = if raw.len() >= 2 {
                    &raw[1..raw.len() - 1]
                } else {
                    ""
                };
                out.push((st, inner.to_string()));
                // two literals next to each other ("a" "b") are two strings
                continue;
            }
            i += 1;
        }
        out
    }

    /// An argument as a path: its literals joined, with each identifier between them as
    /// `{name}`; None when it holds no literal (`consts` resolves a lone identifier).
    fn path_arg(
        &self,
        a: (usize, usize),
        consts: &dyn Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let t = self.text(a);
        let lits = self.literals(a);
        if lits.is_empty() {
            let id = t.trim_start_matches('&').trim();
            let last = id.rsplit(['.', ':']).next().unwrap_or(id);
            if !last.is_empty() && last.bytes().all(is_ident_char) {
                return consts(last).or_else(|| Some(format!("{{{last}}}")));
            }
            return None;
        }
        // a format string: the first literal alone
        let low = t.to_ascii_lowercase();
        if low.starts_with("format!") || low.contains("sprintf(") || low.starts_with("f\"") {
            return Some(lits[0].1.clone());
        }
        let mut out = String::new();
        let mut pos = a.0;
        for (st, lit) in &lits {
            let between = self.s[pos..*st].trim();
            let between = between.trim_matches(|c: char| c == '+' || c.is_whitespace());
            if !between.is_empty() && between.bytes().all(|c| is_ident_char(c) || c == b'.') {
                let last = between.rsplit('.').next().unwrap_or(between);
                out.push_str(&consts(last).unwrap_or_else(|| format!("{{{last}}}")));
            }
            out.push_str(lit);
            pos = st + lit.len() + 2;
        }
        let after = self.s[pos.min(a.1)..a.1].trim();
        let after = after.trim_start_matches(|c: char| c == '+' || c.is_whitespace());
        if !after.is_empty() && after.bytes().all(|c| is_ident_char(c) || c == b'.') {
            let last = after.rsplit('.').next().unwrap_or(after);
            out.push_str(&consts(last).unwrap_or_else(|| format!("{{{last}}}")));
        }
        Some(out)
    }
}

/// Whether a rendered argument reads as a route's path (not a URL of a client).
fn path_like(p: &str) -> bool {
    !(p.contains("://")
        || p.starts_with("http")
        || p.contains(' ')
        || p.contains('\n')
        || p.len() > 200)
        && (p.is_empty() || p.starts_with('/') || p.starts_with('{') || p.starts_with(':'))
}

// ---------------------------------------------------------------- the files

struct Src {
    rel: String,
    lang: Lang,
    flat: Flat,
    code: Vec<String>,
    funcs: Vec<(String, usize, usize)>,
    /// every function header, by name and line (for the handlers)
    heads: Vec<(String, usize)>,
    consts: HashMap<String, String>,
    /// the first part of the file, lower case: its imports
    head: String,
}

impl Src {
    fn enclosing(&self, line: usize) -> Option<&str> {
        self.funcs
            .iter()
            .filter(|f| f.1 <= line && line <= f.2)
            .max_by_key(|f| f.1)
            .map(|f| f.0.as_str())
    }
}

/// The constants of a file whose value is one string literal (`const P: &str = "/x";`).
fn constants(code: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for l in code {
        let t = l.trim().trim_end_matches([';', ',']);
        let Some(eq) = t.find('=') else { continue };
        let rest = &t[eq + 1..];
        if rest.starts_with(['=', '>']) || t[..eq].ends_with(['!', '<', '>', ':', '+', '-']) {
            continue;
        }
        let left = t[..eq].trim();
        if left.contains('(') || left.contains('.') || left.contains('[') {
            continue;
        }
        let left = left.split(':').next().unwrap_or(left).trim();
        let name = left.rsplit(' ').next().unwrap_or(left);
        let v = rest.trim();
        let q = v.chars().next();
        if !matches!(q, Some('"' | '\'' | '`')) || v.len() < 2 || !v.ends_with(q.unwrap()) {
            continue;
        }
        let inner = &v[1..v.len() - 1];
        if inner.contains(q.unwrap()) || name.is_empty() || !name.bytes().all(is_ident_char) {
            continue;
        }
        out.entry(name.to_string())
            .or_insert_with(|| inner.to_string());
    }
    out
}

/// A raw route before its prefixes and handler are resolved.
#[derive(Clone, Debug)]
struct Cand {
    method: String,
    path: String,
    handler: String,
    /// the handler's definition, when the declaration names it by its place (the function after
    /// a decorator)
    handler_line: Option<usize>,
    file: usize,
    line: usize,
    source: &'static str,
    shape: &'static str,
    /// whose mount gives the prefix: a function, a type or a variable (`file#var`)
    owner: Option<String>,
    /// the function that holds the declaration: the owner when the variable is a parameter
    owner_fn: Option<String>,
    also: Vec<String>,
}

const METHODS: &[(&str, &str)] = &[
    ("post", "POST"),
    ("put", "PUT"),
    ("patch", "PATCH"),
    ("delete", "DELETE"),
    ("del", "DELETE"),
];

fn method_of(name: &str) -> Option<&'static str> {
    let l = name.to_ascii_lowercase();
    METHODS.iter().find(|m| m.0 == l).map(|m| m.1)
}

/// A method named in a string or a constant (`"POST"`, `http.MethodPost`, `RequestMethod.PUT`).
fn methods_in(text: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for w in words(text) {
        let m = match w.as_str() {
            "post" => "POST",
            "put" => "PUT",
            "patch" => "PATCH",
            "delete" => "DELETE",
            _ => continue,
        };
        if !out.contains(&m) {
            out.push(m);
        }
    }
    out
}

const GO_ROUTERS: &[&str] = &[
    "github.com/go-chi/chi",
    "github.com/gin-gonic/gin",
    "github.com/labstack/echo",
    "github.com/gorilla/mux",
    "github.com/julienschmidt/httprouter",
    "github.com/gofiber/fiber",
    "/common/route\"",
    "github.com/uptrace/bunrouter",
    "github.com/beego",
];

const JS_SERVERS: &[&str] = &[
    "express",
    "fastify",
    "@nestjs/",
    "koa-router",
    "@koa/router",
    "hono",
    "restify",
    "@hapi/",
    "polka",
];

/// Receivers whose `.post(...)` is a client's request, not a route.
const CLIENTS: &[&str] = &[
    "http",
    "axios",
    "client",
    "request",
    "requests",
    "superagent",
    "fetch",
    "req",
    "res",
    "httpClient",
    "session",
    "ky",
    "got",
    "needle",
    "agent",
    "api",
    "mock",
    "mocker",
    "self",
    "this",
    "s",
    "resp",
    "response",
];

fn is_client(recv: &str) -> bool {
    let l = recv.to_ascii_lowercase();
    CLIENTS.contains(&recv) || l.ends_with("client") || l.contains("mock")
}

// ---------------------------------------------------------------- the recognizers

struct Ctx<'a> {
    fi: usize,
    f: &'a Src,
    out: &'a mut Vec<Cand>,
    /// file-scoped groups: variable -> (prefix, parent variable)
    groups: &'a mut HashMap<String, (String, String)>,
    /// mounts by name: name -> (prefix, owner of the mount)
    mounts: &'a mut HashMap<String, (String, Option<String>)>,
    /// the project's Rust files, for module paths
    rust_files: &'a HashSet<String>,
}

/// A route found in a file: (method, path, handler, handler's line, offset, source, shape,
/// owner).
type Found = (
    String,
    String,
    String,
    Option<usize>,
    usize,
    &'static str,
    &'static str,
    Option<String>,
);

impl Ctx<'_> {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        method: &str,
        path: String,
        handler: String,
        handler_line: Option<usize>,
        at: usize,
        source: &'static str,
        shape: &'static str,
        owner: Option<String>,
    ) {
        let line = self.f.flat.line(at);
        self.out.push(Cand {
            method: method.into(),
            path,
            handler,
            handler_line,
            file: self.fi,
            line,
            source,
            shape,
            owner,
            owner_fn: self.f.enclosing(line).map(String::from),
            also: Vec::new(),
        });
    }
}

/// The prefixes that enclose a position: Rust's `.nest("/p", ...)` and actix-web's
/// `web::scope("/p")`, and chi's `r.Route("/p", func(r chi.Router) { ... })`.
fn scopes_at(f: &Src, at: usize) -> String {
    let fl = &f.flat;
    let mut pre: Vec<(usize, String)> = Vec::new();
    let consts = |n: &str| f.consts.get(n).cloned();
    for c in &fl.calls {
        let inside = match (f.lang, c.name.as_str()) {
            (Lang::Rust, "nest") => true,
            (Lang::Go, "Route") => true,
            (Lang::Rust, "scope") => false,
            _ => continue,
        };
        let Some(a0) = c.args.first() else { continue };
        let Some(p) = fl.path_arg(*a0, &consts) else {
            continue;
        };
        if !path_like(&p) {
            continue;
        }
        if inside {
            if c.open < at && at < c.close {
                pre.push((c.at, p));
            }
        } else {
            // a scope covers the rest of the call that holds it
            let end = fl
                .calls
                .iter()
                .filter(|o| o.open < c.at && c.at < o.close)
                .map(|o| o.close)
                .min()
                .unwrap_or_else(|| {
                    fl.s[c.close..]
                        .find(';')
                        .map(|x| c.close + x)
                        .unwrap_or(fl.s.len())
                });
            if c.close < at && at < end {
                pre.push((c.at, p));
            }
        }
    }
    pre.sort();
    pre.into_iter()
        .fold(String::new(), |acc, (_, p)| join_path(&acc, &p))
}

/// The variable that a statement assigns, searching back from a line (`let local =
/// Router::new()` over a chain of `.route(...)` lines).
fn chain_owner(code: &[String], line: usize) -> Option<String> {
    let lo = line.saturating_sub(80);
    for i in (lo..=line).rev() {
        let t = code[i].trim();
        if i < line && (t.ends_with(';') || t.ends_with('}')) {
            return None;
        }
        if let Some(rest) = t.strip_prefix("let ") {
            let rest = rest.trim_start_matches("mut ");
            let name: String = rest
                .bytes()
                .take_while(|c| is_ident_char(*c))
                .map(|c| c as char)
                .collect();
            return (!name.is_empty()).then_some(name);
        }
        if t.contains("fn ") {
            return None;
        }
    }
    None
}

fn rust_routes(cx: &mut Ctx) {
    let f = cx.f;
    let fl = &f.flat;
    let consts = |n: &str| f.consts.get(n).cloned();
    let mut found: Vec<Found> = Vec::new();
    let mut mounts: Vec<(String, String, Option<String>)> = Vec::new();
    // methods of a router expression between two offsets: axum's `post(h)` and
    // `routing::get(a).post(b)`, actix-web's `web::post().to(h)`
    let methods_between = |a: usize, b: usize| -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        for (k, c) in fl.calls.iter().enumerate() {
            if c.at <= a || c.at >= b {
                continue;
            }
            let Some(m) = method_of(&c.name) else {
                continue;
            };
            if c.name != c.name.to_ascii_lowercase() || c.name == "del" {
                continue;
            }
            let h = if let Some(a0) = c.args.first() {
                fl.text(*a0).to_string()
            } else {
                // web::post().to(h)
                fl.calls[k + 1..]
                    .iter()
                    .find(|n| n.name == "to" && n.at > c.close && n.at < c.close + 8)
                    .and_then(|n| n.args.first())
                    .map(|a| fl.text(*a).to_string())
                    .unwrap_or_default()
            };
            out.push((m, h));
        }
        out
    };
    for (k, c) in fl.calls.iter().enumerate() {
        let line = fl.line(c.at);
        match c.name.as_str() {
            // axum and actix-web: .route("/p", post(h)) / .route("/p", web::post().to(h))
            "route" if c.dot && c.args.len() >= 2 => {
                let Some(p) = fl.path_arg(c.args[0], &consts) else {
                    continue;
                };
                if !path_like(&p) {
                    continue;
                }
                let full = join_path(&scopes_at(f, c.at), &p);
                let owner = chain_owner(&f.code, line)
                    .map(|v| format!("{}#{v}", f.rel))
                    .or_else(|| f.enclosing(line).map(String::from));
                for (m, h) in methods_between(c.args[1].0, c.close) {
                    found.push((
                        m.into(),
                        full.clone(),
                        h,
                        None,
                        c.at,
                        "axum or actix-web",
                        "a router call",
                        owner.clone(),
                    ));
                }
            }
            // actix-web: web::resource("/p").route(web::post().to(h))
            "resource" if !c.args.is_empty() => {
                let Some(p) = fl.path_arg(c.args[0], &consts) else {
                    continue;
                };
                if !path_like(&p) {
                    continue;
                }
                let full = join_path(&scopes_at(f, c.at), &p);
                // the chain after it, up to the next resource or the end of the call that holds it
                let end = fl
                    .calls
                    .iter()
                    .filter(|o| o.open < c.at && c.at < o.close)
                    .map(|o| o.close)
                    .min()
                    .unwrap_or(fl.s.len());
                let next = fl.calls[k + 1..]
                    .iter()
                    .find(|o| o.name == "resource" && o.at > c.close)
                    .map(|o| o.at)
                    .unwrap_or(fl.s.len());
                let owner = f.enclosing(line).map(String::from);
                for (m, h) in methods_between(c.close, end.min(next)) {
                    found.push((
                        m.into(),
                        full.clone(),
                        h,
                        None,
                        c.at,
                        "actix-web",
                        "a resource",
                        owner.clone(),
                    ));
                }
            }
            // a mount: .nest("/p", other_router()) or .nest("/p", var)
            "nest" if c.args.len() >= 2 => {
                let Some(p) = fl.path_arg(c.args[0], &consts) else {
                    continue;
                };
                let t = fl.text(c.args[1]);
                let id: String = t
                    .bytes()
                    .take_while(|b| is_ident_char(*b) || *b == b':')
                    .map(|b| b as char)
                    .collect();
                let last = id.rsplit("::").next().unwrap_or("").to_string();
                if last.is_empty() || id.starts_with("Router") {
                    continue;
                }
                let key = if t.contains('(') {
                    last
                } else {
                    format!("{}#{last}", f.rel)
                };
                let full = join_path(&scopes_at(f, c.at), &p);
                let owner = chain_owner(&f.code, line)
                    .map(|v| format!("{}#{v}", f.rel))
                    .or_else(|| f.enclosing(line).map(String::from));
                mounts.push((key, full, owner));
            }
            // attributes: #[post("/p")] and #[actix_web::post("/p")]
            n if c.pre == '[' && method_of(n).is_some() && n == n.to_ascii_lowercase() => {
                let Some(a0) = c.args.first() else { continue };
                let Some(p) = fl.path_arg(*a0, &consts) else {
                    continue;
                };
                if !path_like(&p) {
                    continue;
                }
                let (h, hl) = next_function(f, line);
                found.push((
                    method_of(n).unwrap().into(),
                    normalize_path(&p),
                    h,
                    hl,
                    c.at,
                    "actix-web or rocket",
                    "an attribute",
                    None,
                ));
            }
            _ => {}
        }
    }
    // route tables of a macro: "/p" => [get(a), post(b)], "/q" => sub(module::Api); a table
    // inside `macro_rules!` whose path is `$route` takes its paths from the macro's invocations
    let b = fl.s.as_bytes();
    let mut i = 0;
    while let Some(x) = fl.s[i..].find("=>") {
        let at = i + x;
        i = at + 2;
        if fl.instr[at] {
            continue;
        }
        let before = fl.s[..at].trim_end();
        let (p, template) = if before.ends_with('"') {
            let q = before.len() - 1;
            if !fl.instr[q] {
                continue;
            }
            let mut st = q;
            while st > 0 && fl.instr[st - 1] {
                st -= 1;
            }
            (fl.s[st + 1..q].to_string(), None)
        } else {
            let var: String = before
                .bytes()
                .rev()
                .take_while(|c| is_ident_char(*c))
                .map(|c| c as char)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            // `$route =>`: `$` is an identifier character, so the name keeps it
            let Some(var) = var
                .strip_prefix('$')
                .filter(|v| !v.is_empty())
                .map(String::from)
            else {
                continue;
            };
            (String::new(), Some(var))
        };
        if template.is_none() && !path_like(&p) {
            continue;
        }
        // the entry ends at the next `,` at its depth or the next `=>`
        let mut depth = 0i32;
        let mut end = fl.s.len();
        for (k, ch) in b.iter().enumerate().skip(at + 2) {
            if fl.instr[k] {
                continue;
            }
            match ch {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth < 0 {
                        end = k;
                        break;
                    }
                }
                b',' if depth == 0 => {
                    end = k;
                    break;
                }
                _ => {}
            }
        }
        let line = fl.line(at);
        let owner = table_owner(&f.code, line).map(|t| format!("{}#{t}", f.rel));
        // (method or `$verb`, handler)
        let mut entry: Vec<(String, String, usize)> = Vec::new();
        for c in fl.calls.iter().filter(|c| c.at > at && c.at < end) {
            if c.name == "sub" {
                if let Some(a0) = c.args.first() {
                    let t = fl.text(*a0);
                    let last = t.rsplit("::").next().unwrap_or(t).to_string();
                    // by its module's file; by the bare name when the module is not found
                    let key = match rust_module(cx.rust_files, &f.rel, t) {
                        Some(file) => format!("{file}#{last}"),
                        None => last,
                    };
                    mounts.push((key, normalize_path(&p), owner.clone()));
                }
                continue;
            }
            // `$verb(...)`: `$` is an identifier character, so the name keeps it
            let var = c.name.starts_with('$');
            if !var
                && (method_of(&c.name).is_none()
                    || c.name != c.name.to_ascii_lowercase()
                    || c.name == "del")
            {
                continue;
            }
            let name = c.name.clone();
            let h = c
                .args
                .first()
                .map(|a| fl.text(*a).to_string())
                .unwrap_or_default();
            entry.push((name, h, c.at));
        }
        match template {
            None => {
                for (m, h, cat) in entry {
                    if let Some(m) = method_of(&m) {
                        found.push((
                            m.into(),
                            normalize_path(&p),
                            h,
                            None,
                            cat,
                            "a route table",
                            "a route table of a macro",
                            owner.clone(),
                        ));
                    }
                }
            }
            Some(var) => {
                // the invocations of the enclosing macro_rules! in this file
                let Some(mr) = fl.s[..at].rfind("macro_rules!") else {
                    continue;
                };
                let mname: String = fl.s[mr + 12..]
                    .trim_start()
                    .bytes()
                    .take_while(|c| is_ident_char(*c))
                    .map(|c| c as char)
                    .collect();
                for inv in fl.calls.iter().filter(|c| {
                    c.name == mname
                        && c.at > 0
                        && b[c.open - 1] == b'!'
                        && !(c.at >= 13 && fl.s[..c.at].trim_end().ends_with("macro_rules!"))
                }) {
                    for g in macro_groups(fl, inv) {
                        let Some(path) = g.get(&var).filter(|v| v.starts_with('"')) else {
                            continue;
                        };
                        let path = path.trim_matches('"').to_string();
                        for (m, h, _) in &entry {
                            let m = match m.strip_prefix('$') {
                                Some(v) => g.get(v).cloned().unwrap_or_default(),
                                None => m.clone(),
                            };
                            let Some(m) = method_of(&m) else { continue };
                            let mut h = h.clone();
                            for (k, v) in &g {
                                h = h.replace(&format!("${k}"), v.trim_matches('"'));
                            }
                            found.push((
                                m.into(),
                                normalize_path(&path),
                                h,
                                None,
                                inv.at,
                                "a route table",
                                "a route table of a macro",
                                owner.clone(),
                            ));
                        }
                    }
                }
            }
        }
    }
    for (m, p, h, hl, at, src, shape, owner) in found {
        cx.push(&m, p, h, hl, at, src, shape, owner);
    }
    for (k, p, o) in mounts {
        cx.mounts.entry(k).or_insert((p, o));
    }
}

/// The `key: value` pairs of each `{ ... }` group in a macro's invocation (a value is a literal
/// or an identifier; the first of each key counts).
fn macro_groups(fl: &Flat, inv: &Call) -> Vec<HashMap<String, String>> {
    let b = fl.s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (k, &c) in b.iter().enumerate().take(inv.close).skip(inv.open + 1) {
        if fl.instr[k] {
            continue;
        }
        match c {
            b'{' | b'(' | b'[' => {
                if depth == 0 && c == b'{' {
                    start = Some(k);
                }
                depth += 1;
            }
            b'}' | b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(st) = start.take() {
                        out.push(group_pairs(fl, st + 1, k));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn group_pairs(fl: &Flat, a: usize, z: usize) -> HashMap<String, String> {
    let b = fl.s.as_bytes();
    let mut m = HashMap::new();
    let mut depth = 0i32;
    let mut k = a;
    while k < z {
        if fl.instr[k] {
            k += 1;
            continue;
        }
        match b[k] {
            b'{' | b'(' | b'[' | b'<' => depth += 1,
            b'}' | b')' | b']' | b'>' => depth -= 1,
            b':' if depth == 0 && b.get(k + 1) != Some(&b':') && (k == 0 || b[k - 1] != b':') => {
                let key: String = fl.s[a..k]
                    .trim_end()
                    .bytes()
                    .rev()
                    .take_while(|c| is_ident_char(*c))
                    .map(|c| c as char)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let rest = fl.s[k + 1..z].trim_start();
                let v: String = if let Some(inner) = rest.strip_prefix('"') {
                    let e = inner.find('"').map(|e| e + 2).unwrap_or(rest.len());
                    rest[..e].to_string()
                } else {
                    rest.bytes()
                        .take_while(|c| is_ident_char(*c))
                        .map(|c| c as char)
                        .collect()
                };
                if !key.is_empty() && !v.is_empty() {
                    m.entry(key).or_insert(v);
                }
            }
            _ => {}
        }
        k += 1;
    }
    m
}

/// The file of a Rust module path named in `file` (`compact::CompactApi` from `tasks.rs` is
/// `tasks/compact.rs`), when the project has it.
fn rust_module(files: &HashSet<String>, file: &str, path: &str) -> Option<String> {
    let segs: Vec<&str> = path.split("::").map(str::trim).collect();
    let (mods, _) = segs.split_at(segs.len().checked_sub(1)?);
    let dir = file.rsplit_once('/').map(|x| x.0).unwrap_or("");
    let name = file.rsplit('/').next().unwrap_or(file);
    let mut base = if matches!(name, "mod.rs" | "lib.rs" | "main.rs") {
        dir.to_string()
    } else {
        format!("{dir}/{}", name.trim_end_matches(".rs"))
            .trim_start_matches('/')
            .to_string()
    };
    let mut mods = mods.to_vec();
    if mods.first() == Some(&"crate") {
        let i = file.find("src/")?;
        base = file[..i + 3].to_string();
        mods.remove(0);
    }
    while mods.first() == Some(&"super") {
        base = base
            .rsplit_once('/')
            .map(|x| x.0.to_string())
            .unwrap_or_default();
        mods.remove(0);
    }
    if mods.first() == Some(&"self") {
        mods.remove(0);
    }
    if mods.is_empty() {
        return files.contains(file).then(|| file.to_string());
    }
    let stem = format!("{base}/{}", mods.join("/"))
        .trim_start_matches('/')
        .to_string();
    [format!("{stem}.rs"), format!("{stem}/mod.rs")]
        .into_iter()
        .find(|c| files.contains(c))
}

/// The type that a route table of an attribute belongs to: the first `struct` after it.
fn table_owner(code: &[String], line: usize) -> Option<String> {
    for l in code.iter().skip(line).take(200) {
        let t = l.trim();
        if let Some(i) = t.find("struct ") {
            let name: String = t[i + 7..]
                .bytes()
                .take_while(|c| is_ident_char(*c))
                .map(|c| c as char)
                .collect();
            return (!name.is_empty()).then_some(name);
        }
    }
    None
}

/// The function defined after a decorator or an annotation at `line`, and its line.
fn next_function(f: &Src, line: usize) -> (String, Option<usize>) {
    if let Some(fd) = f
        .funcs
        .iter()
        .filter(|x| x.1 > line && x.1 <= line + 15)
        .min_by_key(|x| x.1)
    {
        return (fd.0.clone(), Some(fd.1));
    }
    // a method header without a body brace on its line (Java, TypeScript)
    for (i, l) in f.code.iter().enumerate().skip(line + 1).take(15) {
        let t = l.trim();
        if t.is_empty() || t.starts_with('@') || t.starts_with('#') {
            continue;
        }
        if let Some(p) = t.find('(') {
            let name: String = t[..p]
                .trim_end()
                .bytes()
                .rev()
                .take_while(|c| is_ident_char(*c))
                .map(|c| c as char)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !name.is_empty() {
                return (name, Some(i));
            }
        }
        break;
    }
    ("<unknown>".into(), None)
}

fn go_routes(cx: &mut Ctx) {
    let f = cx.f;
    let fl = &f.flat;
    let consts = |n: &str| f.consts.get(n).cloned();
    let router = GO_ROUTERS.iter().any(|r| f.head.contains(r));
    let nethttp = f.head.contains("\"net/http\"");
    let mut found: Vec<Found> = Vec::new();
    let mut groups: Vec<(String, String, String)> = Vec::new();
    let mut mounts: Vec<(String, String, Option<String>)> = Vec::new();
    let owner_of = |recv: &str, line: usize| -> Option<String> {
        if recv.is_empty() {
            return f.enclosing(line).map(String::from);
        }
        Some(format!("{}#{recv}", f.rel))
    };
    for (k, c) in fl.calls.iter().enumerate() {
        let line = fl.line(c.at);
        // a group: v1 := r.Group("/v1"), api := r.PathPrefix("/api").Subrouter(), r.WithPrefix("/p")
        if matches!(
            c.name.as_str(),
            "Group" | "PathPrefix" | "WithPrefix" | "Prefix"
        ) && c.dot
        {
            if let Some(a0) = c.args.first() {
                if let Some(p) = fl.path_arg(*a0, &consts) {
                    if path_like(&p) {
                        let before = fl.s[fl.starts[line]..c.at].trim();
                        let var = before
                            .split(":=")
                            .next()
                            .filter(|_| before.contains(":="))
                            .or_else(|| before.split('=').next().filter(|_| before.contains('=')))
                            .map(|v| v.trim().trim_start_matches("var ").trim().to_string())
                            .filter(|v| v.bytes().all(is_ident_char) && !v.is_empty());
                        if let Some(v) = var {
                            groups.push((v, p.clone(), c.recv.clone()));
                        } else if let Some(outer) = fl
                            .calls
                            .iter()
                            .filter(|o| o.open < c.at && c.at < o.close)
                            .max_by_key(|o| o.open)
                        {
                            // passed to a function: api.Register(r.WithPrefix("/api/v1"))
                            if outer
                                .name
                                .chars()
                                .next()
                                .is_some_and(|ch| ch.is_ascii_alphabetic())
                            {
                                mounts.push((
                                    outer.name.clone(),
                                    p.clone(),
                                    owner_of(&c.recv, line),
                                ));
                            }
                        }
                    }
                }
            }
            continue;
        }
        // net/http 1.22: mux.HandleFunc("POST /p", h)
        if matches!(c.name.as_str(), "HandleFunc" | "Handle") && c.args.len() >= 2 {
            if let Some(p0) = fl.path_arg(c.args[0], &consts) {
                if let Some((m, p)) = p0.split_once(' ') {
                    if let Some(m) = method_of(m).filter(|_| m == m.to_ascii_uppercase()) {
                        if path_like(p) && (nethttp || router) {
                            let h = fl.text(c.args[c.args.len() - 1]).to_string();
                            found.push((
                                m.into(),
                                p.to_string(),
                                h,
                                None,
                                c.at,
                                "net/http",
                                "a router call",
                                owner_of(&c.recv, line),
                            ));
                        }
                        continue;
                    }
                }
                // gorilla/mux: r.HandleFunc("/p", h).Methods("POST")
                if path_like(&p0) && router {
                    let methods = fl.calls[k + 1..]
                        .iter()
                        .take(4)
                        .find(|n| n.name == "Methods" && n.at > c.close && n.at < c.close + 3);
                    if let Some(mc) = methods {
                        let h = fl.text(c.args[c.args.len() - 1]).to_string();
                        for m in methods_in(&fl.s[mc.open..mc.close]) {
                            found.push((
                                m.into(),
                                p0.clone(),
                                h.clone(),
                                None,
                                c.at,
                                "gorilla/mux",
                                "a router call",
                                owner_of(&c.recv, line),
                            ));
                        }
                        continue;
                    }
                }
            }
            // gin, labstack's router, httprouter: r.Handle("POST", "/p", h)
            if c.args.len() >= 3 {
                let m0 = methods_in(fl.text(c.args[0]));
                if m0.len() == 1 && router {
                    if let Some(p) = fl.path_arg(c.args[1], &consts).filter(|p| path_like(p)) {
                        let h = fl.text(c.args[c.args.len() - 1]).to_string();
                        found.push((
                            m0[0].into(),
                            p,
                            h,
                            None,
                            c.at,
                            "a Go router",
                            "a router call",
                            owner_of(&c.recv, line),
                        ));
                    }
                }
            }
            continue;
        }
        // chi, gin, labstack's router, httprouter, fiber, Prometheus's route: r.Post("/p", h), r.POST(...)
        if !router || !c.dot || c.args.len() < 2 || is_client(&c.recv) {
            continue;
        }
        let Some(m) = method_of(&c.name) else {
            continue;
        };
        if c.name == c.name.to_ascii_lowercase() {
            continue; // Go exports: Post, POST, Delete, Del
        }
        let Some(p) = fl.path_arg(c.args[0], &consts) else {
            continue;
        };
        if !path_like(&p) {
            continue;
        }
        let full = join_path(&scopes_at(f, c.at), &p);
        let h = fl.text(c.args[c.args.len() - 1]).to_string();
        found.push((
            m.into(),
            full,
            h,
            None,
            c.at,
            "a Go router",
            "a router call",
            owner_of(&c.recv, line),
        ));
    }
    for (m, p, h, hl, at, src, shape, owner) in found {
        cx.push(&m, p, h, hl, at, src, shape, owner);
    }
    for (v, p, parent) in groups {
        let parent = if parent.is_empty() {
            String::new()
        } else {
            format!("{}#{parent}", f.rel)
        };
        cx.groups.insert(format!("{}#{v}", f.rel), (p, parent));
    }
    for (k, p, o) in mounts {
        cx.mounts.entry(k).or_insert((p, o));
    }
}

/// The gRPC methods of a Go server: the methods of a type that embeds
/// `Unimplemented<Service>Server`.
fn go_grpc(files: &[Src], declared: &HashSet<String>) -> Vec<(String, String, usize, usize)> {
    // type name -> services
    let mut types: HashMap<String, Vec<String>> = HashMap::new();
    for f in files.iter().filter(|f| f.lang == Lang::Go) {
        let mut cur: Option<String> = None;
        for l in &f.code {
            let t = l.trim();
            if let Some(rest) = t.strip_prefix("type ") {
                if rest.contains(" struct") {
                    cur = rest.split_whitespace().next().map(String::from);
                }
                continue;
            }
            if t == "}" {
                cur = None;
                continue;
            }
            if let (Some(ty), Some(i)) = (&cur, t.find("Unimplemented")) {
                let svc: String = t[i + 13..]
                    .bytes()
                    .take_while(|c| is_ident_char(*c))
                    .map(|c| c as char)
                    .collect();
                if let Some(s) = svc.strip_suffix("Server") {
                    types.entry(ty.clone()).or_default().push(s.to_string());
                }
            }
        }
    }
    let mut out = Vec::new();
    // a .proto file of the project is the better source: it names only the service's methods
    types.retain(|_, svcs| !svcs.iter().any(|s| declared.contains(s)));
    if types.is_empty() {
        return out;
    }
    for (fi, f) in files.iter().enumerate().filter(|(_, f)| f.lang == Lang::Go) {
        for (i, l) in f.code.iter().enumerate() {
            let t = l.trim();
            let Some(rest) = t.strip_prefix("func (") else {
                continue;
            };
            let Some(close) = rest.find(')') else {
                continue;
            };
            let ty = rest[..close]
                .split_whitespace()
                .last()
                .unwrap_or("")
                .trim_start_matches('*');
            let Some(svc) = types.get(ty).and_then(|v| v.first()) else {
                continue;
            };
            let after = rest[close + 1..].trim_start();
            let name: String = after
                .bytes()
                .take_while(|c| is_ident_char(*c))
                .map(|c| c as char)
                .collect();
            if name.is_empty()
                || !name.starts_with(|c: char| c.is_ascii_uppercase())
                || name.starts_with("mustEmbed")
            {
                continue;
            }
            if !after.contains("context.Context")
                && !after.contains("_Server")
                && !after.contains("Server)")
            {
                continue;
            }
            out.push((svc.clone(), name, fi, i));
        }
    }
    out
}

/// The gRPC methods of a Rust server: the methods of tonic's `#[async_trait] impl Service for T`
/// that take a `Request<...>`.
fn rust_grpc(files: &[Src]) -> Vec<(String, String, usize, usize)> {
    let mut out = Vec::new();
    for (fi, f) in files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.lang == Lang::Rust)
    {
        let code = &f.code;
        let mut i = 0;
        while i < code.len() {
            let t = code[i].trim();
            let asyncish = (i.saturating_sub(3)..i).any(|k| code[k].contains("async_trait"));
            if !(asyncish && t.starts_with("impl") && t.contains(" for ")) {
                i += 1;
                continue;
            }
            let head = t.split(" for ").next().unwrap_or("");
            let mut trait_path = head.trim_start_matches("impl").trim();
            if trait_path.starts_with('<') {
                // impl<T: FlightCraft> FlightService for ...
                let mut depth = 0;
                let mut cut = trait_path.len();
                for (k, ch) in trait_path.char_indices() {
                    match ch {
                        '<' => depth += 1,
                        '>' => {
                            depth -= 1;
                            if depth == 0 {
                                cut = k + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                trait_path = trait_path[cut..].trim();
            }
            let trait_path = trait_path.split('<').next().unwrap_or(trait_path);
            let svc = trait_path
                .rsplit("::")
                .next()
                .unwrap_or(trait_path)
                .trim()
                .to_string();
            // the impl's body: up to the closing brace at its indent
            let ind = code[i].len() - code[i].trim_start().len();
            let mut end = i + 1;
            while end < code.len() {
                let l = &code[end];
                if l.trim() == "}" && l.len() - l.trim_start().len() == ind {
                    break;
                }
                end += 1;
            }
            let mut methods = Vec::new();
            for k in i + 1..end {
                let l = code[k].trim();
                let Some(p) = l.find("fn ") else { continue };
                if !l.starts_with("async fn") && !l.contains(" async fn") {
                    continue;
                }
                let name: String = l[p + 3..]
                    .bytes()
                    .take_while(|c| is_ident_char(*c))
                    .map(|c| c as char)
                    .collect();
                let sig: String = code[k..(k + 6).min(end)].join(" ");
                if sig.contains("Request<") && sig.contains("Response<") {
                    methods.push((name, k));
                }
            }
            if !methods.is_empty() {
                for (name, k) in methods {
                    out.push((svc.clone(), name, fi, k));
                }
            }
            i = end + 1;
        }
    }
    out
}

fn js_routes(cx: &mut Ctx) {
    let f = cx.f;
    let fl = &f.flat;
    let consts = |n: &str| f.consts.get(n).cloned();
    let server = JS_SERVERS.iter().any(|s| {
        if s.ends_with('/') {
            f.head.contains(s)
        } else {
            f.head.contains(&format!("'{s}")) || f.head.contains(&format!("\"{s}"))
        }
    });
    let mut found: Vec<Found> = Vec::new();
    // NestJS: @Controller('p') before a class, @Post(':id') before a method
    let mut class_prefix: Vec<(usize, String)> = Vec::new();
    for c in &fl.calls {
        if c.pre == '@' && c.name == "Controller" {
            let p = c
                .args
                .first()
                .and_then(|a| {
                    let t = fl.text(*a);
                    if t.starts_with('{') {
                        fl.literals(*a).into_iter().next().map(|x| x.1)
                    } else {
                        fl.path_arg(*a, &consts)
                    }
                })
                .unwrap_or_default();
            class_prefix.push((fl.line(c.at), p));
        }
    }
    let prefix_at = |line: usize| -> String {
        class_prefix
            .iter()
            .filter(|(l, _)| *l <= line)
            .max_by_key(|(l, _)| *l)
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    };
    for c in &fl.calls {
        let line = fl.line(c.at);
        if c.pre == '@' && !c.dot {
            if let Some(m) = method_of(&c.name).filter(|_| c.name.starts_with(char::is_uppercase)) {
                if c.name == "Del" {
                    continue;
                }
                let p = c
                    .args
                    .first()
                    .and_then(|a| fl.path_arg(*a, &consts))
                    .unwrap_or_default();
                let (h, hl) = next_function(f, line);
                let full = join_path(&prefix_at(line), &p);
                found.push((m.into(), full, h, hl, c.at, "NestJS", "a decorator", None));
            }
            continue;
        }
        if !server {
            continue;
        }
        // hapi and fastify: server.route({ method: 'POST', path: '/p', handler })
        if c.name == "route" && c.args.len() == 1 && fl.text(c.args[0]).starts_with('{') {
            let t = &fl.s[c.args[0].0..c.args[0].1];
            let ms = t
                .find("method")
                .map(|i| methods_in(&t[i..(i + 60).min(t.len())]))
                .unwrap_or_default();
            let lits = fl.literals(c.args[0]);
            let path = ["path", "url"].iter().find_map(|k| {
                let i = t.find(&format!("{k}:"))?;
                lits.iter()
                    .find(|(st, _)| *st > c.args[0].0 + i)
                    .map(|x| x.1.clone())
            });
            let h = t.find("handler").map(|i| {
                t[i + 7..]
                    .trim_start_matches([':', ' '])
                    .bytes()
                    .take_while(|c| is_ident_char(*c) || *c == b'.')
                    .map(|c| c as char)
                    .collect::<String>()
            });
            if let Some(p) = path.filter(|p| path_like(p)) {
                for m in ms {
                    found.push((
                        m.into(),
                        normalize_path(&p),
                        h.clone().unwrap_or_else(|| "<inline>".into()),
                        None,
                        c.at,
                        "hapi or fastify",
                        "a route object",
                        None,
                    ));
                }
            }
            continue;
        }
        // express, fastify, koa: app.post('/p', ..., handler)
        if !c.dot || c.args.len() < 2 || is_client(&c.recv) {
            continue;
        }
        let Some(m) = method_of(&c.name).filter(|_| c.name == c.name.to_ascii_lowercase()) else {
            continue;
        };
        if c.name == "del" && !f.head.contains("koa") {
            continue;
        }
        let Some(p) = fl.path_arg(c.args[0], &consts) else {
            continue;
        };
        if !path_like(&p) {
            continue;
        }
        let full = join_path(&scopes_at(f, c.at), &p);
        let h = fl.text(c.args[c.args.len() - 1]).to_string();
        found.push((
            m.into(),
            full,
            h,
            None,
            c.at,
            "express, fastify or koa",
            "a router call",
            Some(format!("{}#{}", f.rel, c.recv)),
        ));
    }
    for (m, p, h, hl, at, src, shape, owner) in found {
        cx.push(&m, p, h, hl, at, src, shape, owner);
    }
}

fn python_routes(cx: &mut Ctx) {
    let f = cx.f;
    let fl = &f.flat;
    let consts = |n: &str| f.consts.get(n).cloned();
    let mut found: Vec<Found> = Vec::new();
    let mut groups: Vec<(String, String)> = Vec::new();
    for c in &fl.calls {
        let line = fl.line(c.at);
        // router = APIRouter(prefix="/p"), bp = Blueprint("n", __name__, url_prefix="/p")
        if matches!(c.name.as_str(), "APIRouter" | "Blueprint") {
            let t = &fl.s[c.open..c.close];
            let lits = fl.literals((c.open, c.close));
            let key = if c.name == "APIRouter" {
                "prefix"
            } else {
                "url_prefix"
            };
            if let Some(i) = t.find(key) {
                if let Some((_, p)) = lits.iter().find(|(st, _)| *st > c.open + i) {
                    let before = fl.s[fl.starts[line]..c.at].trim();
                    if let Some(v) = before.strip_suffix('=').map(|v| v.trim().to_string()) {
                        let v = v
                            .rsplit('.')
                            .next()
                            .unwrap_or(&v)
                            .trim_end_matches(':')
                            .trim()
                            .to_string();
                        groups.push((v, p.clone()));
                    }
                }
            }
            continue;
        }
        if c.pre != '@' || !c.dot || c.args.is_empty() || is_client(&c.recv) || c.recv == "patch" {
            continue;
        }
        let Some(p) = fl.path_arg(c.args[0], &consts) else {
            continue;
        };
        if !path_like(&p) {
            continue;
        }
        let (h, hl) = next_function(f, line);
        let owner = Some(format!("{}#{}", f.rel, c.recv));
        if let Some(m) =
            method_of(&c.name).filter(|_| c.name == c.name.to_ascii_lowercase() && c.name != "del")
        {
            found.push((
                m.into(),
                normalize_path(&p),
                h,
                hl,
                c.at,
                "FastAPI or Flask",
                "a decorator",
                owner,
            ));
            continue;
        }
        if matches!(c.name.as_str(), "route" | "api_route") {
            let t = &fl.s[c.open..c.close];
            if let Some(i) = t.find("methods") {
                for m in methods_in(&t[i..]) {
                    found.push((
                        m.into(),
                        normalize_path(&p),
                        h.clone(),
                        hl,
                        c.at,
                        "FastAPI or Flask",
                        "a decorator",
                        owner.clone(),
                    ));
                }
            }
        }
    }
    for (m, p, h, hl, at, src, shape, owner) in found {
        cx.push(&m, p, h, hl, at, src, shape, owner);
    }
    for (v, p) in groups {
        cx.groups
            .insert(format!("{}#{v}", f.rel), (p, String::new()));
    }
}

/// Django: the views that `urls.py` files name, with the methods each view handles.
fn django_routes(files: &[Src]) -> Vec<Cand> {
    let mut out = Vec::new();
    // class name -> (file, line, methods); function name -> (file, line, methods)
    let mut views: HashMap<String, (usize, usize, Vec<&'static str>)> = HashMap::new();
    const DRF: &[(&str, &[&str])] = &[
        ("ModelViewSet", &["POST", "PUT", "PATCH", "DELETE"]),
        ("CreateAPIView", &["POST"]),
        ("ListCreateAPIView", &["POST"]),
        ("UpdateAPIView", &["PUT", "PATCH"]),
        ("DestroyAPIView", &["DELETE"]),
        ("RetrieveUpdateAPIView", &["PUT", "PATCH"]),
        ("RetrieveDestroyAPIView", &["DELETE"]),
        ("RetrieveUpdateDestroyAPIView", &["PUT", "PATCH", "DELETE"]),
        ("CreateModelMixin", &["POST"]),
        ("UpdateModelMixin", &["PUT", "PATCH"]),
        ("DestroyModelMixin", &["DELETE"]),
        ("CreateView", &["POST"]),
        ("UpdateView", &["POST"]),
        ("DeleteView", &["POST"]),
        ("FormView", &["POST"]),
    ];
    for (fi, f) in files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.lang == Lang::Python)
    {
        let code = &f.code;
        for (i, l) in code.iter().enumerate() {
            let t = l.trim();
            if let Some(rest) = t.strip_prefix("class ") {
                let name: String = rest
                    .bytes()
                    .take_while(|c| is_ident_char(*c))
                    .map(|c| c as char)
                    .collect();
                let bases = rest.split_once('(').map(|x| x.1).unwrap_or("");
                let mut ms: Vec<&'static str> = Vec::new();
                for (b, m) in DRF {
                    if bases.contains(b) {
                        for x in *m {
                            if !ms.contains(x) {
                                ms.push(x);
                            }
                        }
                    }
                }
                let ind = l.len() - t.len();
                for body in code.iter().skip(i + 1) {
                    let bt = body.trim();
                    if !bt.is_empty() && body.len() - bt.len() <= ind {
                        break;
                    }
                    for (d, m) in [
                        ("def post(", "POST"),
                        ("def put(", "PUT"),
                        ("def patch(", "PATCH"),
                        ("def delete(", "DELETE"),
                        ("def create(", "POST"),
                        ("def update(", "PUT"),
                        ("def partial_update(", "PATCH"),
                        ("def destroy(", "DELETE"),
                    ] {
                        let bt2 = bt.trim_start_matches("async ");
                        if bt2.starts_with(d) && !ms.contains(&m) {
                            ms.push(m);
                        }
                    }
                }
                if !name.is_empty() {
                    views.insert(name, (fi, i, ms));
                }
            } else if t.starts_with("def ") || t.starts_with("async def ") {
                let Some((name, s, e)) = f.funcs.iter().find(|x| x.1 == i).cloned() else {
                    continue;
                };
                let mut ms: Vec<&'static str> = Vec::new();
                let first = (0..i)
                    .rev()
                    .take_while(|k| code[*k].trim().starts_with('@'))
                    .last()
                    .unwrap_or(i);
                let deco: String = code[first..i].join(" ");
                if deco.contains("require_POST") {
                    ms.push("POST");
                }
                for key in ["require_http_methods(", "api_view("] {
                    if let Some(p) = deco.find(key) {
                        for m in methods_in(&deco[p..]) {
                            if !ms.contains(&m) {
                                ms.push(m);
                            }
                        }
                    }
                }
                let body = code[s..=e.min(code.len() - 1)].join("\n");
                for m in ["POST", "PUT", "PATCH", "DELETE"] {
                    if (body.contains(&format!("method == \"{m}\""))
                        || body.contains(&format!("method == '{m}'")))
                        && !ms.contains(&m)
                    {
                        ms.push(m);
                    }
                }
                views.entry(name).or_insert((fi, i, ms));
            }
        }
    }
    for (fi, f) in files.iter().enumerate() {
        if f.lang != Lang::Python || !f.rel.ends_with("urls.py") {
            continue;
        }
        let fl = &f.flat;
        for c in &fl.calls {
            let drf = c.name == "register" && c.dot;
            let pattern = matches!(c.name.as_str(), "path" | "re_path" | "url") && !c.dot;
            if !pattern && !drf {
                continue;
            }
            if c.args.len() < 2 {
                continue;
            }
            let Some(p) = fl.literals(c.args[0]).into_iter().next().map(|x| x.1) else {
                continue;
            };
            let view = fl.text(c.args[1]);
            if view.starts_with("include") {
                continue;
            }
            let name = view.split(".as_view").next().unwrap_or(view);
            let name = name.rsplit('.').next().unwrap_or(name).trim();
            let Some((vf, vl, ms)) = views.get(name) else {
                continue;
            };
            let p = p.trim_start_matches('^').trim_end_matches('$').to_string();
            for m in ms {
                out.push(Cand {
                    method: (*m).into(),
                    path: normalize_path(&p),
                    handler: name.to_string(),
                    handler_line: Some(*vl),
                    file: fi,
                    line: fl.line(c.at),
                    source: "Django",
                    shape: if drf {
                        "a REST framework router"
                    } else {
                        "a URL pattern"
                    },
                    owner: None,
                    owner_fn: None,
                    also: vec![format!("{}:{}", files[*vf].rel, vl + 1)],
                });
            }
        }
    }
    out
}

fn jvm_routes(cx: &mut Ctx) {
    let f = cx.f;
    let code = &f.code;
    let mut class_prefix: Vec<(usize, String)> = Vec::new();
    let mut found = Vec::new();
    let first_lit = |s: &str| -> Option<String> {
        let i = s.find('"')?;
        let j = s[i + 1..].find('"')?;
        Some(s[i + 1..i + 1 + j].to_string())
    };
    // annotation groups: consecutive lines that start with @
    let mut i = 0;
    while i < code.len() {
        let t = code[i].trim();
        if !t.starts_with('@') {
            i += 1;
            continue;
        }
        let start = i;
        let mut group = String::new();
        while i < code.len()
            && (code[i].trim().starts_with('@') || (code[i].trim().is_empty() && i > start))
        {
            group.push_str(code[i].trim());
            group.push('\n');
            i += 1;
        }
        let next = code.get(i).map(|s| s.trim()).unwrap_or("");
        let is_class = next.contains("class ") || next.contains("interface ");
        let mut methods: Vec<&'static str> = Vec::new();
        let mut path: Option<String> = None;
        for a in group.lines() {
            let name: String = a
                .get(1..)
                .unwrap_or("")
                .bytes()
                .take_while(|c| is_ident_char(*c) || *c == b'.')
                .map(|c| c as char)
                .collect();
            let name = name.rsplit('.').next().unwrap_or("").to_string();
            let args = a.split_once('(').map(|x| x.1).unwrap_or("");
            match name.as_str() {
                "PostMapping" | "PutMapping" | "PatchMapping" | "DeleteMapping" => {
                    methods.push(method_of(name.trim_end_matches("Mapping")).unwrap_or("POST"));
                    path = Some(first_lit(args).unwrap_or_default());
                }
                "RequestMapping" => {
                    if let Some(p) = args.find("method") {
                        methods.extend(methods_in(&args[p..]));
                    }
                    path = Some(first_lit(args).unwrap_or_default());
                }
                "POST" | "PUT" | "PATCH" | "DELETE" => methods.push(method_of(&name).unwrap()),
                "Path" => path = Some(first_lit(args).unwrap_or_default()),
                _ => {}
            }
        }
        if is_class {
            if let Some(p) = path {
                class_prefix.push((i, p));
            }
            continue;
        }
        if methods.is_empty() {
            continue;
        }
        let prefix = class_prefix
            .iter()
            .filter(|(l, _)| *l <= i)
            .max_by_key(|(l, _)| *l)
            .map(|(_, p)| p.clone())
            .unwrap_or_default();
        let (h, hl) = next_function(f, i.saturating_sub(1));
        let full = join_path(&prefix, &path.unwrap_or_default());
        let at = f.flat.starts[start];
        for m in methods {
            found.push((m, full.clone(), h.clone(), hl, at));
        }
    }
    for (m, p, h, hl, at) in found {
        cx.push(m, p, h, hl, at, "Spring or JAX-RS", "an annotation", None);
    }
}

// ---------------------------------------------------------------- descriptions: protobuf and OpenAPI

struct Rpc {
    service: String,
    method: String,
    path: String,
    http: Option<(&'static str, String)>,
    file: String,
    line: usize,
}

/// The services of a `.proto` file, with each method's HTTP mapping (`google.api.http`).
fn proto_rpcs(rel: &str, text: &str) -> Vec<Rpc> {
    let lines = clockscan::clean_with(Lang::C, text, true);
    let mut pkg = String::new();
    let mut svc: Option<String> = None;
    let mut depth = 0i32;
    let mut svc_depth = 0i32;
    let mut out: Vec<Rpc> = Vec::new();
    let mut open_rpc: Option<usize> = None;
    let mut rpc_depth = 0i32;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if let Some(p) = t.strip_prefix("package ") {
            pkg = p.trim_end_matches(';').trim().to_string();
        }
        if let Some(rest) = t.strip_prefix("service ") {
            svc = rest
                .split_whitespace()
                .next()
                .map(|s| s.trim_end_matches('{').to_string());
            svc_depth = depth;
        }
        if let (Some(s), Some(rest)) = (&svc, t.strip_prefix("rpc ")) {
            let name: String = rest
                .trim()
                .bytes()
                .take_while(|c| is_ident_char(*c))
                .map(|c| c as char)
                .collect();
            let full = if pkg.is_empty() {
                s.clone()
            } else {
                format!("{pkg}.{s}")
            };
            out.push(Rpc {
                service: s.clone(),
                method: name.clone(),
                path: format!("/{full}/{name}"),
                http: None,
                file: rel.to_string(),
                line: i,
            });
            open_rpc = Some(out.len() - 1);
            rpc_depth = depth;
        }
        if let Some(k) = open_rpc {
            for (key, m) in [
                ("post:", "POST"),
                ("put:", "PUT"),
                ("patch:", "PATCH"),
                ("delete:", "DELETE"),
                ("get:", "GET"),
            ] {
                if let Some(p) = t.find(key) {
                    let rest = &t[p + key.len()..];
                    if let Some(a) = rest.find('"') {
                        if let Some(b) = rest[a + 1..].find('"') {
                            out[k].http = Some((m, rest[a + 1..a + 1 + b].to_string()));
                        }
                    }
                }
            }
        }
        for c in t.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if open_rpc.is_some() && depth <= rpc_depth {
            open_rpc = None;
        }
        if svc.is_some() && depth <= svc_depth && t.contains('}') {
            svc = None;
        }
    }
    out
}

/// A `google.api.http` path with each `{field=segments}` written as its segments, a `*` as a
/// parameter (`/api/{name=memos/*}` is `/api/memos/{name}`).
fn http_template(p: &str) -> String {
    let mut out = String::new();
    let mut rest = p;
    while let Some(a) = rest.find('{') {
        out.push_str(&rest[..a]);
        let Some(z) = rest[a..].find('}') else {
            out.push_str(&rest[a..]);
            return out;
        };
        let inner = &rest[a + 1..a + z];
        match inner.split_once('=') {
            Some((field, segs)) => {
                let name = field.rsplit('.').next().unwrap_or(field);
                out.push_str(
                    &segs
                        .replace("**", "{rest}")
                        .replace('*', &format!("{{{name}}}")),
                );
            }
            None => out.push_str(&rest[a..=a + z]),
        }
        rest = &rest[a + z + 1..];
    }
    out.push_str(rest);
    out
}

struct Op {
    method: &'static str,
    path: String,
    op_id: String,
    file: String,
    line: usize,
}

/// The line (from 0) where a key starts in a YAML or JSON text.
fn key_line(text: &str, key: &str) -> usize {
    let forms = [
        format!("{key}:"),
        format!("\"{key}\":"),
        format!("'{key}':"),
        format!("\"{key}\" :"),
    ];
    text.lines()
        .position(|l| {
            let t = l.trim_start();
            forms.iter().any(|f| t.starts_with(f.as_str()))
        })
        .unwrap_or(0)
}

/// The write operations of an OpenAPI 3 or Swagger 2 file, following `$ref` path items into
/// other files.
fn openapi_ops(root: &Path, rel: &str, text: &str) -> Option<Vec<Op>> {
    let spec = hydo_compose::openapi::load_spec(text).ok()?;
    if spec.get("openapi").is_none() && spec.get("swagger").is_none() {
        return None;
    }
    let paths = spec.get("paths")?.as_object()?;
    let base = spec.get("basePath").and_then(Value::as_str).unwrap_or("");
    let dir = Path::new(rel)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let mut cache: HashMap<String, (String, Value)> = HashMap::new();
    let mut out = Vec::new();
    for (p, item) in paths {
        let mut item = item.clone();
        let mut file = rel.to_string();
        let mut ftext = text.to_string();
        let mut key = p.clone();
        if let Some(r) = item.get("$ref").and_then(Value::as_str).map(String::from) {
            let (fpart, ptr) = r.split_once('#').unwrap_or((r.as_str(), ""));
            if !fpart.is_empty() {
                let target = normalize_rel(&dir.join(fpart).to_string_lossy());
                if !cache.contains_key(&target) {
                    let Ok(t) = std::fs::read_to_string(root.join(&target)) else {
                        continue;
                    };
                    let Ok(v) = hydo_compose::openapi::load_spec(&t) else {
                        continue;
                    };
                    cache.insert(target.clone(), (t, v));
                }
                let (t, v) = &cache[&target];
                file = target;
                ftext = t.clone();
                item = v.pointer(ptr).cloned().unwrap_or(Value::Null);
            } else {
                item = spec.pointer(ptr).cloned().unwrap_or(Value::Null);
            }
            key = ptr
                .rsplit('/')
                .next()
                .unwrap_or(&key)
                .replace("~1", "/")
                .replace("~0", "~");
        }
        let Some(obj) = item.as_object() else {
            continue;
        };
        let line = key_line(&ftext, &key);
        for (m, op) in obj {
            let Some(method) = method_of(m).filter(|_| m != "del") else {
                continue;
            };
            let op_id = op
                .get("operationId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let line = (!op_id.is_empty())
                .then(|| {
                    ftext.lines().position(|l| {
                        let t = l.trim_start();
                        (t.starts_with("operationId:") || t.starts_with("\"operationId\""))
                            && t.trim_end()
                                .trim_end_matches(',')
                                .trim_matches(['"', '\''])
                                .ends_with(&op_id)
                    })
                })
                .flatten()
                .unwrap_or(line);
            out.push(Op {
                method,
                path: normalize_path(&format!("{base}{p}")),
                op_id,
                file: file.clone(),
                line,
            });
        }
    }
    Some(out)
}

fn normalize_rel(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            x => out.push(x),
        }
    }
    out.join("/")
}

// ---------------------------------------------------------------- the scan

/// The identifier written before a handler's name in its expression: the module of
/// `event::delete_pipeline`, the receiver of `api.remoteWrite`.
fn qualifier(expr: &str, name: &str) -> Option<String> {
    let at = expr.rfind(name)?;
    let before = expr[..at].trim_end_matches([':', '.']);
    if before.len() == expr[..at].len() {
        return None;
    }
    let q: String = before
        .bytes()
        .rev()
        .take_while(|c| is_ident_char(*c))
        .map(|c| c as char)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (!q.is_empty()).then_some(q)
}

/// A function's name as the scan matches it: lower case letters and digits (`workflow:delete`,
/// `WorkflowDelete` and `workflow_delete` are one name).
fn fn_key(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn snake(s: &str) -> String {
    words(s).join("_")
}

/// The name of the handler in an expression: the innermost identifier, the last of its depth
/// (`SeqHandler(create_index)`, `wrapAgent(api.deleteSeries)`); `<inline>` for a closure.
fn handler_name(expr: &str) -> String {
    let e = expr.trim();
    if e.is_empty() {
        return "<unknown>".into();
    }
    if e.contains("=>")
        || e.starts_with("func(")
        || e.starts_with("function")
        || e.starts_with('|')
        || e.starts_with("move")
        || e.starts_with("async")
        || e.starts_with("lambda")
        || e.starts_with("||")
    {
        return "<inline>".into();
    }
    let b = e.as_bytes();
    let mut best = (i32::MIN, String::new());
    let mut depth = 0;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'(' || c == b'<' {
            depth += 1;
        } else if c == b')' || c == b'>' {
            depth -= 1;
        } else if c == b'"' || c == b'\'' {
            // skip a literal
            let q = c;
            i += 1;
            while i < b.len() && b[i] != q {
                i += 1;
            }
        } else if is_ident_char(c) && (i == 0 || !is_ident_char(b[i - 1])) {
            let st = i;
            while i < b.len() && is_ident_char(b[i]) {
                i += 1;
            }
            let w = &e[st..i];
            let kw = matches!(
                w,
                "move"
                    | "async"
                    | "await"
                    | "self"
                    | "Self"
                    | "new"
                    | "this"
                    | "crate"
                    | "super"
                    | "web"
                    | "routing"
                    | "handler"
                    | "handlers"
            );
            if !kw && !w.chars().next().is_some_and(|c| c.is_ascii_digit()) && depth >= best.0 {
                best = (depth, w.to_string());
            }
            continue;
        }
        i += 1;
    }
    if best.1.is_empty() {
        "<unknown>".into()
    } else {
        best.1
    }
}

/// Reads every route of the project in `root` that writes.
pub fn scan_writes(root: &Path) -> Writes {
    let mut paths = Vec::new();
    crate::rules::walk(root, &mut paths);
    let mut files: Vec<Src> = Vec::new();
    let mut protos: Vec<(String, String)> = Vec::new();
    let mut specs: Vec<(String, String)> = Vec::new();
    let mut tests = clockscan::TestItems::default();
    for p in paths {
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let low = rel.to_ascii_lowercase();
        if clockscan::skipped(&low) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        if meta.len() > 1_500_000 {
            continue;
        }
        let ext = low.rsplit_once('.').map(|x| x.1).unwrap_or("");
        let is_spec = matches!(ext, "yaml" | "yml" | "json");
        let lang = clockscan::lang_of(&rel);
        if lang.is_none() && ext != "proto" && !is_spec {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if ext == "proto" {
            protos.push((rel, text));
            continue;
        }
        if is_spec {
            let head: String = text
                .chars()
                .take(600)
                .collect::<String>()
                .to_ascii_lowercase();
            if head.contains("openapi") || head.contains("swagger") {
                specs.push((rel, text));
            }
            continue;
        }
        let Some(lang) = lang else { continue };
        if matches!(lang, Lang::Sql | Lang::C) || clockscan::generated(&text) {
            continue;
        }
        let o = clockscan::outline(lang, &text);
        tests.functions += o.tests.functions;
        tests.modules += o.tests.modules;
        let head: String = o
            .code
            .iter()
            .take(150)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
            .to_ascii_lowercase();
        let flat = Flat::new(lang, &o.code);
        let consts = constants(&o.code);
        files.push(Src {
            rel,
            lang,
            flat,
            code: o.code,
            funcs: o.funcs,
            heads: o.heads,
            consts,
            head,
        });
    }
    let read = files.len() + protos.len() + specs.len();

    // the routes of the code
    let mut cands: Vec<Cand> = Vec::new();
    let mut groups: HashMap<String, (String, String)> = HashMap::new();
    let mut mounts: HashMap<String, (String, Option<String>)> = HashMap::new();
    let rust_files: HashSet<String> = files
        .iter()
        .filter(|f| f.lang == Lang::Rust)
        .map(|f| f.rel.clone())
        .collect();
    for (fi, f) in files.iter().enumerate() {
        let mut cx = Ctx {
            fi,
            f,
            out: &mut cands,
            groups: &mut groups,
            mounts: &mut mounts,
            rust_files: &rust_files,
        };
        match f.lang {
            Lang::Rust => rust_routes(&mut cx),
            Lang::Go => go_routes(&mut cx),
            Lang::Js => js_routes(&mut cx),
            Lang::Python => python_routes(&mut cx),
            Lang::Jvm => jvm_routes(&mut cx),
            _ => {}
        }
    }
    cands.extend(django_routes(&files));

    // prefixes: groups of a file, then mounts by name
    // a variable that is neither a group nor mounted is a parameter: the function that holds the
    // declaration is mounted instead (`api.Register(r.WithPrefix("/api/v1"))`)
    let resolve = |owner: &Option<String>, owner_fn: &Option<String>| -> String {
        let mut prefix = String::new();
        let mut cur = owner.clone().or_else(|| owner_fn.clone());
        let mut fallback = if owner.is_some() {
            owner_fn.clone()
        } else {
            None
        };
        let mut seen = HashSet::new();
        while let Some(o) = cur.take() {
            if !seen.insert(o.clone()) || seen.len() > 12 {
                break;
            }
            let mut hit = false;
            if let Some((p, parent)) = groups.get(&o) {
                hit = true;
                prefix = join_path(p, &prefix);
                fallback = None;
                if !parent.is_empty() {
                    cur = Some(parent.clone());
                    continue;
                }
            }
            let bare = o
                .rsplit_once('#')
                .map(|x| x.1)
                .filter(|t| t.starts_with(|c: char| c.is_ascii_uppercase()));
            if let Some((p, parent)) = mounts.get(&o).or_else(|| bare.and_then(|t| mounts.get(t))) {
                prefix = join_path(p, &prefix);
                fallback = None;
                cur = parent.clone();
                continue;
            }
            if !hit {
                cur = fallback.take();
            }
        }
        prefix
    };
    // the functions of the code, by name
    let mut index: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (fi, f) in files.iter().enumerate() {
        for (name, start) in &f.heads {
            index.entry(fn_key(name)).or_default().push((fi, *start));
        }
    }
    // `hint`: the module or receiver written before the name (`event::delete_pipeline`)
    let find_fn =
        |name: &str, near: Option<usize>, hint: Option<&str>| -> Option<(String, usize)> {
            let defs = index.get(&fn_key(name))?;
            let pick = defs.iter().max_by_key(|(fi, line)| {
                let f = &files[*fi];
                let low = f.rel.to_ascii_lowercase();
                let mut s = 0i32;
                if let Some(h) = hint.map(str::to_ascii_lowercase) {
                    let stem = low
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .split('.')
                        .next()
                        .unwrap_or("");
                    if stem == h || low.split('/').any(|d| d == h) {
                        s += 100;
                    }
                }
                if let Some(n) = near {
                    if *fi == n {
                        s += 100;
                    }
                    let d1 = files[n].rel.rsplit_once('/').map(|x| x.0).unwrap_or("");
                    let d2 = f.rel.rsplit_once('/').map(|x| x.0).unwrap_or("");
                    if d1 == d2 {
                        s += 50;
                    }
                    if f.lang == files[n].lang {
                        s += 20;
                    }
                    // the directories they share
                    let common = files[n]
                        .rel
                        .split('/')
                        .zip(f.rel.split('/'))
                        .take_while(|(a, b)| a == b)
                        .count() as i32;
                    s += 5 * common;
                }
                if ["handler", "server", "api", "route", "controller", "service"]
                    .iter()
                    .any(|w| low.contains(w))
                {
                    s += 10;
                }
                if ["frontend", "web/", "client", "sdk", "ui/"]
                    .iter()
                    .any(|w| low.contains(w))
                {
                    s -= 30;
                }
                // a definition with a body before a trait's declaration
                if f.funcs.iter().any(|x| x.1 == *line)
                    || f.code.get(*line).is_some_and(|l| l.contains('{'))
                {
                    s += 8;
                }
                // the exact spelling first
                if f.heads.iter().any(|x| x.1 == *line && x.0 == name) {
                    s += 5;
                }
                s
            })?;
            Some((files[pick.0].rel.clone(), pick.1 + 1))
        };

    let mut routes: Vec<Route> = Vec::new();
    let mut reads: Vec<Route> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut add = |r: Route, routes: &mut Vec<Route>, reads: &mut Vec<Route>, read: bool| {
        if read {
            // one entry per route, as for the writes
            let k = path_key(&r.method, &r.path);
            if !reads.iter().any(|x| path_key(&x.method, &x.path) == k) {
                reads.push(r);
            }
            return;
        }
        let mut keys = vec![path_key(&r.method, &r.path)];
        for a in &r.also {
            keys.push(path_key(
                if a.starts_with('/') && r.method == "RPC" {
                    "POST"
                } else {
                    &r.method
                },
                a,
            ));
        }
        if r.method == "POST" {
            keys.push(path_key("RPC", &r.path));
        }
        for k in &keys {
            if let Some(&i) = by_key.get(k) {
                let e: &mut Route = &mut routes[i];
                let place = format!("{}:{} ({})", r.file, r.line, r.source);
                if !e.elsewhere.contains(&place)
                    && format!("{}:{}", e.file, e.line) != format!("{}:{}", r.file, r.line)
                {
                    e.elsewhere.push(place);
                }
                if e.handler_at.is_none() && r.handler_at.is_some() {
                    e.handler = r.handler.clone();
                    e.handler_at = r.handler_at.clone();
                }
                return;
            }
        }
        for k in keys {
            by_key.insert(k, routes.len());
        }
        routes.push(r);
    };

    for c in &cands {
        let f = &files[c.file];
        let path = join_path(&resolve(&c.owner, &c.owner_fn), &c.path);
        let handler = if c.handler_line.is_some() {
            c.handler.clone()
        } else {
            handler_name(&c.handler)
        };
        let handler_at = match c.handler_line {
            Some(l) if c.source != "Django" => Some((f.rel.clone(), l + 1)),
            Some(_) => c.also.first().and_then(|a| {
                a.rsplit_once(':')
                    .map(|(f, l)| (f.to_string(), l.parse().unwrap_or(0)))
            }),
            None if handler == "<inline>" => Some((f.rel.clone(), c.line + 1)),
            None => find_fn(
                &handler,
                Some(c.file),
                qualifier(&c.handler, &handler).as_deref(),
            ),
        };
        // a Django view names its write methods itself (`def post`, a REST framework base class)
        let read = c.method == "POST" && c.source != "Django" && reads_by_name(&path, &handler);
        let r = make_route(
            &c.method,
            path,
            Vec::new(),
            handler,
            handler_at,
            f.rel.clone(),
            c.line + 1,
            c.source,
            c.shape,
        );
        add(r, &mut routes, &mut reads, read);
    }

    // RPC methods: the .proto files first, then servers without one
    let mut declared: HashSet<(String, String)> = HashSet::new();
    for (rel, text) in &protos {
        for rpc in proto_rpcs(rel, text) {
            declared.insert((rpc.service.clone(), rpc.method.clone()));
            let (method, path, also) = match &rpc.http {
                Some((m, p)) => (
                    m.to_string(),
                    normalize_path(&http_template(p)),
                    vec![rpc.path.clone()],
                ),
                None => ("RPC".to_string(), rpc.path.clone(), Vec::new()),
            };
            // a method mapped to GET is a read, and no POST route that reads: leave it out
            if method == "GET" {
                continue;
            }
            let handler_at = find_fn(&rpc.method, None, None)
                .or_else(|| find_fn(&snake(&rpc.method), None, None));
            let read = (method == "RPC" || method == "POST") && reads_by_name("", &rpc.method);
            let r = make_route(
                &method,
                path,
                also,
                rpc.method.clone(),
                handler_at,
                rpc.file.clone(),
                rpc.line + 1,
                "protobuf",
                "a protobuf service",
            );
            add(r, &mut routes, &mut reads, read);
        }
    }
    let services: HashSet<String> = declared.iter().map(|(s, _)| s.clone()).collect();
    let mut servers = go_grpc(&files, &services);
    servers.extend(rust_grpc(&files));
    for (svc, name, fi, line) in servers {
        let camel: String = words(&name)
            .iter()
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|x| x.to_ascii_uppercase().to_string() + c.as_str())
                    .unwrap_or_default()
            })
            .collect();
        if declared.contains(&(svc.clone(), name.clone()))
            || declared.contains(&(svc.clone(), camel.clone()))
        {
            continue;
        }
        let f = &files[fi];
        let path = format!("/{svc}/{camel}");
        let read = reads_by_name("", &name);
        let r = make_route(
            "RPC",
            path,
            Vec::new(),
            name.clone(),
            Some((f.rel.clone(), line + 1)),
            f.rel.clone(),
            line + 1,
            "a gRPC server",
            "a gRPC server's method",
        );
        add(r, &mut routes, &mut reads, read);
    }
    // OpenAPI and Swagger files
    for (rel, text) in &specs {
        let Some(ops) = openapi_ops(root, rel, text) else {
            continue;
        };
        for op in ops {
            // `MemoService_CreateMemo` is the method `CreateMemo` of a generated gateway
            let handler_at = (!op.op_id.is_empty())
                .then(|| {
                    find_fn(&op.op_id, None, None).or_else(|| {
                        op.op_id
                            .rsplit_once('_')
                            .and_then(|(_, m)| find_fn(m, None, None))
                    })
                })
                .flatten();
            let handler = if op.op_id.is_empty() {
                "<no operationId>".to_string()
            } else {
                op.op_id.clone()
            };
            let read = op.method == "POST" && reads_by_name(&op.path, &op.op_id);
            let r = make_route(
                op.method,
                op.path,
                Vec::new(),
                handler,
                handler_at,
                op.file,
                op.line + 1,
                "OpenAPI",
                "an OpenAPI operation",
            );
            add(r, &mut routes, &mut reads, read);
        }
    }
    rank(&mut routes);
    rank(&mut reads);
    Writes {
        read,
        routes,
        reads,
        tests,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_route(
    method: &str,
    path: String,
    also: Vec<String>,
    handler: String,
    handler_at: Option<(String, usize)>,
    file: String,
    line: usize,
    source: &str,
    shape: &str,
) -> Route {
    let (class, word) = classify(method, &path, &handler);
    let what = match class {
        "data" => match &word {
            Some(w) => format!("it changes the system's data ({w})"),
            None => "it changes the system's data".to_string(),
        },
        "admin" => format!(
            "an operation on the stored data as a whole ({})",
            word.clone().unwrap_or_default()
        ),
        "session" => format!(
            "a sign-in or a token ({}), not the system's data",
            word.clone().unwrap_or_default()
        ),
        _ => format!(
            "the process's own state ({}), not durable data",
            word.clone().unwrap_or_default()
        ),
    };
    let how = if method == "RPC" {
        format!("{shape}, not a read by its name")
    } else {
        format!("{method} in {shape}")
    };
    let mut score = class_rank(class);
    if handler_at.is_some() {
        score += 3;
    }
    if word.is_some() && class == "data" {
        score += 2;
    }
    let resource = resource_of(method, &path);
    Route {
        method: method.into(),
        path,
        also,
        handler,
        handler_at,
        file,
        line,
        source: source.into(),
        elsewhere: Vec::new(),
        resource,
        class,
        why: format!("{how}; {what}"),
        score,
    }
}

/// Ranks the routes: resources by their best route, then the routes of each resource.
fn rank(routes: &mut [Route]) {
    for r in routes.iter_mut() {
        if !r.elsewhere.is_empty() {
            r.score += 1;
        }
    }
    let mut best: HashMap<String, i32> = HashMap::new();
    for r in routes.iter() {
        let e = best.entry(r.resource.clone()).or_insert(i32::MIN);
        *e = (*e).max(r.score);
    }
    routes.sort_by(|a, b| {
        best[&b.resource]
            .cmp(&best[&a.resource])
            .then_with(|| a.resource.cmp(&b.resource))
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.method.cmp(&b.method))
    });
}

// ---------------------------------------------------------------- the reports

pub fn route_json(r: &Route, rank: usize) -> Value {
    json!({
        "rank": rank,
        "method": r.method,
        "path": r.path,
        "also": r.also,
        "resource": r.resource,
        "class": r.class,
        "handler": r.handler,
        "handler_file": r.handler_at.as_ref().map(|h| h.0.clone()),
        "handler_line": r.handler_at.as_ref().map(|h| h.1),
        "file": r.file,
        "line": r.line,
        "source": r.source,
        "elsewhere": r.elsewhere,
        "why": r.why,
    })
}

pub fn json_report(dir: &str, w: &Writes) -> String {
    let doc = json!({
        "dir": dir,
        "files_read": w.read,
        "writes": w.routes.iter().enumerate().map(|(i, r)| route_json(r, i + 1)).collect::<Vec<_>>(),
        "reads_left_out": w.reads.iter().enumerate().map(|(i, r)| route_json(r, i + 1)).collect::<Vec<_>>(),
        "test_code_left_out": {"functions": w.tests.functions, "modules": w.tests.modules},
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn by_class(routes: &[Route]) -> String {
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for r in routes {
        *n.entry(r.class).or_default() += 1;
    }
    ["data", "admin", "session", "runtime"]
        .iter()
        .filter_map(|c| n.get(c).map(|k| format!("{k} {c}")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn route_lines(o: &mut String, r: &Route) {
    use std::fmt::Write as _;
    let h = match &r.handler_at {
        Some((f, l)) => format!("{} ({f}:{l})", r.handler),
        None => format!("{} (not found)", r.handler),
    };
    let _ = writeln!(o, "  {:<6} {}  [{}]", r.method, r.path, r.class);
    let _ = writeln!(o, "         handler {h}");
    let _ = writeln!(o, "         declared at {}:{}: {}", r.file, r.line, r.why);
    for e in &r.elsewhere {
        let _ = writeln!(o, "         also at {e}");
    }
    for a in &r.also {
        let _ = writeln!(o, "         also named {a}");
    }
}

pub fn text_report(dir: &str, w: &Writes, max: usize, all: bool) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let resources: HashSet<&str> = w.routes.iter().map(|r| r.resource.as_str()).collect();
    let _ = writeln!(
        o,
        "hydo rules scan --kind writes: {} routes that write, in {} resources, in {} files of {dir}",
        w.routes.len(),
        resources.len(),
        w.read
    );
    if !w.routes.is_empty() {
        let _ = writeln!(o, "  by class: {}", by_class(&w.routes));
    }
    let _ = writeln!(
        o,
        "  left out: {} POST routes and RPC methods whose names say that they read{}",
        w.reads.len(),
        if all || w.reads.is_empty() {
            ""
        } else {
            " (--all lists them)"
        }
    );
    if w.tests.functions + w.tests.modules > 0 {
        let _ = writeln!(
            o,
            "  left out: test code inside source files ({} functions, {} modules or classes)",
            w.tests.functions, w.tests.modules
        );
    }
    let _ = writeln!(
        o,
        "  Each route is a client operation: give the workload one command for each route of class data or admin."
    );
    let limit = if all { usize::MAX } else { max };
    let mut cur = "";
    for r in w.routes.iter().take(limit) {
        if r.resource != cur {
            cur = &r.resource;
            let n = w.routes.iter().filter(|x| x.resource == cur).count();
            let _ = writeln!(o, "\n{cur} ({n})");
        }
        route_lines(&mut o, r);
    }
    if w.routes.len() > limit {
        let _ = writeln!(
            o,
            "\nand {} more: --all, or --json for all",
            w.routes.len() - limit
        );
    }
    if all && !w.reads.is_empty() {
        let _ = writeln!(o, "\nleft out, read by their names ({})", w.reads.len());
        for r in &w.reads {
            let _ = writeln!(
                o,
                "  {:<6} {}  handler {}  ({}:{})",
                r.method, r.path, r.handler, r.file, r.line
            );
        }
    }
    o
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
pub fn scan_texts(texts: &[(&str, &str)]) -> Writes {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("hydo-writescan-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, text) in texts {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    let w = scan_writes(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(w: &Writes) -> Vec<String> {
        w.routes
            .iter()
            .map(|r| format!("{} {} -> {}", r.method, r.path, r.handler))
            .collect()
    }

    fn has_route(w: &Writes, s: &str) {
        let f = found(w);
        assert!(f.iter().any(|x| x == s), "{s} not in {f:#?}");
    }

    #[test]
    fn words_split_names_and_paths() {
        assert_eq!(words("deleteSeries"), ["delete", "series"]);
        assert_eq!(words("/swap-indexes"), ["swap", "indexes"]);
        assert_eq!(words("search_with_post"), ["search", "with", "post"]);
        assert_eq!(words("HTTPServer"), ["httpserver"]);
    }

    #[test]
    fn a_post_that_reads_by_its_name_is_left_out() {
        assert!(reads_by_name("/api/v1/query", "query"));
        assert!(reads_by_name("/indexes/{uid}/search", "search_with_post"));
        assert!(reads_by_name(
            "/indexes/{uid}/documents/fetch",
            "documents_by_query_post"
        ));
        assert!(!reads_by_name(
            "/indexes/{uid}/documents/delete-batch",
            "delete_documents_batch"
        ));
        assert!(!reads_by_name("/swap-indexes", "swap_indexes"));
        assert!(!reads_by_name("/tasks/cancel", "cancel_tasks"));
        assert!(reads_by_name("", "GetMemo"));
        assert!(!reads_by_name("", "CreateMemo"));
        // a leading word that names no action
        assert!(reads_by_name("", "do_get"));
        assert!(!reads_by_name("", "do_put"));
        assert!(reads_by_name("", "BatchGetUsers"));
        assert!(!reads_by_name("", "BatchDeleteAttachments"));
        // a word of querying anywhere in the handler's name
        assert!(reads_by_name("/labels", "labels_query"));
        assert!(!reads_by_name("/settings", "update_query_settings"));
    }

    #[test]
    fn http_templates_read_as_paths() {
        assert_eq!(
            http_template("/api/v1/{name=memos/*}"),
            "/api/v1/memos/{name}"
        );
        assert_eq!(
            http_template("/api/v1/{attachment.name=attachments/*}"),
            "/api/v1/attachments/{name}"
        );
        assert_eq!(
            http_template("/api/v1/{parent=users/*}/views"),
            "/api/v1/users/{parent}/views"
        );
        assert_eq!(http_template("/v1/{id}"), "/v1/{id}");
    }

    #[test]
    fn classes_put_durable_writes_before_logins_and_metrics() {
        assert_eq!(
            classify("POST", "/indexes/{uid}/documents", "replace_documents").0,
            "data"
        );
        assert_eq!(classify("POST", "/dumps", "create_dump").0, "admin");
        assert_eq!(classify("POST", "/users/login", "login_user").0, "session");
        assert_eq!(classify("POST", "/-/reload", "reload").0, "runtime");
        assert_eq!(classify("POST", "/health", "health").0, "runtime");
        // `/metrics` is the process's own, a POST of metrics to an ingest path is data
        assert_eq!(classify("POST", "/metrics", "metrics").0, "runtime");
        assert_eq!(classify("POST", "/otlp/v1/metrics", "otlp_write").0, "data");
        // an RPC is judged by its method, not by its service's name
        assert_eq!(
            classify("RPC", "/v1.AdminService/PutWorkflow", "PutWorkflow").0,
            "data"
        );
    }

    #[test]
    fn resources_skip_versions_and_parameters() {
        assert_eq!(
            resource_of("POST", "/indexes/{index_uid}/documents/delete-batch"),
            "indexes/documents"
        );
        assert_eq!(
            resource_of("POST", "/api/v1/admin/tsdb/snapshot"),
            "admin/tsdb"
        );
        assert_eq!(resource_of("POST", "/swap-indexes"), "swap-indexes");
        assert_eq!(
            resource_of("RPC", "/memos.api.v1.MemoService/CreateMemo"),
            "MemoService"
        );
        assert_eq!(resource_of("POST", "/v1/memos:batchDelete"), "memos");
    }

    #[test]
    fn axum_routes_with_their_handlers_and_nests() {
        let w = scan_texts(&[(
            "src/http.rs",
            r#"
use axum::{routing::{get, post}, Router};
const PING: &str = "/ping";
fn router() -> Router {
    let local = Router::new()
        .route(PING, get(ping))
        .route("/users/login", post(login_user))
        .route(
            "/streams/{id}",
            get(get_stream).put(update_stream).delete(delete_stream),
        )
        .route("/streams", post(create_stream));
    Router::new()
        .nest("/v1", Router::new().route("/write", routing::post(handler::write)))
        .nest("/admin", admin_routes())
        .merge(local)
}
fn admin_routes() -> Router {
    Router::new().route("/compact", post(compact))
}
async fn create_stream() {}
async fn update_stream() {}
async fn delete_stream() {}
async fn login_user() {}
async fn compact() {}
async fn write() {}
#[cfg(test)]
mod tests {
    fn t() { let r = Router::new().route("/test-only", post(x)); }
}
"#,
        )]);
        has_route(&w, "POST /streams -> create_stream");
        has_route(&w, "PUT /streams/{id} -> update_stream");
        has_route(&w, "DELETE /streams/{id} -> delete_stream");
        has_route(&w, "POST /users/login -> login_user");
        has_route(&w, "POST /v1/write -> write");
        has_route(&w, "POST /admin/compact -> compact");
        assert!(
            !found(&w).iter().any(|x| x.contains("test-only")),
            "{:#?}",
            found(&w)
        );
        let c = w.routes.iter().find(|r| r.path == "/streams").unwrap();
        assert_eq!(c.handler_at, Some(("src/http.rs".into(), 21)));
        // a durable write ranks before a sign-in
        let login = w
            .routes
            .iter()
            .position(|r| r.path == "/users/login")
            .unwrap();
        let create = w.routes.iter().position(|r| r.path == "/streams").unwrap();
        assert!(create < login);
    }

    #[test]
    fn actix_routes_and_a_route_table_of_a_macro() {
        let w = scan_texts(&[
            (
                "src/routes/mod.rs",
                r#"
#[routes::routes(
    routes(
        "/health" => get(get_health),
        "/indexes" => sub(indexes::IndexesApi),
        "/swap-indexes" => sub(swap_indexes::SwapIndexesApi),
    ),
)]
pub struct MeiliApi;
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/dumps").route(web::post().to(SeqHandler(create_dump))));
}
async fn create_dump() {}
"#,
            ),
            (
                "src/routes/indexes.rs",
                r#"
#[routes::routes(
    routes(
        "" => [get(list_indexes), post(create_index)],
        "/{index_uid}" => [get(get_index), patch(update_index), delete(delete_index)],
        "/{index_uid}/search" => [get(search_with_url_query), post(search_with_post)],
    ),
    tag = "Indexes",
)]
pub struct IndexesApi;
pub async fn create_index() {}
pub async fn update_index() {}
pub async fn delete_index() {}
pub async fn search_with_post() {}
"#,
            ),
            (
                "src/routes/swap_indexes.rs",
                "#[routes::routes(routes(\"\" => post(swap_indexes)))]\npub struct SwapIndexesApi;\npub async fn swap_indexes() {}\n",
            ),
        ]);
        has_route(&w, "POST /indexes -> create_index");
        has_route(&w, "PATCH /indexes/{index_uid} -> update_index");
        has_route(&w, "DELETE /indexes/{index_uid} -> delete_index");
        has_route(&w, "POST /swap-indexes -> swap_indexes");
        has_route(&w, "POST /dumps -> create_dump");
        assert!(w
            .reads
            .iter()
            .any(|r| r.path == "/indexes/{index_uid}/search"));
        assert!(!found(&w).iter().any(|x| x.contains("search")));
    }

    #[test]
    fn go_routers_groups_and_a_prefix_passed_to_a_function() {
        let w = scan_texts(&[
            (
                "web/api/api.go",
                r#"package api
import "github.com/prometheus/common/route"
func (api *API) Register(r *route.Router) {
	r.Post("/query", wrap(api.query))
	r.Post("/write", api.ready(api.remoteWrite))
	r.Post("/admin/tsdb/snapshot", wrap(api.snapshot))
	r.Del("/series", wrap(api.dropSeries))
}
func (api *API) remoteWrite(w http.ResponseWriter, r *http.Request) {}
func (api *API) snapshot(r *http.Request) apiFuncResult {}
"#,
            ),
            (
                "web/web.go",
                r#"package web
import "github.com/prometheus/common/route"
func New() {
	router := route.New()
	api.Register(router.WithPrefix("/api/v1"))
	router.Post("/-/reload", h.reload)
}
"#,
            ),
            (
                "srv/gin.go",
                r#"package srv
import "github.com/gin-gonic/gin"
func routes(r *gin.Engine) {
	v1 := r.Group("/v1")
	v1.POST("/orders", createOrder)
	resp, _ := http.Post("http://example.com/x", "application/json", nil)
	mux.HandleFunc("DELETE /orders/{id}", deleteOrder)
}
func createOrder(c *gin.Context) {}
"#,
            ),
        ]);
        has_route(&w, "POST /api/v1/write -> remoteWrite");
        has_route(&w, "POST /api/v1/admin/tsdb/snapshot -> snapshot");
        has_route(&w, "DELETE /api/v1/series -> dropSeries");
        has_route(&w, "POST /-/reload -> reload");
        has_route(&w, "POST /v1/orders -> createOrder");
        has_route(&w, "DELETE /orders/{id} -> deleteOrder");
        assert!(w.reads.iter().any(|r| r.path == "/api/v1/query"));
        assert!(!found(&w).iter().any(|x| x.contains("example.com")));
    }

    #[test]
    fn js_python_and_java_routes() {
        let w = scan_texts(&[
            (
                "server/app.js",
                "const express = require('express');\nconst app = express();\napp.post('/orders', auth, ordersController.create);\napp.delete('/orders/:id', async (req, res) => {\n  res.send(1);\n});\n",
            ),
            (
                "web/src/api.ts",
                "import axios from 'axios';\nexport const save = () => axios.post('/api/memos', body);\n",
            ),
            (
                "src/users.controller.ts",
                "import { Controller, Post, Delete } from '@nestjs/common';\n@Controller('users')\nexport class UsersController {\n  @Post()\n  create(@Body() dto: CreateUserDto) {\n    return 1;\n  }\n  @Delete(':id')\n  remove(@Param('id') id: string) {}\n}\n",
            ),
            (
                "app/main.py",
                "from fastapi import APIRouter\nrouter = APIRouter(prefix=\"/items\")\n\n@router.post(\"\")\nasync def create_item(item: Item):\n    return item\n\n@router.put(\"/{item_id}\")\ndef replace_item(item_id: int):\n    pass\n\n@app.route(\"/upload\", methods=[\"GET\", \"POST\"])\ndef upload():\n    pass\n\n@mock.patch(\"x.y\")\ndef helper():\n    pass\n",
            ),
            (
                "src/main/java/OrderController.java",
                "@RestController\n@RequestMapping(\"/api/orders\")\npublic class OrderController {\n    @PostMapping\n    public Order create(@RequestBody Order o) { return o; }\n    @DeleteMapping(\"/{id}\")\n    public void delete(@PathVariable long id) {}\n    @RequestMapping(value = \"/{id}/cancel\", method = RequestMethod.POST)\n    public void cancel(long id) {}\n}\n",
            ),
        ]);
        has_route(&w, "POST /orders -> create");
        has_route(&w, "DELETE /orders/:id -> <inline>");
        has_route(&w, "POST /users -> create");
        has_route(&w, "DELETE /users/:id -> remove");
        has_route(&w, "POST /items -> create_item");
        has_route(&w, "PUT /items/{item_id} -> replace_item");
        has_route(&w, "POST /upload -> upload");
        has_route(&w, "POST /api/orders -> create");
        has_route(&w, "DELETE /api/orders/{id} -> delete");
        has_route(&w, "POST /api/orders/{id}/cancel -> cancel");
        assert!(
            !found(&w)
                .iter()
                .any(|x| x.contains("/api/memos") || x.contains("x.y")),
            "{:#?}",
            found(&w)
        );
    }

    #[test]
    fn gorilla_chi_hapi_koa_flask_and_jax_rs() {
        let w = scan_texts(&[
            (
                "srv/mux.go",
                "package srv\nimport \"github.com/gorilla/mux\"\nfunc routes(r *mux.Router) {\n\tapi := r.PathPrefix(\"/api\").Subrouter()\n\tapi.HandleFunc(\"/carts/{id}\", h.updateCart).Methods(\"PUT\", \"PATCH\")\n\tapi.HandleFunc(\"/carts\", h.listCarts).Methods(http.MethodGet)\n}\n",
            ),
            (
                "srv/chi.go",
                "package srv\nimport \"github.com/go-chi/chi/v5\"\nfunc routes(r chi.Router) {\n\tr.Route(\"/accounts\", func(r chi.Router) {\n\t\tr.Post(\"/\", createAccount)\n\t\tr.Delete(\"/{id}\", closeAccount)\n\t})\n}\n",
            ),
            (
                "server/hapi.js",
                "const Hapi = require('@hapi/hapi');\nserver.route({ method: 'POST', path: '/payments', handler: payments.create });\n",
            ),
            (
                "server/koa.js",
                "const Router = require('@koa/router');\nrouter.del('/sessions/:id', logout);\n",
            ),
            (
                "app/views.py",
                "bp = Blueprint(\"orders\", __name__, url_prefix=\"/orders\")\n\n@bp.route(\"/<int:id>/refund\", methods=[\"POST\"])\ndef refund(id):\n    pass\n",
            ),
            (
                "src/main/java/Ledger.java",
                "@Path(\"/ledger\")\npublic class Ledger {\n    @POST\n    @Path(\"/entries\")\n    public Response add(Entry e) { return null; }\n}\n",
            ),
        ]);
        has_route(&w, "PUT /api/carts/{id} -> updateCart");
        has_route(&w, "PATCH /api/carts/{id} -> updateCart");
        has_route(&w, "POST /accounts -> createAccount");
        has_route(&w, "DELETE /accounts/{id} -> closeAccount");
        has_route(&w, "POST /payments -> create");
        has_route(&w, "DELETE /sessions/:id -> logout");
        has_route(&w, "POST /orders/<int:id>/refund -> refund");
        has_route(&w, "POST /ledger/entries -> add");
        assert!(
            !found(&w).iter().any(|x| x.contains("listCarts")),
            "{:#?}",
            found(&w)
        );
    }

    #[test]
    fn django_views_by_their_methods() {
        let w = scan_texts(&[
            (
                "shop/urls.py",
                "from django.urls import path\nfrom . import views\nurlpatterns = [\n    path(\"orders/\", views.OrderList.as_view()),\n    path(\"orders/<int:pk>/pay/\", views.pay),\n    path(\"about/\", views.about),\n]\n",
            ),
            (
                "shop/views.py",
                "class OrderList(generics.ListCreateAPIView):\n    queryset = Order.objects.all()\n\n@require_POST\ndef pay(request, pk):\n    pass\n\ndef about(request):\n    pass\n",
            ),
        ]);
        has_route(&w, "POST /orders/ -> OrderList");
        has_route(&w, "POST /orders/<int:pk>/pay/ -> pay");
        assert!(!found(&w).iter().any(|x| x.contains("about")));
    }

    #[test]
    fn protobuf_services_grpc_servers_and_openapi_files() {
        let w = scan_texts(&[
            (
                "proto/api/v1/memo_service.proto",
                "syntax = \"proto3\";\npackage memos.api.v1;\nservice MemoService {\n  // Creates a memo.\n  rpc CreateMemo(CreateMemoRequest) returns (Memo) {\n    option (google.api.http) = {\n      post: \"/api/v1/memos\"\n      body: \"memo\"\n    };\n  }\n  rpc GetMemo(GetMemoRequest) returns (Memo) {\n    option (google.api.http) = {get: \"/api/v1/{name=memos/*}\"};\n  }\n  rpc DeleteMemo(DeleteMemoRequest) returns (google.protobuf.Empty);\n}\n",
            ),
            (
                "server/memo.go",
                "package server\nfunc (s *APIV1Service) CreateMemo(ctx context.Context, request *v1pb.CreateMemoRequest) (*v1pb.Memo, error) {\n\treturn nil, nil\n}\n",
            ),
            (
                "engine/dispatcher.go",
                "package engine\ntype DispatcherImpl struct {\n\tcontracts.UnimplementedDispatcherServer\n\trepo Repo\n}\nfunc (d *DispatcherImpl) SendStepActionEvent(ctx context.Context, req *contracts.StepActionEvent) (*contracts.ActionEventResponse, error) {\n\treturn nil, nil\n}\nfunc (d *DispatcherImpl) Listen(req *contracts.WorkerListenRequest, stream contracts.Dispatcher_ListenServer) error {\n\treturn nil\n}\n",
            ),
            (
                "src/grpc.rs",
                "#[tonic::async_trait]\nimpl GreptimeDatabase for DatabaseService {\n    async fn handle(&self, request: Request<GreptimeRequest>) -> TonicResult<Response<GreptimeResponse>> {\n        todo!()\n    }\n    async fn get_status(&self, request: Request<StatusRequest>) -> Result<Response<Status>, Status> {\n        todo!()\n    }\n}\n",
            ),
            (
                "api/openapi.yaml",
                "openapi: 3.0.0\ninfo: {title: t, version: '1'}\npaths:\n  /api/v1/tenants:\n    post:\n      operationId: tenantCreate\n  /api/v1/tenants/{tenant}:\n    $ref: './paths/tenant.yaml#/tenant'\n",
            ),
            (
                "api/paths/tenant.yaml",
                "tenant:\n  get:\n    operationId: tenantGet\n  patch:\n    operationId: tenantUpdate\n",
            ),
            (
                "api/handlers/tenant.go",
                "package handlers\nfunc (t *TenantService) TenantCreate(ctx echo.Context) error {\n\treturn nil\n}\n",
            ),
        ]);
        has_route(&w, "POST /api/v1/memos -> CreateMemo");
        has_route(&w, "RPC /memos.api.v1.MemoService/DeleteMemo -> DeleteMemo");
        has_route(
            &w,
            "RPC /Dispatcher/SendStepActionEvent -> SendStepActionEvent",
        );
        has_route(&w, "RPC /GreptimeDatabase/Handle -> handle");
        has_route(&w, "POST /api/v1/tenants -> tenantCreate");
        has_route(&w, "PATCH /api/v1/tenants/{tenant} -> tenantUpdate");
        let m = w.routes.iter().find(|r| r.handler == "CreateMemo").unwrap();
        assert_eq!(m.handler_at, Some(("server/memo.go".into(), 2)));
        assert_eq!(m.also, ["/memos.api.v1.MemoService/CreateMemo"]);
        let t = w
            .routes
            .iter()
            .find(|r| r.handler == "tenantCreate")
            .unwrap();
        assert_eq!(t.handler_at, Some(("api/handlers/tenant.go".into(), 2)));
        let u = w
            .routes
            .iter()
            .find(|r| r.handler == "tenantUpdate")
            .unwrap();
        assert_eq!((u.file.as_str(), u.line), ("api/paths/tenant.yaml", 5));
        assert!(
            !found(&w)
                .iter()
                .any(|x| x.contains("GetMemo") || x.contains("Listen") || x.contains("get_status")),
            "{:#?}",
            found(&w)
        );
    }
}
