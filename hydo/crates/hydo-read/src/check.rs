//! hydo-check: set-style checks over hydo-read readings, with the Go program's explanation
//! lines, HYDO-ASSERT records and exit statuses.

use crate::json::{self, Obj, Val};
use crate::{encode_all, gofmt, lossy, q, read_store, usage, Error, Record, Res};
use crate::{EXIT_CANNOT, EXIT_OK, EXIT_VIOLATED};
use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

pub const CHECK_USAGE: &str = r#"hydo-check <check> <sources> [options]
checks (A = --left/--in, B = --right; items are compared as strings):
  subset            every item of A is in B
  equal             A and B hold the same items (as sets)
  exactly-once      every item of A occurs exactly once in B (B counted as a multiset)
  disjoint          no item of A is in B
  no-duplicates     no item occurs twice in --in A
  count             the number of records of --in A: --eq N | --min N | --max N
  never-wrong-value --cache C --truth T: every key C holds has T's value for that key; a key
                    missing from C is fine; a key T does not have is wrong (--allow-orphans: fine)
a source is ONE argument: "[name=]<hydo-read store and args> [projection]", e.g.
  'acked=hydo acked'   'kafka 127.0.0.1:9092 orders'
  'mysql hydo:@tcp(127.0.0.1:3306)/ "SELECT id FROM shop.orders WHERE paid = 1"'
projection (inside the source string):
  --field F           the item is field F (default: the only field; kafka/redis/etcd: key;
                      file: line; hydo: id; range: n; else a field named key)
  --item 'T'          the item is the template T with {F} replaced by field F, e.g.
                      --item 'k:{id}=v{id}' or --item '{key}={value}' (compares keys and values)
  --key F --value F   for never-wrong-value (default key / value)
  --strip-prefix P    remove P from the start of items and keys that have it
  --add-prefix P      then put P in front
options:
  --unreadable error|violation   a source that cannot be read: exit 2 (default) or 1
  --oracle            end with a "HYDO-ORACLE: OK|VIOLATION|ERROR <explanation>" line
  --id ID [--type T] [--msg M]   also print a HYDO-ASSERT record (T default always)
  --console PATH      write the HYDO-ASSERT record there instead of stdout (e.g. /dev/ttyS0)
  --show N            list up to N offending items (default 5)
  --timeout S         per source read (default 30)
The last output line is the explanation. Exit 0 holds, 1 violated, 2 could not evaluate."#;

type B = Vec<u8>;

fn cat(parts: &[&[u8]]) -> B {
    parts.concat()
}

pub struct Source {
    pub spec: B,
    pub name: B,
    pub args: Vec<B>,
    pub field: B,
    pub item: B,
    pub key: B,
    pub value: B,
    pub strip: B,
    pub add: B,
    pub records: Vec<Record>,
}

fn is_name(s: &[u8]) -> bool {
    if s.is_empty() || !s[0].is_ascii_alphabetic() {
        return false;
    }
    s.iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.')
}

/// shellWords: split like a POSIX shell (no expansion): '...' literal, "..." with \" \\ \$ \`,
/// a backslash outside quotes escapes the next character.
pub fn shell_words(s: &[u8]) -> Result<Vec<B>, String> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut have = false;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b'\'' => {
                let Some(j) = s[i + 1..].iter().position(|&x| x == b'\'') else {
                    return Err("unterminated '".into());
                };
                cur.extend_from_slice(&s[i + 1..i + 1 + j]);
                i += j + 1;
                have = true;
            }
            b'"' => {
                i += 1;
                while i < s.len() && s[i] != b'"' {
                    if s[i] == b'\\' && i + 1 < s.len() && b"\"\\$`".contains(&s[i + 1]) {
                        i += 1;
                    }
                    cur.push(s[i]);
                    i += 1;
                }
                if i >= s.len() {
                    return Err("unterminated \"".into());
                }
                have = true;
            }
            b'\\' if i + 1 < s.len() => {
                i += 1;
                cur.push(s[i]);
                have = true;
            }
            b' ' | b'\t' | b'\n' => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            _ => {
                cur.push(c);
                have = true;
            }
        }
        i += 1;
    }
    if have {
        out.push(cur);
    }
    Ok(out)
}

