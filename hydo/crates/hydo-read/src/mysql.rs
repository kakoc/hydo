//! MySQL over the classic text protocol, as go-sql-driver/mysql 1.8.1 spoke it for
//! `db.QueryContext(ctx, query)` with no arguments: the same DSN forms, the same handshake
//! (utf8mb4_general_ci, no multi-statements), the same value conversion (integer columns parsed
//! and printed back, FLOAT as float32, DOUBLE as float64, everything else as its bytes) and the
//! same error texts ("Error 1146 (42S02): Table 't.nope' doesn't exist").
//!
//! Authentication: mysql_native_password and caching_sha2_password (the scramble, the fast path,
//! and the full path: the cleartext password over a Unix socket, over TCP the password encrypted
//! with the server's RSA public key, as go-sql-driver does). No TLS.

use crate::json::Val;
use crate::net::{dial_tcp, dial_unix, Conn, NetErr};
use crate::{cannot, gofmt, lossy, q, usage, Error, ReadOpts, Record, Res};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// The Go program's mysqlDSN: the driver form or a mysql:// URL, plus timeouts.
pub fn mysql_dsn(dsn: &str, t: Duration) -> Res<String> {
    let mut dsn = dsn.to_string();
    if dsn.starts_with("mysql://") {
        let u = crate::url::parse(&dsn)
            .map_err(|e| usage(format!("mysql dsn {}: {e}", q(dsn.as_bytes()))))?;
        let mut host = u.host.clone();
        if !host.contains(':') {
            host.push_str(":3306");
        }
        let mut ui = String::new();
        if let Some((n, p)) = &u.user {
            ui.push_str(n);
            if let Some(p) = p {
                ui.push(':');
                ui.push_str(p);
            }
            ui.push('@');
        }
        dsn = format!("{ui}tcp({host}){}", u.path);
        if !dsn.ends_with('/') && u.path.is_empty() {
            dsn.push('/');
        }
        if !u.raw_query.is_empty() {
            dsn.push('?');
            dsn.push_str(&u.raw_query);
        }
    }
    let sec = format!("{}s", (t.as_secs_f64() + 0.999) as i64);
    for p in ["timeout", "readTimeout", "writeTimeout"] {
        if !dsn.contains(&format!("{p}=")) {
            dsn.push(if dsn.contains('?') { '&' } else { '?' });
            dsn.push_str(p);
            dsn.push('=');
            dsn.push_str(&sec);
        }
    }
    Ok(dsn)
}

#[derive(Debug, Default)]
struct Config {
    user: String,
    passwd: String,
    net: String,
    addr: String,
    dbname: String,
    collation: u8,
    /// A collation= parameter, looked up when connecting (as the driver does).
    collation_name: Option<String>,
    charset: Option<String>,
    params: Vec<(String, String)>,
    native: bool,
    multi: bool,
}

fn read_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "true" | "TRUE" | "True" => Some(true),
        "0" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

fn collation_id(name: &str) -> Option<u8> {
    Some(match name {
        "big5_chinese_ci" => 1,
        "latin1_swedish_ci" => 8,
        "ascii_general_ci" => 11,
        "utf8_general_ci" | "utf8mb3_general_ci" => 33,
        "binary" => 63,
        "latin1_bin" => 47,
        "utf8_bin" | "utf8mb3_bin" => 83,
        "utf8mb4_general_ci" => 45,
        "utf8mb4_bin" => 46,
        "utf8mb4_unicode_ci" => 224,
        "utf8mb4_0900_ai_ci" => 255,
        _ => return None,
    })
}

