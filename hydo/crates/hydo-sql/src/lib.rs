//! hydo-sql: ready-made SQL workloads and checks for hydo projects (docs/design/workloads.md;
//! hydo/onboard/MANIFEST.md, "SQL systems: ready-made workloads and checks").
//!
//! One static guest program. `hydo sql-gen` writes a project's test-composer commands and checks
//! that call it; it reads the endpoints and workloads from one small file
//! (`/hydo/workload/helper_sql.conf`, written by the generator; `$HYDO_SQL_CONF` overrides).
//!
//! - **Workloads**, every choice from the command's `HYDO_SEED`: `bank` (transfers that keep the
//!   total), `list-append` (Elle-style transactions recorded for hydo's list-append checker),
//!   `register` (read, write and compare-and-set recorded for the linearizability checker),
//!   `set` (unique inserts) and `counter` (increments); transactions in both shapes (one request,
//!   and interactive: a statement per round trip with the client deciding between them); each
//!   operation routed to one of the endpoints the configuration lists.
//! - **Outcomes**, written once per protocol (src/db.rs): a request that never reached the
//!   server and an error the server answered before or at COMMIT are definite failures (busy,
//!   deadlock and serialization errors are retried, as a careful client does); a missing answer
//!   to COMMIT (or to a one-request transaction) is unknown.
//! - **Dialects**: PostgreSQL and MySQL (hydo-read's protocol code, with a session API added
//!   here) and libSQL / SQLite through sqld's HTTP API (Hrana over HTTP).
//! - **Checks**: the workloads' properties, `none` (a query that must return no rows), and
//!   `read` / `check`: hydo-read's stores and hydo-check's checks with three more sources (`sql`
//!   an endpoint of the configuration, `libsql` a server's HTTP API, `world` the fake outside
//!   world's record).

pub use hydo_read::{
    base64_decode, base64_encode, cannot, encode_all, gofmt, json, lossy, net, q, rec, secs,
    split_read_opts, url, usage, Error, ReadOpts, Record, Res, EXIT_CANNOT, EXIT_OK, EXIT_VIOLATED,
};

/// hydo-read's PostgreSQL module, compiled here, plus the session API (`pg_session.rs`).
#[allow(dead_code, clippy::all)]
pub mod pg {
    include!(concat!(env!("OUT_DIR"), "/pg.rs"));
    include!("pg_session.rs");
}

/// hydo-read's MySQL module, compiled here, plus the session API (`mysql_session.rs`).
#[allow(dead_code, clippy::all)]
pub mod mysql {
    include!(concat!(env!("OUT_DIR"), "/mysql.rs"));
    include!("mysql_session.rs");
}

/// hydo-check, compiled here so that its sources read through this crate's `read_store`.
#[allow(dead_code, clippy::all)]
pub mod check {
    include!(concat!(env!("OUT_DIR"), "/check.rs"));
}

pub mod conf;
pub mod db;
pub mod hist;
pub mod hrana;
pub mod read;
pub mod work;
pub mod world;

pub use read::read_store;

use std::io::Write;

pub const USAGE: &str = r#"hydo-sql: ready-made SQL workloads and checks (hydo/onboard/MANIFEST.md, "SQL systems")
workloads (composer commands; exit 0 acknowledged, 1 failed or unknown, 100 a violation seen):
  hydo-sql init [--wait S]                   the workloads' tables and first rows, on the first
                                             primary; then wait until every endpoint has them
  hydo-sql bank transfer [--shape batch|interactive|seed] [--on <endpoint>|seed]
  hydo-sql list-append txn [--shape ...] [--on ...]
  hydo-sql register op [--on ...]           a read, write or compare-and-set (the seed chooses)
  hydo-sql set add [--on ...]                insert one unique element
  hydo-sql counter add [--on ...]            add one to a counter
checks (exit 0 holds, 1 violated, 2 could not evaluate):
  hydo-sql bank check [<endpoint>...]        the total never changes, no balance is negative
  hydo-sql list-append read-all <endpoint> [process]   the final read-back of the histories
  hydo-sql register read-all <endpoint> [process]
  hydo-sql set check [<endpoint>]            every acknowledged element is there
  hydo-sql counter check [<endpoint>]        no acknowledged increment is lost
  hydo-sql none <endpoint> '<query>' | --file F [--msg M]   the query returns no rows
  hydo-sql dump <endpoint>                   the workloads' rows, one item per row
readers:
  hydo-sql read <store> <args...>            hydo-read's stores, and
      sql <endpoint> '<query>'               a query on an endpoint of the configuration
      libsql <host:port> '<query>'           a query through sqld's HTTP API
      world <route|*> [--all] [--log PATH]   the fake outside world's record (effects only, or
                                             --all requests), one record per request
  hydo-sql check <check> <sources> ...       hydo-check with those sources too
  hydo-sql config                            the configuration as read
"#;

/// The program.
pub fn main_with(args: &[Vec<u8>], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let a: Vec<String> = args.iter().map(|x| lossy(x)).collect();
    let first = a.first().map(String::as_str).unwrap_or("");
    match first {
        "read" => read::read_main(&args[1..], out, err),
        "check" => check::check_main(&args[1..], out, err),
        "" | "-h" | "--help" | "help" => {
            let _ = write!(err, "{USAGE}");
            if first.is_empty() {
                2
            } else {
                0
            }
        }
        _ => work::main(&a, out, err),
    }
}
