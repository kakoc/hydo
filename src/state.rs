//! Full VM state capture: registers, segment/control state, FPU, guest RAM, and
//! device state, plus a canonical fingerprint over all of it.
//!
//! Two encodings serve two needs:
//!   * `fingerprint()` feeds only *meaningful* fields into BLAKE3, skipping
//!     structure padding whose bytes the kernel may leave unspecified. Two runs
//!     that reach the same architectural state therefore hash identically.
//!   * `save()`/`load()` use raw struct bytes so a restore reproduces the exact
//!     KVM structs, padding and all — fidelity matters there, determinism doesn't.

use std::io::{self, Read, Write};
use std::mem::size_of;
use std::path::Path;

use kvm_bindings::{kvm_dtable, kvm_fpu, kvm_regs, kvm_segment, kvm_sregs, kvm_vcpu_events};

/// Device-side state that lives in the VMM, not in KVM.
#[derive(Clone)]
pub struct DeviceState {
    pub serial_out: Vec<u8>,
    pub prng_state: u64,
    pub clock_ticks: u64,
    /// Retired guest instructions (single-step DEBUG exits). The instruction clock.
    pub insn_count: u64,
    pub checkpoints: Vec<u32>,
    pub exit: Option<u32>,
    // Deterministic timer state.
    pub timer_period: u64,
    pub timer_next: u64,
    pub timer_pending: Option<u8>,
    pub timer_count: u64,
}

/// A complete, restorable snapshot of the machine.
pub struct Snapshot {
    pub regs: kvm_regs,
    pub sregs: kvm_sregs,
    pub fpu: kvm_fpu,
    /// Pending exceptions/interrupts/NMI — kernel-side state that a mid-flight
    /// interrupt would otherwise leave uncaptured, breaking branch/restore.
    pub events: kvm_vcpu_events,
    pub mem: Vec<u8>,
    pub device: DeviceState,
}

