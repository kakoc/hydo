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

use crate::seams::Unsupported;
use hydo_manifest::py;
use hydo_manifest::{Manifest, Restart};
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
    /// The image's pinned tools (manifest.sh's HYDO_TC, HYDO_IP and HYDO_IPT; "" = not in the
    /// image). They are image facts: [`Plan::from_manifest`] fills hydo-build's `--print-shell`
    /// placeholders when the manifest's faults need the tool, and the engine side (step C1)
    /// gives the image's real paths. NET-FAULTS, NET-NAMESPACES and FREEZE run them through
    /// `run` (docs/HISTORY.md M124).
    pub tc: String,
    pub ip: String,
    pub iptables: String,
    /// The musl strerror table of the image's busybox, for the words busybox `mount` would print
    /// when the data disk cannot be mounted (an image fact like the tools; [`crate::busybox`],
    /// M131). The default is musl 1.2.x's; `agent::plan_of_image` reads the image's busybox.
    pub strerror: crate::busybox::Strerror,
}

/// hydo-build's `--print-shell` placeholders for the pinned tools (`print_shell_tools`).
pub const TC_PLACEHOLDER: &str = "/nix/store/...-iproute2/bin/tc";
pub const IP_PLACEHOLDER: &str = "/nix/store/...-iproute2/bin/ip";
pub const IPTABLES_PLACEHOLDER: &str = "/nix/store/...-iptables/bin/iptables-legacy";
/// manifest.sh's HYDO_NET_GW (hydo-build shell.rs `NET_GW`): the bridge's own address.
pub const NET_GW: &str = "10.89.0.1";

impl Default for Layout {
    fn default() -> Layout {
        Layout {
            busybox: "/bin/busybox".into(),
            prelude: "/etc/hydo/prelude.sh".into(),
            dir: "/tmp/hydo".into(),
            data_dev: "/dev/vda2".into(),
            data_dir: "/data".into(),
            data_fstype: "ext4".into(),
            tc: String::new(),
            ip: String::new(),
            iptables: String::new(),
            strerror: crate::busybox::Strerror::default(),
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
    /// The service's `addr:` ("" = none): link faults (hydo-init-links.sh), the freeze's pair,
    /// and its namespace's address with `network: bridge`.
    pub addr: String,
    /// `restart:` (M134): what the controller does when the service's process ends by itself
    /// while the test runs (`schedule/restart.rs`). The shell init has no such key; `no`, the
    /// default, is its behaviour.
    pub restart: Restart,
    /// `restart_may_fail:` (M136): the restart at the heal may fail; the run then goes on without
    /// the workload's retry, and without a power cut ends in ERROR naming the service
    /// (`schedule.rs` `restart_dead`; MANIFEST.md "A restart after the faults that may fail").
    pub restart_may_fail: bool,
    /// `start_after: started`: the service starts once every service before it was started,
    /// without waiting for their readiness checks (`schedule.rs` `start_all`); false (`ready`,
    /// the default): once every service before it is ready.
    pub start_after_started: bool,
    /// The service's own log files besides its output (`logs:`, then `errors.files`), absolute
    /// paths or shell patterns: the diagnosis and `hydo inspect logs` read their tails.
    pub logs: Vec<String>,
}

/// One invariant (manifest.sh's `HYDO_INV_<k>_*`).
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
    /// `HYDO_INV_<k>_within/after/poll`; hydo-liveness.sh).
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

/// One knob (manifest.sh's `HYDO_KNOB_<token>_*`).
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
    /// `guest.outcomes_after_cut: true`: the recovery boot after a power cut starts with the
    /// first boot's last outcome of every id in `$HYDO_OUTCOMES` (without it that file starts
    /// empty there).
    pub outcomes_after_cut: bool,
    pub invariants: Vec<PlanInvariant>,
    pub knobs: Vec<PlanKnob>,
    /// `buggify: true`: a B= token is then written to /tmp/hydo-buggify (features/buggify.rs);
    /// without it B= is an unknown knob.
    pub buggify: bool,
    /// `checks:` (the history seam, M123).
    pub history: Vec<crate::schedule::history::HistCheck>,
    /// `errors:` (the error-logs seam, M123).
    pub errors: Option<crate::schedule::errlog::ErrPlan>,
    /// A test composer plan (`test:`; the composer seam, M123).
    pub compose: Option<crate::schedule::composer::ComposePlan>,
    /// `freeze` in `faults:`: the image has hydo-init-freeze.sh, so F= is the freeze at a packet
    /// (without it F= is an unknown knob).
    pub freeze: bool,
    /// `network: bridge`: every service in its own network namespace (hydo-init-netns.sh).
    pub bridge: bool,
    /// manifest.sh's HYDO_NET_EXTRA: the fake world's addresses on the bridge besides NET_GW.
    pub net_extra: Vec<String>,
    /// The fake outside world (`fakes:` / `proxies:`, driven by `schedule/fakes.rs`).
    pub world: Option<PlanWorld>,
    /// `services[].storage` (docs/design/storage-faults.md), in the manifest's order: the
    /// directories G= corrupts and the own file systems L= takes away (schedule/storage.rs).
    pub storage: Vec<PlanStorage>,
    /// `groups:` (Milestone 195): named sets of services that fault targets select from
    /// (`K=<group>.majority`, `N=<group>.halves`), in the manifest's order.
    pub groups: Vec<PlanGroup>,
    pub layout: Layout,
    /// `HYDO_COST` (an experiment, docs/design/cost-tracking.md): read the guest's work counters
    /// (`/proc`) as `HYDO-COST` report lines: 0 never, 1 after each conversation's verdict, 2 also
    /// at the workload's start and end and in a recovery boot before its checks. Not a manifest
    /// key: the host's choice, off by default, so nothing changes without it.
    pub cost: u8,
}

/// One group of services (`groups:`): its members in the manifest's order and the command that
/// prints the current primary's service name ("" = none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanGroup {
    pub name: String,
    pub services: Vec<String>,
    pub primary: String,
}

