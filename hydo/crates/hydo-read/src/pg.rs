//! PostgreSQL over the simple-query protocol, as lib/pq 1.10.9 spoke it for `db.QueryContext`
//! with no arguments: the same DSN forms (URL and key=value, the PG* environment variables, the
//! default sslmode=require), the same startup parameters, the same value conversion (booleans,
//! integers, floats, bytea and times decoded, then printed as the Go program printed them) and
//! the same error texts ("pq: relation \"nope\" does not exist").
//!
//! Authentication: trust, cleartext, md5 and SCRAM-SHA-256. TLS is not supported: a server
//! that accepts the SSL request (sslmode require/verify-*) is refused with a message.

use crate::json::{Obj, Val};
use crate::net::{dial_tcp, dial_unix, Conn, NetErr};
use crate::{cannot, gofmt, lossy, q, ReadOpts, Record, Res};
use hmac::{Hmac, Mac};
use md5::Md5;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The Go program's postgresDSN: a connect_timeout unless the DSN has one.
pub fn postgres_dsn(dsn: &str, t: Duration) -> String {
    let sec = ((t.as_secs_f64() + 0.999) as i64).to_string();
    if dsn.contains("connect_timeout") {
        return dsn.to_string();
    }
    if dsn.starts_with("postgres://") || dsn.starts_with("postgresql://") {
        if dsn.contains('?') {
            return format!("{dsn}&connect_timeout={sec}");
        }
        return format!("{dsn}?connect_timeout={sec}");
    }
    format!("{dsn} connect_timeout={sec}")
}

type Values = BTreeMap<String, String>;

/// lib/pq's ParseURL: the URL as a key='value' string.
fn parse_url(dsn: &str) -> Result<String, String> {
    let u = crate::url::parse(dsn)?;
    if u.scheme != "postgres" && u.scheme != "postgresql" {
        return Err(format!("invalid connection protocol: {}", u.scheme));
    }
    let mut kvs: Vec<String> = Vec::new();
    let mut accrue = |k: &str, v: &str| {
        if !v.is_empty() {
            kvs.push(format!(
                "{k}='{}'",
                v.replace('\\', "\\\\").replace('\'', "\\'")
            ));
        }
    };
    if let Some((n, p)) = &u.user {
        accrue("user", n);
        accrue("password", p.as_deref().unwrap_or(""));
    }
    match crate::net::split_host_port(&u.host) {
        Ok((h, p)) => {
            accrue("host", &h);
            accrue("port", &p);
        }
        Err(_) => accrue("host", &u.host),
    }
    if !u.path.is_empty() {
        accrue("dbname", &u.path[1..]);
    }
    for (k, v) in crate::url::query(&u.raw_query) {
        accrue(&k, &v);
    }
    kvs.sort();
    Ok(kvs.join(" "))
}

/// lib/pq's parseOpts (libpq-style key=value pairs).
fn parse_opts(name: &str, o: &mut Values) -> Result<(), String> {
    let s: Vec<char> = name.chars().collect();
    let mut i = 0;
    let sp = |c: char| gofmt::is_space(c);
    loop {
        while i < s.len() && sp(s[i]) {
            i += 1;
        }
        if i >= s.len() {
            return Ok(());
        }
        let mut key = String::new();
        while i < s.len() && !sp(s[i]) && s[i] != '=' {
            key.push(s[i]);
            i += 1;
        }
        while i < s.len() && sp(s[i]) {
            i += 1;
        }
        if i >= s.len() || s[i] != '=' {
            return Err(format!(
                "missing \"=\" after {} in connection info string\"",
                q(key.as_bytes())
            ));
        }
        i += 1;
        while i < s.len() && sp(s[i]) {
            i += 1;
        }
        if i >= s.len() {
            o.insert(key, String::new());
            return Ok(());
        }
        let mut val = String::new();
        if s[i] != '\'' {
            while i < s.len() && !sp(s[i]) {
                if s[i] == '\\' {
                    i += 1;
                    if i >= s.len() {
                        return Err("missing character after backslash".into());
                    }
                }
                val.push(s[i]);
                i += 1;
            }
        } else {
            i += 1;
            loop {
                if i >= s.len() {
                    return Err("unterminated quoted string literal in connection string".into());
                }
                match s[i] {
                    '\'' => {
                        i += 1;
                        break;
                    }
                    '\\' => {
                        i += 1;
                        if i < s.len() {
                            val.push(s[i]);
                        }
                        i += 1;
                    }
                    c => {
                        val.push(c);
                        i += 1;
                    }
                }
            }
        }
        o.insert(key, val);
    }
}