/// go-sql-driver's ParseDSN, the parts a reader needs.
fn parse_dsn(dsn: &str) -> Result<Config, String> {
    let mut cfg = Config {
        collation: 45,
        native: true,
        ..Config::default()
    };
    let b = dsn.as_bytes();
    let Some(i) = b.iter().rposition(|&c| c == b'/') else {
        if dsn.is_empty() {
            cfg.net = "tcp".into();
            cfg.addr = "127.0.0.1:3306".into();
            return Ok(cfg);
        }
        return Err("invalid DSN: missing the slash separating the database name".into());
    };
    if i > 0 {
        let mut j = i as isize;
        let mut found_at = false;
        while j >= 0 {
            if b[j as usize] == b'@' {
                found_at = true;
                let ju = j as usize;
                let mut k = 0;
                while k < ju {
                    if b[k] == b':' {
                        cfg.passwd = dsn[k + 1..ju].to_string();
                        break;
                    }
                    k += 1;
                }
                cfg.user = dsn[..k].to_string();
                break;
            }
            j -= 1;
        }
        let start = if found_at { j as usize + 1 } else { 0 };
        let mut k = start;
        while k < i {
            if b[k] == b'(' {
                if b[i - 1] != b')' {
                    if dsn[k + 1..i].contains(')') {
                        return Err("invalid DSN: did you forget to escape a param value?".into());
                    }
                    return Err(
                        "invalid DSN: network address not terminated (missing closing brace)"
                            .into(),
                    );
                }
                cfg.addr = dsn[k + 1..i - 1].to_string();
                break;
            }
            k += 1;
        }
        cfg.net = dsn[start..k].to_string();
    }
    let rest = &dsn[i + 1..];
    let (dbname, params) = rest.split_once('?').unwrap_or((rest, ""));
    if !params.is_empty() || rest.contains('?') {
        for v in params.split('&') {
            let Some((key, value)) = v.split_once('=') else {
                continue;
            };
            let bool_of = |v: &str| read_bool(v).ok_or_else(|| format!("invalid bool value: {v}"));
            match key {
                "allowNativePasswords" => cfg.native = bool_of(value)?,
                "multiStatements" => cfg.multi = bool_of(value)?,
                "allowAllFiles"
                | "allowCleartextPasswords"
                | "allowFallbackToPlaintext"
                | "allowOldPasswords"
                | "checkConnLiveness"
                | "clientFoundRows"
                | "columnsWithAlias"
                | "interpolateParams"
                | "parseTime"
                | "rejectReadOnly" => {
                    bool_of(value)?;
                }
                "compress" => return Err("compression not implemented yet".into()),
                "collation" => cfg.collation_name = Some(value.to_string()),
                "timeout"
                | "readTimeout"
                | "writeTimeout"
                | "loc"
                | "timeTruncate"
                | "maxAllowedPacket"
                | "connectionAttributes"
                | "strict"
                | "serverPubKey" => {}
                "tls" => {
                    if !matches!(value, "false" | "") {
                        return Err(format!("tls={value}: TLS is not supported by hydo-read"));
                    }
                }
                _ => {
                    let v = crate::url::unescape(value, true)?;
                    if key == "charset" {
                        cfg.charset = Some(v);
                    } else {
                        cfg.params.push((key.to_string(), v));
                    }
                }
            }
        }
    }
    cfg.dbname = crate::url::unescape(dbname, false)
        .map_err(|e| format!("invalid dbname {}: {e}", q(dbname.as_bytes())))?;
    if cfg.net.is_empty() {
        cfg.net = "tcp".into();
    }
    if cfg.addr.is_empty() {
        cfg.addr = match cfg.net.as_str() {
            "tcp" => "127.0.0.1:3306".into(),
            "unix" => "/tmp/mysql.sock".into(),
            n => return Err(format!("default addr for network '{n}' unknown")),
        };
    } else if cfg.net == "tcp" && crate::net::split_host_port(&cfg.addr).is_err() {
        cfg.addr = if cfg.addr.contains(':') {
            format!("[{}]:3306", cfg.addr)
        } else {
            format!("{}:3306", cfg.addr)
        };
    }
    Ok(cfg)
}

