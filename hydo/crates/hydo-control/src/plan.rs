//! What the controller needs to know about a project: the core project model (services,
//! provisioning, the workload, invariants, knobs, the data disk and the clock) with the defaults
//! the shell init gets from `/etc/hydo/manifest.sh`.
//!
//! [`Plan::from_manifest`] reads a validated manifest (`hydo_manifest`, M114) the way hydo-build's
//! `compile_shell` writes manifest.sh (hydo/crates/hydo-build/src/shell.rs): the same defaults
//! (ready_timeout 120, workload.timeout 300, data.mount `rw`, clock_start 1767225600, an
//! invariant's msg = its id and `final` = no `every`), the workload's process name for `pgrep`
//! (workload.process, else the first word of workload.run without its directory), and an image
//! service's command `/etc/hydo/oci/<name>/run`. A manifest that needs an init extension is
//! refused with the seam that will drive it ([`crate::seams`]).
//!
//! [`Layout`] is where an agent image keeps the files the shell init used: the busybox, the
//! project's prelude, the run directory and the data disk. hydo-build's agent images (step B3)
//! must keep these paths, or give the controller another layout.

use crate::seams::{Seam, Unsupported};
use hydo_manifest::py;
use hydo_manifest::Manifest;
use serde_json::Value;

/// Paths inside the guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The static busybox every shell command runs under (`$BB`).
    pub busybox: String,
    /// The project's environment and shell helpers, sourced by every manifest command.
    pub prelude: String,
    /// The run directory (`$H`): service logs, the acked/outcomes/failed files.
    pub dir: String,
    /// The data disk's partition, its mount point and its file system.
    pub data_dev: String,
    pub data_dir: String,
    pub data_fstype: String,
}

impl Default for Layout {
    fn default() -> Layout {
        Layout {
            busybox: "/bin/busybox".into(),
            prelude: "/etc/hydo/prelude.sh".into(),
            dir: "/tmp/hydo".into(),
            data_dev: "/dev/vda2".into(),
            data_dir: "/data".into(),
            data_fstype: "ext4".into(),
        }
    }
}

impl Layout {
    pub fn acked(&self) -> String {
        format!("{}/acked", self.dir)
    }
    pub fn outcomes(&self) -> String {
        format!("{}/outcomes", self.dir)
    }
    pub fn failed(&self) -> String {
        format!("{}/failed", self.dir)
    }
    pub fn log_of(&self, svc: &str) -> String {
        format!("{}/{svc}.log", self.dir)
    }
}

/// One service, as manifest.sh describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanService {
    pub name: String,
    /// The shell command (`exec`ed by `sh -c`).
    pub cmd: String,
    /// The setup command ("" = none).
    pub setup: String,
    /// The readiness command.
    pub ready: String,
    /// How many readiness polls, one per second (ready_timeout).
    pub timeout: i64,
    /// The service's port ("" = none).
    pub port: String,
    /// Started again in the recovery boot after a power cut (`recovery:`, default true).
    pub recovery: bool,
}

/// One invariant (manifest.sh's HYDO_INV_<k>_*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanInvariant {
    /// The init's key (`i0`, `i1`, ...).
    pub key: String,
    pub id: String,
    /// `always`, `always_or_unreachable` or `eventually`.
    pub kind: String,
    pub msg: String,
    pub cmd: String,
    /// Seconds between checks while the workload runs ("" = not periodic), as written.
    pub every: String,
    /// Checked for the verdict (never for an `eventually` invariant).
    pub final_: bool,
    /// `type: eventually`: its deadline, anchor and poll interval (manifest.sh's
    /// HYDO_INV_<k>_within/after/poll; hydo-liveness.sh).
    pub live: Option<PlanLive>,
}

/// An `eventually` invariant's liveness fields, as manifest.sh writes them (`%d` of the value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanLive {
    /// Seconds of guest time after the anchor.
    pub within: i64,
    /// The anchor: `heal` (the default) or `start`.
    pub after: String,
    /// Seconds between two checks (default 1).
    pub poll: i64,
}

/// One knob (manifest.sh's HYDO_KNOB_<token>_*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanKnob {
    pub token: String,
    pub default: String,
    pub set: String,
    pub name: String,
}