impl PlanGroup {
    fn of(d: &Value) -> Vec<PlanGroup> {
        d.get("groups")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|g| PlanGroup {
                name: py::str_of(&g["name"]),
                services: g
                    .get("services")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(py::str_of)
                    .collect(),
                primary: get(g, "primary").map(py::str_of).unwrap_or_default(),
            })
            .collect()
    }
}

/// One service's storage: its directory, and its own file system's partition when it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStorage {
    pub svc: String,
    /// The directory, absolute (under the data disk's mount point).
    pub dir: String,
    /// The own file system's partition number on the data disk (5, 6, ...), when it has one.
    pub part: Option<u32>,
}

impl PlanStorage {
    /// The own file system's device: the data partition's disk with the partition's number
    /// (/dev/vda2 -> /dev/vda5).
    pub fn dev(&self, l: &Layout) -> Option<String> {
        self.part.map(|n| {
            format!(
                "{}{n}",
                l.data_dev.trim_end_matches(|c: char| c.is_ascii_digit())
            )
        })
    }
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
    /// Whether the test composer runs the workload (a plan with commands to compose). A plan
    /// of only checks or regressions keeps the manifest's own `workload.run`.
    pub fn composed(&self) -> bool {
        self.compose.as_ref().is_some_and(|c| c.composed)
    }

