//! One interface over the three dialects, and the outcome rules, written once.
//!
//! **Outcomes.** Every request ends in one of three ways, and the workloads record exactly that:
//!
//! | what happened | outcome |
//! |---|---|
//! | no connection (refused, timed out, authentication failed): the request never left | failed (definite) |
//! | the server answered an error before COMMIT, or at COMMIT with a rollback (a constraint, a serialization failure, a deadlock, `database is locked`) | failed (definite); *retryable* when the error is a transient conflict |
//! | an interactive transaction lost its connection before COMMIT was sent | failed (definite: an uncommitted transaction is rolled back when its session ends) |
//! | COMMIT (or a one-request transaction) was sent and no complete answer came (a timeout, a reset, a 5xx from a node that forwards writes, a PostgreSQL connection or operator error at COMMIT) | unknown |
//!
//! Retryable: PostgreSQL SQLSTATE 40001 (serialization failure) and 40P01 (deadlock), MySQL
//! errors 1213 (deadlock) and 1205 (lock wait timeout), SQLite's `SQLITE_BUSY` (`database is
//! locked`). The workloads retry those (a new transaction, the same operation) up to
//! `retries` times, as a careful client does; nothing else is retried.

use crate::conf::{Conf, Dialect, Endpoint, Isolation};
use crate::hrana;
use crate::mysql::{MyFail, MySession};
use crate::pg::{PgFail, PgSession};

/// One statement's result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rows {
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows changed by an UPDATE / INSERT / DELETE (-1 when the protocol does not say).
    pub affected: i64,
}

impl Rows {
    /// The first column of the first row as an integer.
    pub fn int(&self) -> Option<i64> {
        self.rows.first()?.first()?.as_deref()?.trim().parse().ok()
    }
    /// Column `c` of every row, NULL as None.
    pub fn column(&self, c: usize) -> Vec<Option<String>> {
        self.rows
            .iter()
            .map(|r| r.get(c).cloned().flatten())
            .collect()
    }
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    /// The request never reached the server: a definite failure.
    NotSent(String),
    /// The server answered with an error: a definite failure; `retry` = a transient conflict.
    Rejected { msg: String, retry: bool },
    /// Sent, and no complete answer: it may or may not have taken effect.
    Unknown(String),
}

impl Fail {
    pub fn text(&self) -> String {
        match self {
            Fail::NotSent(m) => format!("not sent: {m}"),
            Fail::Rejected { msg, .. } => msg.clone(),
            Fail::Unknown(m) => format!("no answer: {m}"),
        }
    }
    /// An error before COMMIT was sent: nothing was committed, whatever the cause.
    pub fn before_commit(self) -> Fail {
        match self {
            Fail::Rejected { .. } => self,
            Fail::NotSent(m) | Fail::Unknown(m) => Fail::Rejected {
                msg: m,
                retry: false,
            },
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Fail::Rejected { retry: true, .. })
    }
}

/// PostgreSQL: retryable SQLSTATEs, and the classes that leave a COMMIT's outcome unknown
/// (connection exception, operator intervention, system error, internal error).
pub fn pg_retry(code: &str) -> bool {
    code == "40001" || code == "40P01"
}
pub fn pg_unknown_at_commit(code: &str) -> bool {
    ["08", "57", "58", "XX"].iter().any(|c| code.starts_with(c))
}
pub fn mysql_retry(no: u16) -> bool {
    no == 1213 || no == 1205
}
pub fn sqlite_retry(msg: &str) -> bool {
    msg.contains("database is locked") || msg.contains("SQLITE_BUSY")
}

fn pg_fail(f: PgFail, commit_index: Option<usize>) -> Fail {
    match f {
        PgFail::Net(m) => Fail::Unknown(m),
        PgFail::Server(e, i) => {
            let text = format!("pq: {} (SQLSTATE {})", e.msg, e.code);
            if Some(i) == commit_index && pg_unknown_at_commit(&e.code) {
                Fail::Unknown(text)
            } else {
                Fail::Rejected {
                    msg: text,
                    retry: pg_retry(&e.code),
                }
            }
        }
    }
}

