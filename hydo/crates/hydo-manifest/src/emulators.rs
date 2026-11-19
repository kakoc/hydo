//! Ready-made emulators of outside services (Milestone 242, docs/design/emulators.md).
//!
//! An `emulators:` entry names a catalog entry (`use: stripe`). The validator turns it into what a
//! project would otherwise write by hand:
//!
//! - a service `<name>_emulator` that runs a pinned emulator from its Docker/OCI image (the image
//!   file is `emulators/<file>-<arch>.docker.tar` next to the manifest; `hydo build` fetches a
//!   missing one by its digest);
//! - a fault proxy `<name>` in front of it, with the provider's routes, its idempotency header
//!   and, for a provider that its SDKs call by a fixed HTTPS name, `tls:`. `H=<name>...` faults,
//!   the record and the side-effect checks then work as for a fake.
//!
//! The emulator answers like the provider; the proxy makes the provider's failures. The
//! expansion consumes `emulators:` and leaves `_emulators` (the expanded names and the image
//! pins) in the validated data: the explorer keeps its faults off those services (they stand for
//! the outside world, whose faults are `H=`), and `hydo build` reads the pins.

use serde_json::{json, Map, Value};

use crate::py::{self, repr_str, str_of};
use crate::validate::{err, R};

/// The keys of an `emulators:` entry.
pub const KNOWN: &[&str] = &[
    "use",
    "name",
    "port",
    "ref",
    "tls",
    "buckets",
    "addr",
    "description",
];

/// The validated data's key that lists the expanded emulators.
pub const META: &str = "_emulators";

/// One emulator of the catalog.
pub struct Entry {
    /// The `use:` value.
    pub id: &'static str,
    /// The emulator: its project and license (what `hydo build` fetches is the project's own
    /// published image; hydo ships none of it).
    pub project: &'static str,
    pub license: &'static str,
    /// The registry repository, the tag recorded in the archive, and the manifest digest of
    /// each architecture (hydo's names: x86_64, arm64).
    pub repo: &'static str,
    pub tag: &'static str,
    pub digests: [(&'static str, &'static str); 2],
    /// The archive's file name without `-<arch>.docker.tar`.
    pub file: &'static str,
    /// The port the system calls (the proxy's plain listener) and the emulator's own port.
    pub port: i64,
    pub inner: i64,
    /// The proxy's protocol: http or tcp.
    pub protocol: &'static str,
    /// The host names its SDKs call over HTTPS (`tls:`), empty for none.
    pub tls_hosts: &'static [&'static str],
    /// Where a request's idempotency key is, if the provider keeps keys.
    pub idempotency: Option<&'static str>,
    /// The proxy's routes: name, method, path, idempotent.
    pub routes: &'static [(&'static str, &'static str, &'static str, bool)],
}

const STRIPE_ROUTES: &[(&str, &str, &str, bool)] = &[
    ("payment_intent", "POST", "/v1/payment_intents", true),
    (
        "payment_intent_confirm",
        "POST",
        "/v1/payment_intents/{id}/confirm",
        true,
    ),
    (
        "payment_intent_capture",
        "POST",
        "/v1/payment_intents/{id}/capture",
        true,
    ),
    (
        "payment_intent_cancel",
        "POST",
        "/v1/payment_intents/{id}/cancel",
        true,
    ),
    ("charge", "POST", "/v1/charges", true),
    ("refund", "POST", "/v1/refunds", true),
    ("customer", "POST", "/v1/customers", true),
    ("checkout_session", "POST", "/v1/checkout/sessions", true),
    ("subscription", "POST", "/v1/subscriptions", true),
    ("invoice", "POST", "/v1/invoices", true),
    ("transfer", "POST", "/v1/transfers", true),
    ("payout", "POST", "/v1/payouts", true),
    ("other_post", "POST", "/v1/*", true),
    ("delete", "DELETE", "/v1/*", false),
    ("read", "GET", "/v1/*", false),
];

const S3_ROUTES: &[(&str, &str, &str, bool)] = &[
    ("create_bucket", "PUT", "/{bucket}", false),
    ("put_object", "PUT", "/*", false),
    ("post", "POST", "/*", false),
    ("delete", "DELETE", "/*", false),
    ("head", "HEAD", "/*", false),
    ("read", "GET", "/*", false),
];

