//! The schema is generated from the types and committed, and version 1 only grows.
//!
//! - `hydo/schema/agent-v1.json` must equal [`hydo_agent_proto::schema::schema_text`]: a change of
//!   the types is a change of the committed schema, reviewed in its diff. `HYDO_BLESS=1` rewrites
//!   it.
//! - `hydo/schema/released/agent-v1.json` is the schema of the last release (until the first
//!   release ships the agent, the schema of M117, which introduced it); the committed schema must
//!   be an additive change of it (`hydo_results::compat::breaking`, the check of the results
//!   format). A release copies the committed schema there (`HYDO_RELEASE_SCHEMA=1`). A breaking
//!   change needs version 2 (`agent-v2.json`), which the host reads alongside version 1.
//! - Every message kind round-trips through the codec with every field and with the required
//!   fields only, and what the codec writes is valid by the schema.

mod common;

use common::{sample, schema_dir, validate};
use hydo_agent_proto::json::{self, Value};
use hydo_agent_proto::{AgentLine, HostLine};

fn current() -> String {
    hydo_agent_proto::schema::schema_text()
}

#[test]
fn the_committed_schema_is_the_generated_one() {
    let p = schema_dir().join("agent-v1.json");
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(&p, current()).unwrap();
    }
    let want = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (write it with HYDO_BLESS=1)", p.display()));
    assert!(
        want == current(),
        "{} differs from the schema generated from the types; review the change and rewrite it \
         with HYDO_BLESS=1 cargo test -p hydo-agent-proto --test schema",
        p.display()
    );
}

#[test]
fn the_schema_only_grows_from_the_released_one() {
    let p = schema_dir().join("released/agent-v1.json");
    if std::env::var_os("HYDO_RELEASE_SCHEMA").is_some_and(|v| v == "1") {
        std::fs::write(&p, current()).unwrap();
    }
    let old: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap();
    let new: serde_json::Value = serde_json::from_str(&current()).unwrap();
    let b = hydo_results::compat::breaking(&old, &new);
    assert!(
        b.is_empty(),
        "agent protocol version 1 changed in ways an agent or host of the released version cannot \
         survive:\n  {}\nMake the change additive (a new op, event, optional field or name), or start \
         version 2.",
        b.join("\n  ")
    );
}

#[test]
fn the_compatibility_check_sees_a_breaking_change() {
    let mut new: serde_json::Value = serde_json::from_str(&current()).unwrap();
    // a required field removed from a command, and an error code removed
    new["$defs"]["command.spawn"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("argv");
    let codes = new["$defs"]["error"]["properties"]["err"]["enum"]
        .as_array_mut()
        .unwrap();
    codes.retain(|c| c != "busy");
    let old: serde_json::Value = serde_json::from_str(&current()).unwrap();
    let b = hydo_results::compat::breaking(&old, &new);
    assert!(
        b.iter().any(|l| l.contains("command.spawn.argv: removed")),
        "{b:?}"
    );
    assert!(
        b.iter().any(|l| l.contains("\"busy\" no longer allowed")),
        "{b:?}"
    );
    // a new op is an addition
    let mut grown: serde_json::Value = serde_json::from_str(&current()).unwrap();
    grown["$defs"]["command.later"] = grown["$defs"]["command.syncfs"].clone();
    grown["oneOf"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"$ref": "#/$defs/command.later"}));
    assert!(hydo_results::compat::breaking(&old, &grown).is_empty());
}

#[test]
fn every_kind_round_trips_and_is_valid() {
    let schema = hydo_agent_proto::schema::schema();
    let Value::Obj(root) = &schema else { panic!() };
    let Some(Value::Obj(defs)) = root.get("$defs") else {
        panic!()
    };
    let mut n = 0;
    for (name, def) in defs {
        for full in [true, false] {
            let inst = sample(def, full, name);
            let text = inst.to_canonical();
            validate(&schema, &inst).unwrap_or_else(|e| panic!("{name}: {e}: {text}"));
            let back = if name.starts_with("command.") {
                HostLine::decode(text.as_bytes(), &[1])
                    .unwrap_or_else(|e| panic!("{name}: {e:?}: {text}"))
                    .encode()
            } else {
                AgentLine::decode(&text)
                    .unwrap_or_else(|e| panic!("{name}: {e}: {text}"))
                    .encode()
            };
            assert_eq!(back, text, "{name}");
            assert!(json::is_canonical(back.as_bytes()));
            n += 1;
        }
    }
    // 17 commands, 17 replies, the refusal and 9 events
    assert_eq!(n, 2 * (17 + 17 + 1 + 9));
}
