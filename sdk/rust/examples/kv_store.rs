use hydo_assert::{declare, details};
use std::collections::HashMap;

declare! {
    static READ_BACK: always("kv.read_back", "every acknowledged write reads back with its value");
    static OVERWRITE: sometimes("kv.overwrite", "a write replaced an existing value");
    static LOST_REPLY: buggify("kv.lost_reply", "the write lands but its reply is lost");
}

fn main() {
    hydo_assert::catalog(); // list every property, also the ones that never run
    let mut store = HashMap::new();
    hydo_assert::setup_complete(); // the system is ready: hydo may start injecting faults

    for (key, value) in [("a", 1), ("b", 2), ("a", 3)] {
        let old = store.insert(key, value);
        OVERWRITE.check(old.is_some(), details! {"key": key});
        if LOST_REPLY.fire(details! {"key": key}) {
            continue; // the client never hears back (only when hydo turns this point on)
        }
        let acked = store.get(key) == Some(&value);
        READ_BACK.check(acked, details! {"key": key, "value": value});
    }
}