const CLIENT_LONG_PASSWORD: u32 = 1;
const CLIENT_LONG_FLAG: u32 = 4;
const CLIENT_CONNECT_WITH_DB: u32 = 8;
const CLIENT_LOCAL_FILES: u32 = 128;
const CLIENT_PROTOCOL_41: u32 = 512;
const CLIENT_TRANSACTIONS: u32 = 8192;
const CLIENT_SECURE_CONN: u32 = 32768;
const CLIENT_MULTI_STATEMENTS: u32 = 1 << 16;
const CLIENT_MULTI_RESULTS: u32 = 1 << 17;
const CLIENT_PLUGIN_AUTH: u32 = 1 << 19;
const CLIENT_CONNECT_ATTRS: u32 = 1 << 20;
const CLIENT_PLUGIN_AUTH_LENENC: u32 = 1 << 21;

/// Why a query failed; `Bad` is driver.ErrBadConn (database/sql retries a new connection).
enum Fail {
    Bad,
    Err(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Fail {
        Fail::Err(s)
    }
}

struct My {
    c: Conn,
    seq: u8,
    in_handshake: bool,
}

fn timeout_text() -> String {
    "context deadline exceeded".into()
}

impl My {
    fn net(&self, e: NetErr) -> Fail {
        match e {
            NetErr::Timeout(_) => Fail::Err(timeout_text()),
            _ if self.in_handshake => Fail::Bad,
            _ => Fail::Err("invalid connection".into()),
        }
    }

    fn read_packet(&mut self) -> Result<Vec<u8>, Fail> {
        let mut out = Vec::new();
        loop {
            let h = self.c.read_exact(4).map_err(|e| self.net(e))?;
            let len = h[0] as usize | (h[1] as usize) << 8 | (h[2] as usize) << 16;
            if h[3] != self.seq {
                return Err(Fail::Err(if h[3] > self.seq {
                    "commands out of sync. Did you run multiple statements at once?".into()
                } else {
                    "commands out of sync. You can't run this command now".into()
                }));
            }
            self.seq = self.seq.wrapping_add(1);
            if len == 0 && out.is_empty() {
                return Err(Fail::Err("invalid connection".into()));
            }
            let body = self.c.read_exact(len).map_err(|e| self.net(e))?;
            out.extend_from_slice(&body);
            if len < 0xffffff {
                return Ok(out);
            }
        }
    }

    fn write_packet(&mut self, data: &[u8]) -> Result<(), Fail> {
        let mut rest = data;
        loop {
            let n = rest.len().min(0xffffff);
            let mut p = vec![n as u8, (n >> 8) as u8, (n >> 16) as u8, self.seq];
            p.extend_from_slice(&rest[..n]);
            self.c.write_all(&p).map_err(|e| self.net(e))?;
            self.seq = self.seq.wrapping_add(1);
            rest = &rest[n..];
            if n < 0xffffff {
                return Ok(());
            }
        }
    }
}

/// MySQLError.Error().
fn error_packet(d: &[u8]) -> String {
    if d.len() < 3 {
        return "malformed packet".into();
    }
    let no = u16::from_le_bytes([d[1], d[2]]);
    if d.len() >= 9 && d[3] == b'#' {
        format!("Error {no} ({}): {}", lossy(&d[4..9]), lossy(&d[9..]))
    } else {
        format!("Error {no}: {}", lossy(&d[3..]))
    }
}

fn lenenc_int(d: &[u8], pos: &mut usize) -> Option<Option<u64>> {
    let c = *d.get(*pos)?;
    *pos += 1;
    let n = match c {
        0xfb => return Some(None),
        0xfc => 2,
        0xfd => 3,
        0xfe => 8,
        _ => return Some(Some(c as u64)),
    };
    let b = d.get(*pos..*pos + n)?;
    *pos += n;
    let mut v = 0u64;
    for (i, &x) in b.iter().enumerate() {
        v |= (x as u64) << (8 * i);
    }
    Some(Some(v))
}

fn lenenc_str<'a>(d: &'a [u8], pos: &mut usize) -> Option<Option<&'a [u8]>> {
    match lenenc_int(d, pos)? {
        None => Some(None),
        Some(n) => {
            let n = n as usize;
            let s = d.get(*pos..*pos + n)?;
            *pos += n;
            Some(Some(s))
        }
    }
}

