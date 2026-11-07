//! The workloads and their checks.
//!
//! Every choice comes from the command's `HYDO_SEED` (the composer's stream, so the schedule
//! token `W=` decides it), never from the clock; every operation's process is the command's
//! `HYDO_CMD_ID` (unique per entry in a run). Outcomes go where hydo reads them:
//! `<id> ack|failed|unknown` to `$HYDO_OUTCOMES`, and an acknowledgement to `$HYDO_ACKED`
//! (`<id>` for bank, list-append and register operations; `set-<element>` and `counter-<id>`
//! for the two workloads whose checks read them back, so other workloads' ids do not count).

use crate::conf::{Conf, Dialect, Endpoint, Role};
use crate::db::{self, batch, interactive, query, with_retries, Fail, Rows, Session};
use crate::hist::{self, Op};
use std::io::Write;

pub const ACCOUNTS: &str = "hydo_accounts";
pub const LISTS: &str = "hydo_lists";
pub const REGISTERS: &str = "hydo_registers";
pub const SET: &str = "hydo_set";
pub const COUNTER: &str = "hydo_counter";

/// A seeded stream (splitmix64).
pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

fn env_u64(name: &str) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

fn append_line(var: &str, line: &str) {
    if let Ok(p) = std::env::var(var) {
        if p.is_empty() {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
        {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }
}

/// Records an operation's outcome for hydo; `acked` is the token for `$HYDO_ACKED`.
fn outcome(id: u64, r: &Result<(), Fail>, acked: &str) {
    let what = match r {
        Ok(()) => "ack",
        Err(Fail::Unknown(_)) => "unknown",
        Err(_) => "failed",
    };
    append_line("HYDO_OUTCOMES", &format!("{id} {what}"));
    if r.is_ok() {
        append_line("HYDO_ACKED", acked);
    }
}

fn outcome_word(r: &Result<(), Fail>) -> String {
    match r {
        Ok(()) => "acknowledged".into(),
        Err(f @ Fail::Unknown(_)) => format!("unknown: {}", f.text()),
        Err(f) => format!("failed: {}", f.text()),
    }
}

/// `--flag value` from the arguments.
fn flag<'a>(a: &'a [String], name: &str) -> Option<&'a str> {
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1))
        .map(String::as_str)
}

fn positional(a: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < a.len() {
        if a[i].starts_with("--") {
            i += 2;
            continue;
        }
        out.push(a[i].as_str());
        i += 1;
    }
    out
}

/// One command's context.
pub struct Ctx {
    pub c: Conf,
    pub id: u64,
    pub rng: Rng,
}

impl Ctx {
    fn new(c: Conf, salt: u64) -> Ctx {
        Ctx {
            c,
            id: env_u64("HYDO_CMD_ID"),
            rng: Rng(env_u64("HYDO_SEED") ^ salt),
        }
    }

    /// The endpoint for an operation of `workload`: `--on <name>`, else drawn from its route.
    fn pick(&mut self, workload: &str, on: Option<&str>) -> Result<Endpoint, String> {
        match on {
            Some(n) if n != "seed" => self
                .c
                .endpoint(n)
                .cloned()
                .ok_or_else(|| format!("no endpoint named {n}")),
            _ => {
                let r = self.c.route(workload);
                let i = self.rng.below(r.len() as u64) as usize;
                Ok(r[i].clone())
            }
        }
    }

    /// `--shape batch|interactive|seed` (default seed).
    fn shape(&mut self, s: Option<&str>) -> Result<bool, String> {
        match s.unwrap_or("seed") {
            "batch" => Ok(false),
            "interactive" => Ok(true),
            "seed" => Ok(self.rng.below(2) == 1),
            x => Err(format!("--shape {x}: batch, interactive or seed")),
        }
    }
}

// ------------------------------------------------------------------ SQL per dialect

/// `INSERT ... ` that leaves an existing row alone.
fn insert_ignore(c: &Conf, table: &str, cols: &str, vals: &str) -> String {
    match c.dialect {
        Dialect::Mysql => format!("INSERT IGNORE INTO {table} ({cols}) VALUES ({vals})"),
        _ => format!("INSERT INTO {table} ({cols}) VALUES ({vals}) ON CONFLICT DO NOTHING"),
    }
}

