//! Web applications tested through their own Playwright tests (Milestone 253,
//! docs/design/web-apps.md, `hydo guide web-apps`).
//!
//! A `browser:` section names the folder of a team's Playwright project. The validator turns it
//! into what a project would otherwise write by hand:
//!
//! - a service `browser` (or `name:`) from the Playwright image of the pinned version (the file is
//!   `browser/playwright-v<version>-noble-<arch>.docker.tar` next to the manifest; `hydo build`
//!   fetches a missing one by its digest). It runs nothing by itself: the workload runs the tests
//!   in it with `hydo-oci`, in its own network namespace (Chromium treats a machine that has only
//!   a loopback interface as offline, so `browser:` needs `network: bridge`);
//! - the project folder at `/opt/hydo/web/tests`, and hydo's three helper files at
//!   `/opt/hydo/web/hydo/` (`hook.cjs` watches the pages, `reporter.cjs` records each test,
//!   `check.mjs` judges what they wrote). `/opt/hydo/web` is mounted into the service, so files
//!   the tests need, such as the built app, go there through `root.files`;
//! - the workload: step i runs shard i+1 of `shards:` of `playwright test`, unchanged, with no
//!   retries and one worker;
//! - one invariant per watched property (`watch:`), and one that every test passes
//!   (`must_pass:`).
//!
//! The expansion consumes `browser:` and leaves `_browser` (the service's name and the image
//! pins) in the validated data: the explorer draws no fault on that service (the tests' faults
//! are `H=` on the API, clock steps and pauses of the workload), and `hydo build` reads the pins.

use serde_json::{json, Map, Value};

use crate::py::{self, repr_str, str_of};
use crate::validate::{err, R};

/// The keys of the `browser:` section.
pub const KNOWN: &[&str] = &[
    "tests",
    "playwright",
    "name",
    "addr",
    "shards",
    "args",
    "env",
    "must_pass",
    "timeout",
    "watch",
    "description",
];

/// The keys of `browser.watch`.
pub const KNOWN_WATCH: &[&str] = &[
    "errors",
    "console",
    "busy",
    "busy_max_ms",
    "alerts",
    "failures_within_ms",
    "requests",
    "text",
];

/// The validated data's key that records the expansion.
pub const META: &str = "_browser";

/// Where the project folder and hydo's helpers are in the guest (and in the browser's root).
pub const WEB_DIR: &str = "/opt/hydo/web";

/// Where the pages' record is (the guest's /tmp, which every image root shares).
pub const RECORD_DIR: &str = "/tmp/hydo/web";

/// hydo's helpers, staged by the expansion as `root.text`.
pub const HOOK: &str = include_str!("browser/hook.cjs");
pub const REPORTER: &str = include_str!("browser/reporter.cjs");
pub const CHECK: &str = include_str!("browser/check.mjs");

/// One pinned Playwright image: the version of `@playwright/test` it matches and the manifest
/// digest of each architecture (hydo's names: x86_64, arm64).
pub struct Image {
    pub version: &'static str,
    pub repo: &'static str,
    pub tag: &'static str,
    pub digests: [(&'static str, &'static str); 2],
}

/// The Playwright images this hydo pins (docs/BUILD-PINS.md, "Playwright"). The image is
/// Microsoft's own: Ubuntu 24.04 with Node.js and the browsers of that Playwright version.
pub const CATALOG: &[Image] = &[Image {
    version: "1.65.1",
    repo: "mcr.microsoft.com/playwright",
    tag: "mcr.microsoft.com/playwright:v1.65.1-noble",
    digests: [
        (
            "x86_64",
            "sha256:766137e2d9caad4744187e53cdc89320ba6f2eb57cff097374020944b44dd0a9",
        ),
        (
            "arm64",
            "sha256:56be08aca06bf736a928da2e3235a3b63ef71ee97313beb18c6019fea791aba4",
        ),
    ],
}];