fn scramble_native(scr: &[u8], pw: &str) -> Vec<u8> {
    if pw.is_empty() {
        return Vec::new();
    }
    let s1 = Sha1::digest(pw.as_bytes());
    let s2 = Sha1::digest(s1);
    let mut h = Sha1::new();
    h.update(scr);
    h.update(s2);
    let mut out = h.finalize().to_vec();
    for (o, a) in out.iter_mut().zip(s1.iter()) {
        *o ^= a;
    }
    out
}

fn scramble_sha256(scr: &[u8], pw: &str) -> Vec<u8> {
    if pw.is_empty() {
        return Vec::new();
    }
    let m1 = Sha256::digest(pw.as_bytes());
    let m1h = Sha256::digest(m1);
    let mut h = Sha256::new();
    h.update(m1h);
    h.update(scr);
    let m2 = h.finalize();
    m1.iter().zip(m2.iter()).map(|(a, b)| a ^ b).collect()
}

/// go-sql-driver's encryptPassword: (password + NUL) XOR the scramble, RSA-OAEP with SHA-1 under
/// the server's PEM public key.
fn encrypt_password(pw: &str, seed: &[u8], pem: &[u8]) -> Result<Vec<u8>, Fail> {
    use rsa::pkcs8::DecodePublicKey;
    let text = String::from_utf8_lossy(pem);
    let Some(start) = text.find("-----BEGIN") else {
        return Err(Fail::Err(format!("no pem data found, data: {}", text)));
    };
    let key = rsa::RsaPublicKey::from_public_key_pem(text[start..].trim_end_matches('\0').trim())
        .or_else(|_| {
            use rsa::pkcs1::DecodeRsaPublicKey;
            rsa::RsaPublicKey::from_pkcs1_pem(text[start..].trim())
        })
        .map_err(|e| Fail::Err(format!("x509: {e}")))?;
    let mut plain = pw.as_bytes().to_vec();
    plain.push(0);
    if !seed.is_empty() {
        for (i, b) in plain.iter_mut().enumerate() {
            *b ^= seed[i % seed.len()];
        }
    }
    key.encrypt(&mut rand_core::OsRng, rsa::Oaep::new::<Sha1>(), &plain)
        .map_err(|e| Fail::Err(format!("crypto/rsa: {e}")))
}

fn auth_resp(cfg: &Config, plugin: &str, data: &[u8]) -> Result<Vec<u8>, Fail> {
    match plugin {
        "caching_sha2_password" => Ok(scramble_sha256(data, &cfg.passwd)),
        "mysql_native_password" => {
            if !cfg.native {
                return Err(Fail::Err(
                    "this user requires mysql native password authentication".into(),
                ));
            }
            Ok(scramble_native(&data[..data.len().min(20)], &cfg.passwd))
        }
        "mysql_clear_password" => Err(Fail::Err("this user requires clear text authentication. If you still want to use it, please add 'allowCleartextPasswords=1' to your DSN".into())),
        "sha256_password" if cfg.passwd.is_empty() => Ok(vec![0]),
        _ => Err(Fail::Err("this authentication plugin is not supported".into())),
    }
}