pub fn schema(c: &Conf) -> Vec<String> {
    let mut s = Vec::new();
    let t = |name: &str, cols: &str| format!("CREATE TABLE IF NOT EXISTS {name} ({cols})");
    if c.has("bank") {
        s.push(t(ACCOUNTS, "id BIGINT PRIMARY KEY, bal BIGINT NOT NULL"));
        for i in 0..c.accounts {
            s.push(insert_ignore(
                c,
                ACCOUNTS,
                "id, bal",
                &format!("{i}, {}", c.balance),
            ));
        }
    }
    if c.has("list-append") {
        s.push(t(LISTS, "k BIGINT PRIMARY KEY, v TEXT NOT NULL"));
    }
    if c.has("register") {
        s.push(t(REGISTERS, "k BIGINT PRIMARY KEY, v BIGINT NOT NULL"));
    }
    if c.has("set") {
        s.push(t(SET, "e BIGINT PRIMARY KEY"));
    }
    if c.has("counter") {
        s.push(t(COUNTER, "k BIGINT PRIMARY KEY, n BIGINT NOT NULL"));
        s.push(insert_ignore(c, COUNTER, "k, n", "0, 0"));
    }
    s
}

pub fn append_sql(c: &Conf, k: u64, e: u64) -> String {
    match c.dialect {
        Dialect::Mysql => format!("INSERT INTO {LISTS} (k, v) VALUES ({k}, '{e}') ON DUPLICATE KEY UPDATE v = CONCAT(v, ',', VALUES(v))"),
        _ => format!("INSERT INTO {LISTS} (k, v) VALUES ({k}, '{e}') ON CONFLICT (k) DO UPDATE SET v = {LISTS}.v || ',' || excluded.v"),
    }
}

pub fn read_list_sql(k: u64) -> String {
    format!("SELECT v FROM {LISTS} WHERE k = {k}")
}

/// The batch transfer: one statement that moves `amt` only when `from` has it.
pub fn transfer_sql(c: &Conf, from: i64, to: i64, amt: i64) -> Vec<String> {
    match c.dialect {
        Dialect::Libsql => vec![
            format!("UPDATE {ACCOUNTS} SET bal = bal - {amt} WHERE id = {from} AND bal >= {amt}"),
            format!("UPDATE {ACCOUNTS} SET bal = bal + {amt} WHERE id = {to} AND changes() = 1"),
        ],
        Dialect::Postgres => vec![format!(
            "WITH d AS (UPDATE {ACCOUNTS} SET bal = bal - {amt} WHERE id = {from} AND bal >= {amt} RETURNING id) UPDATE {ACCOUNTS} SET bal = bal + {amt} WHERE id = {to} AND EXISTS (SELECT 1 FROM d)"
        )],
        Dialect::Mysql => vec![format!(
            "UPDATE {ACCOUNTS} a JOIN {ACCOUNTS} b ON a.id = {from} AND b.id = {to} SET a.bal = a.bal - {amt}, b.bal = b.bal + {amt} WHERE a.bal >= {amt}"
        )],
    }
}