/// The catalog, by `use:`.
pub const CATALOG: &[Entry] = &[
    Entry {
        id: "stripe",
        project: "stripe-mock (https://github.com/stripe/stripe-mock)",
        license: "MIT",
        repo: "docker.io/stripe/stripe-mock",
        tag: "stripe/stripe-mock:v0.206.0",
        digests: [
            (
                "x86_64",
                "sha256:818639c8a513394ec2abda3b42482c0ff32e24f3cb25aca09f91596d7fca2a4e",
            ),
            (
                "arm64",
                "sha256:f6f9903bd335e86f8eb2602c53d8193c6e3f3279c645bdb7a4a888f2650722b6",
            ),
        ],
        file: "stripe-mock-v0.206.0",
        port: 12111,
        inner: 22111,
        protocol: "http",
        tls_hosts: &["api.stripe.com"],
        idempotency: Some("header:Idempotency-Key"),
        routes: STRIPE_ROUTES,
    },
    Entry {
        id: "smtp",
        project: "Mailpit (https://github.com/axllent/mailpit)",
        license: "MIT",
        repo: "docker.io/axllent/mailpit",
        tag: "axllent/mailpit:v1.33.1",
        digests: [
            (
                "x86_64",
                "sha256:c95e8356f55b65520e8b7fd4d74d3e2d06e4a83513f639e316b1e592fbd52646",
            ),
            (
                "arm64",
                "sha256:6bb6d70382fa3915b00389bbc435d41d9b3bc4a599a71f97e82881b80ebe428d",
            ),
        ],
        file: "mailpit-v1.33.1",
        port: 1025,
        inner: 11025,
        protocol: "tcp",
        tls_hosts: &[],
        idempotency: None,
        routes: &[],
    },
    Entry {
        id: "s3",
        project: "rclone serve s3 (https://github.com/rclone/rclone)",
        license: "MIT",
        repo: "docker.io/rclone/rclone",
        tag: "rclone/rclone:1.76.0",
        digests: [
            (
                "x86_64",
                "sha256:075cf0e4e0f623996646cfb8a434c71cb4fcfe75bbbe27ccbb296ca04a7cd260",
            ),
            (
                "arm64",
                "sha256:fbcca19e4dc192f6a12169a7e033e713877e07abca796928bbed27d2a334c8e0",
            ),
        ],
        file: "rclone-1.76.0",
        port: 9000,
        inner: 19000,
        protocol: "http",
        tls_hosts: &[],
        idempotency: None,
        routes: S3_ROUTES,
    },
];

/// The HTTP API of an smtp emulator (Mailpit's own port), for checks over what it received.
pub const SMTP_API_PORT: i64 = 8025;

/// The catalog entry of a `use:` value.
pub fn entry(id: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|e| e.id == id)
}

/// The architecture names of hydo and of the registries.
fn oci_arch(arch: &str) -> &'static str {
    if arch == "arm64" {
        "arm64"
    } else {
        "amd64"
    }
}

/// The image file of an entry for one architecture, relative to the manifest's folder.
pub fn image_file(e: &Entry, arch: &str) -> String {
    format!("emulators/{}-{}.docker.tar", e.file, oci_arch(arch))
}

/// The command line the emulator runs (the image's entrypoint is replaced). `host` is where it
/// listens: 127.0.0.1, or with network: bridge every address of its own network namespace, so
/// that its proxy (on the bridge's address) and its readiness check reach it at its `addr:`.
fn entrypoint(e: &Entry, inner: i64, name: &str, host: &str) -> Vec<String> {
    let v: Vec<String> = match e.id {
        "stripe" => vec![
            "/bin/stripe-mock".into(),
            "-http-port".into(),
            inner.to_string(),
        ],
        "smtp" => vec![
            "/mailpit".into(),
            "--smtp".into(),
            format!("{host}:{inner}"),
            "--listen".into(),
            format!("{host}:{SMTP_API_PORT}"),
            "--smtp-auth-accept-any".into(),
            "--smtp-auth-allow-insecure".into(),
            "--disable-version-check".into(),
        ],
        _ => vec![
            "rclone".into(),
            "serve".into(),
            "s3".into(),
            "--addr".into(),
            format!("{host}:{inner}"),
            format!("/data/{name}"),
        ],
    };
    v
}

