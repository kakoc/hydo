//! The shared-library closure of some ELF files, as absolute guest paths: port of
//! hv/hvm/services/elfdeps.py (deleted in M108; `hydo-elfdeps` of hydo-tools is this module's command line), which hydo_build_arm64.py ran for the aarch64 `ip` and iptables.
//! Reads PT_INTERP, DT_NEEDED and DT_RUNPATH/DT_RPATH from the file (no readelf, so it works for
//! foreign architectures), resolves each needed library through the RUNPATH of the object that
//! needs it (then the loader's directory, then the object's own), and recurses. Every resolved
//! file and every symlink on the way to it is returned once, sorted. With a root, every file
//! access is made under it (a tree holding nix/store/... on a machine without /nix).

use std::collections::HashSet;
use std::path::Path;

pub struct Elfdeps {
    root: String,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|x| {
        let mut a = [0u8; 8];
        a.copy_from_slice(x);
        u64::from_le_bytes(a)
    })
}

type Parsed = (Option<String>, Vec<String>, Vec<String>);

impl Elfdeps {
    /// `root`: "" for the host's own files, else the directory that holds nix/store/...
    pub fn new(root: &str) -> Elfdeps {
        Elfdeps {
            root: root.trim_end_matches('/').to_string(),
        }
    }

    fn h(&self, p: &str) -> String {
        format!("{}{}", self.root, p)
    }

    fn realpath(&self, p: &str) -> Result<String, String> {
        if self.root.is_empty() {
            return Ok(std::fs::canonicalize(p)
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_else(|_| hydo_manifest::py::abspath(p)));
        }
        let mut parts: std::collections::VecDeque<String> = p
            .split('/')
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect();
        let mut out = String::new();
        let mut hops = 0;
        while let Some(x) = parts.pop_front() {
            if x == "." {
                continue;
            }
            if x == ".." {
                out = if out.is_empty() {
                    String::new()
                } else {
                    hydo_manifest::py::dirname(&out)
                };
                continue;
            }
            let nxt = format!("{out}/{x}");
            if crate::fsx::is_link(Path::new(&self.h(&nxt))) {
                hops += 1;
                if hops > 40 {
                    return Err(format!("elfdeps: symlink loop at {p}"));
                }
                let t = std::fs::read_link(self.h(&nxt))
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .into_owned();
                for y in t.split('/').filter(|y| !y.is_empty()).rev() {
                    parts.push_front(y.to_string());
                }
                if t.starts_with('/') {
                    out.clear();
                }
                continue;
            }
            out = nxt;
        }
        Ok(if out.is_empty() { "/".into() } else { out })
    }

