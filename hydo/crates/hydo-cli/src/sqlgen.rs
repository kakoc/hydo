//! `hydo sql-gen`: ready-made SQL workloads and checks for a project's `hydo/` folder
//! (docs/design/workloads.md; MANIFEST.md, "SQL systems: ready-made workloads and checks").
//!
//! It writes test-composer commands and checks that call the guest program `hydo-sql` (staged
//! into the image by `hydo build` because the commands name it), and the one file that program
//! reads, `workload/helper_sql.conf`: the dialect, the endpoints with their roles, the workloads
//! and where their operations go. From the routes it derives what the histories may claim: a
//! replica's reads may be stale, so with replicas a list-append history over every transaction
//! claims `serializable`, and a second one (`lap`: what ran on a primary, plus only the appends
//! of what a replica served) claims real time, which is the one that sees a lost write.

use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: hydo sql-gen --dialect postgres|mysql|libsql --endpoint NAME=ADDRESS[,replica]...
                    [--workloads bank,list-append,register,set,counter | none] [--shapes batch,interactive]
                    [--route NAME,...] [--isolation serializable|repeatable-read|read-committed]
                    [--accounts N] [--balance N] [--keys N] [--timeout-ms N] [--retries N]
                    [--out DIR] [--force]
Writes ready-made SQL workloads and checks into a project's hydo/ folder (DIR, default ./hydo):
workload/helper_sql.conf (what hydo-sql reads), workload/first_sql_init, one parallel_driver per
workload and transaction shape, and the checks each workload has (bank totals, history checks,
set and counter read-backs, replica convergence, a .sql check); --workloads none writes only the
configuration, for SQL checks and hydo-sql check sources of a project's own tables. The address of an endpoint is
host:port for libsql (sqld's HTTP API) and a DSN for postgres and mysql, as the guest reaches it
(postgres://user:pw@10.89.0.2:5432/db?sslmode=disable, user:pw@tcp(10.89.0.3:3306)/db).
Existing files are kept unless --force. MANIFEST.md, \"SQL systems: ready-made workloads and checks\".";

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub dialect: String,
    /// (name, address, replica)
    pub endpoints: Vec<(String, String, bool)>,
    pub workloads: Vec<String>,
    pub batch: bool,
    pub interactive: bool,
    pub route: Vec<String>,
    pub isolation: String,
    pub accounts: i64,
    pub balance: i64,
    pub keys: i64,
    pub timeout_ms: i64,
    pub retries: i64,
}

const WORKLOADS: [&str; 5] = ["bank", "list-append", "register", "set", "counter"];

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// The command line; returns the spec, the output folder and --force.
pub fn parse(argv: &[String]) -> Result<(Spec, PathBuf, bool), String> {
    let mut s = Spec {
        dialect: String::new(),
        endpoints: Vec::new(),
        workloads: vec!["bank".into(), "list-append".into()],
        batch: true,
        interactive: true,
        route: Vec::new(),
        isolation: "serializable".into(),
        accounts: 8,
        balance: 100,
        keys: 6,
        timeout_ms: 3000,
        retries: 2,
    };
    let mut out = PathBuf::from("hydo");
    let mut force = false;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "--force" {
            force = true;
            i += 1;
            continue;
        }
        let v = argv
            .get(i + 1)
            .cloned()
            .ok_or_else(|| format!("{a} needs a value"))?;
        let num = |v: &str| {
            v.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or_else(|| format!("{a} {v}: a number >= 0"))
        };
        match a {
            "--dialect" => {
                if !["postgres", "mysql", "libsql"].contains(&v.as_str()) {
                    return Err(format!("--dialect {v}: postgres, mysql or libsql"));
                }
                s.dialect = v;
            }
            "--endpoint" => {
                let (name, rest) = v
                    .split_once('=')
                    .ok_or_else(|| format!("--endpoint {v}: NAME=ADDRESS[,replica]"))?;
                let (addr, replica) = match rest.rsplit_once(',') {
                    Some((ad, "replica")) => (ad.to_string(), true),
                    Some((ad, "primary")) => (ad.to_string(), false),
                    _ => (rest.to_string(), false),
                };
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(format!(
                        "--endpoint {v}: the name may use letters, digits, '_' and '-'"
                    ));
                }
                if addr.is_empty() || addr.contains(char::is_whitespace) || addr.contains('#') {
                    return Err(format!(
                        "--endpoint {v}: the address may not be empty or hold spaces or '#'"
                    ));
                }
                if s.endpoints.iter().any(|e| e.0 == name) {
                    return Err(format!("--endpoint {name}: named twice"));
                }
                s.endpoints.push((name.into(), addr, replica));
            }
            "--workloads" => {
                s.workloads = if v == "none" { Vec::new() } else { list(&v) };
                for w in &s.workloads {
                    if !WORKLOADS.contains(&w.as_str()) {
                        return Err(format!(
                            "--workloads: unknown workload {w} (known: {})",
                            WORKLOADS.join(", ")
                        ));
                    }
                }
            }
            "--shapes" => {
                let l = list(&v);
                s.batch = l.iter().any(|x| x == "batch");
                s.interactive = l.iter().any(|x| x == "interactive");
                if l.iter().any(|x| x != "batch" && x != "interactive")
                    || !(s.batch || s.interactive)
                {
                    return Err(format!("--shapes {v}: batch, interactive or both"));
                }
            }
            "--route" => s.route = list(&v),
            "--isolation" => {
                if !["serializable", "repeatable-read", "read-committed"].contains(&v.as_str()) {
                    return Err(format!(
                        "--isolation {v}: serializable, repeatable-read or read-committed"
                    ));
                }
                s.isolation = v;
            }
            "--accounts" => s.accounts = num(&v)?.max(2),
            "--balance" => s.balance = num(&v)?,
            "--keys" => s.keys = num(&v)?.max(1),
            "--timeout-ms" => s.timeout_ms = num(&v)?,
            "--retries" => s.retries = num(&v)?,
            "--out" => out = PathBuf::from(v),
            _ => return Err(format!("unknown option {a}")),
        }
        i += 2;
    }
    if s.dialect.is_empty() {
        return Err("--dialect is required".into());
    }
    if s.endpoints.is_empty() {
        return Err("at least one --endpoint".into());
    }
    if !s.endpoints.iter().any(|e| !e.2) {
        return Err("at least one endpoint must be a primary (no ,replica)".into());
    }
    for r in &s.route {
        if !s.endpoints.iter().any(|e| &e.0 == r) {
            return Err(format!("--route {r}: no such endpoint"));
        }
    }
    if s.isolation == "read-committed" && s.has("list-append") {
        return Err("list-append claims at least snapshot isolation: with --isolation read-committed leave it out of --workloads (bank still shows lost updates)".into());
    }
    Ok((s, out, force))
}