/// The core project model the controller drives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub name: String,
    /// Services in start order (depends_on).
    pub services: Vec<PlanService>,
    pub steps: i64,
    pub workload_run: String,
    /// "" = no retry command.
    pub workload_retry: String,
    /// What `pgrep -f` looks for to tell whether a client request is in flight.
    pub workload_pgrep: String,
    /// Seconds per workload call (busybox timeout).
    pub workload_timeout: i64,
    /// "" = no provisioning command.
    pub provision: String,
    /// The data disk's mount options, as written (`rw`, `rw,data=journal`, ...).
    pub data_opts: String,
    pub data_dirs: Vec<String>,
    pub clock_start: i64,
    /// `guest.clock_after_cut`: seconds the machine is off at a power cut; the recovery boot's
    /// clock continues from the first boot's (features/clock.rs).
    pub clock_after_cut: Option<i64>,
    pub invariants: Vec<PlanInvariant>,
    pub knobs: Vec<PlanKnob>,
    /// `buggify: true`: a B= token is then written to /tmp/hydo-buggify (features/buggify.rs);
    /// without it B= is an unknown knob.
    pub buggify: bool,
    /// The fake outside world (`fakes:` / `proxies:`, driven by `schedule/fakes.rs`).
    pub world: Option<PlanWorld>,
    pub layout: Layout,
}

/// The fake outside world of a manifest with `fakes:` or `proxies:` (M89): one `hydo-world`
/// process serves every fake service and every fault proxy (manifest.sh's HYDO_WORLD and
/// HYDO_WORLD_TARGETS; hydo-build writes what it serves).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanWorld {
    /// The program (`$HYDO_WORLD`).
    pub bin: String,
    /// What it serves (`/etc/hydo/world.json`).
    pub config: String,
    /// Every name an `H=` token may name: each fake, then each proxy, each followed by its
    /// routes as `<name>.<route>` (hydo-build's `world_targets`).
    pub targets: Vec<String>,
}

impl PlanWorld {
    /// hydo-build's `has_world` and `world_targets` of a validated manifest.
    pub fn of(d: &Value) -> Option<PlanWorld> {
        let has = |k: &str| get(d, k).is_some_and(py::truthy);
        if !has("fakes") && !has("proxies") {
            return None;
        }
        let list = |k: &str| {
            d.get(k)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let mut targets = Vec::new();
        for e in list("fakes").iter().chain(list("proxies").iter()) {
            let n = py::str_of(&e["name"]);
            targets.push(n.clone());
            for r in e
                .get("routes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                targets.push(format!("{n}.{}", py::str_of(&r["name"])));
            }
        }
        Some(PlanWorld {
            bin: "/bin/hydo-world".into(),
            config: "/etc/hydo/world.json".into(),
            targets,
        })
    }
}

fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}

fn truthy_str(v: &Value, k: &str) -> String {
    get(v, k)
        .filter(|x| py::truthy(x))
        .map(py::str_of)
        .unwrap_or_default()
}

/// Python's `int(x)` of a validated value.
fn int(v: &Value, what: &str) -> Result<i64, Unsupported> {
    let bad = || Unsupported::other(format!("{what}: {} is not a whole number", py::repr(v)));
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(_) => match py::int_of(v) {
            Some(i) => i64::try_from(i).map_err(|_| bad()),
            None => v
                .as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
                .ok_or_else(bad),
        },
        Value::String(s) => s.trim().parse().map_err(|_| bad()),
        _ => Err(bad()),
    }
}

fn int_or(v: &Value, k: &str, dflt: i64, what: &str) -> Result<i64, Unsupported> {
    match get(v, k) {
        None => Ok(dflt),
        Some(x) => int(x, what),
    }
}

