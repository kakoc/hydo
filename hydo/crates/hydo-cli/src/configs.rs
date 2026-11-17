//! `hydo explore --configs` (Milestone 299): configuration as a dimension that hydo explores.
//!
//! The manifest's `configs:` names build parameters (`params:`) and the values to try, the
//! default first (`hydo_manifest::configs`). This command builds one image per combination of
//! values (`hydo build --set <param>=<value>`, through the image cache of `hydo versions`, so a
//! second run builds nothing), explores each image with the same seeds and the same explorer
//! options (a child `hydo explore` per combination), and answers which settings break which
//! promise under which fault: per combination the classes of failure with their minimal
//! schedules and replay commands, and per failure the values it needs. A failure found only with
//! values whose `tradeoff:` note says they weaken a promise, and not found without them, is the
//! setting's documented trade-off, not a bug.
//!
//! The combinations: `all` (every combination), `one` (the defaults, then one setting changed at
//! a time) or `pairs` (a set that covers every pair of values of two settings). Without
//! `--configs-mode`, `all` up to [`ALL_MAX`] combinations, `pairs` above.
//!
//! The answer is `configs.json` (`hydo_results::ConfigsDoc`), its table on stdout, `hydo report
//! <dir>` (the table again) and `hydo report --html <dir>` (a page).
//!
//! Tests: the scripted engine (`HYDO_SCRIPTED_ENGINE`) builds the stand-in images and answers the
//! runs, with the combination's image in `HYDO_SCRIPTED_IMAGE` (tests/configs.rs); no VM runs.

use crate::ci::key;
use crate::ctx::{abspath, die, take, Ctx, Platform};
use crate::versions::{findings_of_exploration, run_logged, slug, Maker};
use hydo_results::{
    config_status as cs, ConfigClass, ConfigCombination, ConfigFinding, ConfigSettingInfo,
    ConfigTradeoff, ConfigValue, ConfigsDoc,
};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Without `--configs-mode`: every combination up to this many, a pairwise set above.
pub const ALL_MAX: usize = 12;

pub const HELP: &str = r#"hydo explore --configs -- explore each combination of the manifest's settings (hydo guide workflow)

    hydo explore --configs [MANIFEST] [--configs-mode all|one|pairs] [--set KEY=VALUE]
                 [--cache DIR] [--out DIR] [any other explorer option]

  builds one image per combination of the values that the manifest's configs: lists (each a
  build parameter of params:, the default first), explores each image with the same seeds and
  options, and prints a table: each combination's values, then the classes of failure it found,
  each with its minimal schedule and the command that replays it. A failure that appears only
  with some values names those values. A failure found only with values whose tradeoff: note
  says they weaken a promise is labeled the setting's documented trade-off, not a bug.

  MANIFEST defaults to hydo/hydo.yaml.
  --configs-mode all    every combination (the default up to 12 combinations)
  --configs-mode one    the defaults, then one setting changed at a time
  --configs-mode pairs  a set that covers every pair of values of two settings (the default above 12)
  --set KEY=VALUE       a parameter for every combination's build (repeatable)
  --cache DIR           the image cache (default $HYDO_HOME/versions, shared with hydo versions)
  --out DIR             where the explorations and configs.json go (default hydo-out/configs)
  Every other option goes to each exploration (--seeds, --strategy, --workers, ...); without
  --seeds the seeds are 1-20.

  `hydo report DIR` prints the table again, and `hydo report --html DIR` writes it as a page.
  The exit status is 0 when every combination was explored, whatever it found, and 1 otherwise.
"#;

// ------------------------------------------------------------------ the combinations (pure)

/// How the combinations are chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    All,
    One,
    Pairs,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::All => "all",
            Mode::One => "one",
            Mode::Pairs => "pairs",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "all" => Some(Mode::All),
            "one" => Some(Mode::One),
            "pairs" => Some(Mode::Pairs),
            _ => None,
        }
    }
}