/// The default watched elements and texts.
pub const BUSY: &[&str] = &["[aria-busy=true]", "[role=progressbar]"];
pub const ALERTS: &[&str] = &["[role=alert]", "[role=alertdialog]"];
pub const TEXT: &[&str] = &["NaN", "undefined", "[object Object]", "Invalid Date"];

fn oci_arch(arch: &str) -> &'static str {
    if arch == "arm64" {
        "arm64"
    } else {
        "amd64"
    }
}

/// The image file of a version for one architecture, relative to the manifest's folder.
pub fn image_file(version: &str, arch: &str) -> String {
    format!(
        "browser/playwright-v{version}-noble-{}.docker.tar",
        oci_arch(arch)
    )
}

fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}

fn ms(v: Option<&Value>, wh: &str, default: i64) -> R<i64> {
    match v {
        None => Ok(default),
        Some(x) => match x.as_i64() {
            Some(n) if x.is_i64() && (100..=3_600_000).contains(&n) => Ok(n),
            _ => Err(err(format!(
                "{wh}: milliseconds, a whole number from 100 to 3600000"
            ))),
        },
    }
}

fn strings(v: Option<&Value>, wh: &str, default: &[&str]) -> R<Vec<String>> {
    match v {
        None => Ok(default.iter().map(|s| s.to_string()).collect()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::String(s) if !s.is_empty() && !s.contains('\n') => Ok(s.clone()),
                _ => Err(err(format!(
                    "{wh}: a list of strings, each on one line (got {})",
                    py::repr(x)
                ))),
            })
            .collect(),
        Some(x) => Err(err(format!(
            "{wh}: a list of strings (got {})",
            py::repr(x)
        ))),
    }
}

fn flag(v: Option<&Value>, wh: &str, default: bool) -> R<bool> {
    match v {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(err(format!("{wh}: true or false"))),
    }
}

/// A word of a shell command line, quoted for `sh -c '...'` inside a `'...'` of the workload.
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_=./:,@%+".contains(&c))
    {
        s.to_string()
    } else {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('`', "\\`")
        )
    }
}