    fn parse(&self, path: &str) -> Result<Option<Parsed>, String> {
        let b = std::fs::read(self.h(path)).map_err(|e| format!("elfdeps: {path}: {e}"))?;
        if b.len() < 0x40 || &b[..4] != b"\x7fELF" || b[4] != 2 {
            return Ok(None);
        }
        let bad = || format!("elfdeps: {path}: a truncated ELF file");
        let phoff = u64_at(&b, 0x20).ok_or_else(bad)? as usize;
        let phentsize = u16_at(&b, 0x36).ok_or_else(bad)? as usize;
        let phnum = u16_at(&b, 0x38).ok_or_else(bad)? as usize;
        let mut loads = Vec::new();
        let mut interp = None;
        let mut dynm = None;
        for i in 0..phnum {
            let o = phoff + i * phentsize;
            let p_type = u32_at(&b, o).ok_or_else(bad)?;
            let p_off = u64_at(&b, o + 8).ok_or_else(bad)? as usize;
            let p_vaddr = u64_at(&b, o + 16).ok_or_else(bad)?;
            let p_filesz = u64_at(&b, o + 32).ok_or_else(bad)? as usize;
            match p_type {
                1 => loads.push((p_vaddr, p_off as u64, p_filesz as u64)),
                3 => {
                    let s = b.get(p_off..p_off + p_filesz).ok_or_else(bad)?;
                    let e = s.iter().rposition(|&c| c != 0).map(|x| x + 1).unwrap_or(0);
                    interp = Some(String::from_utf8_lossy(&s[..e]).into_owned());
                }
                2 => dynm = Some((p_off, p_filesz)),
                _ => {}
            }
        }
        let Some((doff, dsz)) = dynm else {
            return Ok(Some((interp, Vec::new(), Vec::new())));
        };
        let v2o = |va: u64| -> Result<usize, String> {
            for (v, o, n) in &loads {
                if *v <= va && va < v + n {
                    return Ok((va - v + o) as usize);
                }
            }
            Err(format!("elfdeps: {path}: vaddr {va:#x} not in a PT_LOAD"))
        };
        let mut ents = Vec::new();
        let mut strtab = None;
        for k in 0..dsz / 16 {
            let tag = u64_at(&b, doff + 16 * k).ok_or_else(bad)? as i64;
            let val = u64_at(&b, doff + 16 * k + 8).ok_or_else(bad)?;
            if tag == 0 {
                break;
            }
            if tag == 5 {
                strtab = Some(v2o(val)?);
            }
            ents.push((tag, val));
        }
        let s = |off: u64| -> Result<String, String> {
            let st = strtab.ok_or_else(bad)? + off as usize;
            let e = b[st..].iter().position(|&c| c == 0).ok_or_else(bad)?;
            Ok(String::from_utf8_lossy(&b[st..st + e]).into_owned())
        };
        let mut needed = Vec::new();
        for (t, v) in &ents {
            if *t == 1 {
                needed.push(s(*v)?);
            }
        }
        let origin = hydo_manifest::py::dirname(path);
        let mut rp = Vec::new();
        for (t, v) in &ents {
            if *t == 29 || *t == 15 {
                for d in s(*v)?.split(':').filter(|d| !d.is_empty()) {
                    rp.push(d.replace("$ORIGIN", &origin).replace("${ORIGIN}", &origin));
                }
            }
        }
        Ok(Some((interp, needed, rp)))
    }

    fn chain(&self, p: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut p = p.to_string();
        while seen.insert(p.clone()) {
            out.push(p.clone());
            let hp = self.h(&p);
            if !crate::fsx::is_link(Path::new(&hp)) {
                break;
            }
            let t = match std::fs::read_link(&hp) {
                Ok(t) => t.to_string_lossy().into_owned(),
                Err(_) => break,
            };
            let joined = if t.starts_with('/') {
                t
            } else {
                format!("{}/{}", hydo_manifest::py::dirname(&p), t)
            };
            p = hydo_manifest::py::normpath(&joined);
        }
        out
    }

    /// `main(files)`: the closure, sorted.
    pub fn closure(&self, files: &[String]) -> Result<Vec<String>, String> {
        let mut todo: Vec<String> = files.to_vec();
        let mut done = HashSet::new();
        let mut outs = std::collections::BTreeSet::new();
        let mut fallback: Vec<String> = Vec::new();
        while let Some(f) = todo.pop() {
            let real = self.realpath(&f)?;
            if !done.insert(real.clone()) {
                continue;
            }
            let Some((interp, needed, rp)) = self.parse(&real)? else {
                continue;
            };
            if let Some(i) = interp.filter(|i| !i.is_empty()) {
                outs.extend(self.chain(&i));
                todo.push(i.clone());
                let d = hydo_manifest::py::dirname(&i);
                if !fallback.contains(&d) {
                    fallback.push(d);
                }
            }
            for n in needed {
                let mut dirs = rp.clone();
                dirs.extend(fallback.iter().cloned());
                dirs.push(hydo_manifest::py::dirname(&real));
                let mut found = false;
                for d in dirs {
                    let c = if n.starts_with('/') {
                        n.clone()
                    } else {
                        format!("{}/{}", d.trim_end_matches('/'), n)
                    };
                    if Path::new(&self.h(&c)).exists() {
                        outs.extend(self.chain(&c));
                        todo.push(c);
                        found = true;
                        break;
                    }
                }
                if !found {
                    // the runpath as Python printed a list: ['a', 'b']
                    let rps: Vec<String> =
                        rp.iter().map(|d| hydo_manifest::py::repr_str(d)).collect();
                    return Err(format!(
                        "elfdeps: {f}: cannot resolve {n} (runpath [{}])",
                        rps.join(", ")
                    ));
                }
            }
        }
        Ok(outs.into_iter().collect())
    }
}