/// The number of combinations of every value (saturating).
pub fn product(sizes: &[usize]) -> usize {
    sizes
        .iter()
        .fold(1usize, |a, &n| a.saturating_mul(n.max(1)))
}

/// The mode without `--configs-mode`.
pub fn default_mode(sizes: &[usize]) -> Mode {
    if product(sizes) <= ALL_MAX {
        Mode::All
    } else {
        Mode::Pairs
    }
}

/// The combinations as value indexes per setting (0: the default), the defaults first.
pub fn combinations(sizes: &[usize], mode: Mode) -> Vec<Vec<usize>> {
    let n = sizes.len();
    let base = vec![0; n];
    match mode {
        Mode::All => {
            let mut out = vec![base];
            loop {
                let mut c = out.last().unwrap().clone();
                // the next combination in order, the last setting changing fastest
                let mut i = n;
                loop {
                    if i == 0 {
                        return out;
                    }
                    i -= 1;
                    c[i] += 1;
                    if c[i] < sizes[i].max(1) {
                        break;
                    }
                    c[i] = 0;
                }
                out.push(c);
            }
        }
        Mode::One => {
            let mut out = vec![base.clone()];
            for (i, &s) in sizes.iter().enumerate() {
                for v in 1..s {
                    let mut c = base.clone();
                    c[i] = v;
                    out.push(c);
                }
            }
            out
        }
        Mode::Pairs => pairwise(sizes),
    }
}

/// A set of combinations that covers every pair of values of two settings, the defaults first:
/// greedy, deterministic. Each new combination starts from the first pair not covered yet and
/// gives every other setting the value that covers most new pairs (the lowest value of a tie).
/// With one setting, each value once.
pub fn pairwise(sizes: &[usize]) -> Vec<Vec<usize>> {
    let n = sizes.len();
    if n < 2 {
        return combinations(sizes, Mode::One);
    }
    type Pairs = std::collections::BTreeSet<(usize, usize, usize, usize)>;
    let mut covered = Pairs::new();
    let mark = |c: &[usize], cov: &mut Pairs| {
        for i in 0..n {
            for j in i + 1..n {
                cov.insert((i, c[i], j, c[j]));
            }
        }
    };
    let mut out = vec![vec![0; n]];
    mark(&out[0], &mut covered);
    loop {
        let mut first = None;
        'find: for i in 0..n {
            for j in i + 1..n {
                for a in 0..sizes[i] {
                    for b in 0..sizes[j] {
                        if !covered.contains(&(i, a, j, b)) {
                            first = Some((i, a, j, b));
                            break 'find;
                        }
                    }
                }
            }
        }
        let Some((i, a, j, b)) = first else {
            return out;
        };
        let mut c: Vec<Option<usize>> = vec![None; n];
        c[i] = Some(a);
        c[j] = Some(b);
        for k in 0..n {
            if c[k].is_some() {
                continue;
            }
            let gain = |v: usize| {
                (0..n)
                    .filter(|&m| m != k)
                    .filter_map(|m| c[m].map(|x| (m, x)))
                    .filter(|&(m, x)| {
                        let p = if m < k { (m, x, k, v) } else { (k, v, m, x) };
                        !covered.contains(&p)
                    })
                    .count()
            };
            let best = (0..sizes[k].max(1))
                .max_by_key(|&v| (gain(v), std::cmp::Reverse(v)))
                .unwrap_or(0);
            c[k] = Some(best);
        }
        let c: Vec<usize> = c.into_iter().map(|x| x.unwrap_or(0)).collect();
        mark(&c, &mut covered);
        out.push(c);
    }
}

// ------------------------------------------------------------------ the findings across them (pure)

