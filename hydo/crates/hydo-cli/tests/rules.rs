//! `hydo rules check` (Milestone 236) end to end on the scripted engine (`HYDO_SCRIPTED_ENGINE`):
//! the program below stands in for a run. It reads the rules the command added to the run
//! (`HYDO_SCRIPTED_RULES`) and writes the `HYDO-ASSERT` record each rule's wrapper would print,
//! by the rule's id: one that holds, one that judges nothing, one that fails in the second clean
//! run, one whose program is missing, and one that a planted change does not break. No VM runs.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

const ENGINE: &str = r#"#!/bin/sh
set -eu
[ "$1" = one ] || exit 2
dir=$2; sched=$3
echo "$sched" >> "$LOG/runs"
out() {   # id exit mutate cases last
  ok=true; [ "$2" = 1 ] && ok=false
  printf 'HYDO-ASSERT {"t":"always","id":"rule.%s","ok":%s,"msg":"m","details":{"out":"hydo-rule exit=%s mutate=%s cases=%s :: %s"}}\n' \
    "$1" "$ok" "$2" "$3" "$4" "$5" >> "$dir/one.log"
}
: > "$dir/one.log"
for id in $(grep -o '"id": "[^"]*"' "$HYDO_SCRIPTED_RULES" | cut -d'"' -f4); do
  case $dir in
  */mutate)
    case $id in
      holds) out holds 1 0,0 3 "db: 1 rows where none may be" ;;
      blind) out blind 0 0,0 5 "db: no rows" ;;
    esac ;;
  *)
    case $id in
      holds) out holds 0 - 3 "db: no rows" ;;
      blind) out blind 0 - 5 "db: no rows" ;;
      vacuous) out vacuous 0 - 0 "db: no rows" ;;
      fails)
        case $sched in
          *W=00000002*) out fails 1 - 2 "db: 2 rows where none may be"
            printf 'HYDO-CHECK-OUT {"id":"rule.fails","lines":["db: id=7 total=3","db: 2 rows where none may be"],"omitted":0}\n' >> "$dir/one.log" ;;
          *) out fails 0 - 2 "db: no rows" ;;
        esac ;;
      broken) out broken 127 - - "sh: hydo-sql: not found" ;;
    esac ;;
  esac
done
echo "RESULT one fp=0000000000000001 wfp=0 secs=0.1 verdict=HYDO-ORACLE: OK invariants=1 held"
"#;

const RULES: &str = r#"format: 1
store: db
rules:
  - id: holds
    says: A total is the sum of its lines.
    kind: sql
    confidence: high
    evidence: [db/schema.sql:12]
    query: SELECT id FROM orders WHERE total <> 0
    cases: SELECT count(*) FROM orders
    mutate: UPDATE orders SET total = total + 1
  - id: blind
    says: No order is negative.
    kind: sql
    query: SELECT id FROM orders WHERE total < 0
    cases: SELECT count(*) FROM orders
    mutate: UPDATE orders SET note = 'x'
  - id: vacuous
    says: A refund never exceeds its payment.
    kind: sql
    query: SELECT id FROM refunds WHERE amount > paid
    cases: SELECT count(*) FROM refunds
  - id: fails
    says: An order has one payment.
    kind: sql
    query: SELECT order_id FROM payments GROUP BY order_id HAVING count(*) > 1
  - id: broken
    says: Every acknowledged order is stored.
    kind: command
    run: hydo-sql check subset --left 'acked=hydo acked' --right 'stored=sql db "SELECT id FROM orders"'
  - id: delivered
    says: Each message is delivered once.
    kind: messages
"#;

struct T {
    dir: PathBuf,
}

impl T {
    fn new(name: &str) -> T {
        let dir = std::env::temp_dir().join(format!("hydo-rules-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("log")).unwrap();
        std::fs::write(dir.join("rules.yaml"), RULES).unwrap();
        std::fs::write(dir.join("app-disk.img"), "a stand-in image").unwrap();
        let e = dir.join("engine.sh");
        std::fs::write(&e, ENGINE).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755)).unwrap();
        T { dir }
    }