/// The result of one connection attempt.
fn connect(cfg: &Config, deadline: Instant) -> Result<My, Fail> {
    let mut collation = cfg.collation;
    if let Some(n) = &cfg.collation_name {
        collation = collation_id(n)
            .ok_or_else(|| Fail::Err(format!("unknown collation: {}", q(n.as_bytes()))))?;
    }
    let c = if cfg.net == "unix" {
        dial_unix(&cfg.addr, deadline)
    } else {
        dial_tcp(&cfg.addr, deadline)
    }
    .map_err(|e| match e {
        NetErr::Timeout(_) => Fail::Err(timeout_text()),
        e => Fail::Err(e.to_string()),
    })?;
    let mut my = My {
        c,
        seq: 0,
        in_handshake: true,
    };
    let d = my.read_packet()?;
    if d.first() == Some(&0xff) {
        return Err(Fail::Err(error_packet(&d)));
    }
    if d.first().copied().unwrap_or(0) < 10 {
        return Err(Fail::Err(format!(
            "unsupported protocol version {}. Version 10 or higher is required",
            d.first().copied().unwrap_or(0)
        )));
    }
    let bad = || Fail::Err("malformed packet".into());
    let mut pos = 1 + d[1..].iter().position(|&c| c == 0).ok_or_else(bad)? + 1 + 4;
    let mut scramble = d.get(pos..pos + 8).ok_or_else(bad)?.to_vec();
    pos += 9;
    let caps_lo = u16::from_le_bytes([
        *d.get(pos).ok_or_else(bad)?,
        *d.get(pos + 1).ok_or_else(bad)?,
    ]) as u32;
    pos += 2;
    let mut caps = caps_lo;
    let mut plugin = String::from("mysql_native_password");
    if d.len() > pos {
        // charset(1) status(2) caps_hi(2) auth_len(1) reserved(10)
        let caps_hi = u16::from_le_bytes([
            *d.get(pos + 3).ok_or_else(bad)?,
            *d.get(pos + 4).ok_or_else(bad)?,
        ]) as u32;
        caps |= caps_hi << 16;
        pos += 16;
        if let Some(part2) = d.get(pos..pos + 12) {
            scramble.extend_from_slice(part2);
        }
        pos += 13;
        if pos < d.len() {
            let end = d[pos..]
                .iter()
                .position(|&c| c == 0)
                .map(|e| pos + e)
                .unwrap_or(d.len());
            plugin = lossy(&d[pos..end]);
        }
    }
    if caps & CLIENT_PROTOCOL_41 == 0 {
        return Err(Fail::Err(
            "MySQL server does not support required protocol 41+".into(),
        ));
    }
    let mut resp = auth_resp(cfg, &plugin, &scramble).or_else(|e| match e {
        Fail::Err(s) if s == "this authentication plugin is not supported" => {
            plugin = "mysql_native_password".into();
            auth_resp(cfg, &plugin, &scramble)
        }
        e => Err(e),
    })?;
    let mut flags = CLIENT_PROTOCOL_41
        | CLIENT_SECURE_CONN
        | CLIENT_LONG_PASSWORD
        | CLIENT_TRANSACTIONS
        | CLIENT_LOCAL_FILES
        | CLIENT_PLUGIN_AUTH
        | CLIENT_MULTI_RESULTS
        | CLIENT_CONNECT_ATTRS
        | (caps & CLIENT_LONG_FLAG);
    if cfg.multi {
        flags |= CLIENT_MULTI_STATEMENTS;
    }
    if resp.len() > 250 {
        flags |= CLIENT_PLUGIN_AUTH_LENENC;
    }
    if !cfg.dbname.is_empty() {
        flags |= CLIENT_CONNECT_WITH_DB;
    }
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    p.push(collation);
    p.extend_from_slice(&[0u8; 23]);
    p.extend_from_slice(cfg.user.as_bytes());
    p.push(0);
    if resp.len() > 250 {
        p.push(0xfc);
        p.extend_from_slice(&(resp.len() as u16).to_le_bytes());
    } else {
        p.push(resp.len() as u8);
    }
    p.extend_from_slice(&resp);
    if !cfg.dbname.is_empty() {
        p.extend_from_slice(cfg.dbname.as_bytes());
        p.push(0);
    }
    p.extend_from_slice(plugin.as_bytes());
    p.push(0);
    let mut attrs = Vec::new();
    for (k, v) in [
        ("_client_name", "Go-MySQL-Driver"),
        ("_os", "linux"),
        ("_platform", std::env::consts::ARCH),
    ] {
        attrs.push(k.len() as u8);
        attrs.extend_from_slice(k.as_bytes());
        attrs.push(v.len() as u8);
        attrs.extend_from_slice(v.as_bytes());
    }
    p.push(attrs.len() as u8);
    p.extend_from_slice(&attrs);
    my.write_packet(&p)?;
    // the authentication result, with at most one plugin switch
    let mut switched = false;
    loop {
        let d = my.read_packet()?;
        match d.first() {
            Some(0x00) => break,
            Some(0xff) => return Err(Fail::Err(error_packet(&d))),
            Some(0xfe) => {
                if switched {
                    return Err(Fail::Err("malformed packet".into()));
                }
                switched = true;
                if d.len() == 1 {
                    return Err(Fail::Err("this user requires old password authentication. If you still want to use it, please add 'allowOldPasswords=1' to your DSN. See also https://github.com/go-sql-driver/mysql/wiki/old_passwords".into()));
                }
                let end = d.iter().position(|&c| c == 0).ok_or_else(bad)?;
                plugin = lossy(&d[1..end]);
                let mut data = d[end + 1..].to_vec();
                if data.last() == Some(&0) {
                    data.pop();
                }
                scramble = data;
                resp = auth_resp(cfg, &plugin, &scramble)?;
                my.write_packet(&resp)?;
            }
            Some(0x01) if plugin == "caching_sha2_password" => match d.get(1) {
                Some(3) => continue, // fast auth succeeded; the OK packet follows
                Some(4) => {
                    if cfg.net == "unix" {
                        let mut pw = cfg.passwd.clone().into_bytes();
                        pw.push(0);
                        my.write_packet(&pw)?;
                    } else {
                        // the server's RSA public key, then the password XOR the scramble, encrypted
                        my.write_packet(&[2])?;
                        let k = my.read_packet()?;
                        if k.first() != Some(&1) {
                            return Err(Fail::Err("unexpected resp from server for caching_sha2_password, perform full authentication".into()));
                        }
                        let enc = encrypt_password(&cfg.passwd, &scramble, &k[1..])?;
                        my.write_packet(&enc)?;
                    }
                }
                _ => return Err(Fail::Err("malformed packet".into())),
            },
            _ => return Err(Fail::Err("malformed packet".into())),
        }
    }
    my.in_handshake = false;
    if let Some(cs) = &cfg.charset {
        let mut err = None;
        for c in cs.split(',') {
            match exec(&mut my, &format!("SET NAMES {c}")) {
                Ok(()) => {
                    err = None;
                    break;
                }
                Err(e) => err = Some(e),
            }
        }
        if let Some(e) = err {
            return Err(e);
        }
    }
    if !cfg.params.is_empty() {
        let set: Vec<String> = cfg
            .params
            .iter()
            .map(|(k, v)| format!("{k} = {v}"))
            .collect();
        exec(&mut my, &format!("SET {}", set.join(", ")))?;
    }
    Ok(my)
}

