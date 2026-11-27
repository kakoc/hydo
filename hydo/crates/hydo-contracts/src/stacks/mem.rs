//! The state of a stop as an engine wrote it (`DUMP`, the file format of `hydo_hvx::dump`), and
//! guest memory read through page tables: x86_64's four levels and arm64's four levels with 4 KiB
//! pages.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

pub const PAGE: u64 = 4096;

/// Guest-physical memory.
pub trait Phys {
    /// Fill `buf` from guest-physical `gpa`; false when a byte is outside guest RAM.
    fn read(&self, gpa: u64, buf: &mut [u8]) -> bool;
}

/// One vCPU's registers by the dump's names (`rip`, `cr3`, `cs`, ...).
#[derive(Clone, Debug, Default)]
pub struct Vcpu {
    pub regs: HashMap<String, u64>,
}

impl Vcpu {
    pub fn get(&self, k: &str) -> u64 {
        self.regs.get(k).copied().unwrap_or(0)
    }
}

/// A dump file: its header and its RAM, read page by page.
pub struct Dump {
    pub arch: String,
    pub engine: String,
    pub vcpus: Vec<Vcpu>,
    /// (gpa, len, file offset)
    ranges: Vec<(u64, u64, u64)>,
    file: File,
    cache: RefCell<HashMap<u64, Box<[u8; PAGE as usize]>>>,
}

