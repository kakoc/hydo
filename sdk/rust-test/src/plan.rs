//! `System`: the builder whose `plan_json` writes hydo's plan (hvx/onboard/MANIFEST.md, as JSON,
//! keys in a fixed order). Only the host calls it.
use crate::{Registration, HYDO_VERSION, PLAN_FORMAT};

// ------------------------------------------------------------------ a tiny JSON writer
enum J {
    S(String),
    N(i64),
    B(bool),
    A(Vec<J>),
    O(Vec<(String, J)>),
}

fn s(x: &str) -> J {
    J::S(x.to_string())
}

fn obj(kv: Vec<(&str, J)>) -> J {
    J::O(kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn esc(x: &str) -> String {
    let mut o = String::with_capacity(x.len() + 2);
    o.push('"');
    for c in x.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn write(j: &J, ind: usize, out: &mut String) {
    let pad = " ".repeat(ind + 1);
    match j {
        J::S(x) => out.push_str(&esc(x)),
        J::N(n) => out.push_str(&n.to_string()),
        J::B(b) => out.push_str(if *b { "true" } else { "false" }),
        J::A(v) if v.is_empty() => out.push_str("[]"),
        J::A(v) => {
            out.push_str("[\n");
            for (i, x) in v.iter().enumerate() {
                out.push_str(&pad);
                write(x, ind + 1, out);
                out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
            }
            out.push_str(&" ".repeat(ind));
            out.push(']');
        }
        J::O(v) if v.is_empty() => out.push_str("{}"),
        J::O(v) => {
            out.push_str("{\n");
            for (i, (k, x)) in v.iter().enumerate() {
                out.push_str(&pad);
                out.push_str(&esc(k));
                out.push_str(": ");
                write(x, ind + 1, out);
                out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
            }
            out.push_str(&" ".repeat(ind));
            out.push('}');
        }
    }
}

// ------------------------------------------------------------------ the builder
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Kill,
    Pause,
    Network,
    Oneway,
    Cut,
    Clock,
    Disk,
    Freeze,
}

impl Fault {
    fn as_str(self) -> &'static str {
        match self {
            Fault::Kill => "kill",
            Fault::Pause => "pause",
            Fault::Network => "network",
            Fault::Oneway => "oneway",
            Fault::Cut => "cut",
            Fault::Clock => "clock",
            Fault::Disk => "disk",
            Fault::Freeze => "freeze",
        }
    }
}

/// What a saved regression must do when replayed (`hydo repro --regressions`).
#[derive(Clone, Debug)]
pub enum Expect {
    Ok,
    Violation,
    Fingerprint(String),
}

#[derive(Clone, Debug, Default)]
pub struct Service {
    name: String,
    cmd: String,
    ready: String,
    setup: Option<String>,
    port: Option<u16>,
    addr: Option<String>,
    depends_on: Vec<String>,
    ready_timeout: Option<u32>,
    recovery: Option<bool>,
}

impl Service {
    /// A service: its name (also the name fault tokens use) and its command (foreground).
    pub fn new(name: &str, cmd: &str) -> Service {
        Service { name: name.into(), cmd: cmd.into(), ..Default::default() }
    }
    /// The readiness check (a shell command, exit 0 = ready).
    pub fn ready(mut self, cmd: &str) -> Self {
        self.ready = cmd.into();
        self
    }
    pub fn setup(mut self, cmd: &str) -> Self {
        self.setup = Some(cmd.into());
        self
    }
    pub fn port(mut self, p: u16) -> Self {
        self.port = Some(p);
        self
    }
    pub fn addr(mut self, a: &str) -> Self {
        self.addr = Some(a.into());
        self
    }
    pub fn depends_on(mut self, s: &str) -> Self {
        self.depends_on.push(s.into());
        self
    }
    pub fn ready_timeout(mut self, secs: u32) -> Self {
        self.ready_timeout = Some(secs);
        self
    }
    pub fn recovery(mut self, on: bool) -> Self {
        self.recovery = Some(on);
        self
    }
    fn json(&self) -> J {
        let mut v = vec![("name", s(&self.name)), ("cmd", s(&self.cmd)), ("ready", s(&self.ready))];
        if let Some(x) = &self.setup {
            v.push(("setup", s(x)));
        }
        if let Some(p) = self.port {
            v.push(("port", J::N(p as i64)));
        }
        if let Some(a) = &self.addr {
            v.push(("addr", s(a)));
        }
        if !self.depends_on.is_empty() {
            v.push(("depends_on", J::A(self.depends_on.iter().map(|d| s(d)).collect())));
        }
        if let Some(t) = self.ready_timeout {
            v.push(("ready_timeout", J::N(t as i64)));
        }
        if let Some(r) = self.recovery {
            v.push(("recovery", J::B(r)));
        }
        obj(v)
    }
}

/// A durability setting the explorer may relax (MANIFEST.md `knobs:`).
#[derive(Clone, Debug)]
pub struct Knob {
    token: char,
    name: String,
    default: String,
    set: String,
    values: Vec<String>,
}

impl Knob {
    pub fn new(token: char, name: &str, default: &str, set: &str) -> Knob {
        Knob { token, name: name.into(), default: default.into(), set: set.into(), values: vec![] }
    }
    pub fn values(mut self, v: &[&str]) -> Self {
        self.values = v.iter().map(|x| x.to_string()).collect();
        self
    }
}

/// The composer's options (MANIFEST.md "The test composer"); `None` = the engine's default.
#[derive(Clone, Debug, Default)]
pub struct TestParams {
    pub parallel: Option<u32>,
    pub per_round: Option<u32>,
    pub round_ms: Option<u32>,
    pub command_timeout: Option<u32>,
    pub drain_timeout: Option<u32>,
    pub max_commands: Option<u32>,
    pub budget_ms: Option<u32>,
    pub check_every: Option<u32>,
    pub check_within: Option<u32>,
}

/// A ready-made history check (hvx/HISTORIES.md): the host checks the history the drivers record
/// with `hydo_test::assert::history` after the run. Becomes a `test.checks` entry of kind
/// `linearizable` or `list_append`.
#[derive(Clone, Debug, Default)]
pub struct HistoryCheck {
    name: String,
    kind: &'static str,
    history: String,
    model: Option<String>,
    initial: Option<String>,
    consistency: Option<String>,
    msg: Option<String>,
    readback: Option<Vec<String>>,
}

impl HistoryCheck {
    /// Linearizability of a key-value register per key (`model("cas")` adds compare-and-set).
    pub fn linearizable(name: &str, history: &str) -> HistoryCheck {
        HistoryCheck { name: name.into(), kind: "linearizable", history: history.into(), ..Default::default() }
    }
    /// Elle-style list-append checks at `consistency` (default strict-serializable).
    pub fn list_append(name: &str, history: &str) -> HistoryCheck {
        HistoryCheck { name: name.into(), kind: "list_append", history: history.into(), ..Default::default() }
    }
    pub fn model(mut self, m: &str) -> Self {
        self.model = Some(m.into());
        self
    }
    /// `nil` (keys start absent, the default) or `any` (keys written before the history began).
    pub fn initial(mut self, i: &str) -> Self {
        self.initial = Some(i.into());
        self
    }
    pub fn consistency(mut self, c: &str) -> Self {
        self.consistency = Some(c.into());
        self
    }
    pub fn msg(mut self, m: &str) -> Self {
        self.msg = Some(m.into());
        self
    }
    /// The final read-back (an argv run at the end of the same boot or of the recovery boot after
    /// a power cut): read every key or list back and record the reads in the history.
    pub fn readback(mut self, argv: &[&str]) -> Self {
        self.readback = Some(argv.iter().map(|a| a.to_string()).collect());
        self
    }
    fn json(&self) -> J {
        let mut v = vec![("name", s(&self.name)), ("kind", s(self.kind)), ("history", s(&self.history))];
        for (k, x) in [("model", &self.model), ("initial", &self.initial), ("consistency", &self.consistency), ("msg", &self.msg)] {
            if let Some(x) = x {
                v.push((k, s(x)));
            }
        }
        if let Some(r) = &self.readback {
            v.push(("run", J::A(r.iter().map(|a| s(a)).collect())));
        }
        obj(v)
    }
}

#[derive(Clone, Debug, Default)]
pub struct System {
    name: String,
    description: Option<String>,
    files: Vec<(String, String, Option<String>)>,
    text: Vec<(String, String)>,
    root_dirs: Vec<String>,
    nix: Vec<String>,
    data_mib: Option<u32>,
    data_dirs: Vec<String>,
    data_mount: Option<String>,
    env: Vec<(String, String)>,
    shell: Option<String>,
    services: Vec<Service>,
    provision: Option<String>,
    steps: Option<u32>,
    workload_timeout: Option<u32>,
    test: TestParams,
    knobs: Vec<Knob>,
    faults: Vec<Fault>,
    regressions: Vec<(String, String, Expect)>,
    ram_mib: Option<u32>,
    cpu: Option<String>,
    history_checks: Vec<HistoryCheck>,
}

fn host_path(src: &str) -> String {
    // a relative source is relative to where the plan is made (cargo hydo: the crate root)
    if src.starts_with('/') || src.starts_with('~') {
        return src.to_string();
    }
    match std::env::current_dir() {
        Ok(d) => d.join(src).to_string_lossy().to_string(),
        Err(_) => src.to_string(),
    }
}

/// `p` (absolute) relative to the directory `base`; `~` paths and paths on another root stay as they are.
fn relative(p: &str, base: &std::path::Path) -> String {
    if !p.starts_with('/') {
        return p.to_string();
    }
    let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let a: Vec<&str> = p.split('/').filter(|x| !x.is_empty()).collect();
    let bs = base.to_string_lossy().to_string();
    let b: Vec<&str> = bs.split('/').filter(|x| !x.is_empty()).collect();
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return p.to_string();
    }
    let mut out: Vec<String> = std::iter::repeat("..".to_string()).take(b.len() - common).collect();
    out.extend(a[common..].iter().map(|x| x.to_string()));
    if out.is_empty() { ".".into() } else { out.join("/") }
}