/// The name of the user this process runs as (os/user.Current, else $USER).
fn current_user() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if let Ok(md) = std::fs::metadata("/proc/self") {
        let uid = md.uid().to_string();
        if let Ok(pw) = std::fs::read_to_string("/etc/passwd") {
            for l in pw.lines() {
                let f: Vec<&str> = l.split(':').collect();
                if f.len() > 2 && f[2] == uid {
                    return Some(f[0].to_string());
                }
            }
        }
    }
    std::env::var("USER").ok().filter(|u| !u.is_empty())
}

/// NewConnector's option map.
fn options(dsn: &str) -> Result<Values, String> {
    let mut o = Values::new();
    o.insert("host".into(), "localhost".into());
    o.insert("port".into(), "5432".into());
    o.insert("extra_float_digits".into(), "2".into());
    for (k, v) in std::env::vars() {
        let key = match k.as_str() {
            "PGHOST" => "host",
            "PGPORT" => "port",
            "PGDATABASE" => "dbname",
            "PGUSER" => "user",
            "PGPASSWORD" => "password",
            "PGOPTIONS" => "options",
            "PGAPPNAME" => "application_name",
            "PGSSLMODE" => "sslmode",
            "PGCONNECT_TIMEOUT" => "connect_timeout",
            "PGCLIENTENCODING" => "client_encoding",
            "PGDATESTYLE" => "datestyle",
            "PGTZ" => "timezone",
            "PGGEQO" => "geqo",
            _ => continue,
        };
        o.insert(key.into(), v);
    }
    let mut dsn = dsn.to_string();
    if dsn.starts_with("postgres://") || dsn.starts_with("postgresql://") {
        dsn = parse_url(&dsn)?;
    }
    parse_opts(&dsn, &mut o)?;
    if let Some(f) = o.get("fallback_application_name").cloned() {
        o.entry("application_name".into()).or_insert(f);
    }
    if let Some(enc) = o.get("client_encoding") {
        let e = enc.to_ascii_lowercase().replace(['-', '_'], "");
        if e != "utf8" && e != "unicode" {
            return Err("client_encoding must be absent or 'UTF8'".into());
        }
    }
    o.insert("client_encoding".into(), "UTF8".into());
    match o.get("datestyle") {
        Some(d) if d != "ISO, MDY" => {
            return Err(format!(
                "setting datestyle must be absent or ISO, MDY; got {d}"
            ))
        }
        _ => {
            o.insert("datestyle".into(), "ISO, MDY".into());
        }
    }
    if !o.contains_key("user") {
        match current_user() {
            Some(u) => {
                o.insert("user".into(), u);
            }
            None => return Err("user: unknown userid".into()),
        }
    }
    if o.get("host").is_some_and(|h| h.starts_with('/')) {
        o.insert("sslmode".into(), "disable".into());
    }
    Ok(o)
}

fn is_driver_setting(k: &str) -> bool {
    matches!(
        k,
        "host"
            | "port"
            | "password"
            | "sslmode"
            | "sslcert"
            | "sslkey"
            | "sslrootcert"
            | "sslinline"
            | "sslsni"
            | "fallback_application_name"
            | "connect_timeout"
            | "disable_prepared_binary_result"
            | "binary_parameters"
            | "krbsrvname"
            | "krbspn"
    )
}

struct Pg {
    c: Conn,
}

/// One backend message: type and body.
type Msg = (u8, Vec<u8>);

fn timeout_text() -> String {
    "context deadline exceeded".into()
}

fn net_text(e: NetErr) -> String {
    match e {
        NetErr::Timeout(_) => timeout_text(),
        e => e.to_string(),
    }
}

impl Pg {
    fn recv(&mut self) -> Result<Msg, String> {
        let t = self.c.read_byte().map_err(net_text)?;
        let l = self.c.read_exact(4).map_err(net_text)?;
        let n = u32::from_be_bytes([l[0], l[1], l[2], l[3]]) as usize;
        if n < 4 {
            return Err(format!("pq: invalid message length {n}"));
        }
        let body = self.c.read_exact(n - 4).map_err(net_text)?;
        Ok((t, body))
    }
    fn send(&mut self, t: u8, body: &[u8]) -> Result<(), String> {
        let mut p = vec![t];
        p.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
        p.extend_from_slice(body);
        self.c.write_all(&p).map_err(net_text)
    }
}