impl Snapshot {
    /// Canonical 256-bit fingerprint of the entire machine state.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"hydo-state-v1");
        encode_regs(&mut h, &self.regs);
        encode_sregs(&mut h, &self.sregs);
        encode_fpu(&mut h, &self.fpu);
        encode_events(&mut h, &self.events);
        // Guest RAM.
        h.update(&(self.mem.len() as u64).to_le_bytes());
        h.update(&self.mem);
        // Device state.
        let d = &self.device;
        h.update(&(d.serial_out.len() as u64).to_le_bytes());
        h.update(&d.serial_out);
        h.update(&d.prng_state.to_le_bytes());
        h.update(&d.clock_ticks.to_le_bytes());
        h.update(&d.insn_count.to_le_bytes());
        h.update(&(d.checkpoints.len() as u64).to_le_bytes());
        for c in &d.checkpoints {
            h.update(&c.to_le_bytes());
        }
        match d.exit {
            None => h.update(&[0u8]),
            Some(code) => {
                h.update(&[1u8]);
                h.update(&code.to_le_bytes())
            }
        };
        h.update(&d.timer_period.to_le_bytes());
        h.update(&d.timer_next.to_le_bytes());
        h.update(&d.timer_count.to_le_bytes());
        match d.timer_pending {
            None => h.update(&[0u8]),
            Some(vec) => {
                h.update(&[1u8]);
                h.update(&[vec])
            }
        };
        *h.finalize().as_bytes()
    }

    pub fn fingerprint_hex(&self) -> String {
        hex(&self.fingerprint())
    }

    /// Serialize to disk in a simple length-prefixed binary format.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        f.write_all(b"HYDOSNAP")?;
        f.write_all(&1u32.to_le_bytes())?; // format version
        write_blob(&mut f, struct_bytes(&self.regs))?;
        write_blob(&mut f, struct_bytes(&self.sregs))?;
        write_blob(&mut f, struct_bytes(&self.fpu))?;
        write_blob(&mut f, struct_bytes(&self.events))?;
        write_blob(&mut f, &self.mem)?;
        let d = &self.device;
        write_blob(&mut f, &d.serial_out)?;
        f.write_all(&d.prng_state.to_le_bytes())?;
        f.write_all(&d.clock_ticks.to_le_bytes())?;
        f.write_all(&d.insn_count.to_le_bytes())?;
        f.write_all(&(d.checkpoints.len() as u64).to_le_bytes())?;
        for c in &d.checkpoints {
            f.write_all(&c.to_le_bytes())?;
        }
        match d.exit {
            None => f.write_all(&[0u8])?,
            Some(code) => {
                f.write_all(&[1u8])?;
                f.write_all(&code.to_le_bytes())?;
            }
        }
        f.write_all(&d.timer_period.to_le_bytes())?;
        f.write_all(&d.timer_next.to_le_bytes())?;
        f.write_all(&d.timer_count.to_le_bytes())?;
        match d.timer_pending {
            None => f.write_all(&[0u8])?,
            Some(vec) => {
                f.write_all(&[1u8])?;
                f.write_all(&[vec])?;
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> io::Result<Snapshot> {
        let mut f = std::fs::File::open(path)?;
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)?;
        if &magic != b"HYDOSNAP" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad snapshot magic"));
        }
        let mut ver = [0u8; 4];
        f.read_exact(&mut ver)?;
        let regs = read_struct::<kvm_regs>(&read_blob(&mut f)?)?;
        let sregs = read_struct::<kvm_sregs>(&read_blob(&mut f)?)?;
        let fpu = read_struct::<kvm_fpu>(&read_blob(&mut f)?)?;
        let events = read_struct::<kvm_vcpu_events>(&read_blob(&mut f)?)?;
        let mem = read_blob(&mut f)?;
        let serial_out = read_blob(&mut f)?;
        let prng_state = read_u64(&mut f)?;
        let clock_ticks = read_u64(&mut f)?;
        let insn_count = read_u64(&mut f)?;
        let n = read_u64(&mut f)? as usize;
        let mut checkpoints = Vec::with_capacity(n);
        for _ in 0..n {
            let mut b = [0u8; 4];
            f.read_exact(&mut b)?;
            checkpoints.push(u32::from_le_bytes(b));
        }
        let mut tag = [0u8; 1];
        f.read_exact(&mut tag)?;
        let exit = if tag[0] == 0 {
            None
        } else {
            let mut b = [0u8; 4];
            f.read_exact(&mut b)?;
            Some(u32::from_le_bytes(b))
        };
        let timer_period = read_u64(&mut f)?;
        let timer_next = read_u64(&mut f)?;
        let timer_count = read_u64(&mut f)?;
        let mut ptag = [0u8; 1];
        f.read_exact(&mut ptag)?;
        let timer_pending = if ptag[0] == 0 {
            None
        } else {
            let mut b = [0u8; 1];
            f.read_exact(&mut b)?;
            Some(b[0])
        };
        Ok(Snapshot {
            regs,
            sregs,
            fpu,
            events,
            mem,
            device: DeviceState {
                serial_out,
                prng_state,
                clock_ticks,
                insn_count,
                checkpoints,
                exit,
                timer_period,
                timer_next,
                timer_pending,
                timer_count,
            },
        })
    }
}

// --- canonical field encoders (skip padding) ---

fn encode_regs(h: &mut blake3::Hasher, r: &kvm_regs) {
    for v in [
        r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11, r.r12,
        r.r13, r.r14, r.r15, r.rip, r.rflags,
    ] {
        h.update(&v.to_le_bytes());
    }
}

fn encode_segment(h: &mut blake3::Hasher, s: &kvm_segment) {
    h.update(&s.base.to_le_bytes());
    h.update(&s.limit.to_le_bytes());
    h.update(&s.selector.to_le_bytes());
    h.update(&[
        s.type_, s.present, s.dpl, s.db, s.s, s.l, s.g, s.avl, s.unusable,
    ]);
}

fn encode_dtable(h: &mut blake3::Hasher, d: &kvm_dtable) {
    h.update(&d.base.to_le_bytes());
    h.update(&d.limit.to_le_bytes());
}

fn encode_sregs(h: &mut blake3::Hasher, s: &kvm_sregs) {
    for seg in [
        &s.cs, &s.ds, &s.es, &s.fs, &s.gs, &s.ss, &s.tr, &s.ldt,
    ] {
        encode_segment(h, seg);
    }
    encode_dtable(h, &s.gdt);
    encode_dtable(h, &s.idt);
    for v in [s.cr0, s.cr2, s.cr3, s.cr4, s.cr8, s.efer, s.apic_base] {
        h.update(&v.to_le_bytes());
    }
    for v in s.interrupt_bitmap {
        h.update(&v.to_le_bytes());
    }
}