impl System {
    pub fn new(name: &str) -> System {
        System { name: name.into(), ..Default::default() }
    }
    pub fn description(mut self, d: &str) -> Self {
        self.description = Some(d.into());
        self
    }
    /// A host file (or directory) copied to `dst` on the guest's read-only root.
    pub fn file(mut self, src: &str, dst: &str) -> Self {
        self.files.push((host_path(src), dst.into(), Some("0755".into())));
        self
    }
    pub fn file_mode(mut self, src: &str, dst: &str, mode: &str) -> Self {
        self.files.push((host_path(src), dst.into(), Some(mode.into())));
        self
    }
    pub fn text(mut self, dst: &str, content: &str) -> Self {
        self.text.push((dst.into(), content.into()));
        self
    }
    pub fn root_dir(mut self, d: &str) -> Self {
        self.root_dirs.push(d.into());
        self
    }
    /// A Nix store path whose closure goes on the root (needs nix-store on the build machine).
    pub fn nix(mut self, p: &str) -> Self {
        self.nix.push(p.into());
        self
    }
    /// The read-write data disk (/data): its size and the directories created on every mount.
    pub fn data(mut self, mib: u32, dirs: &[&str]) -> Self {
        self.data_mib = Some(mib);
        self.data_dirs = dirs.iter().map(|d| d.to_string()).collect();
        self
    }
    pub fn data_mount(mut self, opts: &str) -> Self {
        self.data_mount = Some(opts.into());
        self
    }
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
    pub fn shell(mut self, helpers: &str) -> Self {
        self.shell = Some(helpers.into());
        self
    }
    pub fn service(mut self, svc: Service) -> Self {
        self.services.push(svc);
        self
    }
    pub fn provision(mut self, cmd: &str) -> Self {
        self.provision = Some(cmd.into());
        self
    }
    /// Rounds of the composition (= workload steps: faults are placed at round numbers).
    pub fn steps(mut self, n: u32) -> Self {
        self.steps = Some(n);
        self
    }
    pub fn workload_timeout(mut self, secs: u32) -> Self {
        self.workload_timeout = Some(secs);
        self
    }
    pub fn test(mut self, p: TestParams) -> Self {
        self.test = p;
        self
    }
    pub fn knob(mut self, k: Knob) -> Self {
        self.knobs.push(k);
        self
    }
    pub fn faults(mut self, f: &[Fault]) -> Self {
        self.faults = f.to_vec();
        self
    }
    pub fn regression(mut self, name: &str, schedule: &str, expect: Expect) -> Self {
        self.regressions.push((name.into(), schedule.into(), expect));
        self
    }
    pub fn ram_mib(mut self, mib: u32) -> Self {
        self.ram_mib = Some(mib);
        self
    }
    pub fn cpu(mut self, level: &str) -> Self {
        self.cpu = Some(level.into());
        self
    }
    /// A ready-made history check (hvx/HISTORIES.md), checked on the host after the run.
    pub fn history_check(mut self, c: HistoryCheck) -> Self {
        self.history_checks.push(c);
        self
    }