fn command(my: &mut My, q: &str) -> Result<Vec<u8>, Fail> {
    my.seq = 0;
    let mut p = vec![3u8];
    p.extend_from_slice(q.as_bytes());
    my.write_packet(&p)?;
    my.read_packet()
}

/// A statement whose result is ignored (SET NAMES, SET params).
fn exec(my: &mut My, q: &str) -> Result<(), Fail> {
    let d = command(my, q)?;
    match d.first() {
        Some(0x00) => Ok(()),
        Some(0xff) => Err(Fail::Err(error_packet(&d))),
        _ => {
            // a result set: skip it
            loop {
                let d = my.read_packet()?;
                if d.first() == Some(&0xfe) && d.len() < 9 {
                    break;
                }
            }
            loop {
                let d = my.read_packet()?;
                if d.first() == Some(&0xfe) && d.len() < 9 {
                    return Ok(());
                }
                if d.first() == Some(&0xff) {
                    return Err(Fail::Err(error_packet(&d)));
                }
            }
        }
    }
}

struct Col {
    name: Vec<u8>,
    typ: u8,
    unsigned: bool,
}

/// sqlValue of what the driver returned for one text-protocol column.
fn column_value(col: &Col, b: &[u8]) -> Result<Val, String> {
    let s = String::from_utf8_lossy(b);
    let perr = |f: &str| format!("strconv.{f}: parsing {}: invalid syntax", q(b));
    let rng = |f: &str| format!("strconv.{f}: parsing {}: value out of range", q(b));
    let int = |s: &str| -> Result<Val, String> {
        match s.parse::<i64>() {
            Ok(n) => Ok(Val::str(n.to_string())),
            Err(e)
                if *e.kind() == std::num::IntErrorKind::PosOverflow
                    || *e.kind() == std::num::IntErrorKind::NegOverflow =>
            {
                Err(rng("ParseInt"))
            }
            Err(_) => Err(perr("ParseInt")),
        }
    };
    match col.typ {
        // TINY, SHORT, LONG, INT24, YEAR
        1 | 2 | 3 | 9 | 13 => int(&s),
        // LONGLONG
        8 if col.unsigned => match s.parse::<u64>() {
            Ok(n) => Ok(Val::str(n.to_string())),
            Err(e) if *e.kind() == std::num::IntErrorKind::PosOverflow => Err(rng("ParseUint")),
            Err(_) => Err(perr("ParseUint")),
        },
        8 => int(&s),
        // FLOAT: parsed as float32
        4 => match s.parse::<f32>() {
            Ok(f) if f.is_finite() || s.contains("inf") => {
                Ok(Val::str(gofmt::format_float(f as f64, true)))
            }
            Ok(_) => Err(rng("ParseFloat")),
            Err(_) => Err(perr("ParseFloat")),
        },
        // DOUBLE
        5 => match s.parse::<f64>() {
            Ok(f) if f.is_finite() || s.contains("inf") => {
                Ok(Val::str(gofmt::format_float(f, false)))
            }
            Ok(_) => Err(rng("ParseFloat")),
            Err(_) => Err(perr("ParseFloat")),
        },
        _ => Ok(Val::str(b.to_vec())),
    }
}

