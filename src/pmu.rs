//! Hardware performance-counter access, scoped to guest execution.
//!
//! A counter opened with `exclude_host` counts only while the vCPU thread is
//! inside the guest (KVM toggles the PMU across VM entry/exit), so it measures
//! the guest's retired instructions or branches without host noise. This is the
//! basis for an *instruction clock*: a measure of guest progress far finer than
//! counting VM exits.
//!
//! Whether the raw count is bit-reproducible run-to-run is a property of the
//! specific CPU and must be measured — see the `pmu` diagnostic. rr famously
//! found retired-instructions unreliable on some parts and switched to retired
//! conditional branches; hence we can open either event.

use std::io;
use std::os::unix::io::RawFd;

use perf_event_open_sys as sys;

// perf_event hardware event selectors.
pub const HW_INSTRUCTIONS: u64 = sys::bindings::PERF_COUNT_HW_INSTRUCTIONS as u64;
pub const HW_BRANCHES: u64 = sys::bindings::PERF_COUNT_HW_BRANCH_INSTRUCTIONS as u64;
pub const HW_CYCLES: u64 = sys::bindings::PERF_COUNT_HW_CPU_CYCLES as u64;
pub const TYPE_HARDWARE: u32 = sys::bindings::PERF_TYPE_HARDWARE;
const PERF_FORMAT_TOTAL_TIME_ENABLED: u32 = 1 << 0;
const PERF_FORMAT_TOTAL_TIME_RUNNING: u32 = 1 << 1;

/// Signal delivered to the vCPU thread when an armed counter overflows. A no-op
/// handler is installed for it; its only job is to make an in-flight `KVM_RUN`
/// return `EINTR` so we can switch to single-stepping for the precise landing.
pub const OVERFLOW_SIGNAL: libc::c_int = libc::SIGUSR2;

// fcntl constants and struct for directing fasync overflow signals to a TID.
// libc does not export these on this target, so we define them (Linux ABI).
const F_SETSIG: libc::c_int = 10;
const F_SETOWN_EX: libc::c_int = 15;
const F_OWNER_TID: libc::c_int = 0;

#[repr(C)]
struct FOwnerEx {
    type_: libc::c_int,
    pid: libc::pid_t,
}

extern "C" fn noop_handler(_sig: libc::c_int) {}

/// Install the overflow-signal handler (idempotent to call once at startup).
/// The handler does nothing but must exist and must NOT set SA_RESTART, so that
/// `KVM_RUN` returns EINTR instead of transparently restarting.
pub fn install_overflow_handler() {
    // SAFETY: installing a trivial signal handler with an empty mask.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = noop_handler as usize;
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(OVERFLOW_SIGNAL, &sa, std::ptr::null_mut());
    }
}

/// A single perf counter attached to the calling (vCPU) thread, counting only
/// guest-mode events.
pub struct PerfCounter {
    fd: RawFd,
    /// Ring-buffer mapping for sampling counters. Overflow notification is only
    /// reliable when the event has an mmap'd buffer for the kernel to signal on.
    mmap: Option<(*mut libc::c_void, usize)>,
}

impl PerfCounter {
    /// Open a guest-only counter for `(type_, config)`, initially disabled.
    pub fn open_guest(type_: u32, config: u64) -> io::Result<Self> {
        Self::open_inner(type_, config, 0)
    }

    /// Open a guest-only counter that raises `OVERFLOW_SIGNAL` on this thread each
    /// time it counts `sample_period` more events. Used to stop a free-running
    /// guest near an instruction target.
    pub fn open_guest_overflow(type_: u32, config: u64, sample_period: u64) -> io::Result<Self> {
        let mut ctr = Self::open_inner(type_, config, sample_period)?;
        ctr.map_ring_buffer()?;
        ctr.arm_signal()?;
        Ok(ctr)
    }

    /// Map a minimal ring buffer (1 metadata + 1 data page). Overflow signals are
    /// only delivered reliably when such a buffer exists for the kernel to fill.
    fn map_ring_buffer(&mut self) -> io::Result<()> {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        let len = page * 2; // must be 1 + 2^n pages
        // SAFETY: mapping the perf fd's ring buffer with a valid length.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        self.mmap = Some((ptr, len));
        Ok(())
    }