/// The failures across the combinations: per finding key (else class and title), where it was
/// found, the values it needs, and whether a documented trade-off explains it.
pub fn findings(
    settings: &[ConfigSettingInfo],
    combos: &[ConfigCombination],
) -> Vec<ConfigFinding> {
    let explored: Vec<&ConfigCombination> =
        combos.iter().filter(|c| c.status == cs::EXPLORED).collect();
    let id = |k: &ConfigClass| {
        k.key
            .clone()
            .unwrap_or_else(|| format!("{} {}", k.class, k.title.clone().unwrap_or_default()))
    };
    let tradeoff_of = |param: &str, value: &str| -> Option<String> {
        settings
            .iter()
            .find(|s| s.param == param)?
            .tradeoffs
            .iter()
            .find(|t| t.value == value)
            .map(|t| t.note.clone())
    };
    let has_tradeoff = |c: &ConfigCombination| {
        c.values
            .iter()
            .any(|v| tradeoff_of(&v.param, &v.value).is_some())
    };
    let mut ids: Vec<String> = Vec::new();
    for c in &explored {
        for k in &c.classes {
            let i = id(k);
            if !ids.contains(&i) {
                ids.push(i);
            }
        }
    }
    let mut out = Vec::new();
    for i in ids {
        let with: Vec<(&ConfigCombination, &ConfigClass)> = explored
            .iter()
            .filter_map(|c| c.classes.iter().find(|k| id(k) == i).map(|k| (*c, k)))
            .collect();
        let found: Vec<&ConfigCombination> = with.iter().map(|(c, _)| *c).collect();
        let not_found: Vec<String> = explored
            .iter()
            .filter(|c| !found.iter().any(|f| f.label == c.label))
            .map(|c| c.label.clone())
            .collect();
        // per setting: the values it appeared with, when not every explored value
        let mut only_with = Vec::new();
        for s in settings {
            let vals_of = |cs: &[&ConfigCombination]| -> Vec<String> {
                s.values
                    .iter()
                    .filter(|v| {
                        cs.iter().any(|c| {
                            c.values
                                .iter()
                                .any(|x| x.param == s.param && &x.value == *v)
                        })
                    })
                    .cloned()
                    .collect()
            };
            let all = vals_of(&explored);
            let some = vals_of(&found);
            if some.len() < all.len() {
                only_with.extend(some.into_iter().map(|v| ConfigValue {
                    param: s.param.clone(),
                    value: v,
                }));
            }
        }
        let by_design =
            found.iter().all(|c| has_tradeoff(c)) && explored.iter().any(|c| !has_tradeoff(c));
        let mut tradeoffs: Vec<ConfigTradeoff> = Vec::new();
        if by_design {
            for c in &found {
                for v in &c.values {
                    if let Some(note) = tradeoff_of(&v.param, &v.value) {
                        let t = ConfigTradeoff {
                            value: format!("{}={}", v.param, v.value),
                            note,
                        };
                        if !tradeoffs.contains(&t) {
                            tradeoffs.push(t);
                        }
                    }
                }
            }
        }
        // the shortest minimal schedule of the combinations that found it
        let (_, best) = with
            .iter()
            .min_by_key(|(_, k)| (k.schedule.split_whitespace().count(), k.schedule.len()))
            .expect("found at least once");
        out.push(ConfigFinding {
            class: best.class.clone(),
            key: best.key.clone(),
            title: best.title.clone(),
            found_with: found.iter().map(|c| c.label.clone()).collect(),
            not_found_with: not_found,
            only_with,
            by_design,
            tradeoffs,
            schedule: best.schedule.clone(),
            replay: best.replay.clone(),
        });
    }
    out
}

fn values_words(vs: &[ConfigValue]) -> String {
    // appendfsync=everysec, no; mode=fast
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < vs.len() {
        let p = &vs[i].param;
        let mut vals = Vec::new();
        while i < vs.len() && &vs[i].param == p {
            vals.push(vs[i].value.clone());
            i += 1;
        }
        parts.push(format!("{p}={}", vals.join(", ")));
    }
    parts.join("; ")
}