/// Runs the query on one connection: rows, or the failure.
fn query(my: &mut My, query: &str) -> Result<Vec<Record>, Fail> {
    let d = command(my, query)?;
    match d.first() {
        Some(0x00) => return Ok(Vec::new()),
        Some(0xff) => return Err(Fail::Err(error_packet(&d))),
        Some(0xfb) => {
            return Err(Fail::Err(
                "LOCAL INFILE is not supported by hydo-read".into(),
            ))
        }
        _ => {}
    }
    let mut pos = 0;
    let n = lenenc_int(&d, &mut pos)
        .flatten()
        .ok_or_else(|| Fail::Err("malformed packet".into()))? as usize;
    let mut cols = Vec::with_capacity(n);
    for _ in 0..n {
        let d = my.read_packet()?;
        if d.first() == Some(&0xff) {
            return Err(Fail::Err(error_packet(&d)));
        }
        let mut p = 0;
        let mut name = Vec::new();
        for i in 0..6 {
            let s = lenenc_str(&d, &mut p).ok_or_else(|| Fail::Err("malformed packet".into()))?;
            if i == 4 {
                name = s.unwrap_or_default().to_vec();
            }
        }
        // 0x0c, charset(2), length(4), type(1), flags(2), decimals(1)
        let typ = *d
            .get(p + 7)
            .ok_or_else(|| Fail::Err("malformed packet".into()))?;
        let flags = u16::from_le_bytes([*d.get(p + 8).unwrap_or(&0), *d.get(p + 9).unwrap_or(&0)]);
        cols.push(Col {
            name,
            typ,
            unsigned: flags & 32 != 0,
        });
    }
    // the EOF after the column definitions
    let d = my.read_packet()?;
    if d.first() == Some(&0xff) {
        return Err(Fail::Err(error_packet(&d)));
    }
    let mut out = Vec::new();
    loop {
        let d = my.read_packet()?;
        if d.first() == Some(&0xfe) && d.len() == 5 {
            return Ok(out);
        }
        if d.first() == Some(&0xff) {
            return Err(Fail::Err(error_packet(&d)));
        }
        let mut p = 0;
        let mut o = crate::json::Obj::new();
        for c in &cols {
            let v = match lenenc_str(&d, &mut p) {
                None => return Err(Fail::Err("EOF".into())),
                Some(None) => Val::Null,
                Some(Some(b)) => column_value(c, b).map_err(Fail::Err)?,
            };
            o.insert(c.name.clone(), v);
        }
        out.push(Val::Obj(o));
    }
}