/// lib/pq's Error.Error(): "pq: " + the message field.
fn parse_error(b: &[u8]) -> String {
    let mut i = 0;
    let mut msg = Vec::new();
    while i < b.len() && b[i] != 0 {
        let t = b[i];
        i += 1;
        let end = b[i..].iter().position(|&c| c == 0).map(|e| i + e).unwrap_or(b.len());
        if t == b'M' {
            msg = b[i..end].to_vec();
        }
        i = end + 1;
    }
    format!("pq: {}", lossy(&msg))
}

fn cstr(b: &[u8], pos: &mut usize) -> Vec<u8> {
    let end = b[*pos..].iter().position(|&c| c == 0).map(|e| *pos + e).unwrap_or(b.len());
    let s = b[*pos..end].to_vec();
    *pos = (end + 1).min(b.len());
    s
}

fn md5_hex(b: &[u8]) -> String {
    Md5::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], data: &[&[u8]]) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
    for d in data {
        m.update(d);
    }
    m.finalize().into_bytes().to_vec()
}

fn nonce() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    }
    crate::base64_encode(&b)
}

fn auth(pg: &mut Pg, body: &[u8], o: &Values) -> Result<(), String> {
    let code = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    let user = o.get("user").cloned().unwrap_or_default();
    let pass = o.get("password").cloned().unwrap_or_default();
    let expect_ok = |pg: &mut Pg| -> Result<(), String> {
        let (t, b) = pg.recv()?;
        if t == b'E' {
            return Err(parse_error(&b));
        }
        if t != b'R' {
            return Err(format!("pq: unexpected password response: {}", q(&[t])));
        }
        if b.len() < 4 || i32::from_be_bytes([b[0], b[1], b[2], b[3]]) != 0 {
            return Err(format!("pq: unexpected authentication response: {}", q(&[t])));
        }
        Ok(())
    };
    match code {
        0 => Ok(()),
        3 => {
            let mut p = pass.into_bytes();
            p.push(0);
            pg.send(b'p', &p)?;
            expect_ok(pg)
        }
        5 => {
            let salt = &body[4..8];
            let inner = md5_hex(format!("{pass}{user}").as_bytes());
            let mut x = inner.into_bytes();
            x.extend_from_slice(salt);
            let mut p = format!("md5{}", md5_hex(&x)).into_bytes();
            p.push(0);
            pg.send(b'p', &p)?;
            expect_ok(pg)
        }
        10 => {
            let cn = nonce();
            let esc_user = user.replace('=', "=3D").replace(',', "=2C");
            let first_bare = format!("n={esc_user},r={cn}");
            let first = format!("n,,{first_bare}");
            let mut p = b"SCRAM-SHA-256\0".to_vec();
            p.extend_from_slice(&(first.len() as i32).to_be_bytes());
            p.extend_from_slice(first.as_bytes());
            pg.send(b'p', &p)?;
            let (t, b) = pg.recv()?;
            if t == b'E' {
                return Err(parse_error(&b));
            }
            if t != b'R' || b.len() < 4 || i32::from_be_bytes([b[0], b[1], b[2], b[3]]) != 11 {
                return Err(format!("pq: unexpected authentication response: {}", q(&[t])));
            }
            let server_first = b[4..].to_vec();
            let sf = lossy(&server_first);
            let fields: Vec<&str> = sf.split(',').collect();
            if fields.len() != 3 {
                return Err(format!(
                    "pq: SCRAM-SHA-256 error: expected 3 fields in first SCRAM-SHA-256 server message, got {}: {}",
                    fields.len(),
                    q(&server_first)
                ));
            }
            let snonce = fields[0].strip_prefix("r=").unwrap_or("");
            let salt = crate::base64_decode(fields[1].strip_prefix("s=").unwrap_or("").as_bytes());
            let iters: u32 = fields[2].strip_prefix("i=").unwrap_or("").parse().unwrap_or(0);
            if !snonce.starts_with(&cn) {
                return Err("pq: SCRAM-SHA-256 error: server SCRAM-SHA-256 nonce is not prefixed by client nonce".into());
            }
            // SaltedPassword = Hi(password, salt, i)
            let mut u = hmac(pass.as_bytes(), &[&salt, &[0, 0, 0, 1]]);
            let mut salted = u.clone();
            for _ in 1..iters {
                u = hmac(pass.as_bytes(), &[&u]);
                for (h, x) in salted.iter_mut().zip(u.iter()) {
                    *h ^= x;
                }
            }
            let client_key = hmac(&salted, &[b"Client Key"]);
            let stored = Sha256::digest(&client_key);
            let without_proof = format!("c=biws,r={snonce}");
            let auth_msg = format!("{first_bare},{sf},{without_proof}");
            let sig = hmac(&stored, &[auth_msg.as_bytes()]);
            let proof: Vec<u8> = client_key.iter().zip(sig.iter()).map(|(a, b)| a ^ b).collect();
            let fin = format!("{without_proof},p={}", crate::base64_encode(&proof));
            pg.send(b'p', fin.as_bytes())?;
            let (t, b) = pg.recv()?;
            if t == b'E' {
                return Err(parse_error(&b));
            }
            if t != b'R' || b.len() < 4 || i32::from_be_bytes([b[0], b[1], b[2], b[3]]) != 12 {
                return Err(format!("pq: unexpected authentication response: {}", q(&[t])));
            }
            let fin = lossy(&b[4..]);
            if let Some(e) = fin.strip_prefix("e=") {
                return Err(format!(
                    "pq: SCRAM-SHA-256 error: SCRAM-SHA-256 authentication error: {e}"
                ));
            }
            let server_key = hmac(&salted, &[b"Server Key"]);
            let want = crate::base64_encode(&hmac(&server_key, &[auth_msg.as_bytes()]));
            if fin.strip_prefix("v=") != Some(want.as_str()) {
                return Err(format!(
                    "pq: SCRAM-SHA-256 error: cannot authenticate SCRAM-SHA-256 server signature: {}",
                    q(fin.strip_prefix("v=").unwrap_or("").as_bytes())
                ));
            }
            Ok(())
        }
        7 | 8 => Err("pq: kerberos error: no GSSAPI provider registered (import github.com/lib/pq/auth/kerberos if you need Kerberos support)".into()),
        n => Err(format!("pq: unknown authentication response: {n}")),
    }
}