impl Spec {
    fn has(&self, w: &str) -> bool {
        self.workloads.iter().any(|x| x == w)
    }
    fn primary(&self) -> &str {
        &self.endpoints.iter().find(|e| !e.2).expect("a primary").0
    }
    fn routed(&self) -> Vec<&(String, String, bool)> {
        if self.route.is_empty() {
            self.endpoints.iter().collect()
        } else {
            self.endpoints
                .iter()
                .filter(|e| self.route.contains(&e.0))
                .collect()
        }
    }
    fn replicas_routed(&self) -> bool {
        self.routed().iter().any(|e| e.2)
    }
    /// What one node's transactions promise at the configured isolation level.
    fn claim(&self) -> &'static str {
        match self.isolation.as_str() {
            "serializable" => "strict-serializable",
            _ => "strong-snapshot-isolation",
        }
    }
    /// The same level without real time (reads of a replica).
    fn claim_stale(&self) -> &'static str {
        match self.isolation.as_str() {
            "serializable" => "serializable",
            _ => "snapshot-isolation",
        }
    }

    pub fn conf(&self) -> String {
        let mut o = String::from(
            "# hydo-sql's configuration (written by hydo sql-gen; MANIFEST.md, \"SQL systems\").\n",
        );
        o.push_str("# The addresses are as the guest reaches the services. Edit freely; hydo-sql reads it at\n");
        o.push_str("# every command from /hydo/workload/helper_sql.conf.\n");
        o.push_str(&format!("dialect {}\n", self.dialect));
        for (n, a, r) in &self.endpoints {
            o.push_str(&format!(
                "endpoint {n} {a} {}\n",
                if *r { "replica" } else { "primary" }
            ));
        }
        o.push_str(&format!("isolation {}\n", self.isolation));
        o.push_str(&format!("workloads {}\n", self.workloads.join(" ")));
        if !self.route.is_empty() {
            o.push_str(&format!("route * {}\n", self.route.join(" ")));
        }
        o.push_str(&format!(
            "accounts {}\nbalance {}\nkeys {}\ntimeout_ms {}\nretries {}\n",
            self.accounts, self.balance, self.keys, self.timeout_ms, self.retries
        ));
        o
    }

    /// Every file: (path under the folder, content, executable).
    pub fn files(&self) -> Vec<(String, String, bool)> {
        let mut f: Vec<(String, String, bool)> = Vec::new();
        let sh = |hdr: &str, body: &str| format!("#!/bin/sh\n{hdr}{body}\n");
        f.push(("workload/helper_sql.conf".into(), self.conf(), false));
        if self.workloads.is_empty() {
            // only the endpoints, for SQL checks and `hydo-sql check` sources
            return f;
        }
        f.push((
            "workload/first_sql_init".into(),
            sh(
                "# hydo: timeout=90\n# The workloads' tables and first rows on the primary (idempotent), then a wait until every\n# endpoint serves them, so the workload starts from the same state everywhere.\n",
                "exec hydo-sql init --wait 60",
            ),
            true,
        ));
        let shapes: Vec<(&str, &str)> = [("batch", ""), ("interactive", "i")]
            .into_iter()
            .filter(|(s, _)| {
                if *s == "batch" {
                    self.batch
                } else {
                    self.interactive
                }
            })
            .collect();
        let dial = if self.dialect == "mysql" {
            "(MySQL's text protocol has no one-request transaction: the statements follow each other with no decision between them)"
        } else {
            "(one request)"
        };
        if self.has("bank") {
            for (shape, p) in &shapes {
                let what = if *shape == "batch" {
                    format!("# One bank transfer as a transaction whose statements are decided before it starts {dial}:\n# the debit only when the balance allows it, the credit only when the debit happened.\n")
                } else {
                    "# One bank transfer as an interactive transaction: read both balances, decide in the client,\n# write the new values (a lost update changes the bank's total).\n".to_string()
                };
                f.push((
                    format!("workload/parallel_driver_sql_{p}transfer"),
                    sh(&format!("# hydo: timeout=20\n{what}# The endpoint comes from the route; outcomes go to $HYDO_OUTCOMES and $HYDO_ACKED.\n"), &format!("exec hydo-sql bank transfer --shape {shape}")),
                    true,
                ));
            }
        }
        if self.has("list-append") {
            for (shape, p) in &shapes {
                let what = if *shape == "batch" {
                    format!("as one transaction {dial}")
                } else {
                    "as an interactive transaction (one statement per round trip)".into()
                };
                f.push((
                    format!("workload/parallel_driver_sql_{p}txn"),
                    sh(
                        &format!("# hydo: timeout=20\n# One list-append transaction (1 to 4 appends of unique elements and whole-list reads) {what},\n# recorded for hydo's list-append checker.\n"),
                        &format!("exec hydo-sql list-append txn --shape {shape}"),
                    ),
                    true,
                ));
            }
        }
        if self.has("register") {
            f.push((
                "workload/parallel_driver_sql_register".into(),
                sh("# hydo: timeout=20\n# One read, write or compare-and-set of a register, recorded for hydo's linearizability checker.\n", "exec hydo-sql register op"),
                true,
            ));
        }
        if self.has("set") {
            f.push(("workload/parallel_driver_sql_set_add".into(), sh("# hydo: timeout=20\n# Insert one unique element; acknowledged elements must survive every fault.\n", "exec hydo-sql set add"), true));
        }
        if self.has("counter") {
            f.push(("workload/parallel_driver_sql_counter_add".into(), sh("# hydo: timeout=20\n# Add one to a counter; no acknowledged increment may be lost.\n", "exec hydo-sql counter add"), true));
        }
        let p = self.primary().to_string();
        let all: Vec<String> = self.endpoints.iter().map(|e| e.0.clone()).collect();
        if self.has("bank") {
            f.push((
                "checks/always_sql_bank_live".into(),
                sh(
                    &format!("# hydo: every=2\n# hydo-msg: the bank's total never changes and no balance is negative, on {}\n# Exit 2 (cannot judge now) when a node does not answer: a killed or paused node is no violation.\n", all.join(" and ")),
                    &format!("exec hydo-sql bank check {}", all.join(" ")),
                ),
                true,
            ));
            f.push((
                "checks/final_sql_bank_after".into(),
                sh(&format!("# hydo-msg: after the faults, the bank's total on {p} is unchanged and no balance is negative\n"), &format!("exec hydo-sql bank check {p}")),
                true,
            ));
            f.push((
                "checks/final_sql_no_negative_balance.sql".into(),
                format!("-- hydo: store={p}\n-- hydo-msg: no account's balance is negative\n-- A SQL check: the query must return no rows (MANIFEST.md, \"SQL checks\").\nSELECT id, bal FROM hydo_accounts WHERE bal < 0\n"),
                false,
            ));
        }
        if self.has("list-append") {
            let replicas = self.replicas_routed();
            let (claim, note) = if replicas {
                (self.claim_stale(), "# History la holds every transaction, also those a replica served, whose reads may be stale,\n# so it claims no real-time order (list_append_sql.lap does, for what ran on a primary).\n")
            } else {
                (self.claim(), "# History la holds every transaction; one primary serves them all, so real time is claimed:\n# an acknowledged append missing from a later read is a lost write, also across a power cut.\n")
            };
            f.push((
                "checks/list_append_sql.la".into(),
                sh(
                    &format!("# hydo: history=la consistency={claim}\n# hydo-msg: the SQL list-append transactions are {claim} (no lost, aborted or duplicated append, no dependency cycle)\n{note}# The final read-back: every list, in one transaction on {p}.\n"),
                    &format!("exec hydo-sql list-append read-all {p} 9999"),
                ),
                true,
            ));
            if replicas {
                f.push((
                    "checks/list_append_sql.lap".into(),
                    sh(
                        &format!("# hydo: history=lap consistency={}\n# hydo-msg: on the primary, the SQL transactions are {}: no acknowledged append is lost, also across a power cut\n# History lap: the transactions a primary ran, and only the appends of those a replica served\n# (a write executes on the primary within the request; a replica's reads may be stale).\n", self.claim(), self.claim()),
                        &format!("exec hydo-sql list-append read-all {p} 9998"),
                    ),
                    true,
                ));
            }
        }
        if self.has("register") {
            f.push((
                "checks/linearizable_sql.rg".into(),
                sh(
                    &format!("# hydo: history=rg model=cas\n# hydo-msg: the SQL registers are linearizable (no lost write, no stale read on a primary)\n# A replica's reads are not recorded (they may be stale); its writes are.\n# The final read-back: every register on {p}.\n"),
                    &format!("exec hydo-sql register read-all {p} 9997"),
                ),
                true,
            ));
        }
        if self.has("set") {
            f.push((
                "checks/final_sql_set".into(),
                sh(
                    &format!("# hydo-msg: every acknowledged insert is in the set on {p}\n"),
                    &format!("exec hydo-sql set check {p}"),
                ),
                true,
            ));
        }
        if self.has("counter") {
            f.push((
                "checks/final_sql_counter".into(),
                sh(
                    &format!(
                        "# hydo-msg: no acknowledged increment of the counter on {p} is lost\n"
                    ),
                    &format!("exec hydo-sql counter check {p}"),
                ),
                true,
            ));
        }
        let reps: Vec<&String> = self
            .endpoints
            .iter()
            .filter(|e| e.2)
            .map(|e| &e.0)
            .collect();
        if !reps.is_empty() {
            let mut body = String::from("rc=0\n");
            for r in &reps {
                body.push_str(&format!(
                    "hydo-sql check equal --show 3 --left '{p}=dump {p}' --right '{r}=dump {r}' || rc=$?\n"
                ));
            }
            body.push_str("exit $rc");
            f.push((
                "checks/eventually_sql_replicas_converge".into(),
                sh(
                    &format!("# hydo: within=120 after=heal poll=2\n# hydo-msg: after the faults heal, every replica holds exactly {p}'s rows\n# A differential check over the workloads' tables (hydo-sql's dump source). Raise within= if a\n# replica needs longer to catch up after a lossy link.\n"),
                    &body,
                ),
                true,
            ));
        }
        f
    }
}