/// An S3 bucket name as rclone and S3 accept it.
fn bucket_ok(b: &str) -> bool {
    let n = b.len();
    (3..=63).contains(&n)
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        && b.as_bytes()[0].is_ascii_alphanumeric()
        && b.as_bytes()[n - 1].is_ascii_alphanumeric()
}

fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}

/// Replace `emulators:` by its services, proxies, data directories and environment, and record
/// `_emulators`. A manifest without `emulators:` is not touched.
pub fn expand(m: &mut Value) -> R<()> {
    let Some(list) = m.as_object_mut().and_then(|o| o.remove("emulators")) else {
        return Ok(());
    };
    if list.is_null() {
        return Ok(());
    }
    let Value::Array(list) = list else {
        return Err(err(
            "emulators: a list of {use: stripe | smtp | s3, name:, port:, ref:, ...}",
        ));
    };
    let bridge = m.get("network").and_then(Value::as_str) == Some("bridge");
    // network: bridge gives every service an address (Milestone 316): an emulator's service
    // takes its entry's `addr:`, or the first free one of 10.89.1.1, 10.89.1.2, ... that no
    // service, fake or proxy of the manifest and no other emulator has
    let mut used: Vec<String> = Vec::new();
    for kind in ["services", "fakes", "proxies"] {
        if let Some(Value::Array(a)) = m.get(kind) {
            used.extend(a.iter().filter_map(|x| get(x, "addr")).map(str_of));
        }
    }
    used.extend(
        list.iter()
            .filter_map(|x| get(x, "addr"))
            .filter(|v| v.is_string())
            .map(str_of),
    );
    let mut services: Vec<Value> = Vec::new();
    let mut proxies: Vec<Value> = Vec::new();
    let mut dirs: Vec<Value> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    let mut meta: Vec<Value> = Vec::new();
    let mut hosts: Vec<(String, String)> = Vec::new();
    let mut per_kind: Vec<&str> = Vec::new();
    for (i, x) in list.iter().enumerate() {
        let wh = format!("emulators[{i}]");
        if !x.is_object() {
            return Err(err(format!("{wh}: must be a mapping")));
        }
        crate::validate::unknown(x, KNOWN, &wh)?;
        let id = match get(x, "use") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let Some(e) = entry(&id) else {
            let ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
            return Err(err(format!(
                "{wh}: use: one of {} (got {})",
                ids.join(", "),
                if id.is_empty() {
                    "nothing".to_string()
                } else {
                    repr_str(&id)
                }
            )));
        };
        let name = match get(x, "name") {
            None => id.clone(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => return Err(err(format!("{wh}: name {}: a name", py::repr(v)))),
        };
        if !crate::pyre::fullmatch(r"[a-z][a-z0-9_]*", &name) || name.len() > 40 {
            return Err(err(format!(
                "{wh}: name {}: [a-z][a-z0-9_]*, at most 40 characters (H= tokens name it)",
                repr_str(&name)
            )));
        }
        let port = match get(x, "port") {
            None => e.port,
            Some(v) => match v.as_i64() {
                Some(p) if (1..=65535).contains(&p) && v.is_i64() => p,
                _ => return Err(err(format!("{wh}: port: a TCP port number"))),
            },
        };
        // a second emulator of one kind gets the next own port
        let nth = per_kind.iter().filter(|k| **k == e.id).count() as i64;
        per_kind.push(e.id);
        let inner = e.inner + nth;
        let tls = match get(x, "tls") {
            None => !e.tls_hosts.is_empty(),
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(err(format!("{wh}: tls: true or false"))),
        };
        if tls && e.tls_hosts.is_empty() {
            return Err(err(format!(
                "{wh}: tls: only for use: stripe (the {id} emulator is called over plain TCP)"
            )));
        }
        let mut buckets: Vec<String> = Vec::new();
        if let Some(b) = get(x, "buckets") {
            if e.id != "s3" {
                return Err(err(format!("{wh}: buckets: only for use: s3")));
            }
            let Value::Array(a) = b else {
                return Err(err(format!("{wh}: buckets: a list of bucket names")));
            };
            for v in a {
                let s = str_of(v);
                if !v.is_string() || !bucket_ok(&s) {
                    return Err(err(format!(
                        "{}: buckets: {} is not a bucket name (3 to 63 of a-z, 0-9, '-' and '.')",
                        wh,
                        py::repr(v)
                    )));
                }
                buckets.push(s);
            }
        }
        let addr = match get(x, "addr") {
            Some(Value::String(a)) => Some(a.clone()),
            Some(v) => return Err(err(format!("{wh}: addr {}: an address", py::repr(v)))),
            None if bridge => Some(free_addr(&mut used)),
            None => None,
        };
        let svc = format!("{name}_emulator");
        let mut images = Map::new();
        let mut pins = Map::new();
        for (arch, digest) in e.digests {
            images.insert(arch.into(), Value::String(image_file(e, arch)));
            pins.insert(
                arch.into(),
                json!({"digest": digest, "path": image_file(e, arch)}),
            );
        }
        let mut s = json!({
            "name": svc,
            "image": Value::Object(images),
            "entrypoint": entrypoint(e, inner, &name, if bridge { "0.0.0.0" } else { "127.0.0.1" }),
            "ready": format!("tcp:{inner}"),
            "port": inner,
            "description": format!("the {id} emulator: {} ({}), pinned by hydo", e.project, e.license),
        });
        if let Some(a) = &addr {
            s["addr"] = Value::String(a.clone());
            if bridge {
                hosts.push((a.clone(), svc.clone()));
                hosts.push((crate::validate::NET_GW.to_string(), name.clone()));
            }
        }
        if e.id == "s3" {
            let mut cmd = format!("mkdir -p /data/{name}");
            for b in &buckets {
                cmd.push_str(&format!(" /data/{name}/{b}"));
            }
            s["setup"] = Value::String(cmd);
            dirs.push(Value::String(name.clone()));
            let host = if bridge { "10.89.0.1" } else { "127.0.0.1" };
            for (k, v) in [
                ("AWS_ENDPOINT_URL_S3", format!("http://{host}:{port}")),
                ("AWS_ACCESS_KEY_ID", "hydo".to_string()),
                ("AWS_SECRET_ACCESS_KEY", "hydo-emulator".to_string()),
                ("AWS_REGION", "us-east-1".to_string()),
            ] {
                if !env.iter().any(|(x, _)| x == k) {
                    env.push((k.to_string(), v));
                }
            }
        }
        services.push(s);
        let mut p = json!({
            "name": name,
            "port": port,
            "protocol": e.protocol,
            "service": svc,
            "description": format!("the {id} emulator's fault proxy (emulators:)"),
        });
        if let Some(k) = e.idempotency {
            p["idempotency"] = Value::String(k.into());
        }
        if let Some(r) = get(x, "ref") {
            p["ref"] = r.clone();
        }
        if !e.routes.is_empty() {
            let routes: Vec<Value> = e
                .routes
                .iter()
                .map(|(n, method, path, idem)| {
                    let mut r = json!({"name": n, "method": method, "path": path});
                    if *idem {
                        r["idempotent"] = Value::Bool(true);
                    }
                    r
                })
                .collect();
            p["routes"] = Value::Array(routes);
        }
        if tls {
            p["tls"] = json!({ "hosts": e.tls_hosts });
        }
        proxies.push(p);
        meta.push(json!({
            "use": id,
            "name": name,
            "service": svc,
            "repo": e.repo,
            "tag": e.tag,
            "images": Value::Object(pins),
        }));
    }
    let Some(o) = m.as_object_mut() else {
        return Ok(());
    };
    // the emulators start first: the system may call them while it starts
    let mut svcs = services;
    match o.get("services") {
        Some(Value::Array(a)) => svcs.extend(a.iter().cloned()),
        Some(v) if !v.is_null() => {
            return Err(err(
                "services: must be a list (it may be empty: a project that is only a workload)",
            ))
        }
        _ => {}
    }
    o.insert("services".into(), Value::Array(svcs));
    if !proxies.is_empty() {
        let mut all = match o.get("proxies") {
            Some(Value::Array(a)) => a.clone(),
            Some(v) if !v.is_null() => return Err(err("proxies: must be a list")),
            _ => Vec::new(),
        };
        all.extend(proxies);
        o.insert("proxies".into(), Value::Array(all));
    }
    if !dirs.is_empty() {
        let data = o.entry("data").or_insert_with(|| json!({}));
        if data.is_null() {
            *data = json!({});
        }
        let Some(d) = data.as_object_mut() else {
            return Err(err("data: must be a mapping"));
        };
        let mut have = match d.get("dirs") {
            Some(Value::Array(a)) => a.clone(),
            Some(v) if !v.is_null() => return Err(err("data.dirs: must be a list")),
            _ => Vec::new(),
        };
        for x in dirs {
            if !have.contains(&x) {
                have.push(x);
            }
        }
        d.insert("dirs".into(), Value::Array(have));
    }
    if !env.is_empty() {
        let e = o.entry("env").or_insert_with(|| json!({}));
        if e.is_null() {
            *e = json!({});
        }
        let Some(eo) = e.as_object_mut() else {
            return Err(err("env: must be a mapping"));
        };
        for (k, v) in env {
            eo.entry(k).or_insert(Value::String(v));
        }
    }
    add_hosts(o, &hosts);
    o.insert(META.into(), Value::Array(meta));
    Ok(())
}

/// The first address of 10.89.1.1, 10.89.1.2, ... (then 10.89.2.1, ...) that is not in `used`,
/// marked used.
fn free_addr(used: &mut Vec<String>) -> String {
    for n in 257..65534u32 {
        let (hi, lo) = (n / 256, n % 256);
        if lo == 0 || lo == 255 {
            continue;
        }
        let a = format!("10.89.{hi}.{lo}");
        if !used.contains(&a) {
            used.push(a.clone());
            return a;
        }
    }
    // the bridge's /16 is full; the validator then names the address that two services share
    used.first().cloned().unwrap_or_default()
}

/// A manifest that writes its own /etc/hosts (`root.text`, as `hydo compose` writes it for
/// network: bridge) gets a line for each name an emulator adds that it does not name yet: the
/// emulator's service at its address and the proxy at the bridge's address, where hydo's proxies
/// listen. Without such a file the default /etc/hosts names every service and proxy already.
fn add_hosts(o: &mut Map<String, Value>, hosts: &[(String, String)]) {
    let Some(Value::String(h)) = o
        .get_mut("root")
        .and_then(|r| r.get_mut("text"))
        .and_then(|t| t.get_mut("/etc/hosts"))
    else {
        return;
    };
    let named = |h: &str, n: &str| {
        h.lines().any(|l| {
            l.split('#')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .skip(1)
                .any(|w| w == n)
        })
    };
    for (a, n) in hosts {
        if !named(h, n) {
            if !h.is_empty() && !h.ends_with('\n') {
                h.push('\n');
            }
            h.push_str(&format!("{a} {n}\n"));
        }
    }
}

/// The services that stand for the outside world (the expanded emulators' services) of a
/// validated manifest.
pub fn services(m: &Value) -> Vec<String> {
    match m.get(META) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.get("service").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// An emulator image a build needs: the registry repository, the digest of the build's
/// architecture, the tag the archive records, and its path relative to the manifest's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub service: String,
    pub repo: String,
    pub digest: String,
    pub tag: String,
    pub path: String,
}

/// The emulator images of a validated manifest for one architecture (x86_64 or arm64).
pub fn images(m: &Value, arch: &str) -> Vec<Image> {
    let Some(Value::Array(a)) = m.get(META) else {
        return Vec::new();
    };
    a.iter()
        .filter_map(|x| {
            let pin = x.get("images")?.get(arch)?;
            Some(Image {
                service: str_of(x.get("service")?),
                repo: str_of(x.get("repo")?),
                digest: str_of(pin.get("digest")?),
                tag: str_of(x.get("tag")?),
                path: str_of(pin.get("path")?),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base(emulators: Value) -> Value {
        json!({
            "name": "shop",
            "services": [{"name": "app", "cmd": "/bin/app", "ready": "tcp:8080", "port": 8080}],
            "workload": {"steps": 4, "run": "true"},
            "invariants": [{"id": "ok", "cmd": "true"}],
            "emulators": emulators,
        })
    }

    fn expanded(emulators: Value) -> Value {
        let mut m = base(emulators);
        crate::validate_data(&mut m).unwrap();
        m
    }

    fn refused(emulators: Value) -> String {
        let mut m = base(emulators);
        crate::validate_data(&mut m).unwrap_err().message
    }

    #[test]
    fn stripe_becomes_a_service_and_an_https_proxy() {
        let m = expanded(json!([{"use": "stripe", "ref": "form:metadata[order]"}]));
        assert!(m.get("emulators").is_none());
        let s = &m["services"][0];
        assert_eq!(s["name"], "stripe_emulator");
        assert_eq!(s["ready"], "tcp:22111");
        assert_eq!(
            s["entrypoint"],
            json!(["/bin/stripe-mock", "-http-port", "22111"])
        );
        assert_eq!(
            s["image"],
            json!({"x86_64": "emulators/stripe-mock-v0.206.0-amd64.docker.tar",
                   "arm64": "emulators/stripe-mock-v0.206.0-arm64.docker.tar"})
        );
        // the system's own service starts after the emulator
        assert_eq!(m["services"][1]["name"], "app");
        let p = &m["proxies"][0];
        assert_eq!(p["name"], "stripe");
        assert_eq!(p["port"], 12111);
        assert_eq!(p["service"], "stripe_emulator");
        assert_eq!(p["idempotency"], "header:Idempotency-Key");
        assert_eq!(p["ref"], "form:metadata[order]");
        assert_eq!(p["tls"], json!({"hosts": ["api.stripe.com"]}));
        assert_eq!(
            p["routes"][0],
            json!({"name": "payment_intent", "method": "POST", "path": "/v1/payment_intents", "idempotent": true})
        );
        assert_eq!(services(&m), ["stripe_emulator"]);
        let imgs = images(&m, "arm64");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].repo, "docker.io/stripe/stripe-mock");
        assert!(imgs[0].digest.starts_with("sha256:f6f9903b"));
        assert_eq!(
            imgs[0].path,
            "emulators/stripe-mock-v0.206.0-arm64.docker.tar"
        );
        // validating the validated data again changes nothing
        let mut again = m.clone();
        crate::validate_data(&mut again).unwrap();
        assert_eq!(again, m);
    }

    #[test]
    fn s3_and_smtp() {
        let m = expanded(json!([
            {"use": "s3", "buckets": ["receipts", "invoices"]},
            {"use": "smtp", "name": "email", "port": 2525},
            {"use": "s3", "name": "archive", "port": 9100}
        ]));
        let names: Vec<&str> = m["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["s3_emulator", "email_emulator", "archive_emulator", "app"]
        );
        assert_eq!(
            m["services"][0]["setup"],
            "mkdir -p /data/s3 /data/s3/receipts /data/s3/invoices"
        );
        // a second emulator of a kind gets the next own port
        assert_eq!(m["services"][2]["port"], 19001);
        assert_eq!(m["data"]["dirs"], json!(["s3", "archive"]));
        assert_eq!(m["env"]["AWS_ENDPOINT_URL_S3"], "http://127.0.0.1:9000");
        let p = &m["proxies"][1];
        assert_eq!(p["name"], "email");
        assert_eq!(p["protocol"], "tcp");
        assert_eq!(p["port"], 2525);
        assert!(p.get("routes").is_none());
        assert_eq!(m["proxies"][2]["port"], 9100);
    }

    #[test]
    fn the_manifest_s_own_env_wins() {
        let mut m = base(json!([{"use": "s3", "port": 9100}]));
        m["env"] = json!({"AWS_REGION": "eu-west-1"});
        crate::validate_data(&mut m).unwrap();
        assert_eq!(m["env"]["AWS_REGION"], "eu-west-1");
        assert_eq!(m["env"]["AWS_ENDPOINT_URL_S3"], "http://127.0.0.1:9100");
    }

    #[test]
    fn refusals() {
        assert_eq!(
            refused(json!([{"use": "paypal"}])),
            "emulators[0]: use: one of stripe, smtp, s3 (got 'paypal')"
        );
        assert!(refused(json!([{"use": "stripe", "bogus": 1}]))
            .starts_with("emulators[0]: unknown field(s) bogus"));
        assert_eq!(
            refused(json!([{"use": "s3", "tls": true}])),
            "emulators[0]: tls: only for use: stripe (the s3 emulator is called over plain TCP)"
        );
        assert_eq!(
            refused(json!([{"use": "stripe", "buckets": ["a1b"]}])),
            "emulators[0]: buckets: only for use: s3"
        );
        assert!(refused(json!([{"use": "s3", "buckets": ["A"]}])).contains("not a bucket name"));
        assert!(refused(json!([{"use": "stripe", "name": "app"}])).contains("already a service"));
        assert!(refused(json!([{"use": "stripe"}, {"use": "stripe"}]))
            .contains("duplicate service name"));
        assert_eq!(
            refused(json!({"use": "stripe"})),
            "emulators: a list of {use: stripe | smtp | s3, name:, port:, ref:, ...}"
        );
    }

    /// Milestone 316: with network: bridge every service needs an address, and an emulator's
    /// service got none, so such a manifest was refused (`services[0]: ... addr: required`).
    #[test]
    fn with_a_bridge_each_emulator_gets_a_free_address_and_its_names() {
        let mut m = base(json!([
            {"use": "s3", "buckets": ["media"]},
            {"use": "smtp", "name": "mail"},
            {"use": "stripe", "addr": "10.89.0.40"}
        ]));
        m["network"] = json!("bridge");
        m["services"][0]["addr"] = json!("10.89.1.1");
        m["root"] = json!({"text": {"/etc/hosts": "127.0.0.1 localhost\n10.89.1.1 app\n"}});
        crate::validate_data(&mut m).unwrap();
        let addr = |n: &str| {
            m["services"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == n)
                .map(|s| s["addr"].clone())
                .unwrap()
        };
        // 10.89.1.1 is the app's, so the first emulator takes the next one
        assert_eq!(addr("s3_emulator"), "10.89.1.2");
        assert_eq!(addr("mail_emulator"), "10.89.1.3");
        assert_eq!(addr("stripe_emulator"), "10.89.0.40");
        assert_eq!(addr("app"), "10.89.1.1");
        assert_eq!(m["env"]["AWS_ENDPOINT_URL_S3"], "http://10.89.0.1:9000");
        // each emulator listens where its proxy and its readiness check reach it
        assert_eq!(m["services"][0]["entrypoint"][4], "0.0.0.0:19000");
        assert_eq!(m["services"][1]["entrypoint"][2], "0.0.0.0:11025");
        assert_eq!(m["services"][1]["entrypoint"][4], "0.0.0.0:8025");
        assert_eq!(
            m["root"]["text"]["/etc/hosts"],
            "127.0.0.1 localhost\n10.89.1.1 app\n10.89.1.2 s3_emulator\n10.89.0.1 s3\n\
             10.89.1.3 mail_emulator\n10.89.0.1 mail\n10.89.0.40 stripe_emulator\n10.89.0.1 stripe\n"
        );
        // an address another service has is refused, as for any two services
        let mut m = base(json!([{"use": "s3", "addr": "10.89.1.1"}]));
        m["network"] = json!("bridge");
        m["services"][0]["addr"] = json!("10.89.1.1");
        assert!(crate::validate_data(&mut m)
            .unwrap_err()
            .message
            .contains("addr 10.89.1.1 is used by two services"));
        // without the bridge nothing changes: no address, no host names
        let m = expanded(json!([{"use": "s3"}]));
        assert!(m["services"][0].get("addr").is_none());
    }

    #[test]
    fn idempotent_proxy_routes_are_for_http_proxies() {
        let mut m = base(Value::Null);
        m["proxies"] = json!([{"name": "g", "port": 9001, "to": "127.0.0.1:9002", "protocol": "grpc",
            "idempotency": "pb:1", "routes": [{"name": "r", "path": "/a.B/C", "idempotent": true}]}]);
        assert_eq!(
            crate::validate_data(&mut m).unwrap_err().message,
            "proxies[0].routes[0]: idempotent: true only on an http proxy"
        );
    }
}