fn list_of(r: &Rows) -> Vec<String> {
    match r.rows.first().and_then(|x| x.first()).cloned().flatten() {
        Some(v) if !v.is_empty() => v.split(',').map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

// ------------------------------------------------------------------ init

fn init(c: &Conf, a: &[String], out: &mut dyn Write) -> i32 {
    let wait: u64 = flag(a, "--wait").and_then(|w| w.parse().ok()).unwrap_or(60);
    let p = c.primary();
    if let Err(e) = with_retries(c, |_| db::exec_all(c, p, &schema(c))) {
        let _ = writeln!(out, "init on {}: {}", p.name, e.text());
        return 1;
    }
    let probe = if c.has("bank") {
        format!("SELECT COUNT(*) FROM {ACCOUNTS}")
    } else {
        let t = [
            (LISTS, "list-append"),
            (REGISTERS, "register"),
            (SET, "set"),
            (COUNTER, "counter"),
        ]
        .iter()
        .find(|(_, w)| c.has(w))
        .map(|(t, _)| *t)
        .unwrap_or(ACCOUNTS);
        format!("SELECT COUNT(*) FROM {t}")
    };
    let want = if c.has("bank") { c.accounts } else { 0 };
    for ep in &c.endpoints {
        let mut last = String::new();
        let mut ok = false;
        for _ in 0..=wait {
            match query(c, ep, &probe) {
                Ok(r) if r.int().unwrap_or(-1) >= want => {
                    ok = true;
                    break;
                }
                Ok(r) => last = format!("{} rows", r.int().unwrap_or(-1)),
                Err(e) => last = e.text(),
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if !ok {
            let _ = writeln!(
                out,
                "init: {} did not have the tables within {wait} s: {last}",
                ep.name
            );
            return 1;
        }
    }
    let _ = writeln!(
        out,
        "init: {} ready on {} ({})",
        c.workloads.join(", "),
        c.endpoints
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        c.dialect.name()
    );
    0
}

// ------------------------------------------------------------------ bank

fn transfer(c: Conf, a: &[String], out: &mut dyn Write) -> i32 {
    let mut x = Ctx::new(c, 0x7472616e73666572);
    let ep = match x.pick("bank", flag(a, "--on")) {
        Ok(e) => e,
        Err(e) => return usage_err(out, &e),
    };
    let inter = match x.shape(flag(a, "--shape")) {
        Ok(s) => s,
        Err(e) => return usage_err(out, &e),
    };
    let n = x.c.accounts;
    let from = x.rng.below(n as u64) as i64;
    let to = (from + 1 + x.rng.below(n as u64 - 1) as i64) % n;
    let amt = 1 + x.rng.below((x.c.balance / 2).max(1) as u64 + 10) as i64;
    let c = &x.c;
    let mut what = String::new();
    let r = with_retries(c, |_| {
        if inter {
            interactive(c, &ep, |s: &mut Session| {
                let fb = s
                    .exec(&format!("SELECT bal FROM {ACCOUNTS} WHERE id = {from}"))?
                    .int();
                let tb = s
                    .exec(&format!("SELECT bal FROM {ACCOUNTS} WHERE id = {to}"))?
                    .int();
                let (Some(fb), Some(tb)) = (fb, tb) else {
                    return Err(Fail::Rejected {
                        msg: "no such account".into(),
                        retry: false,
                    });
                };
                if fb < amt {
                    return Ok(format!("balance {fb} < {amt}"));
                }
                s.exec(&format!(
                    "UPDATE {ACCOUNTS} SET bal = {} WHERE id = {from}",
                    fb - amt
                ))?;
                s.exec(&format!(
                    "UPDATE {ACCOUNTS} SET bal = {} WHERE id = {to}",
                    tb + amt
                ))?;
                Ok(format!("{fb} -> {}, {tb} -> {}", fb - amt, tb + amt))
            })
        } else {
            let st = transfer_sql(c, from, to, amt);
            batch(c, &ep, &st).map(|r| {
                let moved = match c.dialect {
                    Dialect::Libsql => r[0].affected == 1,
                    Dialect::Postgres => r[0].affected == 1,
                    Dialect::Mysql => r[0].affected == 2,
                };
                if moved {
                    "moved".into()
                } else {
                    "insufficient balance".into()
                }
            })
        }
    })
    .map(|w| what = w);
    outcome(x.id, &r, &x.id.to_string());
    let _ = writeln!(
        out,
        "{}transfer {} via {}: {from} -> {to} amount {amt}: {}{}",
        if inter { "i" } else { "" },
        x.id,
        ep.name,
        outcome_word(&r),
        if r.is_ok() {
            format!(" ({what})")
        } else {
            String::new()
        }
    );
    i32::from(r.is_err())
}

/// The bank's property on each endpoint: `accounts` rows, the total `accounts x balance`, no
/// negative balance. A replica that has no accounts yet has nothing to judge; a primary that has
/// none lost them.
fn bank_check(c: &Conf, names: &[&str], out: &mut dyn Write) -> i32 {
    if let Some(n) = names.iter().find(|n| c.endpoint(n).is_none()) {
        return usage_err(out, &format!("bank check: no endpoint named {n}"));
    }
    let eps: Vec<&Endpoint> = if names.is_empty() {
        c.endpoints.iter().collect()
    } else {
        names.iter().filter_map(|n| c.endpoint(n)).collect()
    };
    let want = c.accounts * c.balance;
    let mut rc = 0;
    let mut lines = Vec::new();
    for ep in eps {
        match query(
            c,
            ep,
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(bal), 0), COALESCE(MIN(bal), 0) FROM {ACCOUNTS}"
            ),
        ) {
            Ok(r) => {
                let row = r.rows.first().cloned().unwrap_or_default();
                let g = |i: usize| {
                    row.get(i)
                        .cloned()
                        .flatten()
                        .and_then(|v| v.trim().parse::<i64>().ok())
                        .unwrap_or(-1)
                };
                let (count, sum, min) = (g(0), g(1), g(2));
                if count == 0 && ep.role == Role::Replica {
                    lines.push(format!("{}: no accounts yet", ep.name));
                } else if count != c.accounts || sum != want || min < 0 {
                    let _ = writeln!(out, "{}: count {count} sum {sum} min {min}", ep.name);
                    lines.push(format!("{}: the bank's total is {sum} (want {want}) over {count} accounts (want {}), lowest balance {min}", ep.name, c.accounts));
                    rc = 1;
                } else {
                    lines.push(format!(
                        "{}: total {sum} across {count} accounts, lowest balance {min}",
                        ep.name
                    ));
                }
            }
            Err(e) => {
                lines.push(format!(
                    "{}: cannot read the accounts: {}",
                    ep.name,
                    e.text()
                ));
                if rc == 0 {
                    rc = 2;
                }
            }
        }
    }
    let _ = writeln!(out, "{}", lines.join("; "));
    rc
}

// ------------------------------------------------------------------ list-append

/// The histories a list-append operation on `ep` enters: `la` always (everything, at the level
/// the system promises across its nodes), and, when the route has replicas, `lap` (real time:
/// what ran on a primary whole, and only the appends of what a replica served).
fn la_views(c: &Conf, ep: &Endpoint) -> Vec<hist::View> {
    let mut v: Vec<hist::View> = vec![("la".into(), hist::all)];
    if c.reads_replica("list-append") {
        v.push((
            "lap".into(),
            if ep.role == Role::Primary {
                hist::all
            } else {
                hist::appends_only
            },
        ));
    }
    v
}

fn txn(c: Conf, a: &[String], out: &mut dyn Write) -> i32 {
    let mut x = Ctx::new(c, 0x6c697374617070);
    let ep = match x.pick("list-append", flag(a, "--on")) {
        Ok(e) => e,
        Err(e) => return usage_err(out, &e),
    };
    let inter = match x.shape(flag(a, "--shape")) {
        Ok(s) => s,
        Err(e) => return usage_err(out, &e),
    };
    let n = 1 + x.rng.below(4);
    let mut mops = Vec::new();
    for i in 0..n {
        let k = x.rng.below(x.c.keys);
        if x.rng.below(2) == 0 {
            mops.push((true, k, x.id * 10 + i));
        } else {
            mops.push((false, k, 0));
        }
    }
    let inv: Vec<String> = mops
        .iter()
        .map(|(ap, k, e)| {
            if *ap {
                hist::append_mop(*k, *e)
            } else {
                hist::read_mop(*k)
            }
        })
        .collect();
    let c = &x.c;
    let proc_ = x.id.to_string();
    let mut done = Vec::new();
    let mut attempt_op: Option<Op> = None;
    let r = with_retries(c, |_| {
        if let Some(op) = attempt_op.take() {
            op.fail();
        }
        let op = Op::invoke(la_views(c, &ep), &proc_, "txn", &inv);
        let res = if inter {
            interactive(c, &ep, |s: &mut Session| {
                let mut d = Vec::new();
                for (ap, k, e) in &mops {
                    if *ap {
                        s.exec(&append_sql(c, *k, *e))?;
                        d.push(hist::append_mop(*k, *e));
                    } else {
                        let r = s.exec(&read_list_sql(*k))?;
                        d.push(hist::read_result_mop(*k, &list_of(&r)));
                    }
                }
                Ok(d)
            })
        } else {
            let st: Vec<String> = mops
                .iter()
                .map(|(ap, k, e)| {
                    if *ap {
                        append_sql(c, *k, *e)
                    } else {
                        read_list_sql(*k)
                    }
                })
                .collect();
            batch(c, &ep, &st).map(|res| {
                mops.iter()
                    .zip(res.iter())
                    .map(|((ap, k, e), r)| {
                        if *ap {
                            hist::append_mop(*k, *e)
                        } else {
                            hist::read_result_mop(*k, &list_of(r))
                        }
                    })
                    .collect()
            })
        };
        attempt_op = Some(op);
        res
    })
    .map(|d| done = d);
    if let Some(op) = attempt_op {
        match &r {
            Ok(()) => op.ok(&done),
            Err(Fail::Unknown(_)) => op.info(),
            Err(_) => op.fail(),
        }
    }
    outcome(x.id, &r, &x.id.to_string());
    let _ = writeln!(
        out,
        "{}txn {} via {}: {}{}",
        if inter { "i" } else { "" },
        x.id,
        ep.name,
        outcome_word(&r),
        if r.is_ok() {
            format!(": {}", done.join(" "))
        } else {
            String::new()
        }
    );
    i32::from(r.is_err())
}

/// The final read-back: every list, in one transaction, recorded as process `process`.
fn la_read_all(c: &Conf, ep: &Endpoint, process: &str, out: &mut dyn Write) -> i32 {
    let inv: Vec<String> = (0..c.keys).map(hist::read_mop).collect();
    let op = Op::invoke(la_views(c, ep), process, "txn", &inv);
    let st: Vec<String> = (0..c.keys).map(read_list_sql).collect();
    match batch(c, ep, &st) {
        Ok(res) => {
            let d: Vec<String> = res
                .iter()
                .enumerate()
                .map(|(k, r)| hist::read_result_mop(k as u64, &list_of(r)))
                .collect();
            let _ = writeln!(out, "read-all via {}: {}", ep.name, d.join(" "));
            op.ok(&d);
            0
        }
        Err(e) => {
            op.fail();
            let _ = writeln!(
                out,
                "read-all via {}: could not read: {}",
                ep.name,
                e.text()
            );
            2
        }
    }
}

// ------------------------------------------------------------------ register

/// Registers' history `rg`: every operation, except reads a replica served (they may be stale;
/// linearizability claims real time).
fn register_op(c: Conf, a: &[String], out: &mut dyn Write) -> i32 {
    let mut x = Ctx::new(c, 0x7265676973746572);
    let ep = match x.pick("register", flag(a, "--on")) {
        Ok(e) => e,
        Err(e) => return usage_err(out, &e),
    };
    let k = x.rng.below(x.c.keys);
    let kind = x.rng.below(3);
    let v = x.rng.below(5);
    let mut old = x.rng.below(5);
    if old == v {
        old = (v + 1) % 5;
    }
    let c = &x.c;
    let proc_ = x.id.to_string();
    let rec_read = ep.role == Role::Primary;
    let (f, inv) = match kind {
        0 => ("read", vec![k.to_string()]),
        1 => ("write", vec![k.to_string(), v.to_string()]),
        _ => ("cas", vec![k.to_string(), old.to_string(), v.to_string()]),
    };
    let views: Vec<hist::View> = if f == "read" && !rec_read {
        vec![]
    } else {
        vec![("rg".into(), hist::all)]
    };
    let mut got = String::new();
    let op = Op::invoke(views, &proc_, f, &inv);
    let r = with_retries(c, |_| match kind {
        0 => query(c, &ep, &format!("SELECT v FROM {REGISTERS} WHERE k = {k}")).map(|r| {
            got = r
                .rows
                .first()
                .and_then(|x| x.first())
                .cloned()
                .flatten()
                .unwrap_or_else(|| "nil".into());
        }),
        1 => {
            let sql = match c.dialect {
                Dialect::Mysql => format!("INSERT INTO {REGISTERS} (k, v) VALUES ({k}, {v}) ON DUPLICATE KEY UPDATE v = VALUES(v)"),
                _ => format!("INSERT INTO {REGISTERS} (k, v) VALUES ({k}, {v}) ON CONFLICT (k) DO UPDATE SET v = excluded.v"),
            };
            query(c, &ep, &sql).map(|_| ())
        }
        _ => query(
            c,
            &ep,
            &format!("UPDATE {REGISTERS} SET v = {v} WHERE k = {k} AND v = {old}"),
        )
        .and_then(|r| {
            if r.affected == 1 {
                Ok(())
            } else {
                Err(Fail::Rejected {
                    msg: format!("cas {k} {old} -> {v}: no row had {old}"),
                    retry: false,
                })
            }
        }),
    });
    match &r {
        Ok(()) if f == "read" => op.ok(&[k.to_string(), got.clone()]),
        Ok(()) => op.ok(&inv),
        Err(Fail::Unknown(_)) => op.info(),
        Err(_) => op.fail(),
    }
    outcome(x.id, &r, &x.id.to_string());
    let _ = writeln!(
        out,
        "register {} via {}: {f} {}: {}{}",
        x.id,
        ep.name,
        inv.join(" "),
        outcome_word(&r),
        if f == "read" && r.is_ok() {
            format!(" = {got}")
        } else {
            String::new()
        }
    );
    i32::from(r.is_err())
}

fn register_read_all(c: &Conf, ep: &Endpoint, process: &str, out: &mut dyn Write) -> i32 {
    let inv: Vec<String> = (0..c.keys).map(|k| k.to_string()).collect();
    let op = Op::invoke(vec![("rg".into(), hist::all)], process, "read", &inv);
    match query(c, ep, &format!("SELECT k, v FROM {REGISTERS}")) {
        Ok(r) => {
            let mut d = Vec::new();
            for k in 0..c.keys {
                let v = r
                    .rows
                    .iter()
                    .find(|row| row.first().cloned().flatten().as_deref() == Some(&k.to_string()))
                    .and_then(|row| row.get(1).cloned().flatten());
                d.push(k.to_string());
                d.push(v.unwrap_or_else(|| "nil".into()));
            }
            let _ = writeln!(out, "register read-all via {}: {}", ep.name, d.join(" "));
            op.ok(&d);
            0
        }
        Err(e) => {
            op.fail();
            let _ = writeln!(
                out,
                "register read-all via {}: could not read: {}",
                ep.name,
                e.text()
            );
            2
        }
    }
}

// ------------------------------------------------------------------ set and counter

fn set_add(c: Conf, a: &[String], out: &mut dyn Write) -> i32 {
    let mut x = Ctx::new(c, 0x736574);
    let ep = match x.pick("set", flag(a, "--on")) {
        Ok(e) => e,
        Err(e) => return usage_err(out, &e),
    };
    let e = x.id;
    let r = with_retries(&x.c, |_| {
        query(&x.c, &ep, &format!("INSERT INTO {SET} (e) VALUES ({e})")).map(|_| ())
    });
    outcome(x.id, &r, &format!("set-{e}"));
    let _ = writeln!(
        out,
        "set {} via {}: add {e}: {}",
        x.id,
        ep.name,
        outcome_word(&r)
    );
    i32::from(r.is_err())
}

fn acked_with(prefix: &str) -> Option<Vec<String>> {
    let p = std::env::var("HYDO_ACKED").ok().filter(|p| !p.is_empty())?;
    let t = std::fs::read_to_string(p).unwrap_or_default();
    let mut v: Vec<String> = t
        .lines()
        .filter_map(|l| l.trim().strip_prefix(prefix).map(str::to_string))
        .collect();
    v.sort();
    v.dedup();
    Some(v)
}

fn set_check(c: &Conf, ep: &Endpoint, out: &mut dyn Write) -> i32 {
    let Some(acked) = acked_with("set-") else {
        let _ = writeln!(out, "set check: $HYDO_ACKED is not set");
        return 2;
    };
    match query(c, ep, &format!("SELECT e FROM {SET}")) {
        Ok(r) => {
            let have: std::collections::HashSet<String> =
                r.column(0).into_iter().flatten().collect();
            let lost: Vec<&String> = acked.iter().filter(|e| !have.contains(*e)).collect();
            if lost.is_empty() {
                let _ = writeln!(
                    out,
                    "{}: all {} acknowledged elements present ({} rows)",
                    ep.name,
                    acked.len(),
                    have.len()
                );
                0
            } else {
                let show: Vec<&str> = lost.iter().take(5).map(|s| s.as_str()).collect();
                let _ = writeln!(out, "{}: lost {}", ep.name, show.join(" "));
                let _ = writeln!(
                    out,
                    "{}: {} of {} acknowledged elements missing",
                    ep.name,
                    lost.len(),
                    acked.len()
                );
                1
            }
        }
        Err(e) => {
            let _ = writeln!(out, "{}: cannot read the set: {}", ep.name, e.text());
            2
        }
    }
}

fn counter_add(c: Conf, a: &[String], out: &mut dyn Write) -> i32 {
    let mut x = Ctx::new(c, 0x636f756e746572);
    let ep = match x.pick("counter", flag(a, "--on")) {
        Ok(e) => e,
        Err(e) => return usage_err(out, &e),
    };
    let r = with_retries(&x.c, |_| {
        query(
            &x.c,
            &ep,
            &format!("UPDATE {COUNTER} SET n = n + 1 WHERE k = 0"),
        )
        .map(|_| ())
    });
    outcome(x.id, &r, &format!("counter-{}", x.id));
    let _ = writeln!(
        out,
        "counter {} via {}: add 1: {}",
        x.id,
        ep.name,
        outcome_word(&r)
    );
    i32::from(r.is_err())
}

fn counter_check(c: &Conf, ep: &Endpoint, out: &mut dyn Write) -> i32 {
    let Some(acked) = acked_with("counter-") else {
        let _ = writeln!(out, "counter check: $HYDO_ACKED is not set");
        return 2;
    };
    match query(c, ep, &format!("SELECT n FROM {COUNTER} WHERE k = 0")).map(|r| r.int()) {
        Ok(Some(n)) if n >= acked.len() as i64 => {
            let _ = writeln!(
                out,
                "{}: counter {n}, {} acknowledged increments",
                ep.name,
                acked.len()
            );
            0
        }
        Ok(Some(n)) => {
            let _ = writeln!(
                out,
                "{}: counter {n} < {} acknowledged increments: an acknowledged increment was lost",
                ep.name,
                acked.len()
            );
            1
        }
        Ok(None) => {
            let _ = writeln!(out, "{}: the counter's row is gone", ep.name);
            1
        }
        Err(e) => {
            let _ = writeln!(out, "{}: cannot read the counter: {}", ep.name, e.text());
            2
        }
    }
}

// ------------------------------------------------------------------ none, dump

/// The SQL of a `--file` (a `.sql` check): its text, trailing whitespace and `;` removed.
pub fn sql_of_file(t: &str) -> String {
    t.trim_end().trim_end_matches(';').trim().to_string()
}

/// `none`: the query must return no rows, on every endpoint named (a comma list).
fn none(c: &Conf, a: &[String], out: &mut dyn Write) -> i32 {
    let pos = positional(a);
    let (names, sql) = match (flag(a, "--file"), flag(a, "--on")) {
        (Some(f), on) => {
            let t = match std::fs::read_to_string(f) {
                Ok(t) => t,
                Err(e) => {
                    let _ = writeln!(out, "none: {f}: {e}");
                    return 2;
                }
            };
            (
                on.map(str::to_string)
                    .or_else(|| pos.first().map(|s| s.to_string()))
                    .unwrap_or_else(|| c.primary().name.clone()),
                sql_of_file(&t),
            )
        }
        (None, _) if pos.len() == 2 => (pos[0].to_string(), pos[1].to_string()),
        _ => {
            return usage_err(
                out,
                "none <endpoint>[,<endpoint>...] '<query>' | none --file F [--on <endpoint>]",
            )
        }
    };
    let mut rc = 0;
    let mut lines = Vec::new();
    for n in names.split(',') {
        let Some(ep) = c.endpoint(n) else {
            return usage_err(out, &format!("no endpoint named {n}"));
        };
        match query(c, ep, &sql) {
            Ok(r) if r.rows.is_empty() => lines.push(format!("{n}: no rows")),
            Ok(r) => {
                for row in r.rows.iter().take(5) {
                    let cells: Vec<String> = r
                        .cols
                        .iter()
                        .zip(row.iter())
                        .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("NULL")))
                        .collect();
                    let _ = writeln!(out, "{n}: {}", cells.join(" "));
                }
                lines.push(format!("{n}: {} rows where none may be", r.rows.len()));
                rc = 1;
            }
            Err(e) => {
                lines.push(format!("{n}: cannot run the query: {}", e.text()));
                if rc == 0 {
                    rc = 2;
                }
            }
        }
    }
    let msg = flag(a, "--msg")
        .map(|m| format!("{m}: "))
        .unwrap_or_default();
    let _ = writeln!(out, "{msg}{}", lines.join("; "));
    rc
}