fn my_fail(f: MyFail) -> Fail {
    match f {
        MyFail::Net(m) => Fail::Unknown(m),
        MyFail::Server(no, m) => Fail::Rejected {
            msg: m,
            retry: mysql_retry(no),
        },
    }
}

fn pg_rows(r: crate::pg::PgResult) -> Rows {
    let affected = if r.cols.is_empty() { r.count() } else { -1 };
    Rows {
        cols: r.cols,
        rows: r.rows,
        affected,
    }
}

fn my_rows(r: crate::mysql::MyResult) -> Rows {
    Rows {
        cols: r.cols,
        rows: r.rows,
        affected: r.affected,
    }
}

/// The statements that open a transaction at the configured isolation level.
pub fn begin(c: &Conf) -> Vec<String> {
    match c.dialect {
        Dialect::Postgres => vec![format!("BEGIN ISOLATION LEVEL {}", c.isolation.sql())],
        Dialect::Mysql => vec![
            format!(
                "SET SESSION TRANSACTION ISOLATION LEVEL {}",
                c.isolation.sql()
            ),
            "START TRANSACTION".into(),
        ],
        // SQLite's transactions are serializable; the level is not a choice
        Dialect::Libsql => vec!["BEGIN".into()],
    }
}

/// An open connection, for interactive transactions and single statements.
pub enum Session {
    Pg(PgSession),
    My(MySession),
    Hr(hrana::Stream),
}

impl Session {
    pub fn open(c: &Conf, ep: &Endpoint) -> Result<Session, Fail> {
        let t = c.timeout();
        match c.dialect {
            Dialect::Postgres => PgSession::open(&ep.addr, t)
                .map(Session::Pg)
                .map_err(Fail::NotSent),
            Dialect::Mysql => MySession::open(&ep.addr, t)
                .map(Session::My)
                .map_err(Fail::NotSent),
            Dialect::Libsql => Ok(Session::Hr(hrana::Stream::new(&ep.addr, t))),
        }
    }

    /// One statement. A server error is Rejected, a lost answer Unknown.
    pub fn exec(&mut self, sql: &str) -> Result<Rows, Fail> {
        match self {
            Session::Pg(s) => match s.simple(sql) {
                Ok(mut v) => Ok(v.pop().map(pg_rows).unwrap_or_default()),
                Err(f) => Err(pg_fail(f, None)),
            },
            Session::My(s) => s.exec(sql).map(my_rows).map_err(my_fail),
            Session::Hr(s) => s.exec(sql),
        }
    }

    /// COMMIT: an error the server answered is a definite failure unless its kind leaves the
    /// outcome open; no answer is unknown.
    pub fn commit(&mut self) -> Result<(), Fail> {
        match self {
            Session::Pg(s) => match s.simple("COMMIT") {
                Ok(v) => match v.last() {
                    Some(r) if r.tag == "ROLLBACK" => Err(Fail::Rejected {
                        msg: "COMMIT answered ROLLBACK: the transaction was rolled back".into(),
                        retry: false,
                    }),
                    _ => Ok(()),
                },
                Err(f) => Err(pg_fail(f, Some(0))),
            },
            Session::My(s) => s.exec("COMMIT").map(|_| ()).map_err(my_fail),
            Session::Hr(s) => s.exec("COMMIT").map(|_| ()),
        }
    }

    pub fn close(&mut self) {
        match self {
            Session::Pg(s) => s.close(),
            Session::My(s) => s.close(),
            Session::Hr(s) => s.close(),
        }
    }
}

/// A statement in autocommit mode on its own connection.
pub fn query(c: &Conf, ep: &Endpoint, sql: &str) -> Result<Rows, Fail> {
    let mut s = Session::open(c, ep)?;
    let r = s.exec(sql);
    s.close();
    r
}

/// Several statements in autocommit mode on one connection (DDL, the first rows).
pub fn exec_all(c: &Conf, ep: &Endpoint, sqls: &[String]) -> Result<Vec<Rows>, Fail> {
    if c.dialect == Dialect::Libsql {
        return hrana::autocommit(&ep.addr, sqls, c.timeout());
    }
    let mut s = Session::open(c, ep)?;
    let mut out = Vec::new();
    for q in sqls {
        match s.exec(q) {
            Ok(r) => out.push(r),
            Err(e) => {
                s.close();
                return Err(e);
            }
        }
    }
    s.close();
    Ok(out)
}

