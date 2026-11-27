//! The traced process's memory one word at a time (Milestone 379, the cargo feature `words`).
//!
//! hydo-clock reads and writes the traced process's memory with `process_vm_readv` and
//! `process_vm_writev`. A guest kernel without `CONFIG_CROSS_MEMORY_ATTACH` answers both with
//! `ENOSYS`; the KVM and Mac engines' kernels are such kernels. With the feature, `peek` and
//! `poke` of `main.rs` then fall back to `PTRACE_PEEKDATA` and `PTRACE_POKEDATA`, one aligned
//! 8-byte word per call. An aligned word never crosses a page, so the fallback reads no page that
//! the call itself would not read. A write of part of a word reads the word first and writes it
//! back with the other bytes as they were; the traced task is stopped while hydo-clock does so.
//!
//! Without the feature both functions answer `false` and the compiler removes them, so the
//! default build keeps the bytes of Milestone 372 (`hydo/crates/hydo-clock/build.sh` makes both).

use super::Pid;

#[cfg(feature = "words")]
const PTRACE_PEEKDATA: libc::c_long = 2;
#[cfg(feature = "words")]
const PTRACE_POKEDATA: libc::c_long = 5;

/// The word at the aligned address `at` (raw `PTRACE_PEEKDATA`: the kernel stores the word at the
/// last argument and returns 0).
#[cfg(feature = "words")]
fn peek_word(pid: Pid, at: u64) -> Option<[u8; 8]> {
    let mut w = 0u64;
    let r = super::ptrace(PTRACE_PEEKDATA, pid, at, &mut w as *mut u64 as u64);
    (r == 0).then(|| w.to_ne_bytes())
}

#[cfg(feature = "words")]
fn poke_word(pid: Pid, at: u64, w: [u8; 8]) -> bool {
    super::ptrace(PTRACE_POKEDATA, pid, at, u64::from_ne_bytes(w)) == 0
}

/// The words that hold `len` bytes at `addr`: each as (its address, the first byte of the range
/// in it, the range's index of that byte, the count of the range's bytes in it).
#[cfg(feature = "words")]
fn spans(addr: u64, len: usize) -> impl Iterator<Item = (u64, usize, usize, usize)> {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        if i >= len {
            return None;
        }
        let a = addr + i as u64;
        let at = a & !7;
        let from = (a - at) as usize;
        let n = (8 - from).min(len - i);
        let s = (at, from, i, n);
        i += n;
        Some(s)
    })
}

/// After a `process_vm_readv` that returned `n`: reads `buf` word by word when the kernel has
/// no such call. False when the call worked or failed otherwise (then `peek` fails as before).
#[cfg(feature = "words")]
pub(super) fn peek(pid: Pid, addr: u64, buf: &mut [u8], n: libc::c_long) -> bool {
    if n >= 0 || super::errno() != libc::ENOSYS {
        return false;
    }
    for (at, from, i, k) in spans(addr, buf.len()) {
        let Some(w) = peek_word(pid, at) else {
            return false;
        };
        buf[i..i + k].copy_from_slice(&w[from..from + k]);
    }
    true
}

/// After a `process_vm_writev` that returned `n`: writes `buf` word by word when the kernel has
/// no such call.
#[cfg(feature = "words")]
pub(super) fn poke(pid: Pid, addr: u64, buf: &[u8], n: libc::c_long) -> bool {
    if n >= 0 || super::errno() != libc::ENOSYS {
        return false;
    }
    for (at, from, i, k) in spans(addr, buf.len()) {
        let mut w = if k == 8 {
            [0u8; 8]
        } else {
            match peek_word(pid, at) {
                Some(w) => w,
                None => return false,
            }
        };
        w[from..from + k].copy_from_slice(&buf[i..i + k]);
        if !poke_word(pid, at, w) {
            return false;
        }
    }
    true
}

#[cfg(not(feature = "words"))]
#[inline(always)]
pub(super) fn peek(_: Pid, _: u64, _: &mut [u8], _: libc::c_long) -> bool {
    false
}

#[cfg(not(feature = "words"))]
#[inline(always)]
pub(super) fn poke(_: Pid, _: u64, _: &[u8], _: libc::c_long) -> bool {
    false
}

#[cfg(all(test, feature = "words"))]
mod tests {
    use super::*;

    #[test]
    fn the_words_of_a_range() {
        let v: Vec<_> = spans(0x1003, 16).collect();
        assert_eq!(
            v,
            vec![(0x1000, 3, 0, 5), (0x1008, 0, 5, 8), (0x1010, 0, 13, 3)]
        );
        let v: Vec<_> = spans(0x2000, 8).collect();
        assert_eq!(v, vec![(0x2000, 0, 0, 8)]);
        assert_eq!(spans(0x2000, 0).count(), 0);
    }