/// The answer in one or two sentences.
pub fn summary(doc: &ConfigsDoc) -> String {
    let explored = doc
        .combinations
        .iter()
        .filter(|c| c.status == cs::EXPLORED)
        .count();
    let mut s = format!(
        "{} of {} combination{} of {} setting{} explored with seeds {}.",
        explored,
        doc.combinations.len(),
        if doc.combinations.len() == 1 { "" } else { "s" },
        doc.settings.len(),
        if doc.settings.len() == 1 { "" } else { "s" },
        doc.seeds
    );
    if doc.findings.is_empty() {
        s.push_str(" No combination failed.");
    }
    for f in &doc.findings {
        let what = f.title.clone().unwrap_or_else(|| f.class.clone());
        s.push_str(&format!(" {} ({}) ", f.class, what));
        if f.only_with.is_empty() {
            s.push_str("appears with every value explored");
        } else {
            s.push_str(&format!("appears only with {}", values_words(&f.only_with)));
        }
        if f.by_design {
            s.push_str(": the setting's documented trade-off");
        }
        s.push('.');
    }
    if let Some(d) = doc
        .combinations
        .iter()
        .find(|c| c.defaults && c.status == cs::EXPLORED)
    {
        if d.failed == 0 {
            s.push_str(" The defaults passed every schedule.");
        }
    }
    s
}

/// The table for people (stdout and `hydo report <dir>`).
pub fn text(doc: &ConfigsDoc, dir: &str) -> String {
    let mut o = format!(
        "hydo explore --configs: {} combination{} of {} setting{} ({}), seeds {}, on {}\n\n",
        doc.combinations.len(),
        if doc.combinations.len() == 1 { "" } else { "s" },
        doc.settings.len(),
        if doc.settings.len() == 1 { "" } else { "s" },
        doc.mode,
        doc.seeds,
        doc.engine.backend.as_deref().unwrap_or("?")
    );
    o.push_str("Settings (the default first):\n");
    for s in &doc.settings {
        o.push_str(&format!(
            "  {}{}: {}\n",
            s.param,
            match s.services.len() {
                0 => String::new(),
                1 => format!(" (service {})", s.services[0]),
                _ => format!(" (services {})", s.services.join(", ")),
            },
            s.values.join(", ")
        ));
        for t in &s.tradeoffs {
            o.push_str(&format!(
                "      {}: documented trade-off: {}\n",
                t.value, t.note
            ));
        }
        if let Some(d) = &s.doc {
            o.push_str(&format!("      documented in {d}\n"));
        }
    }
    let w = doc
        .combinations
        .iter()
        .map(|c| c.label.len())
        .max()
        .unwrap_or(0)
        .max("combination".len());
    o.push_str(&format!(
        "\n  {:<w$}  {:>9}  {:>6}  failures found (minimal schedule)\n",
        "combination", "schedules", "failed"
    ));
    for c in &doc.combinations {
        let first = if c.status != cs::EXPLORED {
            format!(
                "{}: {}",
                c.status,
                c.why
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect::<String>()
            )
        } else if c.classes.is_empty() {
            "none".to_string()
        } else {
            format!("{}: {}", c.classes[0].class, c.classes[0].schedule)
        };
        o.push_str(&format!(
            "  {:<w$}  {:>9}  {:>6}  {}\n",
            c.label, c.schedules, c.failed, first
        ));
        for k in c.classes.iter().skip(1) {
            o.push_str(&format!(
                "  {:<w$}  {:>9}  {:>6}  {}: {}\n",
                "", "", "", k.class, k.schedule
            ));
        }
    }
    if !doc.findings.is_empty() {
        o.push_str("\nEach failure across the combinations:\n");
    }
    for (n, f) in doc.findings.iter().enumerate() {
        o.push_str(&format!(
            "  {}. {}{}\n",
            n + 1,
            f.class,
            f.title
                .as_ref()
                .map(|t| format!(": {t}"))
                .unwrap_or_default()
        ));
        if f.only_with.is_empty() {
            o.push_str("     found with every value explored\n");
        } else {
            o.push_str(&format!(
                "     found only with {}; not with {}\n",
                values_words(&f.only_with),
                f.not_found_with.join(", ")
            ));
        }
        if f.by_design {
            o.push_str("     by design: the setting's documented trade-off, not a bug\n");
            for t in &f.tradeoffs {
                o.push_str(&format!("       {}: {}\n", t.value, t.note));
            }
        } else if !f.only_with.is_empty() {
            o.push_str(
                "     no tradeoff: note explains it: a bug of the system under these values, or a trade-off the manifest does not name\n",
            );
        }
        o.push_str(&format!("     minimal schedule: {}\n", f.schedule));
        o.push_str(&format!("     replay: {}\n", f.replay));
    }
    o.push_str(&format!("\n{}\n", doc.summary));
    o.push_str(&format!(
        "hydo report --html {dir} writes this as a page; each combination's exploration is in {dir}/<combination>/ (hydo report {dir}/<combination>)\n"
    ));
    o
}