/// The workloads' rows as items (`<table> <key> <value>`), sorted: for differential checks.
pub fn dump_rows(c: &Conf, ep: &Endpoint) -> Result<Vec<String>, Fail> {
    let mut q = Vec::new();
    if c.has("bank") {
        q.push((ACCOUNTS, format!("SELECT id, bal FROM {ACCOUNTS}")));
    }
    if c.has("list-append") {
        q.push((LISTS, format!("SELECT k, v FROM {LISTS}")));
    }
    if c.has("register") {
        q.push((REGISTERS, format!("SELECT k, v FROM {REGISTERS}")));
    }
    if c.has("set") {
        q.push((SET, format!("SELECT e, e FROM {SET}")));
    }
    if c.has("counter") {
        q.push((COUNTER, format!("SELECT k, n FROM {COUNTER}")));
    }
    let st: Vec<String> = q.iter().map(|(_, s)| s.clone()).collect();
    let res = batch(c, ep, &st)?;
    let mut items = Vec::new();
    for ((t, _), r) in q.iter().zip(res.iter()) {
        for row in &r.rows {
            let g = |i: usize| {
                row.get(i)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| "NULL".into())
            };
            items.push(format!("{t} {} {}", g(0), g(1)));
        }
    }
    items.sort();
    Ok(items)
}