pub fn parse_source(spec: &[u8]) -> Res<Source> {
    let mut w = shell_words(spec).map_err(|e| usage(format!("source {}: {e}", q(spec))))?;
    if w.is_empty() {
        return Err(usage("empty source"));
    }
    let mut s = Source {
        spec: spec.to_vec(),
        name: B::new(),
        args: Vec::new(),
        field: B::new(),
        item: B::new(),
        key: B::new(),
        value: B::new(),
        strip: B::new(),
        add: B::new(),
        records: Vec::new(),
    };
    if let Some(i) = w[0].iter().position(|&c| c == b'=') {
        if i > 0 && is_name(&w[0][..i]) {
            s.name = w[0][..i].to_vec();
            if w[0][i + 1..].is_empty() {
                w.remove(0);
            } else {
                w[0] = w[0][i + 1..].to_vec();
            }
        }
    }
    let mut i = 0;
    while i < w.len() {
        let dst: Option<&mut B> = match w[i].as_slice() {
            b"--field" => Some(&mut s.field),
            b"--item" => Some(&mut s.item),
            b"--key" => Some(&mut s.key),
            b"--value" => Some(&mut s.value),
            b"--strip-prefix" => Some(&mut s.strip),
            b"--add-prefix" => Some(&mut s.add),
            _ => None,
        };
        match dst {
            None => s.args.push(w[i].clone()),
            Some(d) => {
                if i + 1 >= w.len() {
                    return Err(usage(format!(
                        "source {}: {} needs a value",
                        q(spec),
                        lossy(&w[i])
                    )));
                }
                *d = w[i + 1].clone();
                i += 1;
            }
        }
        i += 1;
    }
    if s.args.is_empty() {
        return Err(usage(format!("source {}: no store", q(spec))));
    }
    if s.name.is_empty() {
        s.name = s.args[0].clone();
    }
    Ok(s)
}

fn obj(r: &Record) -> Option<&Obj> {
    r.as_obj()
}

fn rlen(r: &Record) -> usize {
    obj(r).map(|o| o.len()).unwrap_or(0)
}

fn has(r: &Record, f: &[u8]) -> bool {
    obj(r).is_some_and(|o| o.contains_key(f))
}

fn enc(r: &Record) -> String {
    lossy(&json::encode(r))
}

impl Source {
    fn n(&self) -> String {
        lossy(&self.name)
    }

    fn load(&mut self, timeout: Duration) -> Res<()> {
        let mut args = self.args.clone();
        if !args.iter().any(|a| a == b"--timeout") {
            // a --timeout inside the source wins over hydo-check's --timeout
            args.push(b"--timeout".to_vec());
            args.push(gofmt::format_float(timeout.as_secs_f64(), false).into_bytes());
        }
        let (recs, _) = read_store(&args)?;
        self.records = recs;
        Ok(())
    }

    fn fix(&self, x: &[u8]) -> B {
        let x = if !self.strip.is_empty() {
            x.strip_prefix(self.strip.as_slice()).unwrap_or(x)
        } else {
            x
        };
        cat(&[&self.add, x])
    }

