//! The explorer and `emulators:` (Milestone 242): an emulator's service stands for the outside
//! world, so no kill, pause, freeze or network fault is drawn on it; its faults are `H=` tokens on
//! its proxy, which `--protocol-faults` draws as for any proxy.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use serde_json::{json, Value};

const STRATS: [&str; 4] = ["random", "swarm", "swarm-pos", "process"];

fn system() -> Value {
    json!({
        "name": "s",
        "services": [{"name": "app", "cmd": "a", "ready": "true", "port": 8080}],
        "emulators": [{"use": "stripe", "ref": "form:metadata[order]"}, {"use": "s3"}],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    })
}

#[test]
fn an_emulator_s_service_is_never_a_fault_target() {
    let m = system();
    let sp = Space::from_text(&serde_json::to_string(&m).unwrap(), None).unwrap();
    assert_eq!(sp.svcs, vec!["app".to_string()]);
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.new_faults = true;
    e.protocol_faults = true;
    let mut h = 0;
    for st in STRATS {
        for seed in 1..=300u64 {
            let text = e.render(&e.compose_seed(st, seed, false));
            assert!(!text.contains("_emulator"), "{st} {seed}: {text}");
            if text.contains("H=stripe") || text.contains("H=s3") {
                h += 1;
            }
        }
    }
    assert!(h > 0, "no H= token on an emulator's proxy");
}
