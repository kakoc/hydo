//! The line `hydo explore` prints before its runs (Milestone 247): the fault kinds it draws in
//! words, and what `--new-faults` adds; before, it printed the fault space as JSON, which is now
//! `fault-space.json` in the exploration's folder.

use hydo_explore::space::Space;
use serde_json::json;

fn redis(faults: Option<serde_json::Value>) -> Space {
    let mut m = json!({
        "name": "redis",
        "services": [{"name": "redis", "cmd": "redis-server", "ready": "true", "port": 6379}],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
        "knobs": [{"token": "R", "name": "Redis appendfsync", "values": ["always", "everysec", "no"], "default": "always", "set": "true"}],
    });
    if let Some(f) = faults {
        m["faults"] = f;
    }
    Space::from_text(&serde_json::to_string(&m).unwrap(), None).unwrap()
}

#[test]
fn the_kinds_it_draws_in_words() {
    let sp = redis(None);
    assert_eq!(
        sp.plain(false, "random"),
        "hydo explore: the faults it draws: crashes, network faults, power cuts, disk faults and \
         your setting R (service: redis). --new-faults adds pauses, clock jumps and one-way \
         network faults."
    );
    // with the option, or a strategy that draws them, they are in the list
    for (nf, strategy) in [(true, "random"), (false, "swarm-pos")] {
        let l = sp.plain(nf, strategy);
        assert!(
            l.contains("disk faults, pauses, clock jumps, one-way network faults and"),
            "{l}"
        );
        assert!(!l.contains("--new-faults"), "{l}");
    }
    // a project's `faults:` list keeps the others out
    assert_eq!(
        redis(Some(json!(["kill", "cut", "pause"]))).plain(false, "swarm"),
        "hydo explore: the faults it draws: crashes, power cuts and your setting R (service: \
         redis). --new-faults adds pauses."
    );
    // no JSON on the line
    assert!(!sp.plain(false, "random").contains('{'));
}

/// Milestone 301: 0.4.0's trial explored web-shop's walk, `faults: [protocol, clock, pause]`,
/// with `--protocol-faults`. The line said "none of the project's kinds", and the explorer then
/// drew `H=` faults. The options that draw kinds of their own are in the list now, and a kind of
/// `faults:` that only an option draws names that option.
#[test]
fn the_options_that_draw_their_own_kinds_are_in_the_line() {
    let m = json!({
        "name": "web-shop-walk",
        "workload": {"steps": 4, "run": "true"},
        "faults": ["protocol", "clock", "pause"],
        "invariants": [{"id": "x", "cmd": "true"}],
    });
    let sp = Space::from_text(&serde_json::to_string(&m).unwrap(), None).unwrap();
    let with = sp.plain_with(&hydo_explore::space::Drawn {
        strategy: "random".into(),
        protocol: true,
        ..Default::default()
    });
    assert_eq!(
        with,
        "hydo explore: the faults it draws: faults of the outside services. --new-faults adds \
         pauses and clock jumps."
    );
    let without = sp.plain(false, "random");
    assert_eq!(
        without,
        "hydo explore: it draws no fault. --new-faults adds pauses and clock jumps. \
         --protocol-faults adds faults of the outside services."
    );
    let more = sp.plain_with(&hydo_explore::space::Drawn {
        strategy: "random".into(),
        storage: true,
        upgrades: true,
        workload_pauses: true,
        ..Default::default()
    });
    assert!(
        more.contains("the faults it draws: storage faults, pauses of the workload and upgrades."),
        "{more}"
    );
}

/// Milestone 303: a manifest whose `guest:` names a parameter (`cpu: "{{cpu}}"`, so that one
/// manifest builds x86_64 with v3 and arm64 with v2) is read with the image's parameter values;
/// read literally, the explorer stopped at the validation of `guest.cpu`.
#[test]
fn a_guest_key_from_a_parameter_is_read_as_built() {
    let m = json!({
        "name": "p",
        "params": {"cpu": "v3"},
        "guest": {"cpu": "{{cpu}}"},
        "services": [{"name": "s", "cmd": "s", "ready": "true", "port": 1}],
        "workload": {"steps": 4, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    });
    let text = serde_json::to_string(&m).unwrap();
    let e = Space::from_text(&text, None).err().unwrap();
    assert!(e.contains("guest.cpu"), "{e}");
    let built = [("cpu".to_string(), "v2".to_string())];
    let sp = Space::from_text_params(&text, None, &built).unwrap();
    assert!(sp.plain(false, "random").contains("service: s"));
    assert!(
        Space::from_text_params(&text, None, &[]).is_ok(),
        "the parameter's default applies"
    );
}
