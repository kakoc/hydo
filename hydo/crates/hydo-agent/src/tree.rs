//! A process and its descendants, as /proc shows them (what `svc_tree` in hydo-init.sh and
//! `collect` in hydo_fc.c compute).

use std::fs;

/// The most processes a tree holds (hydo_fc's limit).
const MAXP: usize = 512;

/// (state, parent pid) from a /proc/.../stat text.
fn stat_fields(text: &str) -> Option<(char, i32)> {
    let p = text.rfind(')')?;
    let rest = text.get(p + 2..)?;
    let mut it = rest.split(' ');
    let state = it.next()?.chars().next()?;
    let ppid = it.next()?.parse().ok()?;
    Some((state, ppid))
}

/// The process `root` and every descendant: root first, then each process the first time a pass
/// over /proc (in directory order) finds its parent in the set, the passes repeated until one
/// adds nothing (hydo_fc's `collect`).
pub fn collect(root: i32) -> Vec<i32> {
    let mut procs: Vec<(i32, i32)> = Vec::new();
    if let Ok(d) = fs::read_dir("/proc") {
        for e in d.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            let Ok(pid) = name.parse::<i32>() else {
                continue;
            };
            if let Ok(t) = fs::read_to_string(format!("/proc/{name}/stat")) {
                if let Some((_, pp)) = stat_fields(&t) {
                    procs.push((pid, pp));
                }
            }
            if procs.len() >= 4096 {
                break;
            }
        }
    }
    let mut tree = vec![root];
    loop {
        let mut changed = false;
        for &(pid, par) in &procs {
            if !tree.contains(&pid) && tree.contains(&par) && tree.len() < MAXP {
                tree.push(pid);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    tree
}

/// Whether every thread of every process is stopped (T or t), a zombie or gone.
pub fn all_stopped(tree: &[i32]) -> bool {
    for pid in tree {
        let Ok(d) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for e in d.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(t) = fs::read_to_string(format!("/proc/{pid}/task/{name}/stat")) {
                if let Some((st, _)) = stat_fields(&t) {
                    if !"TtZX".contains(st) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Waits until [`all_stopped`], polling every 200 us at most 10,000 times (hydo_fc's loop);
/// returns whether it got there.
pub fn wait_stopped(tree: &[i32]) -> bool {
    let mut w = 0;
    while !all_stopped(tree) && w < 10_000 {
        crate::sys::sleep_us(200);
        w += 1;
    }
    w < 10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_odd_names() {
        assert_eq!(stat_fields("12 (a) b) S 7 12 12 0"), Some(('S', 7)));
        assert_eq!(stat_fields("1 (init) S 0 1 1"), Some(('S', 0)));
        assert_eq!(stat_fields("garbage"), None);
    }
}
