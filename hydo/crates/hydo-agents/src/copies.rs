//! Where the copies of the onboarding skill are, beyond the usual folders (Milestone 315, `hydo
//! update`):
//!
//! - The registry of copies, `~/.hydo/skill-copies.txt` (the install cache's folder,
//!   `HYDO_HOME`): one line `<version>\t<absolute path>` for each copy that `hydo skill install`,
//!   `hydo skill update`, `hydo onboard` or `hydo update` wrote. So a copy made with `--to DIR`,
//!   or in another project, is found again from anywhere. The `hydo` program names the file once
//!   at its start ([`set_registry`]); a program that names none (this crate's tests) records
//!   nothing, and a copy inside the temporary directory is never recorded. A failed write of the
//!   registry never fails the install.
//! - Claude Code's configuration folders: `CLAUDE_CONFIG_DIR`, `~/.claude`, and every other
//!   `~/.claude*` folder that holds `plugins/` or `settings.json` ([`claude_config_dirs`]). Each
//!   has its own `skills/` and its own plugin installs (`plugins/installed_plugins.json`), of
//!   which only the entries of the plugin `hydo@hydo` are read ([`plugin_installs`]). Nothing
//!   else in those folders is read or printed: they can hold another person's or an employer's
//!   settings.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The registry's file name in the install cache's folder.
pub const REGISTRY: &str = "skill-copies.txt";

/// The plugin's id in Claude Code: the plugin `hydo` of the marketplace `hydo`.
pub const PLUGIN_ID: &str = "hydo@hydo";

/// The marketplace's name in Claude Code.
pub const MARKETPLACE: &str = "hydo";

static REGISTRY_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Name the registry file (the `hydo` program does, from `HYDO_HOME`); None: record nothing.
pub fn set_registry(file: Option<PathBuf>) {
    if let Ok(mut f) = REGISTRY_FILE.lock() {
        *f = file;
    }
}

/// The registry file named by [`set_registry`].
pub fn registry_file() -> Option<PathBuf> {
    REGISTRY_FILE.lock().ok().and_then(|f| f.clone())
}

/// The registry's lines: (absolute path, version). Lines that are not `<version>\t<path>` with
/// an absolute path are left out.
pub fn read_registry(file: &Path) -> Vec<(PathBuf, String)> {
    let Ok(t) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let mut v: Vec<(PathBuf, String)> = Vec::new();
    for l in t.lines() {
        let Some((ver, p)) = l.split_once('\t') else {
            continue;
        };
        let p = PathBuf::from(p);
        if ver.is_empty() || !p.is_absolute() || v.iter().any(|(q, _)| *q == p) {
            continue;
        }
        v.push((p, ver.to_string()));
    }
    v
}

/// Write the registry (in one step: a temporary file next to it, then a rename).
pub fn write_registry(file: &Path, entries: &[(PathBuf, String)]) -> std::io::Result<()> {
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut t = String::new();
    for (p, v) in entries {
        t.push_str(&format!("{v}\t{}\n", p.display()));
    }
    let tmp = file.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, t)?;
    std::fs::rename(&tmp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The absolute form of `p` (symbolic links of its parent resolved, the copy's own name kept).
fn absolute(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    match (abs.parent(), abs.file_name()) {
        (Some(d), Some(n)) => d.canonicalize().map(|d| d.join(n)).unwrap_or(abs),
        _ => abs,
    }
}

/// Whether `p` (absolute) is inside the temporary directory, where nothing is kept.
fn temporary(p: &Path) -> bool {
    let t = std::env::temp_dir();
    let t = t.canonicalize().unwrap_or(t);
    p.starts_with(&t)
}

/// Record the copy at `dst` with `version` in the registry named by [`set_registry`] (nothing
/// when none is named). A copy inside the temporary directory is not recorded: it is not kept,
/// and a test's scratch copies would otherwise reach the user's registry. Errors are ignored:
/// the copy is installed either way.
pub fn record(dst: &Path, version: &str) {
    let Some(file) = registry_file() else {
        return;
    };
    let dst = absolute(dst);
    if temporary(&dst) {
        return;
    }
    let mut v = read_registry(&file);
    match v.iter_mut().find(|(p, _)| *p == dst) {
        Some(e) => e.1 = version.to_string(),
        None => v.push((dst, version.to_string())),
    }
    let _ = write_registry(&file, &v);
}

/// Whether `d` is a copy of hydo's skill (a plugin folder `hydo` or a skill folder, with
/// SKILL.md's `name: onboard` and `hydo-min:`), not a symbolic link.
pub fn is_copy(d: &Path) -> bool {
    !d.is_symlink() && d.is_dir() && crate::is_hydo_skill(d)
}

/// Claude Code's configuration folders of this user: `env` (`CLAUDE_CONFIG_DIR`) first, then
/// `<home>/.claude`, then every other `<home>/.claude*` folder that holds `plugins/` or
/// `settings.json`, by name. Each folder once (by its real path); only folders that exist.
pub fn claude_config_dirs(home: &Path, env: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !p.is_dir() {
            return;
        }
        let real = p.canonicalize().unwrap_or_else(|_| p.clone());
        if !seen.contains(&real) {
            seen.push(real);
            v.push(p);
        }
    };
    if let Some(e) = env {
        add(e.to_path_buf());
    }
    add(home.join(".claude"));
    let mut others: Vec<PathBuf> = std::fs::read_dir(home)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with(".claude") && n != ".claude"
                })
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir() && (p.join("plugins").is_dir() || p.join("settings.json").is_file())
                })
                .collect()
        })
        .unwrap_or_default();
    others.sort();
    for p in others {
        add(p);
    }
    v
}

