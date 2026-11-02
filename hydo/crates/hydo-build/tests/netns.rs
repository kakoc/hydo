//! hydo/onboard/tests/test_netns.py (deleted in M107) re-expressed in Rust: `network: bridge`
//! (MANIFEST.md "Service networks"): validation, the compiled shell (the init splice until M136),
//! the default /etc/hosts, hydo-oci, and fakes on the bridge. The explorer's links of the
//! same manifests are checked in hydo-explore's `tests/netns_links.rs`.

use hydo_build::shell::{compile_shell, default_hosts, world_config, ShellTools};
use hydo_build::Ctx;
use serde_json::{json, Value};

fn base() -> Value {
    json!({
        "name": "t", "network": "bridge",
        "services": [
            {"name": "db", "cmd": "db", "ready": "true", "port": 5432, "addr": "10.89.0.11"},
            {"name": "api", "cmd": "api", "ready": "true", "port": 80, "addr": "10.89.0.12", "depends_on": ["db"]},
            {"name": "web", "cmd": "web", "ready": "true", "port": 80, "addr": "10.89.0.13", "depends_on": ["api"]},
        ],
        "workload": {"steps": 10, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    })
}

fn with(kw: Value) -> Value {
    let mut m = base();
    for (k, v) in kw.as_object().unwrap() {
        m[k] = v.clone();
    }
    m
}

fn ok(mut m: Value) -> Value {
    hydo_manifest::validate_data(&mut m).unwrap_or_else(|e| panic!("refused: {}", e.message));
    m
}

fn bad(mut m: Value, text: &str) {
    match hydo_manifest::validate_data(&mut m) {
        Ok(()) => panic!("accepted, expected a refusal with {text:?}"),
        Err(e) => assert!(e.message.contains(text), "{:?} lacks {text:?}", e.message),
    }
}

fn pay(addr: &str) -> Value {
    json!([{"name": "pay", "port": 9000, "addr": addr, "routes": [{"name": "r", "path": "/"}]}])
}

#[test]
fn validate_ok_and_same_port_twice() {
    ok(base()); // api and web both on port 80: own namespaces
}

#[test]
fn validate_modes() {
    let mut m = with(json!({"network": "loopback"}));
    for (i, s) in m["services"].as_array_mut().unwrap().iter_mut().enumerate() {
        s["addr"] = json!(format!("127.0.0.{}", 11 + i));
    }
    ok(m);
    bad(
        with(json!({"network": "host"})),
        "network: loopback or bridge",
    );
}

#[test]
fn validate_addr_required_and_in_subnet() {
    let mut m = base();
    m["services"][0].as_object_mut().unwrap().remove("addr");
    bad(m, "addr: required");
    let mut m = base();
    m["services"][0]["addr"] = json!("127.0.0.11");
    bad(m, "with network: bridge, an address in 10.89.0.0/16");
    let mut m = base();
    m["services"][0]["addr"] = json!("10.89.0.1");
    bad(m, "10.89.0.1");
    let mut m = base();
    m["services"][1]["addr"] = json!("10.89.0.11");
    bad(m, "used by two services");
}

#[test]
fn validate_loopback_unchanged() {
    let mut m = base();
    m.as_object_mut().unwrap().remove("network");
    bad(
        m,
        "must be a loopback address 127.x.y.z other than 127.0.0.1",
    );
}

#[test]
fn validate_world_on_the_bridge() {
    ok(with(
        json!({"proxies": [{"name": "dbp", "port": 6000, "service": "db", "protocol": "tcp"}]}),
    ));
    ok(with(json!({ "fakes": pay("10.89.0.50") })));
    bad(
        with(json!({ "fakes": pay("127.0.0.1") })),
        "with network: bridge, an address in 10.89.0.0/16",
    );
    bad(
        with(json!({ "fakes": pay("10.89.0.11") })),
        "is a service's address",
    );
    bad(
        with(
            json!({"proxies": [{"name": "dbp", "port": 6000, "to": "127.0.0.1:5432", "protocol": "tcp"}]}),
        ),
        "<bridge address 10.89.x.y>:<port>",
    );
}

fn shell(m: &Value) -> String {
    let order = hydo_manifest::service_order(&m["services"]).unwrap();
    let tools = ShellTools {
        tc: "TC".into(),
        fc: None,
        ip: Some("IP".into()),
    };
    compile_shell(&Ctx::standalone(Default::default()), m, &order, &tools)
        .map_err(|e| e.message())
        .unwrap()
        .0
}

#[test]
fn compile_shell_and_hosts() {
    let m = ok(with(json!({ "fakes": pay("10.89.0.50") })));
    let ms = shell(&m);
    for line in [
        "HYDO_NET=bridge",
        "HYDO_IP='IP'",
        "HYDO_NET_GW=10.89.0.1",
        "HYDO_NET_EXTRA='10.89.0.50'",
    ] {
        assert!(ms.contains(&format!("{line}\n")), "{line} not in:\n{ms}");
    }
    let h = default_hosts(&m);
    assert!(h.contains("10.89.0.11 db\n"), "{h}");
    assert!(h.contains("10.89.0.50 pay\n"), "{h}");
    assert!(world_config(&m).contains("\"addr\": \"10.89.0.50\""));
}

#[test]
fn compile_no_network_key_compiles_as_before() {
    let mut m = base();
    m.as_object_mut().unwrap().remove("network");
    for s in m["services"].as_array_mut().unwrap() {
        s.as_object_mut().unwrap().remove("addr");
    }
    let m = ok(m);
    assert!(!shell(&m).contains("HYDO_NET"));
    assert_eq!(default_hosts(&m), "127.0.0.1 localhost\n");
}

#[test]
fn hydo_oci_launchers() {
    // the two launcher texts of an image service (src/oci.rs: HYDO_OCI and hydo_oci_netns())
    let src = include_str!("../src/oci.rs");
    let start = src.find("const HYDO_OCI: &str = ").unwrap();
    let end = start + src[start..].find("\";\n").unwrap();
    let plain = &src[start..end];
    assert!(plain.contains("exec /etc/hydo/oci/$s/exec"));
    assert!(!plain.contains("nsenter"));
    let f = src.find("fn hydo_oci_netns() -> String {").unwrap();
    let body = &src[f..f + src[f..].find("\n}\n").unwrap()];
    assert!(body.contains("nsenter -n/tmp/hydo/ns/$s /etc/hydo/oci/$s/exec"));
}
