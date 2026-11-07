//! `hydo-sql read`: hydo-read's stores plus `sql`, `libsql`, `dump` and `world`; the same
//! reader serves the sources of `hydo-sql check` (hydo-check compiled into this crate).

use crate::conf::{Conf, Dialect, Endpoint, Role};
use crate::db::{self, Rows};
use crate::json::Val;
use crate::{
    encode_all, lossy, rec, split_read_opts, usage, Error, Record, Res, EXIT_CANNOT, EXIT_OK,
};
use std::io::Write;

pub const READ_USAGE: &str = r#"hydo-sql read <store> <args...> [--timeout <s>]
  sql    <endpoint> <query>              a query on an endpoint of the configuration
                                         ($HYDO_SQL_CONF): rows as {"<column>": "<value>"|null}
  libsql <host:port> <query>             a query through sqld's HTTP API (Hrana), the same rows
  dump   <endpoint>                      the workloads' rows: {"row": "<table> <key> <value>"}
  world  <ep>.<route>|<route>|* [--all] [--log PATH]
                                         the fake outside world's record: one record per request
                                         that took effect (--all: every request) and per event,
                                         its fields (seq, ep, route, ref, idem, fx, out, ...)
  and every hydo-read store (mysql, postgres, kafka, redis, etcd, file, jsonl, range, hydo).
Output: JSON Lines, keys sorted, lines sorted bytewise. Exit 0 ok, 2 could not read or bad usage."#;

fn rows_records(r: &Rows) -> Vec<Record> {
    r.rows
        .iter()
        .map(|row| {
            rec(r
                .cols
                .iter()
                .zip(row.iter())
                .map(|(k, v)| {
                    (
                        k.as_str(),
                        v.as_ref()
                            .map(|s| Val::str(s.as_str()))
                            .unwrap_or(Val::Null),
                    )
                })
                .collect())
        })
        .collect()
}

fn conf() -> Res<Conf> {
    Conf::load().map_err(|e| usage(format!("the configuration: {e}")))
}

fn fail(e: db::Fail) -> Error {
    crate::cannot(e.text())
}

/// Runs one store reader; `ordered` = the records are in the store's own order.
pub fn read_store(args: &[Vec<u8>]) -> Res<(Vec<Record>, bool)> {
    let (rest, o) = split_read_opts(args)?;
    let a: Vec<String> = rest.iter().map(|x| lossy(x)).collect();
    match a.first().map(String::as_str) {
        Some("sql") => {
            if a.len() != 3 {
                return Err(usage("sql <endpoint> <query> (quote the query)"));
            }
            let mut c = conf()?;
            c.timeout_ms = o.timeout.as_millis().clamp(100, 600_000) as u64;
            let ep = c
                .endpoint(&a[1])
                .cloned()
                .ok_or_else(|| usage(format!("sql: no endpoint named {}", a[1])))?;
            let r = db::query(&c, &ep, &a[2]).map_err(fail)?;
            Ok((rows_records(&r), false))
        }
        Some("libsql") => {
            if a.len() != 3 {
                return Err(usage("libsql <host:port> <query> (quote the query)"));
            }
            let c = Conf {
                dialect: Dialect::Libsql,
                endpoints: vec![Endpoint {
                    name: "x".into(),
                    addr: a[1].clone(),
                    role: Role::Primary,
                }],
                isolation: crate::conf::Isolation::Serializable,
                workloads: vec![],
                routes: vec![],
                accounts: 0,
                balance: 0,
                keys: 1,
                timeout_ms: o.timeout.as_millis().clamp(100, 600_000) as u64,
                retries: 0,
            };
            let r = db::query(&c, &c.endpoints[0], &a[2]).map_err(fail)?;
            Ok((rows_records(&r), false))
        }
        Some("dump") => {
            if a.len() != 2 {
                return Err(usage("dump <endpoint>"));
            }
            let c = conf()?;
            let ep = c
                .endpoint(&a[1])
                .cloned()
                .ok_or_else(|| usage(format!("dump: no endpoint named {}", a[1])))?;
            let items = crate::work::dump_rows(&c, &ep).map_err(fail)?;
            Ok((
                items
                    .into_iter()
                    .map(|i| rec(vec![("row", Val::str(i))]))
                    .collect(),
                false,
            ))
        }
        Some("world") => Ok((crate::world::read(&a[1..])?, true)),
        _ => hydo_read::read_store(args),
    }
}

pub fn read_main(args: &[Vec<u8>], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.is_empty() || args[0] == b"-h" || args[0] == b"--help" {
        let _ = writeln!(err, "{READ_USAGE}");
        return if args.is_empty() {
            EXIT_CANNOT
        } else {
            EXIT_OK
        };
    }
    match read_store(args) {
        Err(e) => {
            let _ = writeln!(err, "hydo-sql read: {}: {e}", lossy(&args[0]));
            EXIT_CANNOT
        }
        Ok((recs, ordered)) => {
            let mut w = Vec::new();
            for l in encode_all(&recs, ordered) {
                w.extend_from_slice(&l);
                w.push(b'\n');
            }
            let _ = out.write_all(&w);
            EXIT_OK
        }
    }
}
