//! The configuration: one small line-based file that `hydo sql-gen` writes into the project's
//! `hydo/workload/` folder (so it lands at `/hydo/workload/helper_sql.conf`; `$HYDO_SQL_CONF`
//! names another file). Lines, `#` starts a comment:
//!
//! ```text
//! dialect libsql                          postgres | mysql | libsql
//! endpoint primary 10.89.0.11:8080        <name> <address> [primary|replica]
//! endpoint replica 10.89.0.12:8080 replica
//! isolation serializable                  serializable | repeatable-read | read-committed
//! workloads bank list-append              bank list-append register set counter
//! route * primary replica                 <workload|*> <endpoint>...: where operations go
//! accounts 8                              the bank's accounts, each starting at `balance`
//! balance 100
//! keys 6                                  list-append and register keys
//! timeout_ms 3000                         one request's deadline
//! retries 2                               retries of a retryable definite failure
//! ```
//!
//! An address is `host:port` for libsql (sqld's HTTP API) and a DSN for the others, as
//! hydo-read takes it (`postgres://user:pw@10.89.0.2:5432/db?sslmode=disable`,
//! `user:pw@tcp(10.89.0.3:3306)/db`). A `primary` endpoint's reads are current (real time is
//! claimed for them); a `replica`'s reads may be stale, so its reads enter only the histories
//! that claim no real-time order. Writes may go to either (a replica that forwards writes, as
//! sqld's does, executes them on the primary within the request).

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Mysql,
    Libsql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Primary,
    Replica,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    Serializable,
    RepeatableRead,
    ReadCommitted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    pub name: String,
    pub addr: String,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conf {
    pub dialect: Dialect,
    pub endpoints: Vec<Endpoint>,
    pub isolation: Isolation,
    pub workloads: Vec<String>,
    /// (workload or "*", endpoint names)
    pub routes: Vec<(String, Vec<String>)>,
    pub accounts: i64,
    pub balance: i64,
    pub keys: u64,
    pub timeout_ms: u64,
    pub retries: u32,
}

pub const WORKLOADS: [&str; 5] = ["bank", "list-append", "register", "set", "counter"];

impl Dialect {
    pub fn parse(s: &str) -> Option<Dialect> {
        match s {
            "postgres" | "postgresql" => Some(Dialect::Postgres),
            "mysql" => Some(Dialect::Mysql),
            "libsql" | "sqlite" => Some(Dialect::Libsql),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Dialect::Postgres => "postgres",
            Dialect::Mysql => "mysql",
            Dialect::Libsql => "libsql",
        }
    }
}

impl Isolation {
    pub fn parse(s: &str) -> Option<Isolation> {
        match s {
            "serializable" => Some(Isolation::Serializable),
            "repeatable-read" => Some(Isolation::RepeatableRead),
            "read-committed" => Some(Isolation::ReadCommitted),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Isolation::Serializable => "serializable",
            Isolation::RepeatableRead => "repeatable-read",
            Isolation::ReadCommitted => "read-committed",
        }
    }
}

