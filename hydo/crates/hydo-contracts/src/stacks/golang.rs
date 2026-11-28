//! The goroutines of a Go program, read from its runtime's own structures: `runtime.allgs`, each
//! `runtime.g`'s status, wait reason and saved registers (`sched`), and for a goroutine parked
//! on a semaphore (`sync.Mutex`, `sync.RWMutex`) the semaphore's address from its `sudog`. The
//! layouts come from the program's DWARF.

use super::mem::Space;
use super::program::Program;
use gimli::{AttributeValue, EndianSlice, LittleEndian};
use std::collections::HashMap;

/// The offsets hydo reads in the Go runtime's structures.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub goid: u64,
    pub status: u64,
    pub waitreason: u64,
    pub sched_sp: u64,
    pub sched_pc: u64,
    pub sched_bp: Option<u64>,
    pub sched_lr: Option<u64>,
    pub stack_lo: u64,
    pub stack_hi: u64,
    pub waiting: u64,
    pub syscallsp: Option<u64>,
    pub syscallpc: Option<u64>,
    pub sudog_elem: u64,
}

/// Every structure of these names with its members' offsets.
fn structs(p: &Program, names: &[&str]) -> HashMap<String, HashMap<String, u64>> {
    let sec = &p.dwarf;
    let mut out: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let Ok(sections) = gimli::DwarfSections::load(|id| -> Result<&[u8], ()> {
        Ok(sec.get(id.name()).map(Vec::as_slice).unwrap_or(&[]))
    }) else {
        return out;
    };
    let dwarf = sections.borrow(|s| EndianSlice::new(s, LittleEndian));
    let mut it = dwarf.units();
    while let Ok(Some(h)) = it.next() {
        let Ok(u) = dwarf.unit(h) else { continue };
        let mut cur = u.entries();
        let mut in_struct: Option<(String, isize)> = None;
        let mut depth = 0isize;
        while let Ok(Some((delta, e))) = cur.next_dfs() {
            depth += delta;
            if let Some((_, d)) = &in_struct {
                if depth <= *d {
                    in_struct = None;
                }
            }
            let name = || -> Option<String> {
                let v = e.attr_value(gimli::DW_AT_name).ok()??;
                Some(
                    dwarf
                        .attr_string(&u, v)
                        .ok()?
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            if e.tag() == gimli::DW_TAG_structure_type {
                if let Some(n) = name() {
                    if names.contains(&n.as_str()) && !out.contains_key(&n) {
                        out.insert(n.clone(), HashMap::new());
                        in_struct = Some((n, depth));
                    }
                }
            } else if e.tag() == gimli::DW_TAG_member && depth > 0 {
                if let (Some((sn, d)), Some(n)) = (&in_struct, name()) {
                    if depth == d + 1 {
                        let off = match e.attr_value(gimli::DW_AT_data_member_location) {
                            Ok(Some(AttributeValue::Udata(o))) => Some(o),
                            Ok(Some(v)) => v.udata_value(),
                            _ => None,
                        };
                        if let Some(o) = off {
                            out.entry(sn.clone()).or_default().insert(n, o);
                        }
                    }
                }
            }
        }
    }
    out
}

impl Layout {
    /// From the program's DWARF; None for a program without it (built with `-ldflags=-w`).
    pub fn read(p: &Program) -> Option<Layout> {
        let s = structs(
            p,
            &[
                "runtime.g",
                "runtime.gobuf",
                "runtime.stack",
                "runtime.sudog",
            ],
        );
        let g = s.get("runtime.g")?;
        let b = s.get("runtime.gobuf")?;
        let st = s.get("runtime.stack")?;
        let sd = s.get("runtime.sudog")?;
        let sched = *g.get("sched")?;
        let stack = *g.get("stack")?;
        Some(Layout {
            goid: *g.get("goid")?,
            status: *g.get("atomicstatus")?,
            waitreason: *g.get("waitreason")?,
            sched_sp: sched + b.get("sp")?,
            sched_pc: sched + b.get("pc")?,
            sched_bp: b.get("bp").map(|o| sched + o),
            sched_lr: b.get("lr").map(|o| sched + o),
            stack_lo: stack + st.get("lo")?,
            stack_hi: stack + st.get("hi")?,
            waiting: *g.get("waiting")?,
            syscallsp: g.get("syscallsp").copied(),
            syscallpc: g.get("syscallpc").copied(),
            sudog_elem: *sd.get("elem")?,
        })
    }
}

/// One goroutine as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct G {
    pub addr: u64,
    pub goid: u64,
    pub status: u32,
    pub wait: Option<String>,
    pub pc: u64,
    pub sp: u64,
    pub bp: u64,
    pub lr: u64,
    pub stack_hi: u64,
    /// The address a goroutine parked on a semaphore waits on (`sudog.elem`).
    pub sema: Option<u64>,
}

/// The status of a goroutine in words (`_Grunnable` and the rest; the scan bit ignored).
pub fn status_name(s: u32) -> &'static str {
    match s & !0x1000 {
        0 => "idle",
        1 => "runnable",
        2 => "running",
        3 => "syscall",
        4 => "waiting",
        6 => "dead",
        8 => "copystack",
        9 => "preempted",
        _ => "other",
    }
}

