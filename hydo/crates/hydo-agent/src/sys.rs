//! Thin wrappers around the system calls the agent uses (Linux, through libc).

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;

/// errno of the last failed call.
pub fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// The symbolic name of an errno (the same text whatever C library the agent is built with).
pub fn errno_name(e: i32) -> String {
    let n = match e {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::ESRCH => "ESRCH",
        libc::EINTR => "EINTR",
        libc::EIO => "EIO",
        libc::ENXIO => "ENXIO",
        libc::E2BIG => "E2BIG",
        libc::ENOEXEC => "ENOEXEC",
        libc::EBADF => "EBADF",
        libc::ECHILD => "ECHILD",
        libc::EAGAIN => "EAGAIN",
        libc::ENOMEM => "ENOMEM",
        libc::EACCES => "EACCES",
        libc::EFAULT => "EFAULT",
        libc::ENOTBLK => "ENOTBLK",
        libc::EBUSY => "EBUSY",
        libc::EEXIST => "EEXIST",
        libc::EXDEV => "EXDEV",
        libc::ENODEV => "ENODEV",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        libc::ENFILE => "ENFILE",
        libc::EMFILE => "EMFILE",
        libc::ENOTTY => "ENOTTY",
        libc::ETXTBSY => "ETXTBSY",
        libc::EFBIG => "EFBIG",
        libc::ENOSPC => "ENOSPC",
        libc::ESPIPE => "ESPIPE",
        libc::EROFS => "EROFS",
        libc::EMLINK => "EMLINK",
        libc::EPIPE => "EPIPE",
        libc::ERANGE => "ERANGE",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::ENOSYS => "ENOSYS",
        libc::ENOTEMPTY => "ENOTEMPTY",
        libc::ELOOP => "ELOOP",
        libc::ENOBUFS => "ENOBUFS",
        libc::EPROTONOSUPPORT => "EPROTONOSUPPORT",
        libc::EAFNOSUPPORT => "EAFNOSUPPORT",
        libc::EADDRINUSE => "EADDRINUSE",
        libc::ECONNREFUSED => "ECONNREFUSED",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::EOPNOTSUPP => "EOPNOTSUPP",
        _ => return format!("errno {e}"),
    };
    n.to_string()
}

/// The errno of an io::Error (0 when it has none).
pub fn io_errno(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
}

pub fn cstr(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// CLOCK_MONOTONIC in ms.
pub fn now_ms() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1000 + t.tv_nsec as u64 / 1_000_000
}

/// CLOCK_REALTIME (seconds, nanoseconds).
#[allow(clippy::unnecessary_cast)]
pub fn realtime() -> (i64, u32) {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut t) };
    (t.tv_sec as i64, t.tv_nsec as u32)
}

pub fn set_realtime(sec: i64, nsec: u32) -> io::Result<()> {
    let t = libc::timespec {
        tv_sec: sec as _,
        tv_nsec: nsec as libc::c_long,
    };
    check(unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &t) }).map(|_| ())
}

/// Writes all of `b` (retrying short writes and EINTR; a full non-blocking pipe is waited for).
pub fn write_all(fd: RawFd, mut b: &[u8]) -> io::Result<()> {
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr().cast(), b.len()) };
        if n < 0 {
            match errno() {
                libc::EINTR => continue,
                libc::EAGAIN => {
                    let mut p = libc::pollfd {
                        fd,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    unsafe { libc::poll(&mut p, 1, -1) };
                    continue;
                }
                _ => return Err(io::Error::last_os_error()),
            }
        }
        b = &b[n as usize..];
    }
    Ok(())
}

/// read(2); `Ok(0)` is the end of the file, EAGAIN is `Err`.
pub fn read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        if errno() != libc::EINTR {
            return Err(io::Error::last_os_error());
        }
    }
}

pub fn close(fd: RawFd) {
    unsafe { libc::close(fd) };
}

pub fn set_nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

/// Blocks SIGCHLD and returns a non-blocking signalfd for it.
pub fn sigchld_fd() -> io::Result<RawFd> {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        check(libc::sigprocmask(
            libc::SIG_BLOCK,
            &set,
            std::ptr::null_mut(),
        ))?;
        check(libc::signalfd(
            -1,
            &set,
            libc::SFD_NONBLOCK | libc::SFD_CLOEXEC,
        ))
    }
}

/// Empties a signalfd.
pub fn drain_signalfd(fd: RawFd) {
    let mut b = [0u8; 128 * 8];
    while matches!(read(fd, &mut b), Ok(n) if n > 0) {}
}

/// One reaped child: its pid and wait status.
pub fn reap_one() -> Option<(i32, i32)> {
    let mut st = 0;
    let p = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) };
    (p > 0).then_some((p, st))
}

/// (status as a shell reports it, the signal if one ended the process).
pub fn exit_status(st: i32) -> (u32, Option<u32>) {
    if libc::WIFSIGNALED(st) {
        let s = libc::WTERMSIG(st) as u32;
        (128 + s, Some(s))
    } else {
        (libc::WEXITSTATUS(st) as u32, None)
    }
}

pub fn kill(pid: i32, sig: i32) -> bool {
    unsafe { libc::kill(pid, sig) == 0 }
}

/// poll(2) over `fds`; timeout in ms (-1: none). EINTR counts as nothing ready.
pub fn poll(fds: &mut [libc::pollfd], timeout: i32) -> usize {
    let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
    r.max(0) as usize
}

pub fn sleep_us(us: u64) {
    let t = libc::timespec {
        tv_sec: (us / 1_000_000) as _,
        tv_nsec: ((us % 1_000_000) * 1000) as libc::c_long,
    };
    unsafe { libc::nanosleep(&t, std::ptr::null_mut()) };
}

/// A non-blocking inotify instance.
pub fn inotify() -> io::Result<RawFd> {
    check(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })
}

pub fn inotify_watch(fd: RawFd, dir: &str) -> io::Result<i32> {
    let c = cstr(dir)?;
    let mask = libc::IN_MODIFY | libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE;
    check(unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) })
}

/// Sets the terminal on `fd` to raw mode without echo (no-op when it is not a terminal).
pub fn tty_raw(fd: RawFd) {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return;
        }
        libc::cfmakeraw(&mut t);
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        libc::tcsetattr(fd, libc::TCSANOW, &t);
    }
}

pub fn open(path: &str, flags: libc::c_int, mode: libc::mode_t) -> io::Result<RawFd> {
    let c = cstr(path)?;
    check(unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, mode as libc::c_uint) })
}

pub fn mount(
    src: &str,
    dst: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> io::Result<()> {
    let (s, d, t) = (cstr(src)?, cstr(dst)?, cstr(fstype)?);
    let data = data.map(cstr).transpose()?;
    let dp = data
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr().cast());
    check(unsafe { libc::mount(s.as_ptr(), d.as_ptr(), t.as_ptr(), flags, dp) }).map(|_| ())
}

pub fn sync() {
    unsafe { libc::sync() };
}

/// SCHED_FIFO at `prio` (children reset to SCHED_OTHER), or back to SCHED_OTHER with 0.
/// Through the system call: musl's sched_setscheduler() is a stub that fails with ENOSYS.
pub fn set_fifo(prio: i32) -> io::Result<()> {
    // the kernel's struct sched_param is one int
    let sp: libc::c_int = prio;
    let policy = if prio > 0 {
        libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK
    } else {
        libc::SCHED_OTHER
    };
    let r = unsafe {
        libc::syscall(
            libc::SYS_sched_setscheduler,
            0,
            policy,
            &sp as *const libc::c_int,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn getpid() -> i32 {
    unsafe { libc::getpid() }
}