pub fn read(dsn: &[u8], query_b: &[u8], o: &ReadOpts) -> Res<Vec<Record>> {
    let d = mysql_dsn(&lossy(dsn), o.timeout)?;
    let cfg = parse_dsn(&d).map_err(|e| usage(format!("mysql dsn: {e}")))?;
    let deadline = Instant::now() + o.timeout;
    let qs = String::from_utf8_lossy(query_b).into_owned();
    // database/sql: up to two retries on a bad connection, then one more on a new connection
    for _ in 0..3 {
        if Instant::now() >= deadline {
            return Err(cannot(timeout_text()));
        }
        let mut my = match connect(&cfg, deadline) {
            Ok(m) => m,
            Err(Fail::Bad) => continue,
            Err(Fail::Err(e)) => return Err(Error::Cannot(e)),
        };
        let r = query(&mut my, &qs);
        my.seq = 0;
        let _ = my.write_packet(&[1]); // COM_QUIT
        return match r {
            Ok(v) => Ok(v),
            Err(Fail::Bad) => Err(cannot("driver: bad connection")),
            Err(Fail::Err(e)) => Err(cannot(e)),
        };
    }
    Err(cannot("driver: bad connection"))
}

/// Statements for the store tests (DDL and inserts), one at a time.
pub fn exec_statements(dsn: &str, stmts: &[&str]) -> Result<(), String> {
    let d = mysql_dsn(dsn, Duration::from_secs(10)).map_err(|e| e.to_string())?;
    let cfg = parse_dsn(&d)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let text = |f: Fail| match f {
        Fail::Bad => "driver: bad connection".to_string(),
        Fail::Err(e) => e,
    };
    let mut my = connect(&cfg, deadline).map_err(text)?;
    for s in stmts {
        exec(&mut my, s).map_err(text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsn_forms() {
        let d = mysql_dsn("hydo:@tcp(127.0.0.1:3306)/", Duration::from_secs(30)).unwrap();
        assert_eq!(
            d,
            "hydo:@tcp(127.0.0.1:3306)/?timeout=30s&readTimeout=30s&writeTimeout=30s"
        );
        let c = parse_dsn(&d).unwrap();
        assert_eq!(
            (
                c.user.as_str(),
                c.passwd.as_str(),
                c.addr.as_str(),
                c.dbname.as_str()
            ),
            ("hydo", "", "127.0.0.1:3306", "")
        );
        let d = mysql_dsn(
            "mysql://root@127.0.0.1:37306/t",
            Duration::from_millis(2500),
        )
        .unwrap();
        assert_eq!(
            d,
            "root@tcp(127.0.0.1:37306)/t?timeout=3s&readTimeout=3s&writeTimeout=3s"
        );
        let d = mysql_dsn("mysql://u:p@h/", Duration::from_secs(1)).unwrap();
        assert!(d.starts_with("u:p@tcp(h:3306)/?"));
        assert!(parse_dsn("nope").unwrap_err().contains("missing the slash"));
    }

    #[test]
    fn values() {
        let c = |typ, unsigned| Col {
            name: vec![],
            typ,
            unsigned,
        };
        assert_eq!(column_value(&c(3, false), b"0007").unwrap(), Val::str("7"));
        assert_eq!(
            column_value(&c(8, true), b"18446744073709551615").unwrap(),
            Val::str("18446744073709551615")
        );
        assert_eq!(
            column_value(&c(5, false), b"1234567").unwrap(),
            Val::str("1.234567e+06")
        );
        assert_eq!(column_value(&c(4, false), b"0.1").unwrap(), Val::str("0.1"));
        assert_eq!(
            column_value(&c(246, false), b"1.50").unwrap(),
            Val::str("1.50")
        );
    }
}
