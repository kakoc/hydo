//! The explorer's options: explore.py's argparse options, with the same names, defaults and
//! choices (argparse's unambiguous prefixes and `--opt=value` too). `--workers` has no fixed
//! default any more: without it the pool is sized from the machine ([`hydo_engine::Engine::max_workers`]).

pub const USAGE: &str = "usage: hydo explore [--seeds 1-200] [--strategy random|swarm|swarm-pos|process] [--workers N]
       [--image IMG] --out DIR [--schedule S] [--sweep KIND] [--manifest hydo.yaml] [--backend hvx|hvm]
       [--timing] [--new-faults] [--durable-only] [--concurrency] [--more-faults] [--preempt] [--buggify]
       [--freeze-call] [--protocol-faults] [--grpc-faults] [--vcpus N --turns [--bursts]] [--cpu v2|v3]
       [--ram-mib N] [--known-errors FILE] [--errors report|fail] [--branch MARKER]
       [--reactive fault|heal|both [--react-events ID,...] [--heal-events ID,...]] ... (explore.py's options)";

#[derive(Clone, Debug)]
pub struct Args {
    pub seeds: String,
    pub workers: Option<usize>,
    pub image: String,
    pub out: String,
    pub schedule: Option<String>,
    pub no_shrink: bool,
    pub timing: bool,
    pub tj_pm: String,
    pub strategy: String,
    pub sweep: Option<String>,
    pub fc_pairs: Option<String>,
    pub fc_dirs: String,
    pub fc_steps: Option<String>,
    pub fc_match: String,
    pub fc_durs: String,
    pub fc_margin: i64,
    pub fc_traces: Option<String>,
    pub sweep_every: i64,
    pub sweep_with: Option<String>,
    pub sweep_dry: bool,
    pub repro: Option<i64>,
    pub fc_adaptive: bool,
    pub fc_stage1: String,
    pub fc_stride: i64,
    pub fc_window: i64,
    pub fc_links: String,
    pub fc_centres: String,
    pub knob_rule: String,
    pub branch: Option<String>,
    pub variants: i64,
    pub durable_only: bool,
    pub new_faults: bool,
    pub concurrency: bool,
    pub more_faults: bool,
    pub freeze_call: bool,
    pub protocol_faults: bool,
    pub grpc_faults: bool,
    pub fc_nmax: i64,
    pub cf_range: String,
    pub pct_exits: String,
    pub preempt: bool,
    pub buggify: bool,
    pub backend: String,
    pub hvm_kernel: Option<String>,
    pub manifest: Option<String>,
    pub cpu: Option<String>,
    pub vcpus: i64,
    pub turns: bool,
    pub no_compose: bool,
    pub bursts: bool,
    pub turn_margin: Option<i64>,
    pub ram_mib: Option<i64>,
    pub known_errors: Option<String>,
    pub errors: Option<String>,
    pub errors_shrink: i64,
    /// M139: `--reactive fault|heal|both` (None: no reactions).
    pub reactive: Option<String>,
    pub react_events: Option<String>,
    pub heal_events: Option<String>,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            seeds: "1-200".into(),
            workers: None,
            image: "explore-disk.img".into(),
            out: String::new(),
            schedule: None,
            no_shrink: false,
            timing: false,
            tj_pm: "50,100,200".into(),
            strategy: "random".into(),
            sweep: None,
            fc_pairs: None,
            fc_dirs: "<>".into(),
            fc_steps: None,
            fc_match: "all,manifest".into(),
            fc_durs: "150000".into(),
            fc_margin: 2,
            fc_traces: None,
            sweep_every: 1,
            sweep_with: None,
            sweep_dry: false,
            repro: None,
            fc_adaptive: false,
            fc_stage1: "auto".into(),
            fc_stride: 8,
            fc_window: 16,
            fc_links: "svc".into(),
            fc_centres: "fail".into(),
            knob_rule: "cut".into(),
            branch: None,
            variants: 8,
            durable_only: false,
            new_faults: false,
            concurrency: false,
            more_faults: false,
            freeze_call: false,
            protocol_faults: false,
            grpc_faults: false,
            fc_nmax: 64,
            cf_range: "auto".into(),
            pct_exits: "3000,10000".into(),
            preempt: false,
            buggify: false,
            backend: "hvx".into(),
            hvm_kernel: None,
            manifest: None,
            cpu: None,
            vcpus: 1,
            turns: false,
            no_compose: false,
            bursts: false,
            turn_margin: None,
            ram_mib: None,
            known_errors: None,
            errors: None,
            errors_shrink: 8,
            reactive: None,
            react_events: None,
            heal_events: None,
        }
    }
}

