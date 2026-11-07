//! `services[].storage`: where a service keeps its data, for the storage faults of one service
//! (docs/design/storage-faults.md). One reading for the validator, hydo-build (which makes the
//! service's own file system a logical partition of the data disk), the host controller (which
//! mounts it and applies `G=` and `L=`) and the explorer (which draws them).

use serde_json::Value;

/// The smallest and largest own file system (`size_mib`).
pub const MIB_MIN: u64 = 16;
pub const MIB_MAX: u64 = 3072;

/// The first logical partition of the data disk: the own file systems are /dev/vda5, /dev/vda6,
/// ... in the order of the services that have one (an extended partition p3 holds them).
pub const FIRST_LOGICAL: u32 = 5;

/// One service's storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    /// The directory relative to /data, without a leading or trailing '/'.
    pub dir: String,
    /// The own file system's size, when the directory is one.
    pub size_mib: Option<u64>,
}

impl Storage {
    /// Reads and checks a `storage:` value.
    pub fn of(v: &Value) -> Result<Storage, String> {
        let (dir, size) = match v {
            Value::String(s) => (s.clone(), None),
            Value::Object(o) => {
                for k in o.keys() {
                    if !["dir", "size_mib", "description"].contains(&k.as_str()) {
                        return Err(format!("unknown key {k:?} (dir, size_mib, description)"));
                    }
                }
                let dir = match o.get("dir") {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("dir: required, a directory under /data".into()),
                };
                let size = match o.get("size_mib") {
                    None | Some(Value::Null) => None,
                    Some(n) => match n.as_u64() {
                        Some(n) if (MIB_MIN..=MIB_MAX).contains(&n) => Some(n),
                        _ => return Err(format!("size_mib: a number from {MIB_MIN} to {MIB_MAX}")),
                    },
                };
                (dir, size)
            }
            _ => {
                return Err("a directory under /data, or a mapping with dir: and size_mib:".into())
            }
        };
        let rel = dir
            .strip_prefix("/data/")
            .unwrap_or(&dir)
            .trim_end_matches('/')
            .to_string();
        let ok = !rel.is_empty()
            && !rel.starts_with('/')
            && rel.split('/').all(|c| {
                !c.is_empty()
                    && c != "."
                    && c != ".."
                    && c.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
            });
        if !ok {
            return Err(format!(
                "dir {dir:?}: a directory under /data (letters, digits, . _ - and /, no ..)"
            ));
        }
        Ok(Storage {
            dir: rel,
            size_mib: size,
        })
    }
}

/// Every service's storage in the manifest's order: (service, storage). A manifest that was
/// validated never fails here; an unreadable entry is left out.
pub fn of_manifest(m: &Value) -> Vec<(String, Storage)> {
    let mut out = Vec::new();
    for s in m
        .get("services")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(st)) = (
            s.get("name").and_then(Value::as_str),
            s.get("storage").filter(|v| !v.is_null()),
        ) else {
            continue;
        };
        if let Ok(st) = Storage::of(st) {
            out.push((name.to_string(), st));
        }
    }
    out
}

/// The services with their own file system, in the manifest's order, with the partition number
/// each gets on the data disk: (service, storage, partition).
pub fn volumes(m: &Value) -> Vec<(String, Storage, u32)> {
    of_manifest(m)
        .into_iter()
        .filter(|(_, s)| s.size_mib.is_some())
        .enumerate()
        .map(|(k, (n, s))| (n, s, FIRST_LOGICAL + k as u32))
        .collect()
}

/// Two services may not share a storage directory, and no own file system may sit inside
/// another service's storage directory.
pub fn check_distinct(m: &Value) -> Result<(), String> {
    let all = of_manifest(m);
    for (i, (a, sa)) in all.iter().enumerate() {
        for (b, sb) in &all[i + 1..] {
            let inside = |x: &str, y: &str| x == y || x.starts_with(&format!("{y}/"));
            if inside(&sa.dir, &sb.dir) || inside(&sb.dir, &sa.dir) {
                return Err(format!(
                    "services {a} and {b}: storage directories {:?} and {:?} overlap; each service's storage is its own",
                    sa.dir, sb.dir
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn readings() {
        assert_eq!(
            Storage::of(&json!("vol/n1-data")).unwrap(),
            Storage {
                dir: "vol/n1-data".into(),
                size_mib: None
            }
        );
        assert_eq!(
            Storage::of(&json!({"dir": "/data/tb/", "size_mib": 1300})).unwrap(),
            Storage {
                dir: "tb".into(),
                size_mib: Some(1300)
            }
        );
        for bad in [
            json!(""),
            json!("/etc"),
            json!("a/../b"),
            json!("a b"),
            json!({"dir": "x", "size_mib": 8}),
            json!({"dir": "x", "size_mib": 4000}),
            json!({"size_mib": 64}),
            json!({"dir": "x", "size": 64}),
            json!(3),
        ] {
            assert!(Storage::of(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn volumes_in_manifest_order() {
        let m = json!({"services": [
            {"name": "a", "storage": {"dir": "a", "size_mib": 64}},
            {"name": "b", "storage": "b"},
            {"name": "c", "storage": {"dir": "c", "size_mib": 32}},
            {"name": "d"},
        ]});
        let v: Vec<(String, u32)> = volumes(&m).into_iter().map(|(n, _, p)| (n, p)).collect();
        assert_eq!(v, vec![("a".into(), 5), ("c".into(), 6)]);
        assert!(check_distinct(&m).is_ok());
        let m = json!({"services": [
            {"name": "a", "storage": "x"},
            {"name": "b", "storage": "x/y"},
        ]});
        assert!(check_distinct(&m).is_err());
    }
}