    fn default_field(&self) -> &'static [u8] {
        match self.args[0].as_slice() {
            b"kafka" | b"redis" | b"etcd" => b"key",
            b"file" => b"line",
            b"hydo" => b"id",
            b"range" => b"n",
            _ => b"",
        }
    }

    /// One item per record (records whose field is null or missing are skipped and counted).
    fn items(&self) -> Res<(Vec<B>, usize)> {
        let mut out = Vec::new();
        let mut skipped = 0;
        for r in &self.records {
            if !self.item.is_empty() {
                let v = self.render(r)?;
                out.push(self.fix(&v));
                continue;
            }
            let mut f: B = self.field.clone();
            if f.is_empty() {
                if rlen(r) == 1 {
                    f = obj(r).and_then(|o| o.keys().next().cloned()).unwrap_or_default();
                } else {
                    f = self.default_field().to_vec();
                    if f.is_empty() && has(r, b"key") {
                        f = b"key".to_vec();
                    }
                }
                if f.is_empty() {
                    return Err(usage(format!(
                        "source {}: records have {} fields; say which one with --field",
                        self.n(),
                        rlen(r)
                    )));
                }
            }
            match field(r, &f) {
                Some(v) => out.push(self.fix(&v)),
                None => {
                    if !has(r, &f) && !self.field.is_empty() {
                        return Err(usage(format!(
                            "source {}: no field {} in {}",
                            self.n(),
                            q(&f),
                            enc(r)
                        )));
                    }
                    skipped += 1;
                }
            }
        }
        Ok((out, skipped))
    }

    /// The --item template: {name} becomes the record's field, {{ and }} are literal braces.
    fn render(&self, r: &Record) -> Res<B> {
        let t = &self.item;
        let mut b = Vec::new();
        let mut i = 0;
        while i < t.len() {
            if t[i..].starts_with(b"{{") {
                b.push(b'{');
                i += 1;
            } else if t[i..].starts_with(b"}}") {
                b.push(b'}');
                i += 1;
            } else if t[i] == b'{' {
                let Some(j) = t[i..].iter().position(|&c| c == b'}') else {
                    return Err(usage(format!(
                        "source {}: --item {}: unclosed {{",
                        self.n(),
                        q(t)
                    )));
                };
                let name = &t[i + 1..i + j];
                if !has(r, name) {
                    return Err(usage(format!(
                        "source {}: --item {}: no field {} in {}",
                        self.n(),
                        q(t),
                        q(name),
                        enc(r)
                    )));
                }
                b.extend_from_slice(&field(r, name).unwrap_or_default());
                i += j;
            } else {
                b.push(t[i]);
            }
            i += 1;
        }
        Ok(b)
    }

    /// (key, value) per record; a null value (a missing key) is skipped.
    fn pairs(&self) -> Res<Vec<(B, B)>> {
        let kf: &[u8] = if self.key.is_empty() { b"key" } else { &self.key };
        let vf: &[u8] = if self.value.is_empty() { b"value" } else { &self.value };
        let mut out = Vec::new();
        for r in &self.records {
            let Some(k) = field(r, kf) else {
                return Err(usage(format!(
                    "source {}: no key field {} in {} (use --key)",
                    self.n(),
                    q(kf),
                    enc(r)
                )));
            };
            if !has(r, vf) {
                return Err(usage(format!(
                    "source {}: no value field {} in {} (use --value)",
                    self.n(),
                    q(vf),
                    enc(r)
                )));
            }
            let Some(v) = field(r, vf) else { continue };
            out.push((self.fix(&k), v));
        }
        Ok(out)
    }
}

/// A record field as an item string; None for a missing field or null.
fn field(r: &Record, f: &[u8]) -> Option<B> {
    match obj(r)?.get(f)? {
        Val::Null => None,
        Val::Str(s) => Some(s.clone()),
        Val::Int(n) => Some(n.to_string().into_bytes()),
        v => Some(json::encode(v)),
    }
}

/// itemLess: natural order (runs of digits compare as numbers, everything else bytewise), ties
/// broken bytewise.
pub fn item_less(a: &[u8], b: &[u8]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (da, db) = (a[i].is_ascii_digit(), b[j].is_ascii_digit());
        if da && db {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let x = trim_zeros(&a[si..i]);
            let y = trim_zeros(&b[sj..j]);
            if x.len() != y.len() {
                return x.len() < y.len();
            }
            if x != y {
                return x < y;
            }
            continue;
        }
        if a[i] != b[j] {
            return a[i] < b[j];
        }
        i += 1;
        j += 1;
    }
    if a.len() - i != b.len() - j {
        return a.len() - i < b.len() - j;
    }
    a < b
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|&&c| c == b'0').count();
    &s[n..]
}

/// A stable merge sort by a strict "less" (sort.SliceStable); never panics on an inconsistent
/// order.
pub fn stable_sort<T: Clone>(v: &mut [T], less: &dyn Fn(&T, &T) -> bool) {
    if v.len() < 2 {
        return;
    }
    let mid = v.len() / 2;
    stable_sort(&mut v[..mid], less);
    stable_sort(&mut v[mid..], less);
    let mut merged = Vec::with_capacity(v.len());
    let (mut i, mut j) = (0, mid);
    while i < mid && j < v.len() {
        if less(&v[j], &v[i]) {
            merged.push(v[j].clone());
            j += 1;
        } else {
            merged.push(v[i].clone());
            i += 1;
        }
    }
    merged.extend_from_slice(&v[i..mid]);
    merged.extend_from_slice(&v[j..]);
    v.clone_from_slice(&merged);
}

