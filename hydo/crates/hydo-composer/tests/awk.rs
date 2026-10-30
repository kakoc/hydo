//! hydo/onboard/tests/test_composer.py re-expressed in Rust: the host's composition code
//! (`plan`, `kept`, `entries_of`) and the awk programs inside the guest's composer
//! (`hydo-composer.sh`: PLAN_AWK for the seed form, RES_AWK for the resolved form) give the same
//! entries for 3,000 seed-form W values over 80 command sets, and for resolved compositions
//! replayed on a changed, shuffled command list; a removed command fails on both sides. Also the
//! plan rules (a singleton plan runs one singleton and nothing but anytime commands besides it)
//! and that shrink candidates never add entries.
//!
//! The awk is `awk` from PATH, or HYDO_AWK (for example "/path/to/busybox awk", the guest's own).

use hydo_composer as hc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn choice<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len())]
    }
}

fn awk_program(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard/hydo-composer.sh");
    let t = std::fs::read_to_string(p).unwrap();
    let start = t.find(&format!("\n{name}='")).unwrap() + name.len() + 3;
    let len = t[start..].find("'\n").unwrap();
    t[start..start + len].to_string()
}

fn awk() -> Vec<String> {
    std::env::var("HYDO_AWK")
        .unwrap_or_else(|_| "awk".into())
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn cmd_sets() -> Vec<Vec<(String, String)>> {
    let k = |kind: &str, n: usize| -> Vec<(String, String)> {
        (0..n)
            .map(|i| (kind.to_string(), format!("{kind}_{i}")))
            .collect()
    };
    let mut sets = Vec::new();
    for npar in [0, 1, 3] {
        for nser in [0, 1, 2] {
            for nsg in [0, 1, 2] {
                for nany in [0, 1, 2] {
                    if npar + nser + nsg + nany == 0 {
                        continue;
                    }
                    let mut cs = [
                        k("parallel_driver", npar),
                        k("serial_driver", nser),
                        k("singleton_driver", nsg),
                        k("anytime", nany),
                        k("first", 1),
                        k("eventually", 1),
                    ]
                    .concat();
                    cs.sort_by(|a, b| a.1.cmp(&b.1));
                    sets.push(cs);
                }
            }
        }
    }
    sets
}

fn meta(cmds: &[(String, String)], steps: i64) -> Value {
    json!({"params": {"parallel": 4, "per_round": 2, "round_ms": 200, "command_timeout": 60,
                      "drain_timeout": 60, "check_every": 2, "check_within": 30,
                      "max_commands": 0, "budget_ms": 0},
           "commands": cmds.iter().map(|(k, id)| json!({"kind": k, "id": id})).collect::<Vec<_>>(),
           "steps": steps})
}

fn write_list(p: &Path, cmds: &[(String, String)]) {
    let mut s = String::new();
    for (i, (k, id)) in cmds.iter().enumerate() {
        s.push_str(&format!("{i} {k} 60 {id} exec /bin/true\n"));
    }
    std::fs::write(p, s).unwrap();
}

fn lines(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-composer-awk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn awk_matches_rust() {
    let prog = awk_program("PLAN_AWK");
    let res_prog = awk_program("RES_AWK");
    let awk = awk();
    let td = scratch();
    let lst = td.join("compose.list");
    let sets = cmd_sets();
    assert_eq!(sets.len(), 80);
    let mut rnd = Rng(7);
    let n_values = std::env::var("HYDO_AWK_N")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(3000);
    let mut checked = 0;
    for n in 0..n_values {
        let cmds = &sets[n % sets.len()];
        write_list(&lst, cmds);
        let steps = rnd.choice(&[1i64, 4, 20, 57]);
        let m = meta(cmds, steps);
        let (ids, kinds) = hc::ids_kinds(&m);
        let seed = if n % 5 != 0 {
            (rnd.next() & 0xffff_ffff) as i128
        } else {
            n as i128
        };
        let mut d = hc::WSeed {
            seed,
            p: 1 + rnd.below(4) as i128,
            q: 1 + rnd.below(3) as i128,
            r: 100,
            n: rnd.choice(&[0i128, 0, 3, 10]),
            t: 0,
            x: Vec::new(),
            o: Vec::new(),
        };
        let full = hc::plan(d.seed, d.q, steps as i128, &kinds).unwrap();
        if !full.is_empty() && n % 3 == 0 {
            for _ in 0..(1 + rnd.below(5)).min(full.len()) {
                d.x.push(rnd.below(full.len()) as i128);
            }
        }
        if n % 4 == 0 {
            d.o = vec![hc::OItem::I(rnd.below(cmds.len()) as i128)];
        } else if n % 4 == 1 {
            for _ in 0..(1 + rnd.below(2)).min(cmds.len()) {
                d.o.push(hc::OItem::S(ids[rnd.below(ids.len())].clone()));
            }
        }
        let w = hc::render_w(&d);
        let d2 = hc::parse_seed(&w, &m).unwrap();
        assert_eq!(hc::render_w(&d2), w);
        let want: Vec<String> = hc::kept_of(&d2, steps as i128, &m)
            .unwrap()
            .iter()
            .map(|(i, r, c, s)| format!("{i} {r} {c} {} {s}", kinds[*c]))
            .collect();
        let f: Vec<&str> = w.split(',').collect();
        let field = |p: char| {
            f.iter()
                .find(|x| x.starts_with(p))
                .map(|x| x[1..].to_string())
                .unwrap_or_default()
        };
        let out = Command::new(&awk[0])
            .args(&awk[1..])
            .args([
                "-v",
                &format!("seed={}", i64::from_str_radix(f[0], 16).unwrap()),
                "-v",
                &format!("q={}", d.q),
                "-v",
                &format!("S={steps}"),
                "-v",
                &format!("X={}", field('x')),
                "-v",
                &format!("O={}", field('o')),
                "-v",
                &format!("N={}", d.n),
                &prog,
            ])
            .arg(&lst)
            .output()
            .expect("awk is needed (set HYDO_AWK)");
        assert!(out.status.success());
        assert_eq!(lines(&out.stdout), want, "W={w} steps={steps}");
        checked += 1;
    }
    // every kind's rule: a singleton plan has one singleton and nothing but anytime besides it
    for cmds in &sets {
        let kinds: Vec<String> = cmds.iter().map(|c| c.0.clone()).collect();
        for seed in 0..200 {
            let es = hc::plan(seed, 2, 20, &kinds).unwrap();
            let ks: Vec<&str> = es.iter().map(|e| kinds[e.2].as_str()).collect();
            if ks.contains(&"singleton_driver") {
                assert_eq!(ks.iter().filter(|k| **k == "singleton_driver").count(), 1);
                assert!(!ks
                    .iter()
                    .any(|k| *k == "parallel_driver" || *k == "serial_driver"));
            }
            assert!(ks.iter().all(|k| hc::PLAN_KINDS.contains(k)));
        }
    }
    // the shrinker's candidates are valid W values with no more entries than the original
    let m = meta(sets.last().unwrap(), 20);
    for seed in 0..50i128 {
        let w = hc::render_w(&hc::WSeed {
            seed: seed * 7919,
            p: 4,
            q: 2,
            r: 100,
            n: 0,
            t: 0,
            x: vec![],
            o: vec![],
        });
        let n0 = hc::entries_of(&w, &m, None, None).unwrap().len();
        for c in hc::candidates(&w, &m, None).unwrap() {
            assert!(hc::entries_of(&c, &m, None, None).unwrap().len() <= n0);
        }
    }
    // the resolved form (M98): RES_AWK gives the entries entries_of gives, whatever the order of
    // the image's list; an id the list lacks is an error on both sides
    let (mut nres, mut nerr) = (0, 0);
    for n in 0..(n_values / 5).max(200) {
        let cmds = &sets[n % sets.len()];
        let steps = rnd.choice(&[4i64, 20, 57]);
        let w = hc::render_w(&hc::WSeed {
            seed: (rnd.next() & 0xffff_ffff) as i128,
            p: 1 + rnd.below(4) as i128,
            q: 1 + rnd.below(3) as i128,
            r: 100,
            n: rnd.choice(&[0i128, 3]),
            t: rnd.choice(&[0i128, 0, 500]),
            x: vec![],
            o: vec![],
        });
        let m = meta(cmds, steps);
        let old = hc::entries_of(&w, &m, None, None).unwrap();
        if old.is_empty() {
            continue;
        }
        let res = hc::stable_w(&w, &m, None, None).unwrap();
        let mut now = cmds.clone();
        now.push(("parallel_driver".into(), format!("parallel_driver_new{n}")));
        now.push(("anytime".into(), "anytime_0new".into()));
        let mut drop = None;
        if n % 7 == 3 {
            let used: Vec<String> = hc::parse_resolved(&res)
                .unwrap()
                .entries
                .iter()
                .map(|e| e.2.clone())
                .collect();
            let d = used[rnd.below(used.len())].clone();
            now.retain(|c| c.1 != d);
            drop = Some(d);
        }
        now.sort_by(|a, b| a.1.cmp(&b.1));
        if n % 2 == 1 {
            for i in (1..now.len()).rev() {
                now.swap(i, rnd.below(i + 1));
            }
        }
        write_list(&lst, &now);
        let m2 = meta(&now, steps);
        let p = Command::new(&awk[0])
            .args(&awk[1..])
            .args([
                "-v",
                &format!("V={res}"),
                "-v",
                &format!("S={steps}"),
                &res_prog,
            ])
            .arg(&lst)
            .output()
            .unwrap();
        if let Some(d) = drop {
            assert!(!p.status.success() && String::from_utf8_lossy(&p.stdout).contains(&d));
            match hc::entries_of(&res, &m2, None, None) {
                Err(hc::WError::Invalid(e)) => assert!(e.contains(&d)),
                other => panic!("a missing command was accepted: {other:?}"),
            }
            nerr += 1;
            continue;
        }
        assert!(p.status.success(), "{res}");
        let got = hc::entries_of(&res, &m2, None, None).unwrap();
        let want: Vec<String> = got
            .iter()
            .map(|(e, r, c, s)| format!("{e} {r} {c} {} {s}", now[*c].0))
            .collect();
        assert_eq!(lines(&p.stdout), want, "{res}");
        // the same commands, rounds, entry numbers and seeds as the seed form on the old list
        let by_id = |es: &[hc::Entry], l: &[(String, String)]| -> Vec<(i128, i128, String, i128)> {
            es.iter()
                .map(|(e, r, c, s)| (*e, *r, l[*c].1.clone(), *s))
                .collect()
        };
        assert_eq!(by_id(&got, &now), by_id(&old, cmds));
        assert_eq!(hc::render_resolved(&hc::parse_resolved(&res).unwrap()), res);
        for c in hc::candidates(&res, &m2, None).unwrap() {
            assert!(hc::parse_resolved(&c).unwrap().entries.len() <= old.len());
        }
        nres += 1;
    }
    let _ = std::fs::remove_dir_all(&td);
    eprintln!(
        "awk: {checked} W values, awk ({}) == Rust; plan rules and shrink candidates OK; {nres} resolved \
         compositions replay by id on a changed list (awk == Rust), {nerr} with a removed command fail on both sides",
        awk.join(" ")
    );
}