const FLAGS: [&str; 16] = [
    "no-shrink",
    "timing",
    "sweep-dry",
    "fc-adaptive",
    "durable-only",
    "new-faults",
    "concurrency",
    "more-faults",
    "freeze-call",
    "protocol-faults",
    "grpc-faults",
    "preempt",
    "buggify",
    "turns",
    "no-compose",
    "bursts",
];

const VALUED: [&str; 42] = [
    "seeds",
    "workers",
    "image",
    "out",
    "schedule",
    "tj-pm",
    "strategy",
    "sweep",
    "fc-pairs",
    "fc-dirs",
    "fc-steps",
    "fc-match",
    "fc-durs",
    "fc-margin",
    "fc-traces",
    "sweep-every",
    "sweep-with",
    "repro",
    "fc-stage1",
    "fc-stride",
    "fc-window",
    "fc-links",
    "fc-centres",
    "knob-rule",
    "branch",
    "variants",
    "fc-nmax",
    "cf-range",
    "pct-exits",
    "backend",
    "hvm-kernel",
    "manifest",
    "cpu",
    "vcpus",
    "turn-margin",
    "ram-mib",
    "known-errors",
    "errors",
    "errors-shrink",
    "reactive",
    "react-events",
    "heal-events",
];

fn choice(name: &str, v: &str, ok: &[&str]) -> Result<String, String> {
    if ok.contains(&v) {
        Ok(v.to_string())
    } else {
        Err(format!(
            "argument --{name}: invalid choice: '{v}' (choose from {})",
            ok.iter()
                .map(|x| format!("'{x}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

fn int(name: &str, v: &str) -> Result<i64, String> {
    crate::py::py_int(v).ok_or_else(|| format!("argument --{name}: invalid int value: '{v}'"))
}

/// Parse the explorer's command line. Err: argparse's message (the caller prints the usage).
pub fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut out_given = false;
    let names: Vec<&str> = FLAGS.iter().chain(VALUED.iter()).copied().collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let Some(body) = arg.strip_prefix("--") else {
            return Err(format!("unrecognized arguments: {arg}"));
        };
        let (key, inline) = match body.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (body, None),
        };
        let name = if names.contains(&key) {
            key.to_string()
        } else {
            let m: Vec<&&str> = names.iter().filter(|n| n.starts_with(key)).collect();
            match m.len() {
                1 => m[0].to_string(),
                0 => return Err(format!("unrecognized arguments: {arg}")),
                _ => {
                    return Err(format!(
                        "ambiguous option: --{key} could match {}",
                        m.iter()
                            .map(|n| format!("--{n}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            }
        };
        let is_flag = FLAGS.contains(&name.as_str());
        let val = if is_flag {
            if inline.is_some() {
                return Err(format!("argument --{name}: ignored explicit argument"));
            }
            String::new()
        } else if let Some(v) = inline {
            v
        } else {
            i += 1;
            match argv.get(i) {
                Some(v) if !(v.starts_with("--") && v.len() > 2) => v.clone(),
                _ => return Err(format!("argument --{name}: expected one argument")),
            }
        };
        let v = val.as_str();
        match name.as_str() {
            "no-shrink" => a.no_shrink = true,
            "timing" => a.timing = true,
            "sweep-dry" => a.sweep_dry = true,
            "fc-adaptive" => a.fc_adaptive = true,
            "durable-only" => a.durable_only = true,
            "new-faults" => a.new_faults = true,
            "concurrency" => a.concurrency = true,
            "more-faults" => a.more_faults = true,
            "freeze-call" => a.freeze_call = true,
            "protocol-faults" => a.protocol_faults = true,
            "grpc-faults" => a.grpc_faults = true,
            "preempt" => a.preempt = true,
            "buggify" => a.buggify = true,
            "turns" => a.turns = true,
            "no-compose" => a.no_compose = true,
            "bursts" => a.bursts = true,
            "seeds" => a.seeds = val,
            "workers" => a.workers = Some(int("workers", v)?.max(1) as usize),
            "image" => a.image = val,
            "out" => {
                a.out = val;
                out_given = true;
            }
            "schedule" => a.schedule = Some(val),
            "tj-pm" => a.tj_pm = val,
            "strategy" => {
                a.strategy = choice("strategy", v, &["process", "random", "swarm", "swarm-pos"])?
            }
            "sweep" => a.sweep = Some(val),
            "fc-pairs" => a.fc_pairs = Some(val),
            "fc-dirs" => a.fc_dirs = val,
            "fc-steps" => a.fc_steps = Some(val),
            "fc-match" => a.fc_match = val,
            "fc-durs" => a.fc_durs = val,
            "fc-margin" => a.fc_margin = int("fc-margin", v)?,
            "fc-traces" => a.fc_traces = Some(val),
            "sweep-every" => a.sweep_every = int("sweep-every", v)?,
            "sweep-with" => a.sweep_with = Some(val),
            "repro" => a.repro = Some(int("repro", v)?),
            "fc-stage1" => a.fc_stage1 = val,
            "fc-stride" => a.fc_stride = int("fc-stride", v)?,
            "fc-window" => a.fc_window = int("fc-window", v)?,
            "fc-links" => a.fc_links = choice("fc-links", v, &["pair", "svc"])?,
            "fc-centres" => a.fc_centres = val,
            "knob-rule" => a.knob_rule = choice("knob-rule", v, &["cut", "any"])?,
            "branch" => a.branch = Some(val),
            "variants" => a.variants = int("variants", v)?,
            "fc-nmax" => a.fc_nmax = int("fc-nmax", v)?,
            "cf-range" => a.cf_range = val,
            "pct-exits" => a.pct_exits = val,
            "backend" => a.backend = choice("backend", v, &["hvx", "hvm"])?,
            "hvm-kernel" => a.hvm_kernel = Some(val),
            "manifest" => a.manifest = Some(val),
            "cpu" => a.cpu = Some(choice("cpu", v, &["v2", "v3"])?),
            "vcpus" => a.vcpus = int("vcpus", v)?,
            "turn-margin" => a.turn_margin = Some(int("turn-margin", v)?),
            "ram-mib" => a.ram_mib = Some(int("ram-mib", v)?),
            "known-errors" => a.known_errors = Some(val),
            "errors" => a.errors = Some(choice("errors", v, &["report", "fail"])?),
            "errors-shrink" => a.errors_shrink = int("errors-shrink", v)?,
            "reactive" => a.reactive = Some(choice("reactive", v, &["both", "fault", "heal"])?),
            "react-events" => a.react_events = Some(val),
            "heal-events" => a.heal_events = Some(val),
            other => return Err(format!("unrecognized arguments: --{other}")),
        }
        i += 1;
    }
    if !out_given {
        return Err("the following arguments are required: --out".into());
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: &[&str]) -> Result<Args, String> {
        parse(&x.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn like_argparse() {
        let a = p(&[
            "--out",
            "o",
            "--seeds",
            "1-5",
            "--strategy=swarm",
            "--timing",
            "--work",
            "2",
        ])
        .unwrap();
        assert_eq!(
            (a.seeds.as_str(), a.strategy.as_str(), a.timing, a.workers),
            ("1-5", "swarm", true, Some(2))
        );
        assert!(p(&["--seeds", "1-5"]).is_err());
        assert!(p(&["--out", "o", "--strategy", "bogus"]).is_err());
        assert!(p(&["--out", "o", "--fc"]).is_err());
        assert!(p(&["--out", "o", "--bursts"]).unwrap().bursts);
    }
}