/// Every goroutine of the process whose memory is `s`; `bias` as for an unwind.
pub fn goroutines(s: &dyn Space, p: &Program, l: &Layout, bias: u64) -> Vec<G> {
    let mut out = Vec::new();
    let Some(allgs) = p.symbol("runtime.allgs").map(|x| x.addr + bias) else {
        return out;
    };
    let reasons = p.symbol("runtime.waitReasonStrings").map(|x| x.addr + bias);
    let (Some(ptr), Some(len)) = (s.u64(allgs), s.u64(allgs + 8)) else {
        return out;
    };
    for i in 0..len.min(100_000) {
        let Some(g) = s.u64(ptr + i * 8) else {
            continue;
        };
        let Some(status) = s.u32(g + l.status) else {
            continue;
        };
        if status & !0x1000 == 6 {
            continue; // dead: a free goroutine kept for reuse
        }
        let wr = s.bytes(g + l.waitreason, 1).map(|b| b[0]).unwrap_or(0);
        let wait = if status & !0x1000 == 4 {
            reasons.and_then(|r| {
                let sp = s.u64(r + wr as u64 * 16)?;
                let sl = s.u64(r + wr as u64 * 16 + 8)?;
                let b = s.bytes(sp, sl.min(64) as usize)?;
                Some(String::from_utf8_lossy(&b).into_owned())
            })
        } else {
            None
        };
        let sema = if status & !0x1000 == 4 {
            s.u64(g + l.waiting)
                .filter(|&w| w != 0)
                .and_then(|w| s.u64(w + l.sudog_elem))
                .filter(|&e| e != 0)
        } else {
            None
        };
        // a goroutine in a system call: its user position is the one it entered the call at
        let (pc, sp) = match (status & !0x1000, l.syscallpc, l.syscallsp) {
            (3, Some(a), Some(b)) => (s.u64(g + a).unwrap_or(0), s.u64(g + b).unwrap_or(0)),
            _ => (
                s.u64(g + l.sched_pc).unwrap_or(0),
                s.u64(g + l.sched_sp).unwrap_or(0),
            ),
        };
        out.push(G {
            addr: g,
            goid: s.u64(g + l.goid).unwrap_or(0),
            status,
            wait,
            pc,
            sp,
            bp: l.sched_bp.and_then(|o| s.u64(g + o)).unwrap_or(0),
            lr: l.sched_lr.and_then(|o| s.u64(g + o)).unwrap_or(0),
            stack_hi: s.u64(g + l.stack_hi).unwrap_or(0),
            sema,
        });
    }
    out
}

/// Whether a wait reason is a mutex's (`sync.Mutex.Lock`, `sync.RWMutex.Lock`, `sync.RWMutex.RLock`;
/// before Go 1.20 every semaphore wait read `semacquire`).
pub fn is_lock_wait(reason: &str) -> bool {
    reason.contains("Mutex") || reason == "semacquire"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert_eq!(status_name(4), "waiting");
        assert_eq!(status_name(0x1004), "waiting");
        assert_eq!(status_name(3), "syscall");
        assert!(is_lock_wait("sync.Mutex.Lock"));
        assert!(is_lock_wait("sync.RWMutex.RLock"));
        assert!(!is_lock_wait("chan receive"));
    }
}