// ------------------------------------------------------------------ the command

fn kv(s: &str) -> (String, String) {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => (k.to_string(), v.to_string()),
        _ => die(&format!("--set {s}: KEY=VALUE")),
    }
}

/// The replay command of a schedule on an image.
fn replay_line(image: &Path, schedule: &str) -> String {
    format!("hydo run --image {} \"{}\"", image.display(), schedule)
}

/// Explore `image` into `dir` with `args` in a child `hydo explore`.
fn explore_child(
    ctx: &Ctx,
    image: &Path,
    manifest: &Path,
    dir: &Path,
    args: &[String],
) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    if let Some(p) = dir.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let exe = std::env::current_exe().map_err(|e| format!("this program: {e}"))?;
    let mut c = Command::new(exe);
    c.arg("explore")
        .arg("--image")
        .arg(image)
        .arg("--out")
        .arg(dir)
        .arg("--json");
    if ctx.platform == Platform::Linux {
        c.arg("--backend").arg(ctx.backend_name());
    }
    if ctx.config.pool.scripted_engine.is_some() {
        // the tests: the stand-in image has no manifest inside; the engine reads the image
        c.arg("--manifest")
            .arg(manifest)
            .env("HYDO_SCRIPTED_IMAGE", image);
    }
    c.args(args);
    let mut log = dir.as_os_str().to_owned();
    log.push(".log");
    run_logged(c, Path::new(&log), "hydo explore")
}