/// Writes the files; returns what was written and what was kept.
pub fn write(s: &Spec, dir: &Path, force: bool) -> Result<(Vec<String>, Vec<String>), String> {
    use std::os::unix::fs::PermissionsExt;
    let (mut wrote, mut kept) = (Vec::new(), Vec::new());
    for (rel, text, exec) in s.files() {
        let p = dir.join(&rel);
        if p.exists() && !force {
            kept.push(rel);
            continue;
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
        std::fs::set_permissions(
            &p,
            std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }),
        )
        .map_err(|e| format!("{}: {e}", p.display()))?;
        wrote.push(rel);
    }
    Ok((wrote, kept))
}

pub fn cmd_sql_gen(argv: &[String]) -> i32 {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let (s, dir, force) = match parse(argv) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("hydo sql-gen: {e}\n{USAGE}");
            return 2;
        }
    };
    match write(&s, &dir, force) {
        Ok((wrote, kept)) => {
            for w in &wrote {
                println!("wrote {}", dir.join(w).display());
            }
            for k in &kept {
                println!(
                    "kept  {} (exists; --force replaces it)",
                    dir.join(k).display()
                );
            }
            println!(
                "hydo build stages /bin/hydo-sql because these commands call it. Set how many commands run at once\nin hydo.yaml's test: section (parallel, per_round, round_ms); run `hydo build --check`."
            );
            0
        }
        Err(e) => {
            eprintln!("hydo sql-gen: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn replicas_give_two_histories_and_a_convergence_check() {
        let (s, _, _) = parse(&args(&[
            "--dialect",
            "libsql",
            "--endpoint",
            "primary=10.89.0.11:8080",
            "--endpoint",
            "replica=10.89.0.12:8080,replica",
        ]))
        .unwrap();
        let f = s.files();
        let names: Vec<&str> = f.iter().map(|x| x.0.as_str()).collect();
        for want in [
            "workload/helper_sql.conf",
            "workload/first_sql_init",
            "workload/parallel_driver_sql_transfer",
            "workload/parallel_driver_sql_itransfer",
            "workload/parallel_driver_sql_txn",
            "workload/parallel_driver_sql_itxn",
            "checks/always_sql_bank_live",
            "checks/final_sql_bank_after",
            "checks/final_sql_no_negative_balance.sql",
            "checks/list_append_sql.la",
            "checks/list_append_sql.lap",
            "checks/eventually_sql_replicas_converge",
        ] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
        let la = &f
            .iter()
            .find(|x| x.0 == "checks/list_append_sql.la")
            .unwrap()
            .1;
        assert!(la.contains("consistency=serializable\n"), "{la}");
        let lap = &f
            .iter()
            .find(|x| x.0 == "checks/list_append_sql.lap")
            .unwrap()
            .1;
        assert!(lap.contains("consistency=strict-serializable\n"), "{lap}");
        assert!(s
            .conf()
            .contains("endpoint replica 10.89.0.12:8080 replica\n"));
    }

    #[test]
    fn one_primary_claims_real_time() {
        let (s, _, _) = parse(&args(&[
            "--dialect",
            "postgres",
            "--endpoint",
            "db=postgres://postgres:pw@10.89.0.2:5432/postgres?sslmode=disable",
            "--workloads",
            "bank,list-append,register,set,counter",
            "--shapes",
            "batch",
        ]))
        .unwrap();
        let f = s.files();
        let la = &f
            .iter()
            .find(|x| x.0 == "checks/list_append_sql.la")
            .unwrap()
            .1;
        assert!(la.contains("consistency=strict-serializable\n"));
        assert!(!f
            .iter()
            .any(|x| x.0.ends_with(".lap") || x.0.contains("itxn") || x.0.contains("converge")));
        assert!(f.iter().any(|x| x.0 == "checks/linearizable_sql.rg"));
    }

    #[test]
    fn refusals() {
        for (a, want) in [
            (vec!["--endpoint", "a=x"], "--dialect is required"),
            (vec!["--dialect", "oracle"], "postgres, mysql or libsql"),
            (
                vec!["--dialect", "mysql", "--endpoint", "a=x,replica"],
                "must be a primary",
            ),
            (
                vec![
                    "--dialect",
                    "mysql",
                    "--endpoint",
                    "a=x",
                    "--isolation",
                    "read-committed",
                ],
                "snapshot isolation",
            ),
            (
                vec!["--dialect", "mysql", "--endpoint", "a=x", "--route", "b"],
                "no such endpoint",
            ),
        ] {
            let e = parse(&args(&a)).unwrap_err();
            assert!(e.contains(want), "{a:?}: {e}");
        }
    }

    /// The written folder reads into the project model: the commands, the checks (the `.sql`
    /// file as a hydo-sql argv), the history checks.
    #[test]
    fn the_folder_reads_back() {
        let dir = std::env::temp_dir().join(format!("hydo-sqlgen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (s, _, _) = parse(&args(&[
            "--dialect",
            "libsql",
            "--endpoint",
            "primary=10.89.0.11:8080",
            "--endpoint",
            "replica=10.89.0.12:8080,replica",
        ]))
        .unwrap();
        write(&s, &dir, false).unwrap();
        let (w2, kept) = write(&s, &dir, false).unwrap();
        assert!(
            w2.is_empty() && !kept.is_empty(),
            "a second run keeps the files"
        );
        let folder = hydo_manifest::composer::scan_folder(&dir.to_string_lossy())
            .unwrap()
            .unwrap();
        let checks = folder["checks"].as_array().unwrap();
        let sql = checks
            .iter()
            .find(|c| c["name"] == "sql_no_negative_balance")
            .unwrap();
        assert_eq!(sql["kind"], "final");
        assert_eq!(
            sql["run"],
            serde_json::json!([
                "hydo-sql",
                "none",
                "--file",
                "/hydo/checks/final_sql_no_negative_balance.sql",
                "--on",
                "primary"
            ])
        );
        assert_eq!(sql["msg"], "no account's balance is negative");
        let live = checks
            .iter()
            .find(|c| c["name"] == "sql_bank_live")
            .unwrap();
        assert_eq!(live["every"], 2);
        assert!(checks
            .iter()
            .any(|c| c["kind"] == "list_append" && c["history"] == "lap"));
        let cmds = folder["commands"].as_array().unwrap();
        assert_eq!(cmds.len(), 5, "{cmds:?}");
        // an executable .sql file stays an ordinary check (a program), as before Milestone 182
        use std::os::unix::fs::PermissionsExt;
        let ex = dir.join("checks/final_prog.sql");
        std::fs::write(&ex, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&ex, std::fs::Permissions::from_mode(0o755)).unwrap();
        // a bad header key is refused with the allowed ones
        std::fs::write(
            dir.join("checks/always_bad.sql"),
            "-- hydo: store=primary speed=3\nSELECT 1\n",
        )
        .unwrap();
        let e = hydo_manifest::composer::scan_folder(&dir.to_string_lossy()).unwrap_err();
        assert!(
            format!("{e:?}").contains("allowed here: store, every, within, after, poll"),
            "{e:?}"
        );
        std::fs::remove_file(dir.join("checks/always_bad.sql")).unwrap();
        let folder = hydo_manifest::composer::scan_folder(&dir.to_string_lossy())
            .unwrap()
            .unwrap();
        let prog = folder["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "prog.sql")
            .unwrap()
            .clone();
        assert_eq!(
            prog["run"],
            serde_json::json!(["/hydo/checks/final_prog.sql"])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