/// One install of the plugin `hydo@hydo` in a Claude Code configuration folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInstall {
    /// `user`, `project`, `local` or `managed`.
    pub scope: String,
    /// The installed plugin's version.
    pub version: Option<String>,
    /// The project of a `project` or `local` install.
    pub project: Option<PathBuf>,
}

/// The installs of `hydo@hydo` that `<config>/plugins/installed_plugins.json` lists. Only that
/// plugin's entries are read; an unreadable file gives none.
pub fn plugin_installs(config: &Path) -> Vec<PluginInstall> {
    let Ok(b) = std::fs::read(config.join("plugins").join("installed_plugins.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) else {
        return Vec::new();
    };
    let entries = match &v["plugins"][PLUGIN_ID] {
        serde_json::Value::Array(a) => a.clone(),
        o @ serde_json::Value::Object(_) => vec![o.clone()],
        _ => Vec::new(),
    };
    entries
        .iter()
        .map(|e| PluginInstall {
            scope: e["scope"].as_str().unwrap_or("user").to_string(),
            version: e["version"].as_str().map(str::to_string),
            project: e["projectPath"].as_str().map(PathBuf::from),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-copies-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn config_folders_are_found_by_their_contents() {
        let h = scratch("cfg");
        std::fs::create_dir_all(h.join(".claude/skills")).unwrap();
        std::fs::create_dir_all(h.join(".claude-work/plugins")).unwrap();
        std::fs::create_dir_all(h.join(".claude-other")).unwrap();
        std::fs::write(h.join(".claude-other/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(h.join(".claude-empty")).unwrap();
        std::fs::write(h.join(".claude.json"), "{}").unwrap();
        std::fs::create_dir_all(h.join("elsewhere/cfg")).unwrap();
        let env = h.join("elsewhere/cfg");
        assert_eq!(
            claude_config_dirs(&h, Some(&env)),
            vec![
                env.clone(),
                h.join(".claude"),
                h.join(".claude-other"),
                h.join(".claude-work")
            ]
        );
        // the variable naming ~/.claude counts once
        assert_eq!(
            claude_config_dirs(&h, Some(&h.join(".claude"))).len(),
            3,
            "~/.claude once"
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn only_the_hydo_plugin_entries_are_read() {
        let h = scratch("plugins");
        std::fs::create_dir_all(h.join("plugins")).unwrap();
        std::fs::write(
            h.join("plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"other@x":[{"scope":"user","version":"9"}],
               "hydo@hydo":[{"scope":"project","version":"2.0.0","projectPath":"/p"},
                            {"scope":"user","version":"2.3.1"}]}}"#,
        )
        .unwrap();
        let v = plugin_installs(&h);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].scope, "project");
        assert_eq!(v[0].project.as_deref(), Some(Path::new("/p")));
        assert_eq!(v[1].version.as_deref(), Some("2.3.1"));
        assert!(plugin_installs(&h.join("none")).is_empty());
        let _ = std::fs::remove_dir_all(&h);
    }

    /// A copy inside the temporary directory is never recorded (the tests of the `hydo` program
    /// run it with the user's home: their scratch copies must not reach the user's registry).
    #[test]
    fn a_copy_in_the_temporary_directory_is_not_recorded() {
        let h = scratch("tmpcopy");
        let f = h.join(REGISTRY);
        set_registry(Some(f.clone()));
        record(&h.join("x/onboard"), "2.4.0");
        set_registry(None);
        assert!(!f.exists());
        assert!(temporary(&h));
        assert!(!temporary(Path::new("/nonexistent-root/onboard")));
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn the_registry_keeps_one_line_per_copy() {
        let h = scratch("reg");
        let f = h.join("home/.hydo").join(REGISTRY);
        write_registry(
            &f,
            &[
                (h.join("a/onboard"), "2.0.0".into()),
                (h.join("b/hydo"), "2.3.3".into()),
            ],
        )
        .unwrap();
        std::fs::write(
            &f,
            format!(
                "{}not a line\n2.0.0\trelative/path\n",
                std::fs::read_to_string(&f).unwrap()
            ),
        )
        .unwrap();
        let v = read_registry(&f);
        assert_eq!(
            v,
            vec![
                (h.join("a/onboard"), "2.0.0".to_string()),
                (h.join("b/hydo"), "2.3.3".to_string())
            ]
        );
        let _ = std::fs::remove_dir_all(&h);
    }
}
