//! The engine's fingerprints: 64-bit FNV-1a over the console, the snapshot struct's canonical
//! fields, all guest RAM and the data partition (`fingerprint()` in hvx_stack_verify.c), with an
//! exact shortcut for all-zero pages: hashing a zero byte is `h = (h ^ 0) * P = h * P`, so a
//! 4 KiB zero page is one multiplication by `P^4096` (mod 2^64). The value is the C engine's.

use crate::abi::{bytes_of, hvx_sched, hvx_snapshot};

pub const BASIS: u64 = 0xcbf29ce484222325;
pub const PRIME: u64 = 0x100000001b3;
const PAGE: usize = 4096;

/// FNV-1a over `p`, continuing from `h`.
#[inline]
pub fn fnv(mut h: u64, p: &[u8]) -> u64 {
    // eight bytes per step: the same arithmetic, fewer bounds checks
    let mut chunks = p.chunks_exact(8);
    for c in &mut chunks {
        for &b in c {
            h ^= b as u64;
            h = h.wrapping_mul(PRIME);
        }
    }
    for &b in chunks.remainder() {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// P^4096 mod 2^64.
pub const fn prime_pow_page() -> u64 {
    let mut r: u64 = 1;
    let mut i = 0;
    while i < PAGE {
        r = r.wrapping_mul(PRIME);
        i += 1;
    }
    r
}
const P4096: u64 = prime_pow_page();

/// True when the 4 KiB (or shorter) block is all zero.
#[inline]
pub fn is_zero(p: &[u8]) -> bool {
    let (pre, mid, post) = unsafe { p.align_to::<u64>() };
    pre.iter().all(|&b| b == 0) && mid.iter().all(|&w| w == 0) && post.iter().all(|&b| b == 0)
}

/// FNV-1a over `p` that skips all-zero 4 KiB pages exactly (the same value as [`fnv`]).
pub fn fnv_zskip(mut h: u64, p: &[u8]) -> u64 {
    let mut off = 0;
    while off + PAGE <= p.len() {
        let pg = &p[off..off + PAGE];
        if is_zero(pg) {
            h = h.wrapping_mul(P4096);
        } else {
            h = fnv(h, pg);
        }
        off += PAGE;
    }
    fnv(h, &p[off..])
}

/// `fnv_snapshot`: the canonical fields of one vCPU's snapshot.
pub fn fnv_snapshot(mut h: u64, s: &hvx_snapshot) -> u64 {
    macro_rules! f {
        ($($x:expr),*) => { $( h = fnv(h, bytes_of(&$x)); )* };
    }
    f!(
        s.regs,
        s.tsc,
        s.tsc_step,
        s.tsc_aux,
        s.tsc_mode,
        s.icount,
        s.rdtsc_count,
        s.inject_at_icount,
        s.inject_vector,
        s.inject_valid,
        s.sw_msr,
        s.sregs,
        s.lapic_apic_base,
        s.lapic_tsc_deadline,
        s.lapic_reg,
        s.lapic_in_service,
        s.ioapic_ioregsel,
        s.ioapic_id,
        s.ioapic_redir,
        s.pic_mask,
        s.uart_ier,
        s.uart_lcr,
        s.uart_mcr,
        s.uart_fcr,
        s.uart_scr,
        s.uart_dll,
        s.uart_dlm,
        s.vblk_desc,
        s.vblk_avail,
        s.vblk_used,
        s.vblk_qsize,
        s.vblk_qready,
        s.vblk_last_avail,
        s.vblk_isr,
        s.vblk_irq_pending,
        s.vblk_completion_armed,
        s.vblk_completion_due,
        s.vblk_status,
        s.vblk_pci_command,
        s.vblk_pci_bar0,
        s.vblk_msix_control,
        s.vblk_config_msix,
        s.vblk_queue_msix,
        s.vblk_msix_tbl,
        s.fault_seed,
        s.fault_classes,
        s.fault_probability,
        s.fault_req_seq,
        s.fault_count,
        s.irr
    );
    if s.pmu_next != 0 {
        f!(s.pmu_total, s.pmu_next);
    }
    if s.xcr0 != 0 {
        f!(s.xcr0, s.fx, s.xsave_ext);
    }
    h
}

/// The scheduler-state part of `fnv_smp` (after the other vCPUs' snapshots).
pub fn fnv_sched_state(mut h: u64, c: &hvx_sched, vcpus: usize) -> u64 {
    macro_rules! f {
        ($($x:expr),*) => { $( h = fnv(h, bytes_of(&$x)); )* };
    }
    f!(
        c.cur,
        c.in_turn,
        c.turn_over,
        c.pause_run,
        c.turn_end,
        c.turns,
        c.guard,
        c.turn_hash,
        c.tsc,
        c.tsc_step,
        c.tsc_mode,
        c.icount,
        c.pmu_total,
        c.pmu_next
    );
    for v in c.v.iter().take(vcpus) {
        f!(*v);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_skip_is_exact() {
        let mut buf = vec![0u8; 5 * 4096 + 123];
        buf[4096 + 7] = 9;
        buf[3 * 4096] = 1;
        buf[5 * 4096 + 100] = 3;
        assert_eq!(fnv(BASIS, &buf), fnv_zskip(BASIS, &buf));
        let z = vec![0u8; 8192];
        assert_eq!(fnv(42, &z), fnv_zskip(42, &z));
        assert_eq!(fnv(BASIS, b"a"), 0xaf63dc4c8601ec8c);
    }
}
