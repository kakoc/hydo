//! `hydo rules scan --kind sync` (Milestone 381, tentative number): the sync audit end to end on a
//! small Go project with hydo.yaml. No VM runs.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

struct T {
    dir: PathBuf,
}

impl T {
    fn new(name: &str) -> T {
        let dir = std::env::temp_dir().join(format!("hydo-sync-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        T { dir }
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.dir.join("proj").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn hydo(&self, args: &[&str]) -> (i32, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .args(args)
            .current_dir(&self.dir)
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

const QUEUE: &str = r#"package queue

func (q *Queue) put(b []byte) error {
	if q.seg == nil {
		q.seg, _ = os.OpenFile(q.name, os.O_WRONLY|os.O_CREATE|os.O_APPEND, 0600)
	}
	_, err := q.seg.Write(b)
	return err
}

func (q *Queue) handlePut(w http.ResponseWriter, b []byte) {
	if err := q.put(b); err != nil {
		return
	}
	w.WriteHeader(http.StatusOK)
}

func (q *Queue) saveMeta() error {
	f, err := os.Create(q.name + ".tmp")
	if err != nil {
		return err
	}
	fmt.Fprintf(f, "%d", q.depth)
	if err := f.Sync(); err != nil {
		return err
	}
	f.Close()
	return os.Rename(q.name+".tmp", q.name+".meta")
}

func (q *Queue) writeSynced(b []byte) error {
	_, err := q.seg.Write(b)
	if err != nil {
		return err
	}
	return q.seg.Sync()
}
"#;

/// The audit lists the unsynced write that a handler answers and the rename without a sync of
/// the folder, aims a power loss of the service with `storage:` right after the workload's
/// event, and leaves the synced write out.
#[test]
fn the_sync_audit_aims_power_cuts_after_unsynced_answers() {
    let t = T::new("audit");
    t.write("queue/queue.go", QUEUE);
    t.write("queue/queue_test.go", "package queue\n\nfunc TestPut(t *testing.T) {\n\tf, _ := os.Create(\"x\")\n\tf.Write(b)\n}\n");
    t.write(
        "hydo/hydo.yaml",
        "format: 1\nname: q\nservices:\n  - name: queued\n    storage: {dir: vol/q, size_mib: 64}\nworkload:\n  steps: 12\n",
    );
    t.write(
        "hydo/client/main.go",
        "package main\n\nfunc main() {\n\tevent(\"queue.put_acked\", \"the queue answered a put\")\n\tevent(\"queue.started\", \"up\")\n}\n",
    );
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "sync"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.starts_with("hydo rules scan --kind sync: 2 places in 2 source files of proj"),
        "{text}"
    );
    assert!(
        text.contains("\n1. queue/queue.go:7, in put [write, high]"),
        "{text}"
    );
    assert!(
        text.contains("The answer: through handlePut (queue/queue.go:12), line 15: w.WriteHeader(http.StatusOK)"),
        "{text}"
    );
    assert!(
        text.contains("disk=cut | run A=queue.put_acked+10:powerloss-queued"),
        "{text}"
    );
    assert!(
        text.contains("disk=cut | run A=queue.put_acked+10:cut"),
        "{text}"
    );
    assert!(
        text.contains("\n2. queue/queue.go:28, in saveMeta [rename, high]"),
        "{text}"
    );
    assert!(
        text.contains("Missing: a sync of the folder after the rename."),
        "{text}"
    );
    assert!(!text.contains("writeSynced"), "{text}");
    assert!(!text.contains("queue_test.go"), "{text}");
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "sync", "--json"]);
    assert_eq!(rc, 0, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    let places = doc["sync"].as_array().unwrap();
    assert_eq!(places.len(), 2, "{text}");
    assert_eq!(places[0]["function"], "put");
    assert_eq!(places[0]["rank"], 1);
    assert_eq!(places[0]["ack"]["line"], 15);
    assert_eq!(places[0]["schedules"].as_array().unwrap().len(), 2);
    assert_eq!(doc["events"][0], "queue.put_acked");
    // without hydo.yaml the schedules cut at the middle step, with a placeholder
    std::fs::remove_dir_all(t.dir.join("proj/hydo")).unwrap();
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "sync"]);
    assert_eq!(rc, 0, "{text}");
    assert!(text.contains("disk=cut | run L=<service>@10+3"), "{text}");
    assert!(text.contains("disk=cut | run C=10"), "{text}");
    // the kinds of the scan name sync
    let (rc, text) = t.hydo(&["rules", "scan", "proj", "--kind", "nope"]);
    assert_ne!(rc, 0, "{text}");
    assert!(text.contains("clock, writes, sync"), "{text}");
}