pub fn sort_items(s: &mut [B]) {
    stable_sort(s, &|a: &B, b: &B| item_less(a, b));
}

fn counts(items: &[B]) -> HashMap<B, usize> {
    let mut m = HashMap::new();
    for x in items {
        *m.entry(x.clone()).or_insert(0) += 1;
    }
    m
}

fn keys(m: &HashMap<B, usize>) -> Vec<B> {
    let mut out: Vec<B> = m.keys().cloned().collect();
    // Go collects map keys in random order; sort bytewise first so the stable sort is
    // deterministic whatever the order (itemLess is a total order).
    out.sort();
    sort_items(&mut out);
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Up to `show` items joined with ", ", then " (+N more)". A negative --show panics in Go.
fn list(items: &[B], show: i64) -> B {
    if items.is_empty() {
        return B::new();
    }
    let n = items.len();
    if (n as i64) > show {
        if show < 0 {
            eprintln!("panic: runtime error: slice bounds out of range [:{show}]");
            std::process::exit(2);
        }
        let mut o = items[..show as usize].join(&b", "[..]);
        o.extend_from_slice(format!(" (+{} more)", n - show as usize).as_bytes());
        return o;
    }
    items.join(&b", "[..])
}

struct Verdict {
    ok: bool,
    first: B,
    text: B,
}

fn bad(first: B, text: B) -> Verdict {
    Verdict {
        ok: false,
        first,
        text,
    }
}

#[derive(Default)]
struct Opts {
    left: B,
    right: B,
    cache: B,
    truth: B,
    eq: Option<i64>,
    min: Option<i64>,
    max: Option<i64>,
    allow_orphans: bool,
    unreadable_violation: bool,
    oracle: bool,
    id: B,
    typ: B,
    msg: B,
    console: B,
    show: i64,
    timeout: Duration,
}

/// Bytes formatted like Go's fmt with %s args mixed in.
fn bfmt(parts: &[&[u8]]) -> B {
    cat(parts)
}

pub fn check_main(args: &[B], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.is_empty() || args[0] == b"-h" || args[0] == b"--help" {
        let _ = writeln!(err, "{CHECK_USAGE}");
        return if args.is_empty() { EXIT_CANNOT } else { EXIT_OK };
    }
    let name = &args[0];
    let o = match parse_check_opts(&args[1..]) {
        Ok(o) => o,
        Err(e) => {
            let _ = out.write_all(&bfmt(&[b"hydo-check ", name, b": ", e.to_string().as_bytes(), b"\n"]));
            return EXIT_CANNOT;
        }
    };
    match run_check(name, &o) {
        Err(e) => {
            let mut rc = EXIT_CANNOT;
            let mut kind: &[u8] = b"ERROR";
            let es = e.to_string();
            let mut text = bfmt(&[name, b" could not be evaluated: ", es.as_bytes()]);
            if matches!(e, Error::Usage(_)) {
                let _ = writeln!(err, "{CHECK_USAGE}");
            } else if o.unreadable_violation {
                rc = EXIT_VIOLATED;
                kind = b"VIOLATION";
                text = bfmt(&[name, b" violated: ", es.as_bytes()]);
            }
            if rc == EXIT_VIOLATED {
                emit(&o, name, false, b"", &text, out);
            }
            if o.oracle {
                let _ = out.write_all(&bfmt(&[b"HYDO-ORACLE: ", kind, b" ", &text, b"\n"]));
            } else {
                let _ = out.write_all(&bfmt(&[&text, b"\n"]));
            }
            let _ = out.flush();
            rc
        }
        Ok(v) => {
            emit(&o, name, v.ok, &v.first, &v.text, out);
            if o.oracle {
                let k: &[u8] = if v.ok { b"OK" } else { b"VIOLATION" };
                let _ = out.write_all(&bfmt(&[b"HYDO-ORACLE: ", k, b" ", &v.text, b"\n"]));
            } else {
                let _ = out.write_all(&bfmt(&[&v.text, b"\n"]));
            }
            let _ = out.flush();
            if v.ok {
                EXIT_OK
            } else {
                EXIT_VIOLATED
            }
        }
    }
}

fn parse_check_opts(a: &[B]) -> Res<Opts> {
    let mut o = Opts {
        typ: b"always".to_vec(),
        show: 5,
        timeout: Duration::from_secs(30),
        ..Opts::default()
    };
    let num = |s: &[u8]| -> Res<i64> {
        std::str::from_utf8(s)
            .ok()
            .and_then(|x| x.parse::<i64>().ok())
            .ok_or_else(|| usage(format!("{} is not an integer", q(s))))
    };
    let mut i = 0;
    while i < a.len() {
        let flag = a[i].as_slice();
        match flag {
            b"--allow-orphans" => {
                o.allow_orphans = true;
                i += 1;
                continue;
            }
            b"--oracle" => {
                o.oracle = true;
                i += 1;
                continue;
            }
            _ => {}
        }
        if i + 1 >= a.len() {
            return Err(usage(format!("{} needs a value", lossy(flag))));
        }
        i += 1;
        let s = a[i].clone();
        match flag {
            b"--left" | b"--in" => o.left = s,
            b"--right" => o.right = s,
            b"--cache" => o.cache = s,
            b"--truth" => o.truth = s,
            b"--eq" => o.eq = Some(num(&s)?),
            b"--min" => o.min = Some(num(&s)?),
            b"--max" => o.max = Some(num(&s)?),
            b"--unreadable" => match s.as_slice() {
                b"error" => {}
                b"violation" => o.unreadable_violation = true,
                _ => return Err(usage("--unreadable error|violation")),
            },
            b"--id" => o.id = s,
            b"--type" => o.typ = s,
            b"--msg" => o.msg = s,
            b"--console" => o.console = s,
            b"--show" => o.show = num(&s)?,
            b"--timeout" => {
                let f = go_parse_float(&s);
                match f {
                    Some(f) if f > 0.0 => o.timeout = crate::secs(f),
                    _ => return Err(usage(format!("--timeout {}: seconds > 0", q(&s)))),
                }
            }
            _ => return Err(usage(format!("unknown option {}", lossy(flag)))),
        }
        i += 1;
    }
    Ok(o)
}

/// strconv.ParseFloat(s, 64) succeeding (also "inf", "nan", "1e3", "+1").
fn go_parse_float(s: &[u8]) -> Option<f64> {
    let t = std::str::from_utf8(s).ok()?;
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    t.parse::<f64>().ok()
}

fn load_all(o: &Opts, specs: &[&B]) -> Res<Vec<Source>> {
    let mut out = Vec::new();
    for sp in specs {
        out.push(parse_source(sp)?);
    }
    for s in out.iter_mut() {
        if let Err(e) = s.load(o.timeout) {
            if matches!(e, Error::Usage(_)) {
                return Err(usage(format!("source {}: {e}", s.n())));
            }
            let es = e.to_string();
            let es = es.strip_prefix("could not read: ").unwrap_or(&es).to_string();
            return Err(Error::Plain(format!(
                "could not read {} ({}): {es}",
                s.n(),
                lossy(&s.args[0])
            )));
        }
    }
    Ok(out)
}

fn run_check(name: &[u8], o: &Opts) -> Result<Verdict, Error> {
    let n = lossy(name);
    let need = |what: &[&str]| -> Res<()> {
        for w in what {
            let v = match *w {
                "--left" => &o.left,
                "--right" => &o.right,
                "--cache" => &o.cache,
                _ => &o.truth,
            };
            if v.is_empty() {
                return Err(usage(format!("{n} needs {}", what.join(" and "))));
            }
        }
        Ok(())
    };
    match name {
        b"subset" | b"equal" | b"exactly-once" | b"disjoint" => {
            need(&["--left", "--right"])?;
            let src = load_all(o, &[&o.left, &o.right])?;
            set_check(name, &src[0], &src[1], o)
        }
        b"no-duplicates" | b"count" => {
            if o.left.is_empty() {
                return Err(usage(format!("{n} needs --in")));
            }
            if name == b"count" && o.eq.is_none() && o.min.is_none() && o.max.is_none() {
                return Err(usage("count needs --eq, --min or --max"));
            }
            let src = load_all(o, &[&o.left])?;
            one_check(name, &src[0], o)
        }
        b"never-wrong-value" => {
            need(&["--cache", "--truth"])?;
            let src = load_all(o, &[&o.cache, &o.truth])?;
            value_check(&src[0], &src[1], o)
        }
        _ => Err(usage(format!("unknown check {}", q(name)))),
    }
}

fn set_check(name: &[u8], l: &Source, r: &Source, o: &Opts) -> Res<Verdict> {
    let (li, _) = l.items()?;
    let (ri, _) = r.items()?;
    let (lc, rc) = (counts(&li), counts(&ri));
    let lk = keys(&lc);
    let get = |m: &HashMap<B, usize>, x: &B| m.get(x).copied().unwrap_or(0);
    let (ln, rn) = (&l.name, &r.name);
    let mut badv: Vec<B> = Vec::new();
    match name {
        b"subset" | b"equal" => {
            for x in &lk {
                if get(&rc, x) == 0 {
                    badv.push(x.clone());
                }
            }
            let mut extra: Vec<B> = Vec::new();
            if name == b"equal" {
                for x in keys(&rc) {
                    if get(&lc, &x) == 0 {
                        extra.push(x);
                    }
                }
            }
            if badv.is_empty() && extra.is_empty() {
                if name == b"equal" {
                    return Ok(Verdict {
                        ok: true,
                        first: B::new(),
                        text: bfmt(&[
                            b"equal holds: ",
                            ln,
                            b" and ",
                            rn,
                            b" hold the same ",
                            plural(lk.len(), "item", "items").as_bytes(),
                        ]),
                    });
                }
                return Ok(Verdict {
                    ok: true,
                    first: B::new(),
                    text: bfmt(&[
                        b"subset holds: all ",
                        plural(lk.len(), "item", "items").as_bytes(),
                        b" of ",
                        ln,
                        b" are in ",
                        rn,
                        b" (",
                        rn,
                        b" has ",
                        plural(rc.len(), "item", "items").as_bytes(),
                        b")",
                    ]),
                });
            }
            if !badv.is_empty() {
                return Ok(bad(
                    badv[0].clone(),
                    bfmt(&[
                        name,
                        b" violated: ",
                        plural(badv.len(), "item", "items").as_bytes(),
                        format!(" of {} in ", lk.len()).as_bytes(),
                        ln,
                        b" not in ",
                        rn,
                        b": ",
                        &list(&badv, o.show),
                    ]),
                ));
            }
            Ok(bad(
                extra[0].clone(),
                bfmt(&[
                    b"equal violated: ",
                    plural(extra.len(), "item", "items").as_bytes(),
                    b" in ",
                    rn,
                    b" not in ",
                    ln,
                    b": ",
                    &list(&extra, o.show),
                ]),
            ))
        }
        b"exactly-once" => {
            let (mut missing, mut dup): (Vec<B>, Vec<B>) = (Vec::new(), Vec::new());
            for x in &lk {
                let c = get(&rc, x);
                if c == 0 {
                    missing.push(x.clone());
                    badv.push(x.clone());
                } else if c > 1 {
                    dup.push(bfmt(&[x, format!(" (x{c})").as_bytes()]));
                    badv.push(x.clone());
                }
            }
            if badv.is_empty() {
                return Ok(Verdict {
                    ok: true,
                    first: B::new(),
                    text: bfmt(&[
                        b"exactly-once holds: each of the ",
                        plural(lk.len(), "item", "items").as_bytes(),
                        b" of ",
                        ln,
                        b" occurs once in ",
                        rn,
                        b" (",
                        rn,
                        b" has ",
                        plural(ri.len(), "record", "records").as_bytes(),
                        b")",
                    ]),
                });
            }
            let first = badv[0].clone();
            let head = bfmt(&[&first, b" is in ", rn, format!(" {} times", get(&rc, &first)).as_bytes()]);
            let mut parts: Vec<B> = Vec::new();
            if !missing.is_empty() {
                parts.push(bfmt(&[format!("missing {}: ", missing.len()).as_bytes(), &list(&missing, o.show)]));
            }
            if !dup.is_empty() {
                parts.push(bfmt(&[format!("duplicated {}: ", dup.len()).as_bytes(), &list(&dup, o.show)]));
            }
            Ok(bad(
                first,
                bfmt(&[
                    b"exactly-once violated: ",
                    ln,
                    b" ",
                    &head,
                    format!(" (of {} in ", lk.len()).as_bytes(),
                    ln,
                    b"); ",
                    &parts.join(&b"; "[..]),
                ]),
            ))
        }
        b"disjoint" => {
            for x in &lk {
                if get(&rc, x) > 0 {
                    badv.push(x.clone());
                }
            }
            if badv.is_empty() {
                return Ok(Verdict {
                    ok: true,
                    first: B::new(),
                    text: bfmt(&[
                        b"disjoint holds: none of the ",
                        plural(lk.len(), "item", "items").as_bytes(),
                        b" of ",
                        ln,
                        b" is in ",
                        rn,
                    ]),
                });
            }
            let verb: &[u8] = if badv.len() == 1 { b"is" } else { b"are" };
            Ok(bad(
                badv[0].clone(),
                bfmt(&[
                    b"disjoint violated: ",
                    plural(badv.len(), "item", "items").as_bytes(),
                    b" of ",
                    ln,
                    b" ",
                    verb,
                    b" also in ",
                    rn,
                    b": ",
                    &list(&badv, o.show),
                ]),
            ))
        }
        _ => Err(usage(format!("unknown check {}", q(name)))),
    }
}

fn one_check(name: &[u8], s: &Source, o: &Opts) -> Res<Verdict> {
    if name == b"count" {
        let n = s.records.len() as i64;
        let mut want: Vec<String> = Vec::new();
        let mut okv = true;
        if let Some(eq) = o.eq {
            want.push(format!("== {eq}"));
            okv = okv && n == eq;
        }
        if let Some(mn) = o.min {
            want.push(format!(">= {mn}"));
            okv = okv && n >= mn;
        }
        if let Some(mx) = o.max {
            want.push(format!("<= {mx}"));
            okv = okv && n <= mx;
        }
        let w = want.join(" and ");
        let recs = plural(n as usize, "record", "records");
        if okv {
            return Ok(Verdict {
                ok: true,
                first: B::new(),
                text: bfmt(&[b"count holds: ", &s.name, b" has ", recs.as_bytes(), b" (want ", w.as_bytes(), b")"]),
            });
        }
        let mut first = B::new();
        let mut ex = B::new();
        if n > 0 {
            first = encode_all(&s.records, false).swap_remove(0);
            ex = bfmt(&[b"; first: ", &first]);
        }
        return Ok(bad(
            first,
            bfmt(&[b"count violated: ", &s.name, b" has ", recs.as_bytes(), b", want ", w.as_bytes(), &ex]),
        ));
    }
    let (items, _) = s.items()?;
    let c = counts(&items);
    let mut dup: Vec<B> = Vec::new();
    let mut first: Option<B> = None;
    for x in keys(&c) {
        if c[&x] > 1 {
            if first.is_none() {
                first = Some(x.clone());
            }
            dup.push(bfmt(&[&x, format!(" (x{})", c[&x]).as_bytes()]));
        }
    }
    let Some(first) = first else {
        return Ok(Verdict {
            ok: true,
            first: B::new(),
            text: bfmt(&[
                b"no-duplicates holds: ",
                plural(c.len(), "item", "items").as_bytes(),
                b" distinct in ",
                &s.name,
            ]),
        });
    };
    Ok(bad(
        first.clone(),
        bfmt(&[
            b"no-duplicates violated: ",
            &first,
            format!(" occurs {} times in ", c[&first]).as_bytes(),
            &s.name,
            b"; ",
            plural(dup.len(), "item", "items").as_bytes(),
            b" repeated: ",
            &list(&dup, o.show),
        ]),
    ))
}

fn value_check(cache: &Source, truth: &Source, o: &Opts) -> Res<Verdict> {
    let mut cp = cache.pairs()?;
    let tp = truth.pairs()?;
    let mut tv: HashMap<B, Vec<B>> = HashMap::new();
    for (k, v) in tp {
        tv.entry(k).or_default().push(v);
    }
    stable_sort(&mut cp, &|a: &(B, B), b: &(B, B)| {
        if a.0 != b.0 {
            return item_less(&a.0, &b.0);
        }
        a.1 < b.1
    });
    let tn = &truth.name;
    let mut badv: Vec<(B, B)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (k, v) in &cp {
        let t = tv.get(k);
        let text = match t {
            None if !o.allow_orphans => {
                if seen.contains(k) {
                    None
                } else {
                    Some(bfmt(&[k, b"=", v, b" but ", tn, b" has no such key"]))
                }
            }
            Some(t) if t.len() > 1 => {
                if seen.contains(k) {
                    None
                } else {
                    Some(bfmt(&[
                        tn,
                        format!(" has {} values for ", t.len()).as_bytes(),
                        k,
                        b": ",
                        &t.join(&b", "[..]),
                    ]))
                }
            }
            Some(t) if t[0] != *v => Some(bfmt(&[k, b"=", v, b" but ", tn, b" has ", &t[0]])),
            _ => continue,
        };
        if let Some(text) = text {
            badv.push((k.clone(), text));
        }
        seen.insert(k.clone());
    }
    if badv.is_empty() {
        return Ok(Verdict {
            ok: true,
            first: B::new(),
            text: bfmt(&[
                b"never-wrong-value holds: all ",
                plural(cp.len(), "value", "values").as_bytes(),
                b" of ",
                &cache.name,
                b" agree with ",
                tn,
                b" (",
                tn,
                b" has ",
                plural(tv.len(), "key", "keys").as_bytes(),
                b")",
            ]),
        });
    }
    let texts: Vec<B> = badv.iter().map(|b| b.1.clone()).collect();
    Ok(bad(
        badv[0].0.clone(),
        bfmt(&[
            b"never-wrong-value violated: ",
            plural(badv.len(), "value", "values").as_bytes(),
            b" wrong in ",
            &cache.name,
            b": ",
            &list(&texts, o.show),
        ]),
    ))
}

/// The HYDO-ASSERT record when --id is given (hydo/ASSERTIONS.md wire format).
fn emit(o: &Opts, name: &[u8], okv: bool, first: &[u8], text: &[u8], out: &mut dyn Write) {
    if o.id.is_empty() {
        return;
    }
    let msg = if o.msg.is_empty() { name.to_vec() } else { o.msg.clone() };
    let mut d = Obj::new();
    d.insert(b"check".to_vec(), Val::str(name.to_vec()));
    d.insert(b"explanation".to_vec(), Val::str(text.to_vec()));
    if !first.is_empty() {
        d.insert(b"first".to_vec(), Val::str(first.to_vec()));
    }
    let mut rec = Obj::new();
    rec.insert(b"t".to_vec(), Val::str(o.typ.clone()));
    rec.insert(b"id".to_vec(), Val::str(o.id.clone()));
    rec.insert(b"ok".to_vec(), Val::Bool(okv));
    rec.insert(b"msg".to_vec(), Val::str(msg.clone()));
    rec.insert(b"details".to_vec(), Val::Obj(d));
    let mut decl = Obj::new();
    decl.insert(b"t".to_vec(), Val::str(o.typ.clone()));
    decl.insert(b"id".to_vec(), Val::str(o.id.clone()));
    decl.insert(b"msg".to_vec(), Val::str(msg));
    let line = bfmt(&[b"HYDO-ASSERT ", &json::encode_obj(&rec), b"\n"]);
    let decl = bfmt(&[b"HYDO-ASSERT-DECL ", &json::encode_obj(&decl), b"\n"]);
    if !o.console.is_empty() {
        use std::os::unix::ffi::OsStrExt;
        let p = std::ffi::OsStr::from_bytes(&o.console);
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(p) {
            let _ = f.write_all(&decl);
            let _ = f.write_all(&line);
            return;
        }
    }
    let _ = out.write_all(&bfmt(&[&decl, &line]));
}