    /// hydo's plan as JSON: this system plus the registered drivers and checks, which run as
    /// `<guest_path> driver|check <name>` from the guest binary `guest_binary` (copied to
    /// `guest_path` on the root).
    pub fn plan_json(&self, guest_binary: &str, guest_path: &str, regs: &[&Registration]) -> String {
        self.plan_json_at(guest_binary, guest_path, regs, None)
    }

    /// The same, with every local file path written relative to `base` (the directory the plan
    /// file goes to: hydo-build reads relative sources from the plan's own directory, so the
    /// plan and its files can be copied to the build machine together).
    pub fn plan_json_at(&self, guest_binary: &str, guest_path: &str, regs: &[&Registration], base: Option<&std::path::Path>) -> String {
        let rel = |p: &str| match base { Some(b) => relative(p, b), None => p.to_string() };
        let mut top: Vec<(&str, J)> = vec![
            ("format", J::N(PLAN_FORMAT as i64)),
            ("hydo_version", s(HYDO_VERSION)),
            ("name", s(&self.name)),
        ];
        if let Some(d) = &self.description {
            top.push(("description", s(d)));
        }
        let mut g = vec![];
        if let Some(r) = self.ram_mib {
            g.push(("ram_mib", J::N(r as i64)));
        }
        if let Some(c) = &self.cpu {
            g.push(("cpu", s(c)));
        }
        if !g.is_empty() {
            top.push(("guest", obj(g)));
        }
        let mut files: Vec<J> = self.files.iter().map(|(src, dst, mode)| {
            let mut v = vec![("src", s(&rel(src))), ("dst", s(dst))];
            if let Some(m) = mode {
                v.push(("mode", s(m)));
            }
            obj(v)
        }).collect();
        files.push(obj(vec![("src", s(&rel(&host_path(guest_binary)))), ("dst", s(guest_path)), ("mode", s("0755"))]));
        let mut root = vec![("files", J::A(files))];
        if !self.text.is_empty() {
            root.push(("text", J::O(self.text.iter().map(|(k, v)| (k.clone(), s(v))).collect())));
        }
        if !self.root_dirs.is_empty() {
            root.push(("dirs", J::A(self.root_dirs.iter().map(|d| s(d)).collect())));
        }
        if !self.nix.is_empty() {
            root.push(("nix", J::A(self.nix.iter().map(|d| s(d)).collect())));
        }
        top.push(("root", obj(root)));
        let mut data = vec![];
        if let Some(m) = self.data_mib {
            data.push(("size_mib", J::N(m as i64)));
        }
        if let Some(m) = &self.data_mount {
            data.push(("mount", s(m)));
        }
        if !self.data_dirs.is_empty() {
            data.push(("dirs", J::A(self.data_dirs.iter().map(|d| s(d)).collect())));
        }
        if !data.is_empty() {
            top.push(("data", obj(data)));
        }
        if !self.env.is_empty() {
            top.push(("env", J::O(self.env.iter().map(|(k, v)| (k.clone(), s(v))).collect())));
        }
        if let Some(sh) = &self.shell {
            top.push(("shell", s(sh)));
        }
        top.push(("services", J::A(self.services.iter().map(|x| x.json()).collect())));
        if let Some(p) = &self.provision {
            top.push(("provision", s(p)));
        }
        let mut wl = vec![];
        if let Some(n) = self.steps {
            wl.push(("steps", J::N(n as i64)));
        }
        if let Some(t) = self.workload_timeout {
            wl.push(("timeout", J::N(t as i64)));
        }
        if !wl.is_empty() {
            top.push(("workload", obj(wl)));
        }
        let mut t: Vec<(&str, J)> = vec![];
        let p = &self.test;
        for (k, v) in [("parallel", p.parallel), ("per_round", p.per_round), ("round_ms", p.round_ms),
                       ("command_timeout", p.command_timeout), ("drain_timeout", p.drain_timeout),
                       ("max_commands", p.max_commands), ("budget_ms", p.budget_ms),
                       ("check_every", p.check_every), ("check_within", p.check_within)] {
            if let Some(v) = v {
                t.push((k, J::N(v as i64)));
            }
        }
        let run = |role: &str, name: &str| J::A(vec![s(guest_path), s(role), s(name)]);
        let cmds: Vec<J> = regs.iter().filter(|r| r.role == "driver").map(|r| {
            let mut v = vec![("name", s(r.name)), ("kind", s(r.kind)), ("run", run("driver", r.name))];
            if r.timeout > 0 {
                v.push(("timeout", J::N(r.timeout as i64)));
            }
            obj(v)
        }).collect();
        let mut checks: Vec<J> = regs.iter().filter(|r| r.role == "check").map(|r| {
            let mut v = vec![("name", s(r.name)), ("kind", s(r.kind)), ("run", run("check", r.name))];
            if !r.msg.is_empty() {
                v.push(("msg", s(r.msg)));
            }
            for (k, x) in [("every", r.every), ("within", r.within), ("poll", r.poll)] {
                if x > 0 {
                    v.push((k, J::N(x as i64)));
                }
            }
            if !r.after.is_empty() {
                v.push(("after", s(r.after)));
            }
            obj(v)
        }).collect();
        checks.extend(self.history_checks.iter().map(|c| c.json()));
        t.push(("commands", J::A(cmds)));
        t.push(("checks", J::A(checks)));
        if !self.regressions.is_empty() {
            t.push(("regressions", J::A(self.regressions.iter().map(|(n, sc, e)| {
                let e = match e {
                    Expect::Ok => "ok".to_string(),
                    Expect::Violation => "violation".to_string(),
                    Expect::Fingerprint(f) => format!("fp={}", f),
                };
                obj(vec![("name", s(n)), ("schedule", s(sc)), ("expect", J::S(e))])
            }).collect())));
        }
        top.push(("test", obj(t)));
        if !self.knobs.is_empty() {
            top.push(("knobs", J::A(self.knobs.iter().map(|k| {
                let mut v = vec![("token", J::S(k.token.to_string())), ("name", s(&k.name)), ("default", s(&k.default)),
                                 ("set", s(&k.set))];
                if !k.values.is_empty() {
                    v.push(("values", J::A(k.values.iter().map(|x| s(x)).collect())));
                }
                obj(v)
            }).collect())));
        }
        if !self.faults.is_empty() {
            top.push(("faults", J::A(self.faults.iter().map(|f| s(f.as_str())).collect())));
        }
        let mut out = String::new();
        write(&obj(top), 0, &mut out);
        out.push('\n');
        out
    }
}
