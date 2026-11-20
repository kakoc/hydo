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
//!   (`must_pass:`);
//! - with `screens: compare` (Milestone 254), the reference folder at
//!   `/opt/hydo/web/screens-ref` and one invariant that every page's screenshot at the end of its
//!   test is the reference's, pixel for pixel. The controller copies every screenshot out of the
//!   guest into the run directory (`screens/`) after the verdict.
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
    "screens",
    "screens_ref",
    "explore",
    "description",
];

/// The keys of `browser.explore` (Milestone 258, docs/design/ui-exploration.md).
pub const KNOWN_EXPLORE: &[&str] = &[
    "start",
    "server",
    "steps",
    "actions",
    "setup",
    "avoid",
    "within",
    "values",
    "settle_ms",
    "quiet_ms",
];

/// The keys of one `browser.explore.setup` step.
pub const KNOWN_SETUP: &[&str] = &[
    "open", "click", "role", "fill", "with", "press", "see", "wait",
];

/// Where the walker and its playwright-core are in the guest (and in the browser's root).
pub const WALK_DIR: &str = "/opt/hydo/web/walk";

/// hydo's walker, and the stand-in for `@playwright/test` through which the unchanged watcher
/// names a walk's step.
pub const WALK: &str = include_str!("browser/walk.mjs");
pub const WALK_STUB: &str = include_str!("browser/walk-test-stub.cjs");
pub const WALK_STUB_PACKAGE: &str = r#"{"name":"@playwright/test","version":"0.0.0-hydo-walk","main":"index.js","private":true}
"#;