fn words(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .map(py::str_of)
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        Some(x) if py::truthy(x) => py::str_of(x)
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

impl Plan {
    /// The plan of a validated manifest (from `hydo_manifest::load`, `from_embedded` or
    /// `for_explorer`), with the layout of today's images.
    pub fn from_manifest(m: &Manifest) -> Result<Plan, Unsupported> {
        let d = &m.data;
        // The init extensions hydo-build splices for a manifest (hydo-build/src/init.rs
        // `any_extension`): each is a seam. `addr:` alone changes nothing without N=/O= (the
        // links extension only redefines known_svc and net_on), so it is not refused here.
        // Driven: `eventually` invariants (features/liveness.rs), guest.clock_after_cut
        // (features/clock.rs), `buggify: true` (features/buggify.rs).
        if !m.checks.is_empty() {
            return Err(Unsupported::seam(
                Seam::History,
                "checks: (history recording)",
            ));
        }
        if m.errors.is_some() {
            return Err(Unsupported::seam(
                Seam::ErrorLogs,
                "errors: (the error-log scan)",
            ));
        }
        if m.network.as_deref() == Some("bridge") {
            return Err(Unsupported::seam(Seam::NetNamespaces, "network: bridge"));
        }
        if m.faults
            .as_ref()
            .is_some_and(|f| f.iter().any(|x| x == "freeze"))
        {
            return Err(Unsupported::seam(Seam::Freeze, "faults: [freeze]"));
        }
        if get(d, "_compose").is_some_and(py::truthy) {
            return Err(Unsupported::seam(
                Seam::Composer,
                "a test composer plan (test:)",
            ));
        }
        let order = hydo_manifest::service_order(&d["services"])
            .map_err(|e| Unsupported::other(e.message))?;
        let svcs = d["services"].as_array().cloned().unwrap_or_default();
        let mut services = Vec::new();
        for n in &order {
            let s = svcs
                .iter()
                .find(|s| py::str_of(&s["name"]) == *n)
                .ok_or_else(|| Unsupported::other(format!("services: {n} not found")))?;
            let cmd = if get(s, "image").is_some() {
                // hydo-build (oci.rs) replaces an image service's cmd by its run script
                format!("/etc/hydo/oci/{n}/run")
            } else {
                get(s, "cmd")
                    .map(py::str_of)
                    .ok_or_else(|| Unsupported::other(format!("services: {n} has no cmd:")))?
            };
            services.push(PlanService {
                name: n.clone(),
                cmd,
                setup: truthy_str(s, "setup"),
                ready: get(s, "ready").map(py::str_of).unwrap_or_default(),
                timeout: int_or(s, "ready_timeout", 120, "ready_timeout")?,
                port: get(s, "port").map(py::str_of).unwrap_or_default(),
                recovery: get(s, "recovery").map(py::truthy).unwrap_or(true),
            });
        }
        let wl = &d["workload"];
        let run = get(wl, "run").map(py::str_of).unwrap_or_default();
        let pgrep = match get(wl, "process").filter(|p| py::truthy(p)) {
            Some(p) => py::str_of(p),
            None => py::split_ws(&run)
                .first()
                .map(|w| py::basename(w))
                .ok_or_else(|| Unsupported::other("workload.run is only blank space"))?,
        };
        let empty = Value::Object(Default::default());
        let data = get(d, "data").unwrap_or(&empty);
        let guest = get(d, "guest").unwrap_or(&empty);
        let mut invariants = Vec::new();
        for (i, v) in d["invariants"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let every = get(v, "every");
            let kind = get(v, "type")
                .map(py::str_of)
                .unwrap_or_else(|| "always".into());
            let live = if kind == "eventually" {
                Some(PlanLive {
                    within: int(&v["within"], "invariants.within")?,
                    after: get(v, "after")
                        .map(py::str_of)
                        .unwrap_or_else(|| "heal".into()),
                    poll: int_or(v, "poll", 1, "invariants.poll")?,
                })
            } else {
                None
            };
            invariants.push(PlanInvariant {
                key: format!("i{i}"),
                id: py::str_of(&v["id"]),
                kind,
                msg: py::str_of(get(v, "msg").unwrap_or(&v["id"])),
                cmd: py::str_of(&v["cmd"]),
                every: every
                    .filter(|e| py::truthy(e))
                    .map(py::str_of)
                    .unwrap_or_default(),
                // manifest.sh writes final=0 for an eventually invariant, whatever it says
                final_: live.is_none()
                    && match v.get("final") {
                        Some(f) => py::truthy(f),
                        None => every.is_none(),
                    },
                live,
            });
        }
        let knobs = d["knobs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|k| PlanKnob {
                token: py::str_of(&k["token"]),
                default: py::str_of(&k["default"]),
                set: py::str_of(&k["set"]),
                name: get(k, "name")
                    .map(py::str_of)
                    .unwrap_or_else(|| py::str_of(&k["token"])),
            })
            .collect();
        Ok(Plan {
            name: py::str_of(&d["name"]),
            services,
            steps: int(&wl["steps"], "workload.steps")?,
            workload_run: run,
            workload_retry: truthy_str(wl, "retry"),
            workload_pgrep: pgrep,
            workload_timeout: int_or(wl, "timeout", 300, "workload.timeout")?,
            provision: truthy_str(d, "provision"),
            data_opts: get(data, "mount")
                .map(py::str_of)
                .unwrap_or_else(|| "rw".into()),
            data_dirs: words(get(data, "dirs")),
            clock_start: int_or(guest, "clock_start", 1767225600, "guest.clock_start")?,
            clock_after_cut: match get(guest, "clock_after_cut") {
                None => None,
                Some(c) => Some(int(c, "guest.clock_after_cut")?),
            },
            invariants,
            knobs,
            buggify: m.buggify,
            world: PlanWorld::of(d),
            layout: Layout::default(),
        })
    }

    pub fn service(&self, name: &str) -> Option<&PlanService> {
        self.services.iter().find(|s| s.name == name)
    }

    pub fn knob(&self, token: &str) -> Option<&PlanKnob> {
        self.knobs.iter().find(|k| k.token == token)
    }
}