    fn hydo(&self, args: &[&str]) -> (i32, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .args(args)
            .current_dir(&self.dir)
            .env("HYDO_SCRIPTED_ENGINE", self.dir.join("engine.sh"))
            .env("LOG", self.dir.join("log"))
            .env_remove("HYDO_BACKEND")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        (o.status.code().unwrap_or(-1), text)
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rule<'a>(doc: &'a Value, id: &str) -> &'a Value {
    doc["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap()
}

#[test]
fn each_rule_gets_its_status_from_clean_runs() {
    let t = T::new("status");
    let (rc, text) = t.hydo(&[
        "rules",
        "check",
        "rules.yaml",
        "--image",
        "app-disk.img",
        "--mutate",
        "--out",
        "out",
        "--write-checks",
        "checks",
    ]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.contains(
            "6 proposed: 2 hold, 1 vacuous, 1 fail without faults, 1 unevaluated, 1 not run"
        ),
        "{text}"
    );
    // three clean runs and one run of the planted changes, all without faults
    let runs = std::fs::read_to_string(t.dir.join("log/runs")).unwrap();
    assert_eq!(
        runs.lines().collect::<Vec<_>>(),
        [
            "disk=none | run W=00000001",
            "disk=none | run W=00000002",
            "disk=none | run W=00000003",
            "disk=none | run W=00000001"
        ]
    );
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(t.dir.join("out/rules-check.json")).unwrap())
            .unwrap();
    assert_eq!(doc["summary"]["proposed"], 6);
    let st = |id: &str| rule(&doc, id)["status"].as_str().unwrap().to_string();
    assert_eq!(st("holds"), "holds");
    assert_eq!(st("blind"), "holds");
    assert_eq!(st("vacuous"), "vacuous");
    assert_eq!(st("fails"), "fails-clean");
    assert_eq!(st("broken"), "unevaluated");
    assert_eq!(st("delivered"), "not-run");
    assert_eq!(rule(&doc, "holds")["why"], "cases 3, 3, 3");
    assert_eq!(
        rule(&doc, "fails")["why"],
        "run 2: db: 2 rows where none may be"
    );
    assert_eq!(
        rule(&doc, "fails")["output"],
        serde_json::json!(["db: id=7 total=3", "db: 2 rows where none may be"])
    );
    assert!(rule(&doc, "broken")["why"]
        .as_str()
        .unwrap()
        .contains("a program is missing (exit 127)"));
    assert_eq!(rule(&doc, "holds")["mutation"]["verdict"], "caught");
    assert_eq!(rule(&doc, "blind")["mutation"]["verdict"], "blind");
    assert_eq!(
        rule(&doc, "holds")["evidence"],
        serde_json::json!(["db/schema.sql:12"])
    );
    // only the rules that hold become check files
    let mut files: Vec<String> = std::fs::read_dir(t.dir.join("checks"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["final_blind.sql", "final_holds.sql"]);
    let f = std::fs::read_to_string(t.dir.join("checks/final_holds.sql")).unwrap();
    assert!(f.starts_with("-- hydo: store=db\n-- hydo-msg: A total is the sum of its lines.\n"));
}

#[test]
fn a_bad_candidates_file_runs_nothing() {
    let t = T::new("bad");
    std::fs::write(
        t.dir.join("rules.yaml"),
        "format: 1\nrules:\n  - id: a\n    says: x\n    kind: sql\n",
    )
    .unwrap();
    let (rc, text) = t.hydo(&["rules", "check", "rules.yaml", "--image", "app-disk.img"]);
    assert_ne!(rc, 0);
    assert!(
        text.contains("rules.yaml: rule a: a `sql` rule needs `query`"),
        "{text}"
    );
    assert!(!t.dir.join("log/runs").exists());
}

#[test]
fn the_scan_lists_evidence_by_kind() {
    let t = T::new("scan");
    std::fs::create_dir_all(t.dir.join("src/db")).unwrap();
    std::fs::write(
        t.dir.join("src/db/schema.sql"),
        "CREATE TABLE seats (id INT, booking INT UNIQUE, state TEXT);\n",
    )
    .unwrap();
    std::fs::write(
        t.dir.join("src/README.md"),
        "A seat is never booked twice.\n",
    )
    .unwrap();
    let (rc, text) = t.hydo(&["rules", "scan", "src"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.starts_with("hydo rules scan: 2 lines of evidence in 2 files of src"),
        "{text}"
    );
    assert!(
        text.contains("  db/schema.sql:1: CREATE TABLE seats"),
        "{text}"
    );
    assert!(
        text.contains("  README.md:1: A seat is never booked twice."),
        "{text}"
    );
    let (rc, text) = t.hydo(&["rules", "scan", "src", "--kind", "docs", "--json"]);
    assert_eq!(rc, 0, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(doc["evidence"][0]["kind"], "docs");
}

/// The write routes (Milestone 344): an axum router and a `.proto` service, grouped by resource
/// and ranked by class, with the reads by name left out and listed by `--all`.
#[test]
fn the_write_scan_lists_the_routes_that_write() {
    let t = T::new("writes");
    std::fs::create_dir_all(t.dir.join("proj/src")).unwrap();
    std::fs::create_dir_all(t.dir.join("proj/proto")).unwrap();
    std::fs::write(
        t.dir.join("proj/src/http.rs"),
        "fn router() -> Router {\n    Router::new()\n        .route(\"/orders\", get(list_orders).post(create_order))\n        .route(\"/orders/{id}\", delete(delete_order))\n        .route(\"/orders/search\", post(search_orders))\n        .route(\"/users/login\", post(login))\n        .route(\"/-/reload\", post(reload))\n}\nasync fn create_order() {}\nasync fn delete_order() {}\n",
    )
    .unwrap();
    std::fs::write(
        t.dir.join("proj/proto/ledger.proto"),
        "syntax = \"proto3\";\npackage ledger.v1;\nservice Ledger {\n  rpc PostTransfer(TransferRequest) returns (Transfer);\n  rpc GetBalance(BalanceRequest) returns (Balance);\n}\n",
    )
    .unwrap();
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "writes"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.starts_with(
            "hydo rules scan --kind writes: 5 routes that write, in 4 resources, in 2 files of proj"
        ),
        "{text}"
    );
    assert!(
        text.contains("  by class: 3 data, 1 session, 1 runtime"),
        "{text}"
    );
    assert!(text.contains("  left out: 2 POST routes and RPC methods whose names say that they read (--all lists them)"), "{text}");
    assert!(text.contains("\norders (2)\n  POST   /orders  [data]\n         handler create_order (src/http.rs:9)\n"), "{text}");
    assert!(
        text.contains("  RPC    /ledger.v1.Ledger/PostTransfer  [data]"),
        "{text}"
    );
    // the data first, the sign-in and the health check last
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} not in {text}"));
    assert!(at("/orders/{id}") < at("/users/login"));
    assert!(at("/users/login") < at("/-/reload"));
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "writes", "--all"]);
    assert_eq!(rc, 0, "{text}");
    assert!(text.contains("left out, read by their names (2)"), "{text}");
    assert!(
        text.contains("/orders/search  handler search_orders"),
        "{text}"
    );
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "writes", "--json"]);
    assert_eq!(rc, 0, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    let w = doc["writes"].as_array().unwrap();
    assert_eq!(w.len(), 5);
    assert_eq!(w[0]["rank"], 1);
    assert_eq!(w[0]["class"], "data");
    assert_eq!(doc["reads_left_out"].as_array().unwrap().len(), 2);
    // the scan without --kind prints one line per route in its own section
    let (rc, text) = t.hydo(&["rules", "scan", "proj"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.contains("\nwrites (5): routes of the API that write"),
        "{text}"
    );
    assert!(
        text.contains("  src/http.rs:3: [data] POST /orders\n"),
        "{text}"
    );
}

/// The clock audit (Milestone 248): a throttle measured on the wall clock (Temporal's shape),
/// a lease stamped by the database's clock (River's), and a test file that is not read. The
/// schedules name the database and the lease holder of hydo/hydo.yaml.
#[test]
fn the_clock_audit_aims_clock_steps_at_durations_on_the_wall_clock() {
    let t = T::new("clock");
    let w = |rel: &str, text: &str| {
        let p = t.dir.join("proj").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w(
        "clock/source.go",
        "package clock\n\nfunc (ts RealTimeSource) Now() time.Time {\n\treturn time.Now().UTC()\n}\n",
    );
    w(
        "store/handle.go",
        "package store\n\nconst refreshMin = 1 * time.Second\n\nfunc (h *Handle) reconnect() {\n\tnow := h.clock.Now()\n\tif now.Sub(h.lastRefresh) < refreshMin {\n\t\treturn\n\t}\n\th.lastRefresh = now\n}\n",
    );
    w(
        "db/leader.sql",
        "UPDATE leader SET expires_at = now() + make_interval(secs => @ttl)\nWHERE expires_at >= now() AND leader_id = @leader_id;\n",
    );
    w(
        "store/handle_test.go",
        "package store\n\nfunc TestX(t *testing.T) {\n\tif time.Now().UTC().Sub(start) > timeout {\n\t}\n}\n",
    );
    // five places in code that only measures for statistics: ranked after the two above
    let stats: String = (1..=5)
        .map(|k| format!("func (s *S) f{k}() {{\n\tif time.Now().UTC().Sub(s.last{k}) > s.timeout{k} {{\n\t}}\n}}\n"))
        .collect();
    w("stats/stats.go", &format!("package stats\n\n{stats}"));
    w(
        "hydo/hydo.yaml",
        "format: 1\nname: p\ngroups:\n  - name: workers\n    services: [w1, w2]\n    primary: \"cat /tmp/leader\"\nservices:\n  - name: postgres\n    image_ref: postgres:16\n  - name: w1\n  - name: w2\nworkload:\n  steps: 12\n",
    );
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.starts_with("hydo rules scan --kind clock: 7 functions in 4 source files of proj"),
        "{text}"
    );
    // the most likely places first, five of them, and one line for the rest
    assert!(
        text.contains("\n1. store/handle.go:7, in reconnect [high]"),
        "{text}"
    );
    assert!(text.contains("\n2. db/leader.sql:1 [high]"), "{text}");
    assert!(text.contains("\n5. stats/stats.go:"), "{text}");
    assert!(!text.contains("\n6. "), "{text}");
    assert!(
        text.contains("\nand 2 more: hydo rules scan --kind clock --all\n"),
        "{text}"
    );
    assert!(
        text.contains("refreshMin, 1 second by default (store/handle.go:3)"),
        "{text}"
    );
    assert!(
        text.contains("disk=none | run T=1:-300 K=postgres@2+3"),
        "{text}"
    );
    assert!(
        text.contains("disk=none | run K=postgres@2+3 T=10:30"),
        "{text}"
    );
    assert!(
        text.contains("disk=none | run K=workers.primary@2+3 T=10:-300"),
        "{text}"
    );
    assert!(!text.contains("handle_test.go"), "{text}");
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock", "--all"]);
    assert_eq!(rc, 0, "{text}");
    assert!(text.contains("\n7. stats/stats.go:"), "{text}");
    assert!(!text.contains("--all\n"), "{text}");
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock", "--json"]);
    assert_eq!(rc, 0, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    let places = doc["clock"].as_array().unwrap();
    assert_eq!(places.len(), 8, "{text}");
    let ranked = places.iter().filter(|p| p["rank"].is_u64()).count();
    assert_eq!(ranked, 7, "{text}");
    let go = places
        .iter()
        .find(|p| p["file"] == "store/handle.go")
        .unwrap();
    assert_eq!(go["shape"], "elapsed");
    assert_eq!(go["limit"]["seconds"], 1.0);
    assert_eq!(go["rank"], 1);
    assert_eq!(go["schedules"].as_array().unwrap().len(), 3);
    // the scan without --kind lists the places in its own section
    let (rc, text) = t.hydo(&["rules", "scan", "proj"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.contains("\nclock (8): durations measured with the wall clock"),
        "{text}"
    );
}

/// Milestone 324: the clock audit maps the code to the services of hydo.yaml. The Go module
/// builds two programs: `queue`, which the service runs, and `report`, which nothing runs. The
/// places in the code that `queue` reaches come first, each names who runs it, and a schedule
/// names the one service instead of `<service>`. A Go test function inside a source file is
/// left out.
#[test]
fn the_clock_audit_ranks_the_code_that_the_services_run_first() {
    let t = T::new("clockmap");
    let w = |rel: &str, text: &str| {
        let p = t.dir.join("proj").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w("go.mod", "module example.com/q\n\ngo 1.22\n");
    w(
        "cmd/queue/main.go",
        "package main\n\nimport \"example.com/q/store\"\n\nfunc main() { store.Run() }\n",
    );
    w(
        "cmd/report/main.go",
        "package main\n\nimport \"example.com/q/stats\"\n\nfunc main() { stats.Print() }\n",
    );
    // the place that the service runs has a weaker score than the report's
    w(
        "store/idle.go",
        "package store\n\nfunc (s *Server) expireIdle() {\n\tif time.Now().UTC().Sub(s.lastSeen) > s.idleTimeout {\n\t}\n}\n\nfunc TestExpireIdle(t *testing.T) {\n\tif time.Now().UTC().Sub(start) > timeout {\n\t}\n}\n",
    );
    w(
        "stats/retry.go",
        "package stats\n\nconst retryMin = 2 * time.Second\n\nfunc (r *Retrier) retryLease() {\n\tif time.Now().UTC().Sub(r.last) > retryMin {\n\t}\n}\n",
    );
    w(
        "hydo/hydo.yaml",
        "format: 1\nname: q\nservices:\n  - name: queue\n    cmd: \"/bin/queue serve\"\n  - name: postgres\n    image_ref: postgres:16\nworkload:\n  steps: 12\n",
    );
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock"]);
    assert_eq!(rc, 0, "{text}");
    for want in [
        "\nIt left out 1 test function inside the source files, besides the test files.\n",
        "\nQueue runs queue (its command), built from cmd/queue and 1 Go package it uses;",
        "the places in the code that hydo.yaml runs come first",
        "\n1. store/idle.go:4, in expireIdle [high]\n",
        "\n   Run by queue (queue).\n",
        "disk=none | run T=1:-300 N=queue@2-5:loss,30%",
        "\n2. stats/retry.go:6, in retryLease [high]\n",
        "\n   No service of hydo.yaml and not the workload runs this code.\n",
        "<leader>: no service of hydo.yaml runs this code, so no fault of one makes it run.",
    ] {
        assert!(text.contains(want), "{want:?}\n{text}");
    }
    assert!(!text.contains("idle.go:9"), "{text}");
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock", "--json"]);
    assert_eq!(rc, 0, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    let places = doc["clock"].as_array().unwrap();
    let idle = places
        .iter()
        .find(|p| p["file"] == "store/idle.go")
        .unwrap();
    assert_eq!(idle["run_by"], serde_json::json!(["queue"]));
    assert_eq!(idle["rank"], 1);
    assert_eq!(doc["tests_left_out"]["functions"], 1);
}

/// Milestone 350: the clock audit's second kind of place, a wall-clock reading kept as the
/// version that a merge orders by (a last-writer-wins register). The report shows it in its own
/// section with its schedules, `--json` gives it under `clock_orders`, and the scan without
/// `--kind` counts it in a line of its own.
#[test]
fn the_clock_audit_names_versions_taken_from_the_wall_clock() {
    let t = T::new("clockorder");
    let w = |rel: &str, text: &str| {
        let p = t.dir.join("proj").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w(
        "src/lww.rs",
        "pub struct Lww<T> {\n    ts: u64,\n    v: T,\n}\n\nimpl<T> Lww<T> {\n    pub fn new(v: T) -> Self {\n        Self {\n            ts: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64,\n            v,\n        }\n    }\n\n    pub fn merge(&mut self, other: &Self) {\n        if other.ts > self.ts {\n            self.ts = other.ts;\n        }\n    }\n}\n",
    );
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock"]);
    assert_eq!(rc, 0, "{text}");
    for want in [
        "hydo rules scan --kind clock: 0 functions in 1 source files of proj measure a duration",
        "\nNo lease, timeout, throttle, lag or delay was found that reads the wall clock.\n",
        "\n1 function keeps a wall-clock reading as the order or the version of stored data.\n",
        "\n1. src/lww.rs:9, in new [high]\n",
        "It keeps a wall-clock reading as ts, and the code orders data by comparing two values of it (src/lww.rs:15).",
        "     back           disk=none | run T=10:-300\n",
        "     back-long      disk=none | run T=10:-3600\n",
        "     back-after     disk=none | run K=<service>@2+3 T=10:-300\n",
        "\nTo try a schedule: hydo explore --schedule",
    ] {
        assert!(text.contains(want), "{want:?}\n{text}");
    }
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock", "--json"]);
    assert_eq!(rc, 0, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["clock"].as_array().unwrap().len(), 0, "{text}");
    let o = &doc["clock_orders"][0];
    assert_eq!(o["shape"], "order");
    assert_eq!(o["rank"], 1);
    assert_eq!(o["order"]["name"], "ts");
    assert_eq!(o["order"]["compared_at"], "src/lww.rs:15");
    assert_eq!(o["schedules"].as_array().unwrap().len(), 3);
    let (rc, text) = t.hydo(&["rules", "scan", "proj"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.contains("\nclock orders (1): orders and versions of data taken from the wall clock"),
        "{text}"
    );
}

/// Milestone 363: a place whose words name a connection, in a project whose hydo.yaml has no
/// database, names the one service that runs the place, not `<database>` (NSQ's nsqlookupd in
/// the 0.6.0 trial: `K=<database>@2+3` under `Run by nsqlookupd`).
#[test]
fn the_clock_audit_names_the_service_when_there_is_no_database() {
    let t = T::new("clocknodb");
    let w = |rel: &str, text: &str| {
        let p = t.dir.join("proj").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w("go.mod", "module example.com/l\n\ngo 1.22\n");
    w(
        "cmd/lookupd/main.go",
        "package main\n\nimport \"example.com/l/peers\"\n\nfunc main() { peers.Run() }\n",
    );
    w(
        "peers/conn.go",
        "package peers\n\nfunc (p *Peer) reconnectIdle() {\n\tif time.Now().UTC().Sub(p.lastConnection) > p.connectionTimeout {\n\t}\n}\n",
    );
    w(
        "hydo/hydo.yaml",
        "format: 1\nname: l\nservices:\n  - name: lookupd\n    cmd: \"/bin/lookupd\"\nworkload:\n  steps: 12\n",
    );
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "clock"]);
    assert_eq!(rc, 0, "{text}");
    assert!(text.contains("\n   Run by lookupd (lookupd).\n"), "{text}");
    assert!(
        text.contains("disk=none | run T=1:-300 K=lookupd@2+3"),
        "{text}"
    );
    assert!(!text.contains("<database>"), "{text}");
}