fn usage_err(out: &mut dyn Write, m: &str) -> i32 {
    let _ = writeln!(out, "hydo-sql: {m}");
    2
}

/// The workload commands.
pub fn main(a: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let c = match Conf::load() {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(out, "hydo-sql: the configuration: {e}");
            return 2;
        }
    };
    let w = a[0].as_str();
    let sub = a.get(1).map(String::as_str).unwrap_or("");
    let rest = if a.len() > 2 { &a[2..] } else { &[] };
    let ep_arg = |i: usize| -> Option<Endpoint> {
        match rest.get(i) {
            Some(n) => c.endpoint(n).cloned(),
            None => Some(c.primary().clone()),
        }
    };
    if w != "config"
        && w != "init"
        && w != "none"
        && w != "dump"
        && !c.has(w)
        && crate::conf::WORKLOADS.contains(&w)
    {
        return usage_err(
            out,
            &format!("the configuration has no workload {w} (its `workloads` line)"),
        );
    }
    match (w, sub) {
        ("config", _) => {
            let _ = write!(out, "{}", c.render());
            0
        }
        ("init", _) => init(&c, &a[1..], out),
        ("bank", "transfer") => transfer(c, rest, out),
        ("bank", "check") => {
            let names: Vec<&str> = rest.iter().map(String::as_str).collect();
            bank_check(&c, &names, out)
        }
        ("list-append", "txn") => txn(c, rest, out),
        ("list-append", "read-all") => match ep_arg(0) {
            Some(ep) => la_read_all(
                &c,
                &ep,
                rest.get(1).map(String::as_str).unwrap_or("final"),
                out,
            ),
            None => usage_err(out, "list-append read-all <endpoint> [process]"),
        },
        ("register", "op") => register_op(c, rest, out),
        ("register", "read-all") => match ep_arg(0) {
            Some(ep) => register_read_all(
                &c,
                &ep,
                rest.get(1).map(String::as_str).unwrap_or("final"),
                out,
            ),
            None => usage_err(out, "register read-all <endpoint> [process]"),
        },
        ("set", "add") => set_add(c, rest, out),
        ("set", "check") => match ep_arg(0) {
            Some(ep) => set_check(&c, &ep, out),
            None => usage_err(out, "set check [<endpoint>]"),
        },
        ("counter", "add") => counter_add(c, rest, out),
        ("counter", "check") => match ep_arg(0) {
            Some(ep) => counter_check(&c, &ep, out),
            None => usage_err(out, "counter check [<endpoint>]"),
        },
        ("none", _) => none(&c, &a[1..], out),
        ("dump", _) => match c.endpoint(sub) {
            Some(ep) => match dump_rows(&c, ep) {
                Ok(items) => {
                    for i in items {
                        let _ = writeln!(out, "{i}");
                    }
                    0
                }
                Err(e) => {
                    let _ = writeln!(out, "dump {sub}: {}", e.text());
                    2
                }
            },
            None => usage_err(out, "dump <endpoint>"),
        },
        _ => {
            let _ = write!(err, "{}", crate::USAGE);
            2
        }
    }
}