fn open(o: &Values, deadline: Instant) -> Result<Pg, String> {
    for k in ["disable_prepared_binary_result", "binary_parameters"] {
        if let Some(v) = o.get(k) {
            if v != "yes" && v != "no" {
                return Err(format!("unrecognized value {} for {k}", q(v.as_bytes())));
            }
        }
    }
    let host = o.get("host").cloned().unwrap_or_default();
    let port = o.get("port").cloned().unwrap_or_default();
    let mut dl = deadline;
    if let Some(t) = o.get("connect_timeout").filter(|t| *t != "0") {
        let secs: i64 = t.parse().map_err(|_| {
            format!(
                "invalid value for parameter connect_timeout: strconv.ParseInt: parsing {}: invalid syntax",
                q(t.as_bytes())
            )
        })?;
        dl = dl.min(Instant::now() + Duration::from_secs(secs.max(0) as u64));
    }
    let c = if host.starts_with('/') {
        dial_unix(&format!("{}/.s.PGSQL.{port}", host.trim_end_matches('/')), dl)
    } else {
        let addr = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        dial_tcp(&addr, dl)
    }
    .map_err(|e| e.to_string())?;
    let mut pg = Pg { c };
    pg.c.deadline = dl;
    match o.get("sslmode").map(String::as_str).unwrap_or("") {
        "disable" => {}
        "" | "require" | "verify-ca" | "verify-full" => {
            pg.c
                .write_all(&[0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f])
                .map_err(net_text)?;
            let b = pg.c.read_byte().map_err(net_text)?;
            if b != b'S' {
                return Err("pq: SSL is not enabled on the server".into());
            }
            return Err("pq: the server offers SSL, which hydo-read does not support (use sslmode=disable)".into());
        }
        m => {
            return Err(format!(
                "pq: unsupported sslmode {}; only \"require\" (default), \"verify-full\", \"verify-ca\", and \"disable\" supported",
                q(m.as_bytes())
            ))
        }
    }
    let mut body = 196608u32.to_be_bytes().to_vec();
    for (k, v) in o {
        if is_driver_setting(k) {
            continue;
        }
        body.extend_from_slice(if k == "dbname" { b"database" } else { k.as_bytes() });
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut p = ((body.len() + 4) as u32).to_be_bytes().to_vec();
    p.extend_from_slice(&body);
    pg.c.write_all(&p).map_err(net_text)?;
    loop {
        let (t, b) = pg.recv()?;
        match t {
            b'K' | b'S' | b'N' => {}
            b'R' => auth(&mut pg, &b, o)?,
            b'Z' => break,
            b'E' => return Err(parse_error(&b)),
            _ => return Err(format!("pq: unknown response for startup: {}", q(&[t]))),
        }
    }
    pg.c.deadline = deadline;
    Ok(pg)
}

// ------------------------------------------------------------------ values

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `t.UTC().Format(time.RFC3339Nano)` of a civil time with an offset (seconds east of UTC).
#[allow(clippy::too_many_arguments)]
fn rfc3339_utc(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64, ns: i64, off: i64) -> String {
    // time.Date normalises month overflow, then everything else through the absolute time
    let (y, mo) = (y + (mo - 1).div_euclid(12), (mo - 1).rem_euclid(12) + 1);
    let total_ns = (days_from_civil(y, mo, 1) + d - 1) as i128 * 86_400_000_000_000
        + (h * 3600 + mi * 60 + s - off) as i128 * 1_000_000_000
        + ns as i128;
    let secs = total_ns.div_euclid(1_000_000_000) as i64;
    let ns = total_ns.rem_euclid(1_000_000_000) as i64;
    let (yy, mm, dd) = civil_from_days(secs.div_euclid(86400));
    let sod = secs.rem_euclid(86400);
    let year = if yy < 0 {
        format!("-{:04}", -yy)
    } else {
        format!("{yy:04}")
    };
    let mut out = format!(
        "{year}-{mm:02}-{dd:02}T{:02}:{:02}:{:02}",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    );
    if ns != 0 {
        let f = format!("{ns:09}");
        out.push('.');
        out.push_str(f.trim_end_matches('0'));
    }
    out.push('Z');
    out
}

/// lib/pq's ParseTimestamp (timestamptz, timestamp, date), printed as the Go program did.
fn parse_ts(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut err: Option<String> = None;
    let atoi = |begin: isize, end: isize, err: &mut Option<String>| -> i64 {
        if err.is_some() {
            return 0;
        }
        if begin < 0 || end < 0 || begin > end || end as usize > b.len() {
            *err = Some("invalid timestamp".into());
            return 0;
        }
        let part = &s[begin as usize..end as usize];
        match part.parse::<i64>() {
            Ok(v) => v,
            Err(_) => {
                *err = Some(format!("expected number; got '{part}'"));
                0
            }
        }
    };
    let expect = |c: u8, pos: isize, err: &mut Option<String>| {
        if err.is_some() {
            return;
        }
        if pos < 0 || pos as usize + 1 > b.len() {
            *err = Some("invalid timestamp".into());
            return;
        }
        if b[pos as usize] != c {
            *err = Some(format!(
                "expected '{c}' at position {pos}; got '{}'",
                b[pos as usize]
            ));
        }
    };
    let mon_sep = s.find('-').map(|i| i as isize).unwrap_or(-1);
    let year = atoi(0, mon_sep, &mut err);
    let day_sep = mon_sep + 3;
    let month = atoi(mon_sep + 1, day_sep, &mut err);
    expect(b'-', day_sep, &mut err);
    let time_sep = day_sep + 3;
    let day = atoi(day_sep + 1, time_sep, &mut err);
    let mut min_len = mon_sep + 6;
    let is_bc = s.ends_with(" BC");
    if is_bc {
        min_len += 3;
    }
    let (mut hour, mut minute, mut second) = (0, 0, 0);
    if b.len() as isize > min_len {
        expect(b' ', time_sep, &mut err);
        let min_sep = time_sep + 3;
        expect(b':', min_sep, &mut err);
        hour = atoi(time_sep + 1, min_sep, &mut err);
        let sec_sep = min_sep + 3;
        expect(b':', sec_sep, &mut err);
        minute = atoi(min_sep + 1, sec_sep, &mut err);
        second = atoi(sec_sep + 1, sec_sep + 3, &mut err);
    }
    let mut rem = mon_sep + 15;
    let mut nano = 0i64;
    let mut tz_off = 0i64;
    if rem >= 0 && (rem as usize) < b.len() && b[rem as usize] == b'.' {
        let fs = rem + 1;
        let off = s[fs as usize..]
            .find(['-', '+', 'Z', ' '])
            .map(|i| i as isize)
            .unwrap_or(b.len() as isize - fs);
        let frac = atoi(fs, fs + off, &mut err);
        let div = 10i64.checked_pow(off as u32).unwrap_or(i64::MAX);
        nano = if div == 0 { 0 } else { frac * (1_000_000_000 / div.max(1)) };
        rem += off + 1;
    }
    if rem >= 0 && (rem as usize) < b.len() && (b[rem as usize] == b'-' || b[rem as usize] == b'+') {
        let sign = if b[rem as usize] == b'-' { -1 } else { 1 };
        let tzs = rem;
        let th = atoi(tzs + 1, tzs + 3, &mut err);
        rem += 3;
        let (mut tm, mut ts) = (0, 0);
        if (rem as usize) < b.len() && b[rem as usize] == b':' {
            tm = atoi(rem + 1, rem + 3, &mut err);
            rem += 3;
        }
        if (rem as usize) < b.len() && b[rem as usize] == b':' {
            ts = atoi(rem + 1, rem + 3, &mut err);
            rem += 3;
        }
        tz_off = sign * (th * 3600 + tm * 60 + ts);
    } else if rem >= 0 && (rem as usize) < b.len() && b[rem as usize] == b'Z' {
        rem += 1;
    }
    let iso_year = if is_bc {
        rem += 3;
        1 - year
    } else {
        year
    };
    if rem >= 0 && (rem as usize) < b.len() {
        return Err(format!("expected end of input, got {}", &s[rem as usize..]));
    }
    if let Some(e) = err {
        return Err(e);
    }
    Ok(rfc3339_utc(iso_year, month, day, hour, minute, second, nano, tz_off))
}

/// time and timetz: `time.Parse("15:04:05[-07[:00[:00]]]", s)` on 0000-01-01.
fn parse_clock(s: &str, tz: bool) -> Result<String, String> {
    let mut st = s.to_string();
    let mut plus_day = false;
    // the 24:00 special case
    if let Some(rest) = st.strip_prefix("24:00") {
        let mut r = rest;
        if let Some(x) = r.strip_prefix(":00") {
            r = x;
            if let Some(x) = r.strip_prefix('.') {
                let z = x.len() - x.trim_start_matches('0').len();
                if z > 0 {
                    r = &x[z..];
                }
            }
        }
        if r.is_empty() || r.starts_with(['Z', '+', '-']) {
            st = format!("00:00:00{r}");
            plus_day = true;
        }
    }
    let bad = || format!("pq: decode: parsing time {} as \"15:04:05\": cannot parse", q(s.as_bytes()));
    let b = st.as_bytes();
    let digits = |from: usize, n: usize| -> Option<i64> {
        let p = b.get(from..from + n)?;
        if p.iter().all(|c| c.is_ascii_digit()) {
            std::str::from_utf8(p).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (h, mut i) = if b.len() > 1 && b[1].is_ascii_digit() {
        (digits(0, 2).ok_or_else(bad)?, 2)
    } else {
        (digits(0, 1).ok_or_else(bad)?, 1)
    };
    if b.get(i) != Some(&b':') {
        return Err(bad());
    }
    let mi = digits(i + 1, 2).ok_or_else(bad)?;
    if b.get(i + 3) != Some(&b':') {
        return Err(bad());
    }
    let se = digits(i + 4, 2).ok_or_else(bad)?;
    i += 6;
    let mut ns = 0i64;
    if b.len() > i + 1 && (b[i] == b'.' || b[i] == b',') && b[i + 1].is_ascii_digit() {
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        let f = &st[i + 1..j];
        let mut v = 0i64;
        for (k, c) in f.bytes().enumerate().take(9) {
            v += (c - b'0') as i64 * 10i64.pow(8 - k as u32);
        }
        ns = v;
        i = j;
    }
    let mut off = 0i64;
    if tz {
        let sign = match b.get(i) {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return Err(bad()),
        };
        let th = digits(i + 1, 2).ok_or_else(bad)?;
        i += 3;
        let (mut tm, mut ts) = (0, 0);
        if b.get(i) == Some(&b':') {
            tm = digits(i + 1, 2).ok_or_else(bad)?;
            i += 3;
        }
        if b.get(i) == Some(&b':') {
            ts = digits(i + 1, 2).ok_or_else(bad)?;
            i += 3;
        }
        off = sign * (th * 3600 + tm * 60 + ts);
    }
    if i != b.len() || h > 23 || mi > 59 || se > 59 {
        return Err(bad());
    }
    let day = if plus_day { 2 } else { 1 };
    Ok(rfc3339_utc(0, 1, day, h, mi, se, ns, off))
}

fn parse_bytea(s: &[u8]) -> Result<Vec<u8>, String> {
    if let Some(h) = s.strip_prefix(b"\\x") {
        let mut out = Vec::with_capacity(h.len() / 2);
        for p in h.chunks(2) {
            let v = |c: u8| (c as char).to_digit(16);
            match (v(p[0]), p.get(1).and_then(|&c| v(c))) {
                (Some(a), Some(b)) => out.push((a * 16 + b) as u8),
                _ => return Err("pq: encoding/hex: invalid byte".into()),
            }
        }
        return Ok(out);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' {
            if s.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            let oct = s.get(i + 1..i + 4).ok_or("pq: invalid bytea sequence")?;
            let v = u8::from_str_radix(std::str::from_utf8(oct).unwrap_or("x"), 8)
                .map_err(|_| "pq: could not parse bytea value".to_string())?;
            out.push(v);
            i += 4;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// textDecode + sqlValue for one column.
fn value(oid: u32, b: &[u8]) -> Result<Val, String> {
    let s = String::from_utf8_lossy(b);
    Ok(match oid {
        17 => Val::Str(parse_bytea(b)?),
        1184 | 1114 | 1082 => {
            if s == "infinity" || s == "-infinity" {
                Val::str(b.to_vec())
            } else {
                Val::str(parse_ts(&s).map_err(|e| format!("pq: {e}"))?)
            }
        }
        1083 => Val::str(parse_clock(&s, false)?),
        1266 => Val::str(parse_clock(&s, true)?),
        16 => Val::str(if b.first() == Some(&b't') { "true" } else { "false" }),
        20 | 23 | 21 => match s.parse::<i64>() {
            Ok(n) => Val::str(n.to_string()),
            Err(_) => {
                return Err(format!(
                    "pq: strconv.ParseInt: parsing {}: invalid syntax",
                    q(b)
                ))
            }
        },
        700 | 701 => match s.parse::<f64>() {
            Ok(f) => Val::str(gofmt::format_float(f, false)),
            Err(_) => {
                return Err(format!(
                    "pq: strconv.ParseFloat: parsing {}: invalid syntax",
                    q(b)
                ))
            }
        },
        _ => Val::str(b.to_vec()),
    })
}

struct Col {
    name: Vec<u8>,
    oid: u32,
}

fn row_description(b: &[u8]) -> Vec<Col> {
    let n = u16::from_be_bytes([b[0], b[1]]) as usize;
    let mut pos = 2;
    let mut cols = Vec::with_capacity(n);
    for _ in 0..n {
        let name = cstr(b, &mut pos);
        let oid = b
            .get(pos + 6..pos + 10)
            .map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]))
            .unwrap_or(0);
        pos += 18;
        cols.push(Col { name, oid });
    }
    cols
}

fn data_row(cols: &[Col], b: &[u8]) -> Result<Record, String> {
    let n = u16::from_be_bytes([b[0], b[1]]) as usize;
    let mut pos = 2;
    let mut o = Obj::new();
    for c in cols.iter().take(n) {
        let l = i32::from_be_bytes([b[pos], b[pos + 1], b[pos + 2], b[pos + 3]]);
        pos += 4;
        let v = if l < 0 {
            Val::Null
        } else {
            let v = value(c.oid, &b[pos..pos + l as usize])?;
            pos += l as usize;
            v
        };
        o.insert(c.name.clone(), v);
    }
    // a row shorter than the description leaves the other columns NULL (database/sql's dest)
    for c in cols.iter().skip(n) {
        o.insert(c.name.clone(), Val::Null);
    }
    Ok(Val::Obj(o))
}

/// simpleQuery + rows.Next as lib/pq ran them.
fn simple_query(pg: &mut Pg, query: &[u8]) -> Result<Vec<Record>, String> {
    let mut b = query.to_vec();
    b.push(0);
    pg.send(b'Q', &b)?;
    let mut cols: Option<Vec<Col>> = None;
    let mut err: Option<String> = None;
    // simpleQuery: until the first DataRow, the end of a statement with columns, or
    // ReadyForQuery
    let mut first_row: Option<Vec<u8>> = None;
    loop {
        let (t, b) = pg.recv()?;
        match t {
            b'C' if cols.is_some() => break,
            b'C' | b'I' => {}
            b'Z' => {
                return match err {
                    Some(e) => Err(e),
                    None => Ok(Vec::new()),
                }
            }
            b'E' => {
                cols = None;
                err = Some(parse_error(&b));
            }
            b'D' => {
                if cols.is_none() {
                    return Err("pq: unexpected DataRow in simple query execution".into());
                }
                first_row = Some(b);
                break;
            }
            b'T' => cols = Some(row_description(&b)),
            b'N' | b'S' | b'A' => {}
            _ => return Err(format!("pq: unknown response for simple query: {}", q(&[t]))),
        }
    }
    let cols = cols.unwrap_or_default();
    let mut out = Vec::new();
    let mut err: Option<String> = None;
    if let Some(r) = first_row {
        out.push(data_row(&cols, &r)?);
    }
    // rows.Next
    loop {
        let (t, b) = pg.recv()?;
        match t {
            b'E' => err = Some(parse_error(&b)),
            b'C' | b'I' => {}
            b'Z' => {
                return match err {
                    Some(e) => Err(e),
                    None => Ok(out),
                }
            }
            b'D' => {
                if let Some(e) = err {
                    return Err(format!("pq: unexpected DataRow after error {e}"));
                }
                out.push(data_row(&cols, &b)?);
            }
            b'T' => return Ok(out),
            b'N' | b'S' | b'A' => {}
            _ => return Err(format!("pq: unexpected message after execute: {}", q(&[t]))),
        }
    }
}

pub fn read(dsn: &[u8], query: &[u8], o: &ReadOpts) -> Res<Vec<Record>> {
    let d = postgres_dsn(&lossy(dsn), o.timeout);
    let deadline = Instant::now() + o.timeout;
    let opts = options(&d).map_err(cannot)?;
    let mut pg = open(&opts, deadline).map_err(cannot)?;
    let r = simple_query(&mut pg, query).map_err(cannot);
    let _ = pg.send(b'X', &[]);
    r
}

/// Statements for the store tests.
pub fn exec_statements(dsn: &str, stmts: &[&str]) -> Result<(), String> {
    let opts = options(dsn)?;
    let mut pg = open(&opts, Instant::now() + Duration::from_secs(30))?;
    for s in stmts {
        simple_query(&mut pg, s.as_bytes())?;
    }
    let _ = pg.send(b'X', &[]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_ts("2026-01-01 00:00:00+00").unwrap(), "2026-01-01T00:00:00Z");
        assert_eq!(parse_ts("2026-01-01 02:30:00.5+05:30").unwrap(), "2025-12-31T21:00:00.5Z");
        assert_eq!(parse_ts("2026-01-01").unwrap(), "2026-01-01T00:00:00Z");
        assert_eq!(parse_ts("0001-01-01 00:00:00 BC").unwrap(), "0000-01-01T00:00:00Z");
        assert_eq!(parse_ts("12026-03-01 00:00:00.000123").unwrap(), "12026-03-01T00:00:00.000123Z");
        assert_eq!(parse_clock("12:34:56.789", false).unwrap(), "0000-01-01T12:34:56.789Z");
        assert_eq!(parse_clock("02:00:00+05", true).unwrap(), "-0001-12-31T21:00:00Z");
        assert_eq!(parse_clock("24:00:00", false).unwrap(), "0000-01-02T00:00:00Z");
    }

    #[test]
    fn dsns() {
        assert_eq!(
            postgres_dsn("postgres://miniflux@127.0.0.1/miniflux?sslmode=disable", Duration::from_secs(30)),
            "postgres://miniflux@127.0.0.1/miniflux?sslmode=disable&connect_timeout=30"
        );
        assert_eq!(
            parse_url("postgres://miniflux@127.0.0.1/miniflux?sslmode=disable&connect_timeout=30").unwrap(),
            "connect_timeout='30' dbname='miniflux' host='127.0.0.1' sslmode='disable' user='miniflux'"
        );
        let mut o = Values::new();
        parse_opts("host=x user='a b' password=p\\ q", &mut o).unwrap();
        assert_eq!(o["user"], "a b");
        assert_eq!(o["password"], "p q");
    }

    #[test]
    fn bytea() {
        assert_eq!(parse_bytea(b"\\x00ff41").unwrap(), vec![0, 255, b'A']);
        assert_eq!(parse_bytea(b"a\\\\b\\001").unwrap(), b"a\\b\x01".to_vec());
    }
}