fn encode_events(h: &mut blake3::Hasher, e: &kvm_vcpu_events) {
    // exception
    h.update(&[
        e.exception.injected,
        e.exception.nr,
        e.exception.has_error_code,
        e.exception.pending,
    ]);
    h.update(&e.exception.error_code.to_le_bytes());
    // interrupt
    h.update(&[
        e.interrupt.injected,
        e.interrupt.nr,
        e.interrupt.soft,
        e.interrupt.shadow,
    ]);
    // nmi
    h.update(&[e.nmi.injected, e.nmi.pending, e.nmi.masked]);
    h.update(&e.sipi_vector.to_le_bytes());
    h.update(&e.flags.to_le_bytes());
}

fn encode_fpu(h: &mut blake3::Hasher, f: &kvm_fpu) {
    for reg in &f.fpr {
        h.update(reg);
    }
    h.update(&f.fcw.to_le_bytes());
    h.update(&f.fsw.to_le_bytes());
    h.update(&[f.ftwx]);
    h.update(&f.last_opcode.to_le_bytes());
    h.update(&f.last_ip.to_le_bytes());
    h.update(&f.last_dp.to_le_bytes());
    for reg in &f.xmm {
        h.update(reg);
    }
    h.update(&f.mxcsr.to_le_bytes());
}

// --- raw struct <-> bytes (POD kvm structs) ---

fn struct_bytes<T>(v: &T) -> &[u8] {
    // SAFETY: kvm_regs/sregs/fpu are repr(C) plain-old-data.
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, size_of::<T>()) }
}

fn read_struct<T: Copy>(bytes: &[u8]) -> io::Result<T> {
    if bytes.len() != size_of::<T>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot struct size mismatch",
        ));
    }
    // SAFETY: length checked; T is POD; source is a byte buffer we own.
    Ok(unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

// --- disk helpers ---

fn write_blob(f: &mut impl Write, data: &[u8]) -> io::Result<()> {
    f.write_all(&(data.len() as u64).to_le_bytes())?;
    f.write_all(data)
}

fn read_blob(f: &mut impl Read) -> io::Result<Vec<u8>> {
    let len = read_u64(f)? as usize;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_u64(f: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    f.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Snapshot {
        let mut regs = kvm_regs::default();
        regs.rip = 0x1_0000;
        regs.rax = 0xdead_beef;
        let mut events = kvm_vcpu_events::default();
        events.interrupt.injected = 1;
        events.interrupt.nr = 0x20;
        Snapshot {
            regs,
            sregs: kvm_sregs::default(),
            fpu: kvm_fpu::default(),
            events,
            mem: vec![0u8; 4096],
            device: DeviceState {
                serial_out: b"hello".to_vec(),
                prng_state: 0x1234,
                clock_ticks: 42,
                insn_count: 137,
                checkpoints: vec![1, 2, 3],
                exit: Some(0),
                timer_period: 30,
                timer_next: 60,
                timer_pending: Some(0x20),
                timer_count: 7,
            },
        }
    }

    #[test]
    fn save_load_preserves_fingerprint() {
        let snap = sample();
        let fp = snap.fingerprint();

        let dir = std::env::temp_dir();
        let path = dir.join(format!("hydo-test-{}.snap", std::process::id()));
        snap.save(&path).unwrap();
        let loaded = Snapshot::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded.fingerprint(), fp, "fingerprint must survive disk round-trip");
    }

    #[test]
    fn fingerprint_is_sensitive_to_state() {
        let a = sample();
        let mut b = sample();
        b.device.clock_ticks += 1;
        assert_ne!(a.fingerprint(), b.fingerprint());

        let mut c = sample();
        c.mem[0] = 1;
        assert_ne!(a.fingerprint(), c.fingerprint());

        let mut d = sample();
        d.regs.rip += 1;
        assert_ne!(a.fingerprint(), d.fingerprint());
    }
}