    fn open_inner(type_: u32, config: u64, sample_period: u64) -> io::Result<Self> {
        // SAFETY: zeroed perf_event_attr is a valid "all defaults" starting point.
        let mut attr: sys::bindings::perf_event_attr = unsafe { std::mem::zeroed() };
        attr.type_ = type_;
        attr.size = std::mem::size_of::<sys::bindings::perf_event_attr>() as u32;
        attr.config = config;
        attr.set_disabled(1);
        // Count guest execution only, at all guest privilege levels. Our guests run
        // at guest ring 0, so we must NOT set exclude_kernel/exclude_hv (those would
        // drop guest-ring0 events). This requires perf_event_paranoid <= 1.
        attr.set_exclude_host(1);
        attr.set_exclude_guest(0);
        // Return enabled/running times with each read so we can detect when the
        // event was descheduled off the PMU mid-run (which corrupts the count).
        attr.read_format = (PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING) as u64;
        if sample_period > 0 {
            attr.__bindgen_anon_1.sample_period = sample_period;
            attr.__bindgen_anon_2.wakeup_events = 1;
            // Pin the event so the kernel never descheds it off the PMU (which
            // would silently stop counting); it uses the fixed instruction counter.
            attr.set_pinned(1);
        }

        // pid = 0 (this thread), cpu = -1 (whichever CPU it runs on).
        // SAFETY: attr is a fully-initialized perf_event_attr.
        let fd = unsafe { sys::perf_event_open(&mut attr, 0, -1, -1, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PerfCounter { fd, mmap: None })
    }

    /// Route this counter's overflow notifications to OVERFLOW_SIGNAL on the
    /// current thread (fasync + F_SETSIG + F_SETOWN_EX to a TID).
    fn arm_signal(&self) -> io::Result<()> {
        // SAFETY: fcntl on our own fd with valid arguments.
        unsafe {
            let flags = libc::fcntl(self.fd, libc::F_GETFL);
            if libc::fcntl(self.fd, libc::F_SETFL, flags | libc::O_ASYNC) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(self.fd, F_SETSIG, OVERFLOW_SIGNAL) < 0 {
                return Err(io::Error::last_os_error());
            }
            let owner = FOwnerEx {
                type_: F_OWNER_TID,
                pid: libc::gettid(),
            };
            if libc::fcntl(self.fd, F_SETOWN_EX, &owner as *const FOwnerEx) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn enable(&self) {
        // SAFETY: fd is a live perf event.
        unsafe {
            sys::ioctls::ENABLE(self.fd, 0);
        }
    }

    pub fn disable(&self) {
        // SAFETY: fd is a live perf event.
        unsafe {
            sys::ioctls::DISABLE(self.fd, 0);
        }
    }

    pub fn reset(&self) {
        // SAFETY: fd is a live perf event.
        unsafe {
            sys::ioctls::RESET(self.fd, 0);
        }
    }

    /// Reprogram the overflow period in place (no reopen). The cumulative count
    /// read via `read()` is unaffected; only the next-overflow point changes.
    pub fn set_period(&self, period: u64) -> io::Result<()> {
        // PERF_EVENT_IOC_PERIOD takes a userspace pointer to the new u64 period;
        // this crate's wrapper passes its u64 arg straight through as the ioctl
        // arg, so we hand it the address of `period`.
        let p = period;
        // SAFETY: ioctl on our own perf fd; &p outlives the call.
        let ret = unsafe { sys::ioctls::PERIOD(self.fd, &p as *const u64 as u64) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Current counter value (the raw count; use `read_scheduled` when scheduling
    /// integrity matters).
    pub fn read(&self) -> io::Result<u64> {
        Ok(self.read_full()?.0)
    }

    /// Read (count, time_enabled, time_running). If `time_running < time_enabled`
    /// the event was descheduled off the PMU for part of the interval and the
    /// count is only a partial measurement.
    fn read_full(&self) -> io::Result<(u64, u64, u64)> {
        let mut buf = [0u8; 24];
        // SAFETY: read_format yields 24 bytes (value, enabled, running).
        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, 24) };
        if n != 24 {
            return Err(io::Error::last_os_error());
        }
        let val = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let enabled = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let running = u64::from_le_bytes(buf[16..24].try_into().unwrap());
        Ok((val, enabled, running))
    }

    /// Read the count, returning an error if the event was ever descheduled
    /// (running != enabled). A clean, fully-scheduled read is required for the
    /// instruction count to be trustworthy.
    pub fn read_scheduled(&self) -> io::Result<u64> {
        let (val, enabled, running) = self.read_full()?;
        if running != enabled {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "perf event was descheduled (count unreliable)",
            ));
        }
        Ok(val)
    }
}

impl Drop for PerfCounter {
    fn drop(&mut self) {
        // SAFETY: unmapping our ring buffer (if any) and closing our fd once.
        unsafe {
            if let Some((ptr, len)) = self.mmap {
                libc::munmap(ptr, len);
            }
            libc::close(self.fd);
        }
    }
}