impl Conf {
    pub fn parse(text: &str) -> Result<Conf, String> {
        let mut c = Conf {
            dialect: Dialect::Libsql,
            endpoints: Vec::new(),
            isolation: Isolation::Serializable,
            workloads: Vec::new(),
            routes: Vec::new(),
            accounts: 8,
            balance: 100,
            keys: 6,
            timeout_ms: 3000,
            retries: 2,
        };
        let mut dialect = None;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let w: Vec<&str> = line.split_whitespace().collect();
            let bad = |m: &str| format!("line {}: {m}: {raw}", i + 1);
            let num = |s: Option<&&str>| -> Result<i64, String> {
                s.and_then(|v| v.parse::<i64>().ok())
                    .filter(|n| *n >= 0)
                    .ok_or_else(|| bad("want a number >= 0"))
            };
            match w[0] {
                "dialect" => {
                    dialect = Some(
                        w.get(1)
                            .and_then(|d| Dialect::parse(d))
                            .ok_or_else(|| bad("dialect postgres|mysql|libsql"))?,
                    )
                }
                "endpoint" => {
                    if w.len() < 3 || w.len() > 4 {
                        return Err(bad("endpoint <name> <address> [primary|replica]"));
                    }
                    let role = match w.get(3).copied().unwrap_or("primary") {
                        "primary" => Role::Primary,
                        "replica" => Role::Replica,
                        _ => return Err(bad("the role is primary or replica")),
                    };
                    if c.endpoints.iter().any(|e| e.name == w[1]) {
                        return Err(bad("endpoint named twice"));
                    }
                    c.endpoints.push(Endpoint {
                        name: w[1].into(),
                        addr: w[2].into(),
                        role,
                    });
                }
                "isolation" => {
                    c.isolation = w.get(1).and_then(|d| Isolation::parse(d)).ok_or_else(|| {
                        bad("isolation serializable|repeatable-read|read-committed")
                    })?
                }
                "workloads" => {
                    for x in &w[1..] {
                        if !WORKLOADS.contains(x) {
                            return Err(bad(&format!(
                                "unknown workload {x} (known: {})",
                                WORKLOADS.join(" ")
                            )));
                        }
                        c.workloads.push(x.to_string());
                    }
                }
                "route" => {
                    if w.len() < 3 {
                        return Err(bad("route <workload|*> <endpoint>..."));
                    }
                    c.routes
                        .push((w[1].into(), w[2..].iter().map(|s| s.to_string()).collect()));
                }
                "accounts" => c.accounts = num(w.get(1))?.max(2),
                "balance" => c.balance = num(w.get(1))?,
                "keys" => c.keys = num(w.get(1))?.max(1) as u64,
                "timeout_ms" => c.timeout_ms = num(w.get(1))?.clamp(100, 60_000) as u64,
                "retries" => c.retries = num(w.get(1))?.min(10) as u32,
                _ => return Err(bad("unknown setting")),
            }
        }
        c.dialect = dialect.ok_or("no dialect line")?;
        if c.endpoints.is_empty() {
            return Err("no endpoint line".into());
        }
        if !c.endpoints.iter().any(|e| e.role == Role::Primary) {
            return Err("no primary endpoint".into());
        }
        for (w, eps) in &c.routes {
            if w != "*" && !WORKLOADS.contains(&w.as_str()) {
                return Err(format!("route {w}: unknown workload"));
            }
            for e in eps {
                if c.endpoint(e).is_none() {
                    return Err(format!("route {w}: no endpoint named {e}"));
                }
            }
        }
        Ok(c)
    }

    /// `$HYDO_SQL_CONF`, else `/hydo/workload/helper_sql.conf`.
    pub fn load() -> Result<Conf, String> {
        let p = std::env::var("HYDO_SQL_CONF")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "/hydo/workload/helper_sql.conf".into());
        let t = std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?;
        Conf::parse(&t).map_err(|e| format!("{p}: {e}"))
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub fn endpoint(&self, name: &str) -> Option<&Endpoint> {
        self.endpoints.iter().find(|e| e.name == name)
    }

    /// The first primary: where `init` creates the tables and the final read-backs read.
    pub fn primary(&self) -> &Endpoint {
        self.endpoints
            .iter()
            .find(|e| e.role == Role::Primary)
            .expect("a primary")
    }

    /// The endpoints operations of `workload` go to (its route, else `*`'s, else every one).
    pub fn route(&self, workload: &str) -> Vec<&Endpoint> {
        let names = self
            .routes
            .iter()
            .find(|(w, _)| w == workload)
            .or_else(|| self.routes.iter().find(|(w, _)| w == "*"))
            .map(|(_, e)| e.clone());
        match names {
            Some(n) => n.iter().filter_map(|x| self.endpoint(x)).collect(),
            None => self.endpoints.iter().collect(),
        }
    }

    /// Whether some operation of `workload` may read a replica: its history then has a second,
    /// real-time projection (`<history>p`).
    pub fn reads_replica(&self, workload: &str) -> bool {
        self.route(workload).iter().any(|e| e.role == Role::Replica)
    }

    pub fn has(&self, workload: &str) -> bool {
        self.workloads.iter().any(|w| w == workload)
    }

    /// The text `parse` reads back to this configuration.
    pub fn render(&self) -> String {
        let mut o = format!("dialect {}\n", self.dialect.name());
        for e in &self.endpoints {
            o.push_str(&format!(
                "endpoint {} {} {}\n",
                e.name,
                e.addr,
                if e.role == Role::Primary {
                    "primary"
                } else {
                    "replica"
                }
            ));
        }
        o.push_str(&format!("isolation {}\n", self.isolation.name()));
        o.push_str(&format!("workloads {}\n", self.workloads.join(" ")));
        for (w, e) in &self.routes {
            o.push_str(&format!("route {w} {}\n", e.join(" ")));
        }
        o.push_str(&format!(
            "accounts {}\nbalance {}\nkeys {}\ntimeout_ms {}\nretries {}\n",
            self.accounts, self.balance, self.keys, self.timeout_ms, self.retries
        ));
        o
    }
}