/// Replace `browser:` by its service, files, workload and invariants, and record `_browser`. A
/// manifest without `browser:` is not touched.
pub fn expand(m: &mut Value) -> R<()> {
    let Some(b) = m.as_object_mut().and_then(|o| o.remove("browser")) else {
        return Ok(());
    };
    if b.is_null() {
        return Ok(());
    }
    let wh = "browser";
    if !b.is_object() {
        return Err(err(
            "browser: a mapping {tests: <the Playwright project's folder>, ...}",
        ));
    }
    crate::validate::unknown(&b, KNOWN, wh)?;
    if m.get("network").and_then(Value::as_str) != Some("bridge") {
        return Err(err(
            "browser: needs network: bridge (the browser runs in its own network namespace: Chromium treats a machine with only a loopback interface as offline)",
        ));
    }
    if get(m, "workload").is_some() {
        return Err(err(
            "browser: writes the workload (step i runs shard i+1 of the Playwright tests); remove workload:",
        ));
    }
    let tests = match get(&b, "tests") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(err(
                "browser.tests: the folder of the Playwright project (its playwright.config and node_modules), relative to the manifest",
            ))
        }
    };
    let version = match get(&b, "playwright") {
        None => CATALOG[0].version.to_string(),
        Some(v) => str_of(v),
    };
    let Some(image) = CATALOG.iter().find(|i| i.version == version) else {
        let have: Vec<&str> = CATALOG.iter().map(|i| i.version).collect();
        return Err(err(format!(
            "browser.playwright: {} is not pinned by this hydo (pinned: {}); the tests' @playwright/test must be that version",
            repr_str(&version),
            have.join(", ")
        )));
    };
    let name = match get(&b, "name") {
        None => "browser".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => return Err(err(format!("browser.name {}: a name", py::repr(v)))),
    };
    if !crate::pyre::fullmatch(crate::validate::NAME_RE, &name) || name.len() > 40 {
        return Err(err(format!(
            "browser.name {}: [a-z][a-z0-9_]*, at most 40 characters",
            repr_str(&name)
        )));
    }
    let addr = match get(&b, "addr") {
        None => "10.89.0.250".to_string(),
        Some(v) => str_of(v),
    };
    let shards = match get(&b, "shards") {
        None => 1,
        Some(v) => match v.as_i64() {
            Some(n) if v.is_i64() && (1..=100).contains(&n) => n,
            _ => return Err(err(
                "browser.shards: the number of workload steps, 1 to 100 (step i runs shard i+1)",
            )),
        },
    };
    let timeout = match get(&b, "timeout") {
        None => 600,
        Some(v) => match v.as_i64() {
            Some(n) if v.is_i64() && (10..=7200).contains(&n) => n,
            _ => {
                return Err(err(
                    "browser.timeout: seconds per workload call, 10 to 7200",
                ))
            }
        },
    };
    let args = strings(get(&b, "args"), "browser.args", &[])?;
    let must_pass = flag(get(&b, "must_pass"), "browser.must_pass", true)?;
    let mut env = Map::new();
    match get(&b, "env") {
        None => {}
        Some(Value::Object(o)) => {
            for (k, v) in o {
                if !crate::pyre::fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", k) {
                    return Err(err(format!(
                        "browser.env: {} is not a variable name",
                        repr_str(k)
                    )));
                }
                if !(v.is_string() || v.is_number() || v.is_boolean()) {
                    return Err(err(format!("browser.env.{k}: a string or a number")));
                }
                env.insert(k.clone(), Value::String(str_of(v)));
            }
        }
        Some(_) => return Err(err("browser.env: a mapping of variable names to values")),
    }
    let w = match get(&b, "watch") {
        None => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err(err("browser.watch: a mapping")),
    };
    crate::validate::unknown(&w, KNOWN_WATCH, "browser.watch")?;
    let errors = flag(get(&w, "errors"), "browser.watch.errors", true)?;
    let console = flag(get(&w, "console"), "browser.watch.console", true)?;
    let busy = strings(get(&w, "busy"), "browser.watch.busy", BUSY)?;
    let alerts = strings(get(&w, "alerts"), "browser.watch.alerts", ALERTS)?;
    let text = strings(get(&w, "text"), "browser.watch.text", TEXT)?;
    let busy_max = ms(get(&w, "busy_max_ms"), "browser.watch.busy_max_ms", 10_000)?;
    let within = ms(
        get(&w, "failures_within_ms"),
        "browser.watch.failures_within_ms",
        5_000,
    )?;
    let requests = match get(&w, "requests") {
        None => String::new(),
        Some(Value::String(s)) if s.starts_with('/') => s.clone(),
        Some(_) => {
            return Err(err(
                "browser.watch.requests: a path prefix such as /api/ (only the pages' fetch and XHR requests under it are judged)",
            ))
        }
    };

    // the service: the Playwright image, idle; the workload and the checks run in it
    let watch = json!({
        "busy": busy,
        "alert": alerts,
        "text": text,
        "console": console,
        "requests": requests,
    });
    let mut senv = json!({
        "HOME": format!("{RECORD_DIR}/home"),
        "TZ": "UTC",
        "CI": "1",
        "HYDO_WEB_DIR": RECORD_DIR,
        "HYDO_WEB_WATCH": serde_json::to_string(&watch).unwrap_or_default(),
    });
    for (k, v) in env {
        senv[k] = v;
    }
    let mut images = Map::new();
    let mut pins = Map::new();
    for (arch, digest) in image.digests {
        images.insert(arch.into(), Value::String(image_file(image.version, arch)));
        pins.insert(
            arch.into(),
            json!({"digest": digest, "path": image_file(image.version, arch)}),
        );
    }
    let others: Vec<Value> = match get(m, "services") {
        Some(Value::Array(a)) => a.iter().filter_map(|s| s.get("name").cloned()).collect(),
        Some(_) => {
            return Err(err(
                "services: must be a list (it may be empty: a project that is only a workload)",
            ))
        }
        None => Vec::new(),
    };
    let svc = json!({
        "name": name,
        "image": Value::Object(images),
        "image_ref": image.tag,
        "entrypoint": ["/bin/sleep", "infinity"],
        "addr": addr,
        "depends_on": others,
        "ready": "true",
        "env": senv,
        "mounts": [
            {"src": WEB_DIR, "dst": WEB_DIR},
            {"tmpfs": true, "dst": "/dev/shm", "size": "256m"},
        ],
        "description": format!("the browser: Playwright {} with its Chromium (browser:)", image.version),
    });

    // the workload: step i runs shard i+1 of the tests, unchanged, one worker, no retries (the
    // shard number is the inner shell's $1). Without --fully-parallel Playwright shards whole
    // files, each into the shard of its first test, so a shard can be empty; with it each test is
    // its own unit, and one worker still runs them in the files' order.
    let split = if shards > 1 { " --fully-parallel" } else { "" };
    let mut cmd = format!(
        "cd {WEB_DIR}/tests && NODE_OPTIONS=\"--require {WEB_DIR}/hydo/hook.cjs\" exec node node_modules/@playwright/test/cli.js test --shard=$1/{shards}{split} --pass-with-no-tests --retries=0 --workers=1 --reporter={WEB_DIR}/hydo/reporter.cjs --output={RECORD_DIR}/results"
    );
    for a in &args {
        if a.contains('\'') {
            return Err(err(format!(
                "browser.args: {} has a single quote, which the workload's command line cannot carry",
                repr_str(a)
            )));
        }
        cmd.push(' ');
        cmd.push_str(&sh_quote(a));
    }
    let run = format!(
        "mkdir -p {RECORD_DIR}/home\n\
         i=$HYDO_FROM\n\
         while [ $i -lt $HYDO_TO ]; do\n\
         \x20 hydo-oci {name} /bin/sh -c '{cmd}' sh $((i + 1)) > {RECORD_DIR}/step-$i.log 2>&1\n\
         \x20 rc=$?\n\
         \x20 grep '^hydo-web:' {RECORD_DIR}/step-$i.log > /dev/console\n\
         \x20 echo \"hydo-web: step $i: playwright test exited $rc\" > /dev/console\n\
         \x20 i=$((i + 1))\n\
         done\n"
    );
    // a test program (M205): the explorer takes fewer than 4 steps, draws only the fault kinds
    // that `faults:` lists, and classes a failure test-failed
    let workload = json!({
        "kind": "test",
        "steps": shards,
        "run": run,
        "timeout": timeout,
        "process": "playwright/test/cli.js",
    });

    // the invariants
    let check = |rule: &str| format!("hydo-oci {name} node {WEB_DIR}/hydo/check.mjs {rule}");
    let mut invs: Vec<Value> = Vec::new();
    if errors {
        invs.push(json!({
            "id": format!("{name}.no_page_errors"),
            "msg": if console {
                "no page throws an uncaught exception, leaves a promise rejection unhandled, or calls console.error"
            } else {
                "no page throws an uncaught exception or leaves a promise rejection unhandled"
            },
            "cmd": check("errors"),
        }));
    }
    if !busy.is_empty() {
        invs.push(json!({
            "id": format!("{name}.busy_ends"),
            "msg": format!("no busy element stays for {} s, or until a test ends after a failed request", busy_max as f64 / 1000.0),
            "cmd": check(&format!("busy {busy_max}")),
        }));
    }
    if !alerts.is_empty() {
        invs.push(json!({
            "id": format!("{name}.failures_shown"),
            "msg": format!("every failed request is followed by an error on the page, or sent again, within {} s", within as f64 / 1000.0),
            "cmd": check(&format!("failures {within}")),
        }));
    }
    if !text.is_empty() {
        invs.push(json!({
            "id": format!("{name}.no_bad_text"),
            "msg": format!("no page prints {}", text.join(", ")),
            "cmd": check("text"),
        }));
    }
    // last: the verdict names the first property that fails, and a page's own failure says
    // more than the test that it made fail
    if must_pass {
        invs.push(json!({
            "id": format!("{name}.tests_pass"),
            "msg": "every Playwright test passes",
            "cmd": check("tests"),
        }));
    }

    let Some(o) = m.as_object_mut() else {
        return Ok(());
    };
    let mut svcs = match o.get("services") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    svcs.push(svc);
    o.insert("services".into(), Value::Array(svcs));
    o.insert("workload".into(), workload);
    let mut all = match o.get("invariants") {
        Some(Value::Array(a)) => a.clone(),
        Some(v) if !v.is_null() => return Err(err("invariants: must be a list")),
        _ => Vec::new(),
    };
    all.extend(invs);
    o.insert("invariants".into(), Value::Array(all));
    let root = o.entry("root").or_insert_with(|| json!({}));
    if root.is_null() {
        *root = json!({});
    }
    let Some(r) = root.as_object_mut() else {
        return Err(err("root: must be a mapping"));
    };
    let mut files = match r.get("files") {
        Some(Value::Array(a)) => a.clone(),
        Some(v) if !v.is_null() => return Err(err("root.files: must be a list")),
        _ => Vec::new(),
    };
    files.push(json!({"src": tests, "dst": format!("{WEB_DIR}/tests")}));
    r.insert("files".into(), Value::Array(files));
    let text_files = r.entry("text").or_insert_with(|| json!({}));
    if text_files.is_null() {
        *text_files = json!({});
    }
    let Some(t) = text_files.as_object_mut() else {
        return Err(err("root.text: must be a mapping"));
    };
    for (f, body) in [
        ("hook.cjs", HOOK),
        ("reporter.cjs", REPORTER),
        ("check.mjs", CHECK),
    ] {
        t.insert(
            format!("{WEB_DIR}/hydo/{f}"),
            Value::String(body.to_string()),
        );
    }
    let guest = o.entry("guest").or_insert_with(|| json!({}));
    if let Some(g) = guest.as_object_mut() {
        // Chromium's browser, renderer and GPU processes, Node.js and the test runner
        g.entry("ram_mib").or_insert(json!(4096));
    }
    o.insert(
        META.into(),
        json!({
            "service": name,
            "playwright": image.version,
            "repo": image.repo,
            "tag": image.tag,
            "images": Value::Object(pins),
            "tests": tests,
        }),
    );
    Ok(())
}