/// An interactive transaction: open, BEGIN, the body's statements one round trip each with the
/// client's decisions between them, COMMIT. Anything that goes wrong before COMMIT is sent is a
/// definite failure (the server rolls the transaction back).
pub fn interactive<T>(
    c: &Conf,
    ep: &Endpoint,
    body: impl FnOnce(&mut Session) -> Result<T, Fail>,
) -> Result<T, Fail> {
    let mut s = Session::open(c, ep)?;
    let pre = (|| {
        for b in begin(c) {
            s.exec(&b)?;
        }
        body(&mut s)
    })();
    let v = match pre {
        Ok(v) => v,
        Err(e) => {
            let _ = s.exec("ROLLBACK");
            s.close();
            return Err(e.before_commit());
        }
    };
    let r = s.commit();
    s.close();
    r.map(|_| v)
}

/// A transaction whose statements are all decided before it starts, sent as one request where
/// the protocol has one: PostgreSQL one simple-query message (`BEGIN ...; s1; ...; COMMIT`),
/// libSQL one Hrana batch whose steps run only if the previous ones succeeded. MySQL's text
/// protocol has no multi-statement request without a connection flag hydo-read does not set,
/// so there the statements follow each other on one connection with no decision between them.
/// The result of each of `stmts`.
pub fn batch(c: &Conf, ep: &Endpoint, stmts: &[String]) -> Result<Vec<Rows>, Fail> {
    match c.dialect {
        Dialect::Postgres => {
            let mut s = PgSession::open(&ep.addr, c.timeout()).map_err(Fail::NotSent)?;
            let b = begin(c);
            let mut all = b.clone();
            all.extend(stmts.iter().cloned());
            all.push("COMMIT".into());
            let commit_at = all.len() - 1;
            let r = s.simple(&all.join("; "));
            s.close();
            match r {
                Err(f) => Err(pg_fail(f, Some(commit_at))),
                Ok(v) => {
                    if v.len() != all.len() || v[commit_at].tag != "COMMIT" {
                        return Err(Fail::Rejected {
                            msg: format!(
                                "the transaction did not commit (tags: {})",
                                v.iter()
                                    .map(|r| r.tag.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            retry: false,
                        });
                    }
                    Ok(v.into_iter()
                        .skip(b.len())
                        .take(stmts.len())
                        .map(pg_rows)
                        .collect())
                }
            }
        }
        Dialect::Mysql => interactive(c, ep, |s| {
            let mut out = Vec::new();
            for q in stmts {
                out.push(s.exec(q)?);
            }
            Ok(out)
        }),
        Dialect::Libsql => hrana::batch(&ep.addr, stmts, c.timeout()),
    }
}

/// Runs `op` again while it fails with a retryable error, up to `c.retries` more times.
pub fn with_retries<T>(c: &Conf, mut op: impl FnMut(u32) -> Result<T, Fail>) -> Result<T, Fail> {
    let mut attempt = 0;
    loop {
        match op(attempt) {
            Err(e) if e.retryable() && attempt < c.retries => attempt += 1,
            r => return r,
        }
    }
}

/// The SQL text of an integer.
pub fn int(n: i64) -> String {
    n.to_string()
}

/// A text literal: single quotes doubled (and, for MySQL, backslashes doubled).
pub fn text(c: &Conf, s: &str) -> String {
    let mut t = s.replace('\'', "''");
    if c.dialect == Dialect::Mysql {
        t = t.replace('\\', "\\\\");
    }
    format!("'{t}'")
}

impl Isolation {
    pub fn sql(&self) -> &'static str {
        match self {
            Isolation::Serializable => "SERIALIZABLE",
            Isolation::RepeatableRead => "REPEATABLE READ",
            Isolation::ReadCommitted => "READ COMMITTED",
        }
    }
}