pub fn cmd(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args: Vec<String> = argv.iter().filter(|a| *a != "--configs").cloned().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return 0;
    }
    let mode_flag = take(&mut args, "--configs-mode");
    let cache_flag = take(&mut args, "--cache");
    let out_flag = take(&mut args, "--out");
    let mut sets: Vec<(String, String)> = Vec::new();
    while let Some(s) = take(&mut args, "--set") {
        sets.push(kv(&s));
    }
    // --json: this command's own end is the table either way
    args.retain(|a| a != "--json");
    if args.iter().any(|a| a == "--image") {
        die("hydo explore --configs builds its own images from the manifest; leave out --image");
    }
    let manifest = match args.first().filter(|a| !a.starts_with('-')) {
        Some(m) if m.ends_with(".yaml") || m.ends_with(".yml") || m.ends_with(".json") => {
            let m = m.clone();
            args.remove(0);
            m
        }
        _ => "hydo/hydo.yaml".to_string(),
    };
    let manifest = abspath(Path::new(&manifest));
    if !manifest.is_file() {
        die(&format!(
            "hydo explore --configs: no manifest {} (give it as the first argument)",
            manifest.display()
        ));
    }
    if !args.iter().any(|a| a == "--seeds" || a == "--schedule") {
        args.push("--seeds".into());
        args.push("1-20".into());
    }
    let seeds = args
        .iter()
        .rposition(|a| a == "--seeds")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "schedule".into());

    // the settings
    let arch = match ctx.platform {
        Platform::Linux => "x86_64",
        Platform::Mac => "arm64",
    };
    let opts = hydo_manifest::LoadOptions {
        arch: arch.into(),
        sets: sets.clone(),
        onboard_dir: ctx.onboard(),
        ..Default::default()
    };
    let loaded = hydo_manifest::load(&manifest, &opts)
        .unwrap_or_else(|e| die(&format!("{}: {e}", manifest.display())));
    let settings = hydo_manifest::configs::read(&loaded.document)
        .unwrap_or_else(|e| die(&format!("{}: {e}", manifest.display())));
    if settings.is_empty() {
        die(&format!(
            "{}: no configs: to explore (MANIFEST.md \"Settings to explore\", hydo guide workflow)",
            manifest.display()
        ));
    }
    if let Some((k, _)) = sets
        .iter()
        .find(|(k, _)| settings.iter().any(|s| &s.param == k))
    {
        die(&format!(
            "--set {k}: {k} is a setting that configs: explores; leave it out of --set"
        ));
    }
    let sizes: Vec<usize> = settings.iter().map(|s| s.values.len()).collect();
    let mode = match &mode_flag {
        Some(m) => {
            Mode::parse(m).unwrap_or_else(|| die(&format!("--configs-mode {m}: all, one or pairs")))
        }
        None => default_mode(&sizes),
    };
    let combos = combinations(&sizes, mode);
    let out = abspath(
        &out_flag
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(crate::ctx::OUT).join("configs")),
    );
    let _ = std::fs::create_dir_all(&out);
    let cache = cache_flag
        .map(|c| abspath(Path::new(&c)))
        .unwrap_or_else(|| ctx.config.cli.cache.home.join("versions"));
    let maker = Maker::for_sets(ctx, cache, out.clone(), "hydo explore --configs");
    println!(
        "hydo explore --configs: {} combination{} of {} setting{} ({}{}), each explored with seeds {}",
        combos.len(),
        if combos.len() == 1 { "" } else { "s" },
        settings.len(),
        if settings.len() == 1 { "" } else { "s" },
        mode.name(),
        if mode_flag.is_none() && mode == Mode::Pairs {
            format!(", of {} in all", product(&sizes))
        } else {
            String::new()
        },
        seeds
    );

    let infos: Vec<ConfigSettingInfo> = settings
        .iter()
        .map(|s| ConfigSettingInfo {
            param: s.param.clone(),
            default: s.default.clone(),
            values: s.values.clone(),
            services: s.services.clone(),
            doc: s.doc.clone(),
            tradeoffs: s
                .tradeoffs
                .iter()
                .map(|(v, n)| ConfigTradeoff {
                    value: v.clone(),
                    note: n.clone(),
                })
                .collect(),
        })
        .collect();
    let mut results: Vec<ConfigCombination> = Vec::new();
    for (n, c) in combos.iter().enumerate() {
        let values: Vec<ConfigValue> = settings
            .iter()
            .zip(c)
            .map(|(s, &i)| ConfigValue {
                param: s.param.clone(),
                value: s.values[i].clone(),
            })
            .collect();
        let label = values
            .iter()
            .map(|v| format!("{}={}", v.param, v.value))
            .collect::<Vec<_>>()
            .join(" ");
        let mut r = ConfigCombination {
            label: label.clone(),
            values: values.clone(),
            defaults: c.iter().all(|&i| i == 0),
            ..Default::default()
        };
        let mut bsets = sets.clone();
        bsets.extend(values.iter().map(|v| (v.param.clone(), v.value.clone())));
        let t0 = std::time::Instant::now();
        let made = match maker.param_image(&manifest, &bsets, &label) {
            Ok(m) => m,
            Err(e) => {
                println!("hydo explore --configs: {label}: the image did not build: {e}");
                r.status = cs::CANNOT_BUILD.into();
                r.why = Some(e);
                results.push(r);
                continue;
            }
        };
        r.image = Some(made.path.display().to_string());
        r.image_sha256 = made.sha.clone();
        r.image_reused = Some(made.reused);
        let rel = format!("{:02}-{}", n + 1, slug(&label));
        let dir = out.join(&rel);
        r.dir = Some(rel.clone());
        println!(
            "hydo explore --configs: {}/{} {label}: hydo explore --image {} {}",
            n + 1,
            combos.len(),
            made.path.display(),
            args.join(" ")
        );
        let ran = explore_child(ctx, &made.path, &manifest, &dir, &args);
        let doc = std::fs::read_to_string(dir.join(hydo_results::EXPLORATION_DOC))
            .ok()
            .and_then(|t| match hydo_results::reader::document(&t).ok()? {
                hydo_results::reader::Record::Exploration(d) => Some(d),
                _ => None,
            });
        let Some(doc) = doc else {
            r.status = cs::NOT_EXPLORED.into();
            r.why = Some(match ran {
                Err(e) => e,
                Ok(()) => "the exploration wrote no exploration.json".into(),
            });
            println!(
                "hydo explore --configs: {label}: not explored: {}",
                r.why.clone().unwrap_or_default()
            );
            results.push(r);
            continue;
        };
        r.status = cs::EXPLORED.into();
        r.schedules = doc.summary.schedules;
        r.failed = doc.summary.failures;
        for f in findings_of_exploration(&doc, &dir) {
            let runs = doc
                .failures
                .iter()
                .filter(|x| {
                    key::of_verdict(&x.run.verdict.text, &x.run.findings).map(|k| k.2) == f.key
                })
                .count() as u64;
            r.classes.push(ConfigClass {
                class: f.class.clone().unwrap_or_else(|| "failure".into()),
                key: f.key.clone(),
                title: f.title.clone(),
                replay: replay_line(&made.path, &f.schedule),
                schedule: f.schedule,
                runs: (runs > 0).then_some(runs),
            });
        }
        println!(
            "hydo explore --configs: {label}: {} of {} schedules failed{} ({} s)",
            r.failed,
            r.schedules,
            if r.classes.is_empty() {
                String::new()
            } else {
                format!(
                    ": {}",
                    r.classes
                        .iter()
                        .map(|k| format!("{} ({})", k.class, k.schedule))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            t0.elapsed().as_secs()
        );
        results.push(r);
    }
    let mut doc = ConfigsDoc {
        hydo_version: Some(hydo_image::hydo_version().to_string()),
        epoch: Some(hydo_image::EPOCH),
        engine: hydo_results::Engine {
            backend: Some(ctx.backend_name().into()),
            ..Default::default()
        },
        manifest: manifest.display().to_string(),
        mode: mode.name().into(),
        seeds,
        explore_args: args.clone(),
        findings: findings(&infos, &results),
        settings: infos,
        combinations: results,
        ..Default::default()
    };
    doc.summary = summary(&doc);
    let file = out.join(hydo_results::CONFIGS_DOC);
    if let Err(e) = std::fs::write(&file, hydo_results::to_document(&doc)) {
        die(&format!("{}: {e}", file.display()));
    }
    print!("\n{}", text(&doc, &out.display().to_string()));
    println!("hydo explore --configs: wrote {}", file.display());
    let all = doc.combinations.iter().all(|c| c.status == cs::EXPLORED);
    i32::from(!all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_combination_in_order_with_the_defaults_first() {
        assert_eq!(
            combinations(&[3], Mode::All),
            vec![vec![0], vec![1], vec![2]]
        );
        assert_eq!(
            combinations(&[2, 2], Mode::All),
            vec![vec![0, 0], vec![0, 1], vec![1, 0], vec![1, 1]]
        );
        assert_eq!(product(&[2, 3, 4]), 24);
    }

    #[test]
    fn one_at_a_time_changes_one_setting() {
        assert_eq!(
            combinations(&[3, 2], Mode::One),
            vec![vec![0, 0], vec![1, 0], vec![2, 0], vec![0, 1]]
        );
    }

    #[test]
    fn pairs_cover_every_pair_of_two_settings() {
        for sizes in [vec![2, 2, 2], vec![3, 3, 3, 3], vec![2, 4, 3], vec![5, 2]] {
            let cs = pairwise(&sizes);
            assert_eq!(cs[0], vec![0; sizes.len()]);
            for i in 0..sizes.len() {
                for j in i + 1..sizes.len() {
                    for a in 0..sizes[i] {
                        for b in 0..sizes[j] {
                            assert!(
                                cs.iter().any(|c| c[i] == a && c[j] == b),
                                "{sizes:?}: pair ({i}={a}, {j}={b}) not covered by {cs:?}"
                            );
                        }
                    }
                }
            }
            assert!(
                cs.len() < product(&sizes) || sizes.len() == 2,
                "{sizes:?}: {cs:?}"
            );
        }
        // four settings of three values: far fewer than the 81 combinations
        assert!(pairwise(&[3, 3, 3, 3]).len() <= 15);
        assert_eq!(default_mode(&[3, 3, 3]), Mode::Pairs);
        assert_eq!(default_mode(&[3, 2]), Mode::All);
    }

    fn setting(p: &str, vals: &[&str], tradeoffs: &[(&str, &str)]) -> ConfigSettingInfo {
        ConfigSettingInfo {
            param: p.into(),
            default: vals[0].into(),
            values: vals.iter().map(|s| s.to_string()).collect(),
            tradeoffs: tradeoffs
                .iter()
                .map(|(v, n)| ConfigTradeoff {
                    value: v.to_string(),
                    note: n.to_string(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn combo(p: &str, v: &str, classes: &[(&str, &str)]) -> ConfigCombination {
        ConfigCombination {
            label: format!("{p}={v}"),
            values: vec![ConfigValue {
                param: p.into(),
                value: v.into(),
            }],
            status: cs::EXPLORED.into(),
            classes: classes
                .iter()
                .map(|(k, s)| ConfigClass {
                    class: "lost-acked-writes".into(),
                    key: Some(k.to_string()),
                    schedule: s.to_string(),
                    replay: format!("hydo run \"{s}\""),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_failure_only_with_tradeoff_values_is_by_design() {
        let s = vec![setting(
            "appendfsync",
            &["always", "everysec", "no"],
            &[("everysec", "may lose a second"), ("no", "may lose 30 s")],
        )];
        let cs = vec![
            combo("appendfsync", "always", &[]),
            combo(
                "appendfsync",
                "everysec",
                &[("f1:inv:a", "disk=cut | run C=3 W=1")],
            ),
            combo("appendfsync", "no", &[("f1:inv:a", "disk=cut | run C=2")]),
        ];
        let f = findings(&s, &cs);
        assert_eq!(f.len(), 1);
        assert!(f[0].by_design);
        assert_eq!(f[0].schedule, "disk=cut | run C=2");
        assert_eq!(f[0].not_found_with, vec!["appendfsync=always"]);
        assert_eq!(values_words(&f[0].only_with), "appendfsync=everysec, no");
        assert_eq!(f[0].tradeoffs.len(), 2);
    }

    #[test]
    fn a_failure_with_a_safe_value_is_not_by_design() {
        let s = vec![setting(
            "appendfsync",
            &["always", "everysec"],
            &[("everysec", "may lose a second")],
        )];
        let cs = vec![
            combo(
                "appendfsync",
                "always",
                &[("f1:inv:b", "disk=cut | run K=redis@3+1")],
            ),
            combo(
                "appendfsync",
                "everysec",
                &[("f1:inv:b", "disk=cut | run K=redis@3+1")],
            ),
        ];
        let f = findings(&s, &cs);
        assert!(!f[0].by_design);
        assert!(f[0].only_with.is_empty());
        // without a value free of trade-offs there is nothing to compare with
        let s2 = vec![setting(
            "conf",
            &["lazy", "lazier"],
            &[("lazy", "a"), ("lazier", "b")],
        )];
        let cs2 = vec![
            combo("conf", "lazy", &[("f1:inv:c", "disk=cut | run C=1")]),
            combo("conf", "lazier", &[]),
        ];
        let f2 = findings(&s2, &cs2);
        assert!(!f2[0].by_design);
        assert_eq!(values_words(&f2[0].only_with), "conf=lazy");
    }
}
