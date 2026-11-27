//! `DUMP <path>` (Milestone 383, docs/design/inspect-stacks.md): the state of a stop for `hydo
//! inspect stacks`, written to a file. The file is the format `hydo_stacks::dump` reads:
//!
//! ```text
//! HYDODUMP 1\n
//! {"arch":"x86_64","engine":"hvx","vcpus":[{"rip":...,"cr3":...},...],"ram":[{"gpa":0,"len":...,"off":4096}]}\n
//! (zero bytes up to the first range's offset)
//! the guest's RAM, range after range, each at its offset; a page of zeros is a hole of the file
//! ```
//!
//! It only reads: the vCPUs' registers (`HVX_GET_SNAPSHOT`, which copies them) and the guest RAM
//! this process maps. The guest, its clock and the run's stops stay as they were, and nothing of
//! it reaches a fingerprint.

use crate::abi::hvx_snapshot;
use crate::boot::{RAM_HIGH_BASE, RAM_LOW_MAX};
use crate::vm::Vm;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::FileExt;

/// The page size of the holes: a page of zeros is not written.
const PAGE: usize = 4096;

/// One vCPU's registers as the dump's JSON names them.
fn vcpu_json(s: &hvx_snapshot) -> String {
    let r = &s.regs;
    let sr = &s.sregs;
    let pairs: [(&str, u64); 26] = [
        ("rax", r.rax),
        ("rbx", r.rbx),
        ("rcx", r.rcx),
        ("rdx", r.rdx),
        ("rsi", r.rsi),
        ("rdi", r.rdi),
        ("rbp", r.rbp),
        ("rsp", r.rsp),
        ("r8", r.r8),
        ("r9", r.r9),
        ("r10", r.r10),
        ("r11", r.r11),
        ("r12", r.r12),
        ("r13", r.r13),
        ("r14", r.r14),
        ("r15", r.r15),
        ("rip", r.rip),
        ("rflags", r.rflags),
        ("cr0", sr.cr0),
        ("cr3", sr.cr3),
        ("cr4", sr.cr4),
        ("efer", sr.efer),
        ("cs", sr.cs.selector as u64),
        ("fs_base", sr.fs.base),
        ("gs_base", sr.gs.base),
        ("icount", s.icount),
    ];
    let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

/// The guest-physical ranges of a VM with `size` bytes of RAM, and where each starts in
/// [`Vm::mem`]: the low block from 0, and above [`RAM_LOW_MAX`] the high block from
/// [`RAM_HIGH_BASE`] (Milestone 179).
pub fn ranges(size: u64) -> Vec<(u64, u64, u64)> {
    let low = size.min(RAM_LOW_MAX);
    let mut v = vec![(0, low, 0)];
    if size > low {
        v.push((RAM_HIGH_BASE, size - low, low));
    }
    v
}

/// Write the dump of `vm` (with `vcpus` vCPUs) to `path`; the file's length.
pub fn write(vm: &Vm, vcpus: u32, path: &str) -> Result<u64, String> {
    let mut cpus = Vec::new();
    let n = vcpus.max(1);
    for i in 0..n {
        if n > 1 {
            vm.set_vcpu(i)
                .map_err(|e| format!("HVX_SET_VCPU {i}: {e}"))?;
        }
        // SAFETY: hvx_snapshot is plain old data; the ioctl fills it
        let mut s: hvx_snapshot = unsafe { std::mem::zeroed() };
        let r = vm.get_snapshot(&mut s);
        if n > 1 {
            let _ = vm.set_vcpu(0);
        }
        r.map_err(|e| format!("HVX_GET_SNAPSHOT {i}: {e}"))?;
        cpus.push(vcpu_json(&s));
    }
    let mem = vm.mem();
    let rs = ranges(mem.len() as u64);
    // the header, then each range at a page-aligned offset
    let mut off = PAGE as u64;
    let mut ram = Vec::new();
    let mut place = Vec::new();
    for &(gpa, len, at) in &rs {
        ram.push(format!("{{\"gpa\":{gpa},\"len\":{len},\"off\":{off}}}"));
        place.push((at, len, off));
        off += len.div_ceil(PAGE as u64) * PAGE as u64;
    }
    let head = format!(
        "HYDODUMP 1\n{{\"arch\":\"x86_64\",\"engine\":\"hvx\",\"vcpus\":[{}],\"ram\":[{}]}}\n",
        cpus.join(","),
        ram.join(",")
    );
    if head.len() > PAGE {
        return Err(format!("{path}: a header of {} bytes", head.len()));
    }
    let mut f = File::create(path).map_err(|e| format!("{path}: {e}"))?;
    f.write_all(head.as_bytes())
        .map_err(|e| format!("{path}: {e}"))?;
    for (at, len, foff) in place {
        let src = &mem[at as usize..(at + len) as usize];
        for (i, page) in src.chunks(PAGE).enumerate() {
            if page.iter().all(|&b| b == 0) {
                continue;
            }
            f.write_all_at(page, foff + (i * PAGE) as u64)
                .map_err(|e| format!("{path}: {e}"))?;
        }
    }
    f.set_len(off).map_err(|e| format!("{path}: {e}"))?;
    Ok(off)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_follow_the_e820_map() {
        assert_eq!(ranges(1 << 30), vec![(0, 1 << 30, 0)]);
        assert_eq!(
            ranges(4 << 30),
            vec![
                (0, RAM_LOW_MAX, 0),
                (RAM_HIGH_BASE, (4 << 30) - RAM_LOW_MAX, RAM_LOW_MAX)
            ]
        );
    }
}