/// The browser's service of a validated manifest (none without `browser:`).
pub fn services(m: &Value) -> Vec<String> {
    m.get(META)
        .and_then(|x| x.get("service"))
        .and_then(Value::as_str)
        .map(|s| vec![s.to_string()])
        .unwrap_or_default()
}

/// The Playwright image a build needs for one architecture (x86_64 or arm64), as
/// [`crate::emulators::Image`], and the version the tests' `@playwright/test` must have.
pub fn images(m: &Value, arch: &str) -> Vec<(crate::emulators::Image, String, String)> {
    let Some(x) = m.get(META) else {
        return Vec::new();
    };
    let Some(pin) = x.get("images").and_then(|i| i.get(arch)) else {
        return Vec::new();
    };
    let s = |v: Option<&Value>| v.map(str_of).unwrap_or_default();
    vec![(
        crate::emulators::Image {
            service: s(x.get("service")),
            repo: s(x.get("repo")),
            digest: s(pin.get("digest")),
            tag: s(x.get("tag")),
            path: s(pin.get("path")),
        },
        s(x.get("playwright")),
        s(x.get("tests")),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(b: Value) -> Value {
        json!({"name": "web", "network": "bridge", "browser": b})
    }

    #[test]
    fn a_manifest_without_browser_is_unchanged() {
        let mut m = json!({"name": "x"});
        expand(&mut m).unwrap();
        assert_eq!(m, json!({"name": "x"}));
    }

    #[test]
    fn the_section_becomes_a_service_a_workload_and_invariants() {
        let mut m = base(
            json!({"tests": "e2e", "shards": 2, "args": ["--project=chromium"],
                                "watch": {"busy": [".spinner"], "requests": "/api/"}}),
        );
        expand(&mut m).unwrap();
        assert!(m.get("browser").is_none());
        let s = &m["services"][0];
        assert_eq!(s["name"], "browser");
        assert_eq!(s["addr"], "10.89.0.250");
        assert_eq!(
            s["image"]["x86_64"],
            "browser/playwright-v1.65.1-noble-amd64.docker.tar"
        );
        assert_eq!(m["workload"]["steps"], 2);
        assert_eq!(m["workload"]["kind"], "test");
        let run = m["workload"]["run"].as_str().unwrap();
        assert!(run.contains("--shard=$1/2 --fully-parallel "), "{run}");
        assert!(run.contains("' sh $((i + 1)) >"), "{run}");
        assert!(run.contains(" --project=chromium'"), "{run}");
        assert!(
            run.contains("NODE_OPTIONS=\"--require /opt/hydo/web/hydo/hook.cjs\""),
            "{run}"
        );
        let ids: Vec<&str> = m["invariants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "browser.no_page_errors",
                "browser.busy_ends",
                "browser.failures_shown",
                "browser.no_bad_text",
                "browser.tests_pass"
            ]
        );
        assert_eq!(m["root"]["files"][0]["dst"], "/opt/hydo/web/tests");
        assert_eq!(m["root"]["text"]["/opt/hydo/web/hydo/hook.cjs"], HOOK);
        assert_eq!(m["guest"]["ram_mib"], 4096);
        let watch: Value =
            serde_json::from_str(s["env"]["HYDO_WEB_WATCH"].as_str().unwrap()).unwrap();
        assert_eq!(watch["busy"], json!([".spinner"]));
        assert_eq!(watch["alert"], json!(ALERTS));
        assert_eq!(services(&m), ["browser"]);
        let imgs = images(&m, "x86_64");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].1, "1.65.1");
        assert_eq!(imgs[0].2, "e2e");
    }

    #[test]
    fn what_is_refused() {
        for (b, net, want) in [
            (json!({"tests": "e2e"}), "loopback", "network: bridge"),
            (json!({}), "bridge", "browser.tests"),
            (
                json!({"tests": "e2e", "playwright": "1.0.0"}),
                "bridge",
                "not pinned",
            ),
            (
                json!({"tests": "e2e", "shards": 0}),
                "bridge",
                "browser.shards",
            ),
            (
                json!({"tests": "e2e", "watch": {"spinners": []}}),
                "bridge",
                "unknown field",
            ),
            (
                json!({"tests": "e2e", "watch": {"requests": "api"}}),
                "bridge",
                "path prefix",
            ),
            (
                json!({"tests": "e2e", "nope": 1}),
                "bridge",
                "unknown field",
            ),
        ] {
            let mut m = json!({"name": "web", "network": net, "browser": b});
            let e = expand(&mut m).unwrap_err().message;
            assert!(e.contains(want), "{e}");
        }
        let mut m = json!({"name": "web", "network": "bridge", "browser": {"tests": "e2e"},
                           "workload": {"steps": 1, "run": "true"}});
        assert!(expand(&mut m)
            .unwrap_err()
            .message
            .contains("remove workload"));
    }

    #[test]
    fn an_empty_watch_list_drops_its_invariant() {
        let mut m = base(json!({"tests": "e2e", "must_pass": false,
                                "watch": {"busy": [], "alerts": [], "text": [], "errors": false}}));
        expand(&mut m).unwrap();
        assert!(m.get("invariants").unwrap().as_array().unwrap().is_empty());
    }

    #[test]
    fn arguments_are_quoted_for_the_shell() {
        assert_eq!(sh_quote("--grep=checkout"), "--grep=checkout");
        assert_eq!(sh_quote("a b"), "\"a b\"");
        assert_eq!(sh_quote("$x"), "\"\\$x\"");
    }
}