    /// The fallback against this process's own child, which stops itself: a read and a write of
    /// a range that starts and ends inside words, and the bytes around it kept.
    #[test]
    fn a_child_read_and_written_word_by_word() {
        let mut data: [u8; 24] = std::array::from_fn(|i| i as u8);
        let base = std::hint::black_box(data.as_mut_ptr());
        let addr = base as u64 + 3;
        // SAFETY: the child only stops itself, reads one byte and exits; the parent traces it.
        let child = unsafe { libc::fork() };
        if child == 0 {
            crate::ptrace(0, 0, 0, 0); // PTRACE_TRACEME
                                       // SAFETY: the child's own copy of the array, read after the parent wrote it.
            unsafe {
                libc::raise(libc::SIGSTOP);
                libc::_exit(std::ptr::read_volatile(base.add(5)) as i32);
            }
        }
        let mut st = 0;
        // SAFETY: waits for our child.
        unsafe { libc::waitpid(child, &mut st, 0) };
        let read = |addr: u64, len: usize| {
            let mut b = vec![0u8; len];
            for (at, from, i, k) in spans(addr, len) {
                let w = peek_word(child, at).expect("a word of the child");
                b[i..i + k].copy_from_slice(&w[from..from + k]);
            }
            b
        };
        assert_eq!(read(addr, 13), (3u8..16).collect::<Vec<_>>());
        // the write: poke's loop without its ENOSYS test
        let new = [0xaau8; 13];
        for (at, from, i, k) in spans(addr, new.len()) {
            let mut w = peek_word(child, at).unwrap();
            w[from..from + k].copy_from_slice(&new[i..i + k]);
            assert!(poke_word(child, at, w));
        }
        let mut want: Vec<u8> = (0u8..24).collect();
        want[3..16].fill(0xaa);
        assert_eq!(read(base as u64, 24), want);
        crate::cont(child, 0);
        // SAFETY: waits for our child.
        unsafe { libc::waitpid(child, &mut st, 0) };
        assert!(libc::WIFEXITED(st));
        assert_eq!(
            libc::WEXITSTATUS(st),
            0xaa,
            "the child read the byte the parent wrote"
        );
        assert_eq!(data[5], 5, "the parent's own copy is its own");
    }

    /// `peek` and `poke` of main.rs in a process whose kernel answers ENOSYS: a tracer child with
    /// a seccomp filter that fails both calls so, against a child of its own.
    #[test]
    fn peek_and_poke_fall_back_when_the_kernel_lacks_the_calls() {
        // SAFETY: the tracer child runs the checks and exits with their outcome.
        let tracer = unsafe { libc::fork() };
        if tracer == 0 {
            let ok = std::panic::catch_unwind(tracer_checks).unwrap_or(false);
            // SAFETY: ends the child.
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        let mut st = 0;
        // SAFETY: waits for our child.
        unsafe { libc::waitpid(tracer, &mut st, 0) };
        assert!(
            libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0,
            "status {st:#x}"
        );
    }

    fn tracer_checks() -> bool {
        use crate::{SockFilter, SockFprog, BPF_JEQ_K, BPF_LD_W_ABS, BPF_RET_K, SD_NR};
        let enosys = 0x0005_0000 | libc::ENOSYS as u32; // SECCOMP_RET_ERRNO
        let f = [
            SockFilter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: SD_NR,
            },
            SockFilter {
                code: BPF_JEQ_K,
                jt: 2,
                jf: 0,
                k: libc::SYS_process_vm_readv as u32,
            },
            SockFilter {
                code: BPF_JEQ_K,
                jt: 1,
                jf: 0,
                k: libc::SYS_process_vm_writev as u32,
            },
            SockFilter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: crate::SECCOMP_RET_ALLOW,
            },
            SockFilter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: enosys,
            },
        ];
        let prog = SockFprog {
            len: f.len() as u16,
            filter: f.as_ptr(),
        };
        // SAFETY: plain prctls; prog lives until the call returns.
        unsafe {
            libc::prctl(crate::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0);
            if libc::prctl(
                crate::PR_SET_SECCOMP,
                crate::SECCOMP_MODE_FILTER,
                &prog as *const SockFprog as libc::c_ulong,
                0,
                0,
            ) != 0
            {
                return false;
            }
        }
        let mut data: [u8; 24] = std::array::from_fn(|i| i as u8);
        let base = std::hint::black_box(data.as_mut_ptr()) as u64;
        // SAFETY: the child only stops itself and exits.
        let child = unsafe { libc::fork() };
        if child == 0 {
            crate::ptrace(0, 0, 0, 0); // PTRACE_TRACEME
                                       // SAFETY: ends the child after its stop.
            unsafe {
                libc::raise(libc::SIGSTOP);
                libc::_exit(0);
            }
        }
        let mut st = 0;
        // SAFETY: waits for our child.
        unsafe { libc::waitpid(child, &mut st, 0) };
        let mut b = [0u8; 13];
        let read = crate::peek(child, base + 3, &mut b);
        let wrote = crate::poke(child, base + 5, &[0xaa; 4]);
        let mut all = [0u8; 24];
        let reread = crate::peek(child, base, &mut all);
        crate::cont(child, libc::SIGKILL);
        // SAFETY: waits for our child.
        unsafe { libc::waitpid(child, &mut st, 0) };
        let mut want: Vec<u8> = (0u8..24).collect();
        want[5..9].fill(0xaa);
        read && b.to_vec() == (3u8..16).collect::<Vec<_>>()
            && wrote
            && reread
            && all.to_vec() == want
    }
}