impl Dump {
    pub fn open(path: &Path) -> Result<Dump, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut head = vec![0u8; PAGE as usize];
        let n = file
            .read_at(&mut head, 0)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        head.truncate(n);
        Dump::from_header(&head, file)
    }

    fn from_header(head: &[u8], file: File) -> Result<Dump, String> {
        let text = String::from_utf8_lossy(head);
        let mut lines = text.lines();
        if lines.next() != Some("HYDODUMP 1") {
            return Err("not a dump of format 1 (the first line is not `HYDODUMP 1`)".into());
        }
        let v: Value = serde_json::from_str(lines.next().unwrap_or(""))
            .map_err(|e| format!("the dump's header: {e}"))?;
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let vcpus = v["vcpus"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| Vcpu {
                        regs: c
                            .as_object()
                            .map(|o| {
                                o.iter()
                                    .filter_map(|(k, x)| Some((k.clone(), x.as_u64()?)))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ranges = v["ram"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some((r["gpa"].as_u64()?, r["len"].as_u64()?, r["off"].as_u64()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Dump {
            arch: s("arch"),
            engine: s("engine"),
            vcpus,
            ranges,
            file,
            cache: RefCell::new(HashMap::new()),
        })
    }

    fn page(&self, pfn: u64) -> Option<Box<[u8; PAGE as usize]>> {
        if let Some(p) = self.cache.borrow().get(&pfn) {
            return Some(p.clone());
        }
        let gpa = pfn * PAGE;
        let &(base, _, off) = self
            .ranges
            .iter()
            .find(|(b, l, _)| *b <= gpa && gpa < b + l)?;
        let mut buf = Box::new([0u8; PAGE as usize]);
        // a hole of the file reads as zeros
        let _ = self.file.read_at(&mut buf[..], off + gpa - base);
        let mut c = self.cache.borrow_mut();
        if c.len() > 65536 {
            c.clear();
        }
        c.insert(pfn, buf.clone());
        Some(buf)
    }
}

impl Phys for Dump {
    fn read(&self, gpa: u64, buf: &mut [u8]) -> bool {
        let mut done = 0usize;
        while done < buf.len() {
            let a = gpa + done as u64;
            let Some(p) = self.page(a / PAGE) else {
                return false;
            };
            let o = (a % PAGE) as usize;
            let n = (PAGE as usize - o).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&p[o..o + n]);
            done += n;
        }
        true
    }
}

/// Guest RAM in a vector (the tests' memory): gpa 0 is the first byte.
impl Phys for Vec<u8> {
    fn read(&self, gpa: u64, buf: &mut [u8]) -> bool {
        let Some(s) = self.get(gpa as usize..gpa as usize + buf.len()) else {
            return false;
        };
        buf.copy_from_slice(s);
        true
    }
}

/// The machine's page tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Arm64,
}

/// Translate a virtual address through the page tables whose top table is at `root`.
pub fn translate(m: &dyn Phys, arch: Arch, root: u64, va: u64) -> Option<u64> {
    match arch {
        Arch::X86_64 => walk_x86(m, root, va),
        Arch::Arm64 => walk_arm64(m, root, va),
    }
}

fn rd64(m: &dyn Phys, gpa: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    m.read(gpa, &mut b).then(|| u64::from_le_bytes(b))
}

/// x86_64, four levels: present bit 0, a large page by bit 7 at the second and third level.
fn walk_x86(m: &dyn Phys, root: u64, va: u64) -> Option<u64> {
    const ADDR: u64 = 0x000f_ffff_ffff_f000;
    let mut table = root & ADDR;
    for level in (1..=4u32).rev() {
        let shift = 12 + 9 * (level - 1);
        let e = rd64(m, table + ((va >> shift) & 511) * 8)?;
        if e & 1 == 0 {
            return None;
        }
        if level == 1 || (level <= 3 && e & 0x80 != 0) {
            let mask = (1u64 << shift) - 1;
            return Some((e & ADDR & !mask) | (va & mask));
        }
        table = e & ADDR;
    }
    None
}

/// arm64 with 4 KiB pages and 48-bit addresses: four levels; a descriptor's low two bits are 3
/// for a table or a page and 1 for a block (the first and second level).
fn walk_arm64(m: &dyn Phys, root: u64, va: u64) -> Option<u64> {
    const ADDR: u64 = 0x0000_ffff_ffff_f000;
    let mut table = root & 0x0000_ffff_ffff_fffe & !0xfff;
    for level in 0..4u32 {
        let shift = 39 - 9 * level;
        let e = rd64(m, table + ((va >> shift) & 511) * 8)?;
        match (e & 3, level) {
            (3, 3) => return Some((e & ADDR) | (va & 0xfff)),
            (3, _) => table = e & ADDR,
            (1, 1) | (1, 2) => {
                let mask = (1u64 << shift) - 1;
                return Some((e & ADDR & !mask) | (va & mask));
            }
            _ => return None,
        }
    }
    None
}

/// A virtual address space: a translation from virtual addresses to guest-physical ones.
pub trait Space {
    fn phys(&self) -> &dyn Phys;
    fn gpa(&self, va: u64) -> Option<u64>;

    fn read(&self, va: u64, buf: &mut [u8]) -> bool {
        let mut done = 0usize;
        while done < buf.len() {
            let a = va.wrapping_add(done as u64);
            let Some(g) = self.gpa(a) else { return false };
            let n = ((PAGE - a % PAGE) as usize).min(buf.len() - done);
            if !self.phys().read(g, &mut buf[done..done + n]) {
                return false;
            }
            done += n;
        }
        true
    }
    fn u64(&self, va: u64) -> Option<u64> {
        let mut b = [0u8; 8];
        self.read(va, &mut b).then(|| u64::from_le_bytes(b))
    }
    fn u32(&self, va: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(va, &mut b).then(|| u32::from_le_bytes(b))
    }
    fn bytes(&self, va: u64, n: usize) -> Option<Vec<u8>> {
        let mut b = vec![0u8; n];
        self.read(va, &mut b).then_some(b)
    }
    /// A NUL-ended string of at most `max` bytes.
    fn cstr(&self, va: u64, max: usize) -> Option<String> {
        let mut out = Vec::new();
        let mut a = va;
        while out.len() < max {
            let n = ((PAGE - a % PAGE) as usize).min(max - out.len()).min(256);
            let b = self.bytes(a, n)?;
            if let Some(z) = b.iter().position(|&c| c == 0) {
                out.extend_from_slice(&b[..z]);
                return Some(String::from_utf8_lossy(&out).into_owned());
            }
            out.extend_from_slice(&b);
            a += n as u64;
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }
}

/// A process's address space: its page tables.
pub struct Paged<'m> {
    pub m: &'m dyn Phys,
    pub arch: Arch,
    pub root: u64,
}

impl Space for Paged<'_> {
    fn phys(&self) -> &dyn Phys {
        self.m
    }
    fn gpa(&self, va: u64) -> Option<u64> {
        translate(self.m, self.arch, self.root, va)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(m: &mut [u8], gpa: u64, v: u64) {
        m[gpa as usize..gpa as usize + 8].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn x86_64_four_levels_and_large_pages() {
        let mut m = vec![0u8; 0x40_0000];
        // tables at 0x1000 (PML4), 0x2000 (PDPT), 0x3000 (PD), 0x4000 (PT)
        let va = 0x0000_7f12_3456_7abcu64;
        put(&mut m, 0x1000 + ((va >> 39) & 511) * 8, 0x2000 | 1);
        put(&mut m, 0x2000 + ((va >> 30) & 511) * 8, 0x3000 | 1);
        put(&mut m, 0x3000 + ((va >> 21) & 511) * 8, 0x4000 | 1);
        put(
            &mut m,
            0x4000 + ((va >> 12) & 511) * 8,
            0x9000 | 1 | (1 << 63),
        );
        assert_eq!(translate(&m, Arch::X86_64, 0x1000, va), Some(0x9abc));
        // a 2 MiB page
        let vb = 0x0000_0000_4060_1234u64;
        put(&mut m, 0x1000 + ((vb >> 39) & 511) * 8, 0x5000 | 1);
        put(&mut m, 0x5000 + ((vb >> 30) & 511) * 8, 0x6000 | 1);
        put(&mut m, 0x6000 + ((vb >> 21) & 511) * 8, 0x20_0000 | 0x81);
        assert_eq!(translate(&m, Arch::X86_64, 0x1000, vb), Some(0x20_1234));
        // not present
        assert_eq!(translate(&m, Arch::X86_64, 0x1000, 0x1234), None);
        m[0x9abc] = 7;
        let p = Paged {
            m: &m,
            arch: Arch::X86_64,
            root: 0x1000,
        };
        assert_eq!(p.u32(va), Some(7));
    }

    #[test]
    fn arm64_tables_pages_and_blocks() {
        let mut m = vec![0u8; 0x40_0000];
        let va = 0x0000_ffff_8123_4560u64;
        put(&mut m, 0x1000 + ((va >> 39) & 511) * 8, 0x2000 | 3);
        put(&mut m, 0x2000 + ((va >> 30) & 511) * 8, 0x3000 | 3);
        put(&mut m, 0x3000 + ((va >> 21) & 511) * 8, 0x4000 | 3);
        put(
            &mut m,
            0x4000 + ((va >> 12) & 511) * 8,
            0x7000 | 3 | (1 << 10),
        );
        // the root's ASID bits are ignored
        assert_eq!(
            translate(&m, Arch::Arm64, 0x0005_0000_0000_1000, va),
            Some(0x7560)
        );
        // a 2 MiB block at the second level
        let vb = 0x0000_0000_4030_0010u64;
        put(&mut m, 0x1000 + ((vb >> 39) & 511) * 8, 0x5000 | 3);
        put(&mut m, 0x5000 + ((vb >> 30) & 511) * 8, 0x6000 | 3);
        put(&mut m, 0x6000 + ((vb >> 21) & 511) * 8, 0x20_0000 | 1);
        assert_eq!(translate(&m, Arch::Arm64, 0x1000, vb), Some(0x30_0010));
        // an invalid descriptor
        assert_eq!(translate(&m, Arch::Arm64, 0x1000, 0x10), None);
    }

    #[test]
    fn a_dump_reads_its_ranges_and_holes() {
        let dir = std::env::temp_dir().join(format!("hydo-dump-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("d");
        let head = "HYDODUMP 1\n{\"arch\":\"x86_64\",\"engine\":\"hvx\",\"vcpus\":[{\"rip\":5,\"cr3\":4096}],\"ram\":[{\"gpa\":0,\"len\":8192,\"off\":4096},{\"gpa\":4294967296,\"len\":4096,\"off\":12288}]}\n";
        let mut f = vec![0u8; 16384];
        f[..head.len()].copy_from_slice(head.as_bytes());
        f[4096 + 4100] = 0xaa;
        f[12288 + 1] = 0xbb;
        std::fs::write(&p, &f).unwrap();
        let d = Dump::open(&p).unwrap();
        assert_eq!(d.arch, "x86_64");
        assert_eq!(d.vcpus[0].get("cr3"), 4096);
        let mut b = [0u8; 2];
        assert!(d.read(4099, &mut b));
        assert_eq!(b, [0, 0xaa]);
        assert!(d.read(1 << 32, &mut b));
        assert_eq!(b, [0, 0xbb]);
        assert!(!d.read(8192, &mut b));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
