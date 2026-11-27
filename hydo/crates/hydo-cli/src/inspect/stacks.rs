//! `hydo inspect stacks` (Milestone 383, docs/design/inspect-stacks.md): every guest thread's
//! stack at the session's stop. The engine writes the stop's state to a file (`DUMP`, which only
//! reads the guest), and `hydo_contracts::stacks` reads it with the guest kernel's `System.map`
//! and the image's own programs.

use super::Problem;
use hydo_results::InspectStacks;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where this process's engine runs, for the reading: the image, the guest kernel, and a folder
/// for the dump. `machine()` sets it.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub image: PathBuf,
    pub kernel: PathBuf,
    pub dir: PathBuf,
}

static ENV: Mutex<Option<Env>> = Mutex::new(None);

pub fn set_env(e: Env) {
    if let Ok(mut g) = ENV.lock() {
        *g = Some(e);
    }
}

fn env() -> Option<Env> {
    ENV.lock().ok().and_then(|g| g.clone())
}

/// The file the engine writes the stop's state to.
pub fn dump_path() -> PathBuf {
    env()
        .map(|e| e.dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("stacks.dump")
}

/// The `System.map` of a guest kernel: `--kernel-map`, or beside the kernel as
/// `<kernel>.System.map` or `System.map`.
pub fn kernel_map(kernel: &Path, given: Option<&str>) -> Result<String, Problem> {
    let mut tried = Vec::new();
    let mut cands: Vec<PathBuf> = given.map(PathBuf::from).into_iter().collect();
    if given.is_none() {
        let mut k = kernel.as_os_str().to_owned();
        k.push(".System.map");
        cands.push(PathBuf::from(k));
        if let Some(d) = kernel.parent() {
            cands.push(d.join("System.map"));
        }
    }
    for c in cands {
        match std::fs::read_to_string(&c) {
            Ok(s) => return Ok(s),
            Err(_) => tried.push(c.display().to_string()),
        }
    }
    Err(Problem::new(
        "refused",
        format!(
            "no System.map of the guest kernel {} (looked at {}); the stacks need the kernel's \
             symbols",
            kernel.display(),
            tried.join(", ")
        ),
        vec![format!(
            "hydo inspect stacks <session> --kernel-map <the System.map of {}>",
            kernel.display()
        )],
    ))
}

/// The image's root tree on the host: `<name>-root` beside `<name>-disk.img` (hydo build keeps
/// it).
pub fn root_of(image: &Path) -> Option<PathBuf> {
    let name = image.file_name()?.to_string_lossy().into_owned();
    let stem = name.strip_suffix("-disk.img")?;
    let r = image.with_file_name(format!("{stem}-root"));
    r.is_dir().then_some(r)
}

/// The host's copy of the guest program at `guest`: a `--program GUEST=HOST` pair, the image's
/// root tree at that path, or a file of that name in the tree (an image service's own root).
pub fn find_program(
    root: Option<&Path>,
    pairs: &[(String, String)],
    guest: &str,
) -> Option<String> {
    for (g, h) in pairs {
        let base = Path::new(guest)
            .file_name()
            .map(|b| b.to_string_lossy().into_owned());
        // the path itself, its last name, or its end: an image service's program is in the
        // service's own root (`/oci/<svc>/usr/bin/x` for `/usr/bin/x`)
        if g == guest
            || Some(g.as_str()) == base.as_deref()
            || (g.starts_with('/') && guest.ends_with(g.as_str()))
        {
            return Some(h.clone());
        }
    }
    let root = root?;
    let direct = root.join(guest.trim_start_matches('/'));
    if direct.is_file() {
        return Some(direct.display().to_string());
    }
    let base = Path::new(guest).file_name()?.to_os_string();
    let mut stack = vec![root.to_path_buf()];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            n += 1;
            if n > 200_000 {
                return None;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() && e.file_name() == base {
                return Some(e.path().display().to_string());
            }
        }
    }
    None
}

/// The options of a `stacks` call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    pub pid: Option<u64>,
    pub kernel_threads: bool,
    pub kernel_map: Option<String>,
    /// `GUEST=HOST` pairs.
    pub programs: Vec<(String, String)>,
    pub max_frames: u64,
}

/// Read the dump the engine wrote, then remove it.
pub fn read(o: &Opts) -> Result<InspectStacks, Problem> {
    let e = env().ok_or_else(|| Problem::new("engine", "no engine for the stacks", vec![]))?;
    let dump = dump_path();
    let map = kernel_map(&e.kernel, o.kernel_map.as_deref())?;
    let root = root_of(&e.image);
    let find = |g: &str| find_program(root.as_deref(), &o.programs, g);
    let inp = hydo_contracts::stacks::Inputs {
        kernel_map: &map,
        find_program: &find,
        pid: o.pid,
        kernel_threads: o.kernel_threads,
        max_frames: o.max_frames as usize,
    };
    let r = hydo_contracts::stacks::read(&dump, &inp);
    let _ = std::fs::remove_file(&dump);
    let mut s = r.map_err(|m| Problem::new("engine", m, vec![]))?;
    if root.is_none() && o.programs.is_empty() {
        s.notes.push(format!(
            "the image's root tree ({}) is not beside it, so the programs' frames are named only \
             with --program GUEST=HOST",
            e.image.display()
        ));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_found_by_pair_path_and_name() {
        let d = std::env::temp_dir().join(format!("hydo-stacks-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("usr/bin")).unwrap();
        std::fs::create_dir_all(d.join("oci/svc/app")).unwrap();
        std::fs::write(d.join("usr/bin/bank"), b"x").unwrap();
        std::fs::write(d.join("oci/svc/app/server"), b"x").unwrap();
        let pairs = vec![("/opt/x".to_string(), "/host/x".to_string())];
        assert_eq!(
            find_program(None, &pairs, "/oci/svc/opt/x").as_deref(),
            Some("/host/x")
        );
        assert_eq!(
            find_program(Some(&d), &pairs, "/opt/x").as_deref(),
            Some("/host/x")
        );
        assert!(find_program(Some(&d), &[], "/usr/bin/bank")
            .unwrap()
            .ends_with("usr/bin/bank"));
        assert!(find_program(Some(&d), &[], "/app/server")
            .unwrap()
            .ends_with("oci/svc/app/server"));
        assert_eq!(find_program(Some(&d), &[], "/nope"), None);
        assert_eq!(
            root_of(Path::new("/a/.hydo/explore-x-disk.img")),
            None,
            "no tree beside it"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_map_is_looked_for_beside_the_kernel() {
        let d = std::env::temp_dir().join(format!("hydo-stacks-map-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let k = d.join("bzImage");
        assert!(kernel_map(&k, None).is_err());
        std::fs::write(d.join("bzImage.System.map"), "ffffffff81000000 T _stext\n").unwrap();
        assert!(kernel_map(&k, None).unwrap().contains("_stext"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