/// The npm package the walker drives the browser with: playwright-core of the image's version.
/// `hydo build` fetches it by its sha256 into `browser/` and unpacks it there.
pub struct Core {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

/// The playwright-core packages this hydo pins, one per pinned Playwright image.
pub const CORES: &[Core] = &[Core {
    version: "1.65.1",
    url: "https://registry.npmjs.org/playwright-core/-/playwright-core-1.65.1.tgz",
    sha256: "6f5a60be62a4fddfbe7d10c448d8fbd54547da3358f2f8a04211440471ee863e",
}];

/// The folder of the unpacked playwright-core, relative to the manifest.
pub fn core_dir(version: &str) -> String {
    format!("browser/playwright-core-{version}")
}

/// The default controls a walk never uses.
pub const AVOID: &[&str] = &["log ?out", "sign ?out"];

/// The keys of `browser.watch`.
pub const KNOWN_WATCH: &[&str] = &[
    "errors",
    "console",
    "busy",
    "busy_max_ms",
    "alerts",
    "failures_within_ms",
    "failures_of",
    "requests",
    "text",
];

/// The validated data's key that records the expansion.
pub const META: &str = "_browser";

/// Where the project folder and hydo's helpers are in the guest (and in the browser's root).
pub const WEB_DIR: &str = "/opt/hydo/web";

/// Where the pages' record is (the guest's /tmp/hydo, which every image root shares).
pub const RECORD_DIR: &str = "/tmp/hydo/web";

/// Where a recording replay finds the actions it repeats (Milestone 270): the host writes the
/// `walk-act` lines of the run without `rec` there, one `<step>.<k> <json>` per line, before the
/// workload starts (`hydo_control`'s walk step range).
pub const WALK_SCRIPT: &str = "/tmp/hydo/web/walk-script.txt";

/// The prefix of the console line with which the walker prints each action it took.
pub const WALK_ACT: &str = "hydo-web: walk-act ";

/// The script of a recording replay from a run's console (Milestone 270): each `walk-act` line's
/// `<step>.<k> <json>`, in the console's order, or None for a console without them (a run of an
/// image built before Milestone 270, or a walk that took no action).
pub fn walk_script(console: &str) -> Option<String> {
    let mut out = String::new();
    for l in console.lines() {
        let Some(rest) = l.trim_end_matches('\r').strip_prefix(WALK_ACT) else {
            continue;
        };
        let Some((at, json)) = rest.split_once(' ') else {
            continue;
        };
        let ok = at.split_once('.').is_some_and(|(s, k)| {
            !s.is_empty() && !k.is_empty() && s.bytes().chain(k.bytes()).all(|c| c.is_ascii_digit())
        });
        if ok && serde_json::from_str::<Value>(json).is_ok_and(|v| v.is_object()) {
            out.push_str(at);
            out.push(' ');
            out.push_str(json);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The actions of a recording replay's console that it left out (Milestone 270): a control that
/// was not on the page, or an action that the script did not have. Each as `<step>.<k>`, with the
/// walker's line about it.
pub fn walk_left_out(console: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for l in console.lines() {
        if let Some(rest) = l.strip_prefix("hydo-web: walk ") {
            if let Some((at, what)) = rest.split_once(": ") {
                if what.starts_with("not in the script") {
                    out.push((at.to_string(), what.to_string()));
                }
            }
            continue;
        }
        let Some(rest) = l.strip_prefix(WALK_ACT) else {
            continue;
        };
        let Some((at, json)) = rest.split_once(' ') else {
            continue;
        };
        let missing = serde_json::from_str::<Value>(json)
            .ok()
            .and_then(|v| v.get("missing").and_then(Value::as_bool))
            .unwrap_or(false);
        if missing {
            let what = console
                .lines()
                .find_map(|x| x.strip_prefix(&format!("hydo-web: walk {at}: ")))
                .unwrap_or("")
                .to_string();
            out.push((at.to_string(), what));
        }
    }
    out
}

/// Where the watcher saves the screenshots, with their `index.jsonl` (the controller copies them
/// out into the run directory's `screens/`).
pub const SHOTS_DIR: &str = "/tmp/hydo/web/shots";

/// Where `screens: compare` finds the reference screenshots in the guest.
pub const REF_DIR: &str = "/opt/hydo/web/screens-ref";

/// The values of `browser.screens`: which pages get a screenshot, and whether it is compared.
pub const SCREENS: &[&str] = &["failures", "all", "compare"];

/// The default reference folder of `screens: compare`, relative to the manifest.
pub const SCREENS_REF: &str = "hydo-screens";

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
    let explore = match get(&b, "explore") {
        None => None,
        Some(e) => Some(explore_cfg(e, m)?),
    };
    let tests = match get(&b, "tests") {
        Some(Value::String(s)) if !s.is_empty() => {
            if explore.is_some() {
                return Err(err(
                    "browser: tests and explore are two workloads; give one (tests runs your Playwright tests, explore lets hydo walk through the pages itself)",
                ));
            }
            s.clone()
        }
        None if explore.is_some() => String::new(),
        _ => {
            return Err(err(
                "browser.tests: the folder of the Playwright project (its playwright.config and node_modules), relative to the manifest; or browser.explore, for a walk through the pages that hydo makes itself",
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
    // a walk's step "passes" when the walker ran it: hydo's own walk has no assertions of its own
    let must_pass = flag(get(&b, "must_pass"), "browser.must_pass", explore.is_none())?;
    if explore.is_some() {
        for k in ["shards", "args"] {
            if get(&b, k).is_some() {
                return Err(err(format!(
                    "browser.{k}: only with tests (browser.explore.steps is the walk's number of workload steps)"
                )));
            }
        }
    }
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
    let screens = match get(&b, "screens") {
        None => "failures".to_string(),
        Some(Value::String(s)) if SCREENS.contains(&s.as_str()) => s.clone(),
        // a {{param}} is replaced before the image is built (and checked then); a reading of the
        // manifest without its parameters expands the default
        Some(Value::String(s)) if s.contains("{{") => "failures".to_string(),
        Some(v) => {
            return Err(err(format!(
                "browser.screens {}: failures (a screenshot of a page that ends its test with a busy or error element, the default), all (every page at the end of its test), or compare (all, each compared with its reference in screens_ref)",
                py::repr(v)
            )))
        }
    };
    let screens_ref = match get(&b, "screens_ref") {
        None => SCREENS_REF.to_string(),
        Some(Value::String(s)) if !s.is_empty() && !s.starts_with('/') => s.clone(),
        Some(_) => {
            return Err(err(
                "browser.screens_ref: the folder of the reference screenshots, relative to the manifest",
            ))
        }
    };
    let unresolved = get(&b, "screens")
        .and_then(Value::as_str)
        .is_some_and(|s| s.contains("{{"));
    if get(&b, "screens_ref").is_some() && screens != "compare" && !unresolved {
        return Err(err(
            "browser.screens_ref: only with screens: compare (the references are compared only then)",
        ));
    }
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
    let mut watch = json!({
        "busy": busy,
        "alert": alerts,
        "text": text,
        "console": console,
        "requests": requests,
    });
    if screens != "failures" {
        watch["screens"] = json!(screens);
    }
    // Milestone 258: only these requests must show their failure (a manifest without the key
    // keeps the watcher's settings as they were)
    match get(&w, "failures_of") {
        None => {}
        Some(Value::String(s)) if !s.is_empty() && regex::Regex::new(s).is_ok() => {
            watch["failures_of"] = json!(s)
        }
        Some(_) => {
            return Err(err(
                "browser.watch.failures_of: a regular expression over a request's path or its `<METHOD> <path>`, such as (Create|Update|Delete)[A-Za-z]*$ or ^(POST|PATCH|DELETE) /api/ (only a failed request that it matches must be shown)",
            ))
        }
    }
    let mut senv = json!({
        "HOME": format!("{RECORD_DIR}/home"),
        "TZ": "UTC",
        "CI": "1",
        "HYDO_WEB_DIR": RECORD_DIR,
        "HYDO_WEB_WATCH": serde_json::to_string(&watch).unwrap_or_default(),
    });
    if let Some(x) = &explore {
        let mut cfg = x.cfg.clone();
        cfg["text"] = json!(text);
        senv["HYDO_WEB_WALK"] = Value::String(serde_json::to_string(&cfg).unwrap_or_default());
    }
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
    let workload = match &explore {
        None => json!({
            "kind": "test",
            "steps": shards,
            "run": run,
            "timeout": timeout,
            "process": "playwright/test/cli.js",
        }),
        // a walk (Milestone 258): step i runs part i of the walk. The controller gives the
        // schedule's I= value as $HYDO_WALK and the schedule's guest part as $HYDO_GUEST (for
        // repro.spec.ts); a schedule without I= walks seed 0. An ordinary workload: the explorer
        // draws every fault kind that `faults:` allows
        Some(x) => json!({
            "steps": x.steps,
            "run": format!(
                "mkdir -p {RECORD_DIR}/home\n\
                 i=$HYDO_FROM\n\
                 while [ $i -lt $HYDO_TO ]; do\n\
                 \x20 hydo-oci {name} /bin/sh -c 'cd {WALK_DIR} && NODE_OPTIONS=\"--require {WEB_DIR}/hydo/hook.cjs\" exec node walk.mjs \"$1\" \"$2\" \"$3\" \"$4\"' sh $i \"$HYDO_WALK\" \"$HYDO_GUEST\" \"$HYDO_ACKED\" > {RECORD_DIR}/step-$i.log 2>&1\n\
                 \x20 rc=$?\n\
                 \x20 grep '^hydo-web:' {RECORD_DIR}/step-$i.log > /dev/console\n\
                 \x20 echo \"hydo-web: step $i: the walker exited $rc\" > /dev/console\n\
                 \x20 i=$((i + 1))\n\
                 done\n"
            ),
            "timeout": timeout,
            "process": "walk.mjs",
        }),
    };

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
    if screens == "compare" {
        invs.push(json!({
            "id": format!("{name}.screens_match"),
            "msg": "every page's screenshot at the end of its test is its reference, pixel for pixel",
            // the PNG library comes from the tests' playwright-core, or the walker's
            "cmd": if explore.is_some() {
                check(&format!("screens {REF_DIR} {WALK_DIR}"))
            } else {
                check(&format!("screens {REF_DIR}"))
            },
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
    let core = CORES.iter().find(|c| c.version == image.version);
    match (&explore, core) {
        (None, _) => files.push(json!({"src": tests, "dst": format!("{WEB_DIR}/tests")})),
        (Some(_), Some(c)) => files.push(json!({
            "src": core_dir(c.version),
            "dst": format!("{WALK_DIR}/node_modules/playwright-core"),
        })),
        (Some(_), None) => {
            return Err(err(format!(
                "browser.explore: this hydo pins no playwright-core for Playwright {}",
                image.version
            )))
        }
    }
    if screens == "compare" {
        files.push(json!({"src": screens_ref, "dst": REF_DIR}));
    }
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
    if explore.is_some() {
        let stub = format!("{WALK_DIR}/node_modules/@playwright/test");
        t.insert(format!("{WALK_DIR}/walk.mjs"), Value::String(WALK.into()));
        t.insert(format!("{stub}/index.js"), Value::String(WALK_STUB.into()));
        t.insert(
            format!("{stub}/package.json"),
            Value::String(WALK_STUB_PACKAGE.into()),
        );
    }
    let guest = o.entry("guest").or_insert_with(|| json!({}));
    if let Some(g) = guest.as_object_mut() {
        // Chromium's browser, renderer and GPU processes, Node.js and the test runner
        g.entry("ram_mib").or_insert(json!(4096));
    }
    let mut meta = json!({
        "service": name,
        "playwright": image.version,
        "repo": image.repo,
        "tag": image.tag,
        "images": Value::Object(pins),
        "tests": tests,
        "screens": screens,
        "screens_ref": (screens == "compare").then_some(screens_ref),
    });
    // only with explore:, so that the record of a manifest with tests stays as it was
    if let (Some(x), Some(c)) = (&explore, core) {
        meta["explore"] = json!({
            "steps": x.steps,
            "actions": x.actions,
            "core": {"version": c.version, "url": c.url, "sha256": c.sha256, "dir": core_dir(c.version)},
        });
    }
    o.insert(META.into(), meta);
    Ok(())
}

/// A validated `browser.explore`: the walker's configuration ($HYDO_WEB_WALK) and the walk's
/// shape.
struct Explore {
    cfg: Value,
    steps: i64,
    actions: i64,
}

fn int_in(v: Option<&Value>, wh: &str, lo: i64, hi: i64, default: i64, what: &str) -> R<i64> {
    match v {
        None => Ok(default),
        Some(x) => match x.as_i64() {
            Some(n) if x.is_i64() && (lo..=hi).contains(&n) => Ok(n),
            _ => Err(err(format!("{wh}: {what}, {lo} to {hi}"))),
        },
    }
}

/// `browser.explore` (Milestone 258, docs/design/ui-exploration.md): where the walk starts, how
/// long it is, how it signs in, and what it never touches.
fn explore_cfg(e: &Value, m: &Value) -> R<Explore> {
    let wh = "browser.explore";
    if !e.is_object() {
        return Err(err(
            "browser.explore: a mapping {start: <the URL the walk starts on>, ...}",
        ));
    }
    crate::validate::unknown(e, KNOWN_EXPLORE, wh)?;
    let start = match get(e, "start") {
        Some(Value::String(s))
            if (s.starts_with("http://") || s.starts_with("https://")) && !s.contains(' ') =>
        {
            s.clone()
        }
        _ => {
            return Err(err(
                "browser.explore.start: the URL the walk starts on, as the guest's browser reaches the app (http://<service address or localhost>:<port>/<path>)",
            ))
        }
    };
    let server = match get(e, "server") {
        None => Value::Null,
        Some(Value::String(s)) if !s.trim().is_empty() => Value::String(s.clone()),
        Some(_) => {
            return Err(err(
                "browser.explore.server: a command line that starts the app's web server in the browser's service, before each step (as Playwright's webServer)",
            ))
        }
    };
    let steps = int_in(
        get(e, "steps"),
        "browser.explore.steps",
        4,
        100,
        4,
        "the walk's workload steps (a fault can fall between two)",
    )?;
    let actions = int_in(
        get(e, "actions"),
        "browser.explore.actions",
        1,
        hydo_schedule::MAX_ACTIONS as i64,
        10,
        "actions in each step",
    )?;
    let settle = ms(get(e, "settle_ms"), "browser.explore.settle_ms", 700)?;
    let quiet = ms(get(e, "quiet_ms"), "browser.explore.quiet_ms", 3000)?;
    let avoid = strings(get(e, "avoid"), "browser.explore.avoid", AVOID)?;
    for a in &avoid {
        if regex::Regex::new(&format!("(?i){a}")).is_err() {
            return Err(err(format!(
                "browser.explore.avoid: {} is not a regular expression",
                repr_str(a)
            )));
        }
    }
    let values = strings(get(e, "values"), "browser.explore.values", &[])?;
    let within = match get(e, "within") {
        None => "/".to_string(),
        Some(Value::String(s)) if s.starts_with('/') => s.clone(),
        Some(_) => {
            return Err(err(
                "browser.explore.within: a path prefix such as /app/ (the walk follows only links under it)",
            ))
        }
    };
    let mut setup = Vec::new();
    match get(e, "setup") {
        None => {}
        Some(Value::Array(a)) => {
            for (i, s) in a.iter().enumerate() {
                let w = format!("browser.explore.setup[{i}]");
                if !s.is_object() {
                    return Err(err(format!("{w}: a mapping such as {{open: /login}}")));
                }
                crate::validate::unknown(s, KNOWN_SETUP, &w)?;
                let acts: Vec<&str> = ["open", "click", "fill", "press", "see", "wait"]
                    .into_iter()
                    .filter(|k| get(s, k).is_some())
                    .collect();
                if acts.len() != 1 {
                    return Err(err(format!(
                        "{w}: one of open, click, fill (with with:), press, see or wait"
                    )));
                }
                let mut one = Map::new();
                for (k, v) in s.as_object().into_iter().flatten() {
                    if k == "wait" {
                        one.insert(k.clone(), json!(ms(Some(v), &format!("{w}.wait"), 0)?));
                    } else if v.is_string() || v.is_number() || v.is_boolean() {
                        one.insert(k.clone(), Value::String(str_of(v)));
                    } else {
                        return Err(err(format!("{w}.{k}: a string")));
                    }
                }
                if acts[0] == "fill" && !one.contains_key("with") {
                    return Err(err(format!("{w}: fill needs with: <the value to type>")));
                }
                setup.push(Value::Object(one));
            }
        }
        Some(_) => {
            return Err(err(
                "browser.explore.setup: a list of steps, such as [{open: /login}, {fill: Username, with: demo}, {click: Sign in}]",
            ))
        }
    }
    // the fakes' routes, for repro.spec.ts: an H= fault on a route becomes a page.route
    let mut routes = Map::new();
    if let Some(Value::Array(fakes)) = m.get("fakes") {
        for f in fakes {
            let fname = f.get("name").map(str_of).unwrap_or_default();
            if let Some(Value::Array(rs)) = f.get("routes") {
                for r in rs {
                    let (Some(n), Some(p)) = (r.get("name"), r.get("path")) else {
                        continue;
                    };
                    routes.insert(
                        format!("{fname}.{}", str_of(n)),
                        json!({"method": r.get("method").map(str_of).unwrap_or_default(), "path": str_of(p)}),
                    );
                }
            }
        }
    }
    Ok(Explore {
        cfg: json!({
            "start": start,
            "server": server,
            "actions": actions,
            "setup": setup,
            "avoid": avoid,
            "within": within,
            "values": values,
            "steps": steps,
            "settle_ms": settle,
            "quiet_ms": quiet,
            "routes": Value::Object(routes),
        }),
        steps,
        actions,
    })
}

/// The playwright-core package a build needs for `browser.explore` (none otherwise): its version,
/// the URL, its sha256 and the folder it is unpacked into, relative to the manifest.
pub fn walk_core(m: &Value) -> Option<(String, String, String, String)> {
    let c = m.get(META)?.get("explore")?.get("core")?;
    let s = |k: &str| c.get(k).map(str_of).unwrap_or_default();
    Some((s("version"), s("url"), s("sha256"), s("dir")))
}

/// Whether a validated manifest walks through its pages (`browser.explore`).
pub fn walks(m: &Value) -> bool {
    m.get(META).and_then(|x| x.get("explore")).is_some()
}

/// The guest folder of the screenshots that the controller copies out (none without `browser:`).
pub fn screens_dir(m: &Value) -> Option<String> {
    m.get(META).map(|_| SHOTS_DIR.to_string())
}

/// The reference folder of `screens: compare`, relative to the manifest (none otherwise).
pub fn screens_ref(m: &Value) -> Option<String> {
    m.get(META)
        .and_then(|x| x.get("screens_ref"))
        .and_then(Value::as_str)
        .map(str::to_string)
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
    fn a_console_gives_the_script_of_a_recording_replay() {
        let console = "hydo-web: walk 0: setup (4 steps)\n\
            hydo-web: walk 0.0: click button \"Share\"\n\
            hydo-web: walk-act 0.0 {\"do\":\"click\",\"c\":{\"role\":\"button\",\"name\":\"Share\",\"m\":0}}\n\
            HYDO-ORACLE: noise\n\
            hydo-web: walk-act 0.1 {\"do\":\"wait\",\"ms\":1000}\r\n\
            hydo-web: walk-act x.1 {\"do\":\"wait\"}\n\
            hydo-web: walk-act 0.2 {\"do\":\"wait\"\n";
        assert_eq!(
            walk_script(console).unwrap(),
            "0.0 {\"do\":\"click\",\"c\":{\"role\":\"button\",\"name\":\"Share\",\"m\":0}}\n\
             0.1 {\"do\":\"wait\",\"ms\":1000}\n"
        );
        assert_eq!(
            walk_script("hydo-web: walk 0.0: click button \"Share\"\n"),
            None
        );
        let replay =
            "hydo-web: walk 4.7: click button \"Copy link\": not on the page within 10 s\n\
            hydo-web: walk-act 4.7 {\"do\":\"click\",\"missing\":true}\n\
            hydo-web: walk 4.8: not in the script: the run that this replays took no action 4.8\n\
            hydo-web: walk-act 4.9 {\"do\":\"wait\",\"ms\":1000}\n";
        let left: Vec<String> = walk_left_out(replay).into_iter().map(|(a, _)| a).collect();
        assert_eq!(left, ["4.7", "4.8"]);
        assert!(walk_left_out(replay)[0]
            .1
            .starts_with("click button \"Copy link\""));
    }

    /// `node check.mjs <rule> <ms>` over the given events, as the guest runs it: its exit
    /// status and its sentence. None when this machine has no `node`.
    fn check_mjs(rule: &str, ms: u64, events: &[Value]) -> Option<(i32, String)> {
        struct Gone(std::path::PathBuf);
        impl Drop for Gone {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "hydo-manifest-check-mjs-{}-{rule}-{}",
            std::process::id(),
            events.len()
        ));
        let _gone = Gone(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("check.mjs"), CHECK).unwrap();
        let lines: String = events.iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
        let out = std::process::Command::new("node")
            .arg(dir.join("check.mjs"))
            .arg(rule)
            .arg(ms.to_string())
            .env("HYDO_WEB_DIR", &dir)
            .output()
            .ok()?;
        Some((
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    }

    /// Milestone 301: web-shop's walk went back from `/` while its products were loading, and
    /// the spinner of the page it left stayed open in the events until the test ended, 10.2 s
    /// later: a stuck spinner that no page showed. A new document ends the elements of the one
    /// before it; a spinner that stays on its own page is still found.
    #[test]
    fn a_spinner_ends_with_its_document() {
        let t = "walk step 2";
        let busy = |at: u64, on: bool| {
            json!({"t": at, "test": t, "page": 1, "url": "/", "kind": "busy", "selector": ".spinner",
                   "on": on, "text": if on { json!("Loading products…") } else { Value::Null }, "scan": 1})
        };
        let doc =
            |at: u64| json!({"t": at, "test": t, "page": 1, "url": "about:blank", "kind": "doc"});
        let nav =
            |at: u64| json!({"t": at, "test": t, "page": 1, "url": "about:blank", "kind": "nav"});
        let close =
            |at: u64| json!({"t": at, "test": t, "page": 1, "url": "about:blank", "kind": "close"});
        let test = json!({"t": 20_000, "kind": "test", "test": t, "status": "passed", "expected": "passed", "ms": 10_500});
        // the page left by going back: its new document ends the spinner
        let left = [busy(1_000, true), doc(1_200), close(11_200), test.clone()];
        let Some((rc, said)) = check_mjs("busy", 10_000, &left) else {
            eprintln!("no node on this machine: check.mjs not run");
            return;
        };
        assert_eq!(rc, 0, "{said}");
        // a navigation to about:blank ends it too, when the new document sends nothing
        let blank = [busy(1_000, true), nav(1_150), close(11_200), test.clone()];
        assert_eq!(check_mjs("busy", 10_000, &blank).unwrap().0, 0);
        // the events of the trial, without the new document: the false finding of 0.4.0's trial
        let old = [busy(1_000, true), close(11_200), test.clone()];
        let (rc, said) = check_mjs("busy", 10_000, &old).unwrap();
        assert_eq!(rc, 1, "{said}");
        assert!(said.contains("for 10.2 s, until the test ended"), "{said}");
        // a spinner that stays on its own page is still a finding, and one that goes is not
        let stuck = [busy(1_000, true), close(12_000), test.clone()];
        assert_eq!(check_mjs("busy", 10_000, &stuck).unwrap().0, 1);
        let gone = [
            busy(1_000, true),
            busy(1_400, false),
            doc(1_500),
            close(12_000),
            test,
        ];
        assert_eq!(check_mjs("busy", 10_000, &gone).unwrap().0, 0);
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
    fn screens_compare_adds_the_references_and_a_check() {
        let mut m = base(json!({"tests": "e2e", "screens": "compare", "screens_ref": "refs"}));
        expand(&mut m).unwrap();
        let ids: Vec<&str> = m["invariants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids[ids.len() - 2..],
            ["browser.screens_match", "browser.tests_pass"]
        );
        assert_eq!(
            m["root"]["files"][1],
            json!({"src": "refs", "dst": "/opt/hydo/web/screens-ref"})
        );
        let env = &m["services"][0]["env"]["HYDO_WEB_WATCH"];
        let watch: Value = serde_json::from_str(env.as_str().unwrap()).unwrap();
        assert_eq!(watch["screens"], "compare");
        assert_eq!(screens_ref(&m).as_deref(), Some("refs"));
        assert_eq!(screens_dir(&m).as_deref(), Some(SHOTS_DIR));
        // the default keeps the watcher's settings of Milestone 253
        let mut d = base(json!({"tests": "e2e"}));
        expand(&mut d).unwrap();
        let env = &d["services"][0]["env"]["HYDO_WEB_WATCH"];
        let watch: Value = serde_json::from_str(env.as_str().unwrap()).unwrap();
        assert!(watch.get("screens").is_none());
        assert_eq!(screens_ref(&d), None);
        // a {{param}} not yet replaced expands the default
        let mut u = base(json!({"tests": "e2e", "screens": "{{screens}}"}));
        expand(&mut u).unwrap();
        assert_eq!(u[META]["screens"], "failures");
        for (b, want) in [
            (
                json!({"tests": "e2e", "screens": "some"}),
                "browser.screens",
            ),
            (
                json!({"tests": "e2e", "screens_ref": "r"}),
                "only with screens: compare",
            ),
            (
                json!({"tests": "e2e", "screens": "compare", "screens_ref": "/r"}),
                "relative",
            ),
        ] {
            let mut m = base(b);
            let e = expand(&mut m).unwrap_err().message;
            assert!(e.contains(want), "{e}");
        }
    }

    #[test]
    fn explore_becomes_a_walk_with_the_walker_and_its_playwright_core() {
        let mut m = json!({"name": "web", "network": "bridge",
            "browser": {"explore": {"start": "http://localhost:3000/", "server": "node s.mjs",
                                    "actions": 6,
                                    "setup": [{"open": "/login"}, {"fill": "Name", "with": "a"},
                                              {"click": "Sign in"}, {"wait": 500}]}},
            "fakes": [{"name": "api", "routes": [{"name": "add", "method": "POST", "path": "/api/x/{id}"}]}]});
        expand(&mut m).unwrap();
        let w = &m["workload"];
        assert!(w.get("kind").is_none(), "a walk is an ordinary workload");
        assert_eq!(w["steps"], 4);
        let run = w["run"].as_str().unwrap();
        assert!(
            run.contains("cd /opt/hydo/web/walk && NODE_OPTIONS=\"--require /opt/hydo/web/hydo/hook.cjs\" exec node walk.mjs"),
            "{run}"
        );
        assert!(
            run.contains("sh $i \"$HYDO_WALK\" \"$HYDO_GUEST\" \"$HYDO_ACKED\""),
            "{run}"
        );
        let ids: Vec<&str> = m["invariants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        assert!(!ids.contains(&"browser.tests_pass"), "{ids:?}");
        assert_eq!(
            m["root"]["files"][0],
            json!({"src": "browser/playwright-core-1.65.1", "dst": "/opt/hydo/web/walk/node_modules/playwright-core"})
        );
        let t = &m["root"]["text"];
        assert_eq!(t["/opt/hydo/web/walk/walk.mjs"], WALK);
        assert_eq!(
            t["/opt/hydo/web/walk/node_modules/@playwright/test/index.js"],
            WALK_STUB
        );
        assert_eq!(t["/opt/hydo/web/hydo/hook.cjs"], HOOK);
        let cfg: Value =
            serde_json::from_str(m["services"][0]["env"]["HYDO_WEB_WALK"].as_str().unwrap())
                .unwrap();
        assert_eq!(cfg["actions"], 6);
        assert_eq!(cfg["server"], "node s.mjs");
        assert_eq!(cfg["setup"][3]["wait"], 500);
        assert_eq!(cfg["avoid"], json!(AVOID));
        assert_eq!(cfg["routes"]["api.add"]["path"], "/api/x/{id}");
        assert_eq!(cfg["text"], json!(TEXT));
        assert!(walks(&m));
        let (v, url, sha, dir) = walk_core(&m).unwrap();
        assert_eq!(v, "1.65.1");
        assert!(url.ends_with("playwright-core-1.65.1.tgz"));
        assert_eq!(sha.len(), 64);
        assert_eq!(dir, "browser/playwright-core-1.65.1");
        assert_eq!(images(&m, "x86_64")[0].2, "");
        // a manifest with tests keeps its record as before Milestone 258
        let mut d = base(json!({"tests": "e2e"}));
        expand(&mut d).unwrap();
        assert!(d[META].get("explore").is_none());
        assert!(!walks(&d));
        assert!(d["services"][0]["env"].get("HYDO_WEB_WALK").is_none());
        let w: Value =
            serde_json::from_str(d["services"][0]["env"]["HYDO_WEB_WATCH"].as_str().unwrap())
                .unwrap();
        assert!(w.get("failures_of").is_none());
        // watch.failures_of narrows the failures that must be shown
        let mut f =
            base(json!({"tests": "e2e", "watch": {"failures_of": "(Create|Delete)[A-Za-z]*$"}}));
        expand(&mut f).unwrap();
        let w: Value =
            serde_json::from_str(f["services"][0]["env"]["HYDO_WEB_WATCH"].as_str().unwrap())
                .unwrap();
        assert_eq!(w["failures_of"], "(Create|Delete)[A-Za-z]*$");
        // Milestone 273: a pattern over `<METHOD> <path>` (the writes of a REST API)
        let mut m = base(
            json!({"tests": "e2e", "watch": {"failures_of": "^(POST|PATCH|DELETE) /api/v[12]/"}}),
        );
        expand(&mut m).unwrap();
        let w: Value =
            serde_json::from_str(m["services"][0]["env"]["HYDO_WEB_WATCH"].as_str().unwrap())
                .unwrap();
        assert_eq!(w["failures_of"], "^(POST|PATCH|DELETE) /api/v[12]/");
        let mut bad = base(json!({"tests": "e2e", "watch": {"failures_of": "(x"}}));
        assert!(expand(&mut bad)
            .unwrap_err()
            .message
            .contains("failures_of"));
    }

    #[test]
    fn what_explore_refuses() {
        for (b, want) in [
            (json!({"explore": {}}), "browser.explore.start"),
            (
                json!({"explore": {"start": "localhost:3000"}}),
                "browser.explore.start",
            ),
            (
                json!({"tests": "e2e", "explore": {"start": "http://a/"}}),
                "two workloads",
            ),
            (
                json!({"explore": {"start": "http://a/"}, "shards": 2}),
                "only with tests",
            ),
            (
                json!({"explore": {"start": "http://a/", "steps": 3}}),
                "explore.steps",
            ),
            (
                json!({"explore": {"start": "http://a/", "actions": 0}}),
                "explore.actions",
            ),
            (
                json!({"explore": {"start": "http://a/", "setup": [{"fill": "x"}]}}),
                "with:",
            ),
            (
                json!({"explore": {"start": "http://a/", "setup": [{"open": "/", "click": "x"}]}}),
                "one of open",
            ),
            (
                json!({"explore": {"start": "http://a/", "avoid": ["(x"]}}),
                "regular expression",
            ),
            (
                json!({"explore": {"start": "http://a/", "nope": 1}}),
                "unknown field",
            ),
        ] {
            let mut m = base(b);
            let e = expand(&mut m).unwrap_err().message;
            assert!(e.contains(want), "{e}");
        }
    }

    #[test]
    fn arguments_are_quoted_for_the_shell() {
        assert_eq!(sh_quote("--grep=checkout"), "--grep=checkout");
        assert_eq!(sh_quote("a b"), "\"a b\"");
        assert_eq!(sh_quote("$x"), "\"\\$x\"");
    }
}