    /// The plan of a validated manifest (from `hydo_manifest::load`, `from_embedded` or
    /// `for_explorer`), with the layout of today's images.
    pub fn from_manifest(m: &Manifest) -> Result<Plan, Unsupported> {
        let d = &m.data;
        // The init extensions hydo-build splices for a manifest (hydo-build/src/init.rs
        // `any_extension`): each is a seam. `addr:` alone changes nothing without N=/O= (the
        // links extension only redefines known_svc and net_on), so it is not refused here.
        // Driven: `eventually` invariants (features/liveness.rs), guest.clock_after_cut
        // (features/clock.rs), `buggify: true` (features/buggify.rs); checks:, errors: and test:
        // (schedule/history.rs, errlog.rs, composer.rs); network: bridge, fakes:, freeze.
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
                // `tcp:<port>` becomes the connect check it stands for (Milestone 239)
                ready: hydo_manifest::ready_command(
                    &get(s, "ready").map(py::str_of).unwrap_or_default(),
                    get(s, "addr").map(py::str_of).as_deref(),
                ),
                timeout: int_or(s, "ready_timeout", 120, "ready_timeout")?,
                port: get(s, "port").map(py::str_of).unwrap_or_default(),
                recovery: get(s, "recovery").map(py::truthy).unwrap_or(true),
                addr: get(s, "addr").map(py::str_of).unwrap_or_default(),
                restart: match get(s, "restart") {
                    Some(r) => Restart::parse(r)
                        .map_err(|m| Unsupported::other(format!("services: {n}: restart: {m}")))?,
                    None => Restart::No,
                },
                restart_may_fail: get(s, "restart_may_fail") == Some(&Value::Bool(true)),
                start_after_started: get(s, "start_after").map(py::str_of).as_deref()
                    == Some("started"),
                logs: {
                    let mut v: Vec<String> = Vec::new();
                    let lists = [
                        get(s, "logs"),
                        get(s, "errors").and_then(|e| get(e, "files")),
                    ];
                    for x in lists
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_array)
                        .flatten()
                    {
                        let x = py::str_of(x);
                        if !v.contains(&x) {
                            v.push(x);
                        }
                    }
                    v
                },
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
        // hydo-build shell.rs: `faults:` as listed, and `faults or DEFAULT_FAULT_KINDS`
        let listed: Vec<String> = match get(d, "faults") {
            Some(Value::Array(a)) => a.iter().map(py::str_of).collect(),
            _ => Vec::new(),
        };
        let kinds: Vec<String> = if get(d, "faults").is_some_and(py::truthy) {
            listed.clone()
        } else {
            ["kill", "pause", "network", "oneway", "cut", "clock", "disk"]
                .iter()
                .map(|x| x.to_string())
                .collect()
        };
        let freeze = listed.iter().any(|f| f == "freeze");
        let bridge = get(d, "network").map(py::str_of).as_deref() == Some("bridge");
        // world_addrs: the addresses hydo-world listens on besides NET_GW (network: bridge)
        let mut net_extra: Vec<String> = Vec::new();
        if bridge {
            for kind in ["fakes", "proxies"] {
                for e in d[kind].as_array().cloned().unwrap_or_default() {
                    let a = get(&e, "addr")
                        .map(py::str_of)
                        .unwrap_or_else(|| NET_GW.into());
                    if a != NET_GW && !net_extra.contains(&a) {
                        net_extra.push(a);
                    }
                }
            }
        }
        let layout = Layout {
            // hydo-build x86.rs: tc when the fault kinds have network or oneway, iptables with
            // freeze, ip with network: bridge
            tc: if kinds.iter().any(|f| f == "network" || f == "oneway") {
                TC_PLACEHOLDER.into()
            } else {
                String::new()
            },
            ip: if bridge {
                IP_PLACEHOLDER.into()
            } else {
                String::new()
            },
            iptables: if freeze {
                IPTABLES_PLACEHOLDER.into()
            } else {
                String::new()
            },
            ..Layout::default()
        };
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
            outcomes_after_cut: get(guest, "outcomes_after_cut").is_some_and(py::truthy),
            cost: 0,
            invariants,
            knobs,
            buggify: m.buggify,
            history: crate::schedule::history::plan_of(d),
            errors: crate::schedule::errlog::ErrPlan::of(d, &order),
            compose: crate::schedule::composer::ComposePlan::of(d)?,
            freeze,
            bridge,
            net_extra,
            world: PlanWorld::of(d),
            storage: {
                let vols = hydo_manifest::storage::volumes(d);
                hydo_manifest::storage::of_manifest(d)
                    .into_iter()
                    .map(|(svc, st)| PlanStorage {
                        part: vols.iter().find(|(n, _, _)| *n == svc).map(|v| v.2),
                        dir: format!("{}/{}", layout.data_dir, st.dir),
                        svc,
                    })
                    .collect()
            },
            groups: PlanGroup::of(d),
            layout,
        })
    }

    pub fn service(&self, name: &str) -> Option<&PlanService> {
        self.services.iter().find(|s| s.name == name)
    }

    pub fn group(&self, name: &str) -> Option<&PlanGroup> {
        self.groups.iter().find(|g| g.name == name)
    }

    pub fn storage_of(&self, svc: &str) -> Option<&PlanStorage> {
        self.storage.iter().find(|s| s.svc == svc)
    }

    pub fn knob(&self, token: &str) -> Option<&PlanKnob> {
        self.knobs.iter().find(|k| k.token == token)
    }
}
