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
