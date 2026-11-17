//! Function fuzzing inside hydo's virtual machine (Milestone 292, docs/design/function-fuzzing.md).
//!
//! Three functions that write files, each with a durability check that `hydo fuzz --vm` calls
//! after the run: in the recovery boot after a power cut, or at the end of a run without one.
//! Every call runs in an empty folder of its own, so each function opens its files relative to
//! the current folder.
//!
//! - [`commit_all`] commits transactions to redb 4.6.0, the embedded key-value store, with its
//!   default durability.
//! - [`save`] replaces a file by writing a temporary file and renaming it with tempfile 3.27.0,
//!   and syncs nothing: a common way to save a file.
//! - [`save_synced`] does the same and syncs the file and its folder.
//!
//! ```text
//! hydo fuzz --vm hydo/onboard/examples/fuzz-vm
//! ```

use redb::{
    Database, DatabaseError, ReadOnlyDatabase, ReadableDatabase, StorageError, TableDefinition,
};
use std::io::Write as _;

const TABLE: TableDefinition<u32, u64> = TableDefinition::new("t");
const DB: &str = "db.redb";

/// The key of the k-th key of transaction `tx`: each transaction has keys of its own.
fn key(tx: usize, k: u16) -> u32 {
    ((tx as u32) << 16) | k as u32
}

fn val(value: u64, key: u32) -> u64 {
    value.wrapping_add(key as u64)
}

/// Commits each list of keys as one write transaction of redb, with its default durability
/// (a commit returns after redb synced it), and returns how many commits returned `Ok`. It stops
/// at the first error.
// hydo: input txs len 1..=6
// hydo: input txs[] len 1..=24
// hydo: ensures result <= len(txs)
// hydo: durable committed_survive
pub fn commit_all(txs: Vec<Vec<u16>>, value: u64) -> u32 {
    let Ok(db) = Database::create(DB) else {
        return 0;
    };
    let mut done = 0;
    for (i, keys) in txs.iter().enumerate() {
        let Ok(tx) = db.begin_write() else {
            break;
        };
        let mut ok = true;
        {
            let Ok(mut t) = tx.open_table(TABLE) else {
                break;
            };
            for k in keys {
                let key = key(i, *k);
                if t.insert(key, val(value, key)).is_err() {
                    ok = false;
                    break;
                }
            }
        }
        if !ok || tx.commit().is_err() {
            break;
        }
        done += 1;
    }
    done
}

/// How much of one transaction the database holds.
#[derive(PartialEq)]
enum Held {
    All,
    Nothing,
    Part,
}

/// After the run: every transaction whose commit returned `Ok` is in the database, the
/// transaction after them is there whole or not at all, and no later one is there. `done` is
/// `commit_all`'s result, or `None` when the call did not return (a power cut in the middle of
/// it): then the transactions in the database are whole and come first.
// hydo: ensures result
pub fn committed_survive(txs: Vec<Vec<u16>>, value: u64, done: Option<u32>) -> Option<bool> {
    let acked = done.unwrap_or(0) as usize;
    // opened for writing, redb repairs a file that a power cut left; a file system that a disk
    // error made read-only allows only the read-only open
    // (each database stays open while its read transaction reads: a dropped one closes the file)
    let (rw, ro);
    let read = match Database::create(DB) {
        Ok(db) => {
            rw = db;
            rw.begin_read().ok()
        }
        Err(_) => match ReadOnlyDatabase::open(DB) {
            Ok(db) => {
                ro = db;
                ro.begin_read().ok()
            }
            // after disk errors the file system answers reads with EIO: nothing can be judged
            Err(DatabaseError::Storage(StorageError::Io(_))) => return None,
            Err(_) => None,
        },
    };
    // the file cannot be read: only a call without a commit may leave that
    let Some(r) = read else {
        return Some(acked == 0);
    };
    let table = r.open_table(TABLE).ok();
    let held: Vec<Held> = txs
        .iter()
        .enumerate()
        .map(|(i, keys)| {
            let n = keys
                .iter()
                .filter(|k| {
                    let key = key(i, **k);
                    table
                        .as_ref()
                        .and_then(|t| t.get(key).ok().flatten())
                        .is_some_and(|v| v.value() == val(value, key))
                })
                .count();
            if n == keys.len() {
                Held::All
            } else if n == 0 {
                Held::Nothing
            } else {
                Held::Part
            }
        })
        .collect();
    if held.contains(&Held::Part) {
        return Some(false);
    }
    // whole transactions, in order: no gap before a transaction that is there
    let first_missing = held.iter().position(|h| *h == Held::Nothing).unwrap_or(held.len());
    if held[first_missing..].contains(&Held::All) {
        return Some(false);
    }
    let there = first_missing;
    Some(match done {
        Some(n) => there >= n as usize && there <= n as usize + 1,
        None => true,
    })
}

/// Saves `text` as the file `saved.txt` by writing a temporary file and renaming it over the
/// old one, so that a reader sees the old text or the new one. It syncs nothing.
// hydo: input text len 1..=300
// hydo: durable save_survives
pub fn save(text: String) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new_in(".").map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    f.persist("saved.txt").map_err(|e| e.to_string())?;
    Ok(())
}

/// After the run: a save that returned `Ok` left the whole text.
// hydo: ensures result
pub fn save_survives(text: String, done: Option<Result<(), String>>) -> Option<bool> {
    match done {
        Some(Ok(())) => Some(std::fs::read("saved.txt").is_ok_and(|b| b == text.as_bytes())),
        // a save that failed or did not return: the old text (none here) or the whole new one
        _ => match std::fs::read("saved.txt") {
            Ok(b) => Some(b == text.as_bytes()),
            Err(_) => None,
        },
    }
}

/// [`save`], with the temporary file synced before the rename and the folder synced after it.
// hydo: input text len 1..=300
// hydo: durable save_synced_survives
pub fn save_synced(text: String) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new_in(".").map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    f.as_file().sync_all().map_err(|e| e.to_string())?;
    f.persist("saved.txt").map_err(|e| e.to_string())?;
    std::fs::File::open(".")
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// After the run: a synced save that returned `Ok` left the whole text.
// hydo: ensures result
pub fn save_synced_survives(text: String, done: Option<Result<(), String>>) -> Option<bool> {
    save_survives(text, done)
}
