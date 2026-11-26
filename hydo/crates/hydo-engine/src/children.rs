//! Milestone 367: every engine program this process started, so that a stop by SIGINT or SIGTERM
//! stops them too, and the list of their pids in a file where a user finds them.
//!
//! The SeaweedFS onboarding stopped an exploration and found its engine `hydo-hvx` alive with
//! 14 GB of the host; nothing named its pid. hvx's engine runs under `sudo` (through the module
//! lock's `flock`), so this process cannot signal it: it is root. It can signal the `sudo`
//! process, whose real user is this one, and `sudo` relays SIGTERM to the command it runs. So a
//! stop sends SIGTERM to each engine child and every process under it that this user may
//! signal, waits for them, and then kills what is left that it can kill. The engine also ends
//! when its standard input closes, which the stop of this process does too.
//!
//! [`set_list`] names the file; it holds one line per running engine program, rewritten when one
//! starts, becomes ready, or ends:
//!
//! ```text
//! hydo-hvx 336735 (under sudo 336733, flock 336723; stop it with: sudo kill 336735)
//! ```

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The engine children: the pid this process spawned.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// Where the list of engine pids goes ([`set_list`]).
static LIST: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Write the list of the running engine programs to `path` from now on (None: no list).
pub fn set_list(path: Option<PathBuf>) {
    *LIST.lock().unwrap_or_else(|e| e.into_inner()) = path;
    refresh();
}

/// A wrapper becomes its engine program a moment after it starts (`sudo env` runs `hydo-hvx`),
/// and when the wrapper ends first its engine runs on under another parent: every 2 s while
/// engines run, the processes under them are remembered ([`KNOWN`]) and the list rewritten.
fn ticker() {
    static TICKER: std::sync::Once = std::sync::Once::new();
    TICKER.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(2));
            if !live().is_empty() {
                refresh();
            }
        });
    });
}

/// Every process seen under an engine child: a stop ends those still running, also when their
/// wrapper ended first (the stop's own signal to the process group ends `flock` at once).
static KNOWN: Mutex<Vec<Proc>> = Mutex::new(Vec::new());

/// Remember the processes under the engine children now.
fn remember(all: &[Proc]) {
    let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    for pid in live() {
        for p in tree(all, pid).into_iter().filter(|p| !p.ended) {
            if !k.iter().any(|q| q.pid == p.pid && q.start == p.start) {
                k.push(p);
            }
        }
    }
}

/// The remembered processes that still run (the same pid and start).
fn known_alive(all: &[Proc]) -> Vec<Proc> {
    let k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    k.iter()
        .filter(|q| {
            all.iter()
                .any(|p| p.pid == q.pid && p.start == q.start && !p.ended)
        })
        .cloned()
        .collect()
}

pub(crate) fn add(pid: u32) {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(pid);
    ticker();
    if stopping() {
        // started while the stop began: it ends with the others
        // SAFETY: a signal to the child this process just started
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    refresh();
}

pub(crate) fn remove(pid: u32) {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|p| *p != pid);
    refresh();
}

/// The engine children now.
pub fn live() -> Vec<u32> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// One process: its pid, parent, real user, command name, and whether it has ended (a zombie).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub name: String,
    pub ended: bool,
    /// When it started, in clock ticks after boot: with the pid, which process it is
    pub start: u64,
}

/// Every process of the machine (Linux: `/proc`); empty elsewhere.
pub fn processes() -> Vec<Proc> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Some(p) = read_proc(pid) {
            out.push(p);
        }
    }
    out
}

fn read_proc(pid: u32) -> Option<Proc> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_proc(pid, &stat, &status)
}

/// A process from its `/proc/<pid>/stat` and `/proc/<pid>/status`.
pub fn parse_proc(pid: u32, stat: &str, status: &str) -> Option<Proc> {
    // "pid (comm) S ppid ...": the name may hold spaces and parentheses, so read from the last ')'
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    let mut rest = stat.get(close + 1..)?.split_whitespace();
    let state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    // the 22nd field: the 17 after the parent's and then the start time
    let start = rest.nth(17).and_then(|v| v.parse().ok()).unwrap_or(0);
    let uid = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())?;
    Some(Proc {
        pid,
        ppid,
        uid,
        name,
        ended: state == "Z" || state == "X",
        start,
    })
}

/// `root` and every process under it, parents first.
pub fn tree(all: &[Proc], root: u32) -> Vec<Proc> {
    let mut out: Vec<Proc> = all.iter().filter(|p| p.pid == root).cloned().collect();
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].pid;
        out.extend(all.iter().filter(|p| p.ppid == parent).cloned());
        i += 1;
    }
    out
}

/// The list's line of one engine child: the deepest process under it (the engine program) and
/// the processes it runs under. None when it has ended.
pub fn line(chain: &[Proc]) -> Option<String> {
    let alive: Vec<&Proc> = chain.iter().filter(|p| !p.ended).collect();
    let leaf = alive
        .iter()
        .rev()
        .find(|p| p.name.starts_with("hydo-") || p.name.starts_with("hvm"))
        .or(alive.last())?;
    // the processes it runs under: its parent, that one's parent, ... up to the child
    let mut under: Vec<String> = Vec::new();
    let mut at = leaf.ppid;
    while let Some(p) = alive.iter().find(|p| p.pid == at) {
        under.push(format!("{} {}", p.name, p.pid));
        at = p.ppid;
    }
    let stop = if leaf.uid == 0 { "sudo kill" } else { "kill" };
    Some(if under.is_empty() {
        format!(
            "{} {} (stop it with: {stop} {})",
            leaf.name, leaf.pid, leaf.pid
        )
    } else {
        format!(
            "{} {} (under {}; stop it with: {stop} {})",
            leaf.name,
            leaf.pid,
            under.join(", "),
            leaf.pid
        )
    })
}

/// Rewrite the list ([`set_list`]) from the engine children and the processes under them.
pub(crate) fn refresh() {
    // one writer at a time (workers start and end engines at once)
    static WRITE: Mutex<()> = Mutex::new(());
    let _one = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let all = processes();
    remember(&all);
    let Some(path) = LIST.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return;
    };
    let pids = live();
    // the file comes with the first engine (an exploration of the scripted engine has none)
    if pids.is_empty() && !path.exists() {
        return;
    }
    let mut text = String::new();
    let mut named: Vec<u32> = Vec::new();
    for pid in pids {
        let chain = tree(&all, pid);
        named.extend(chain.iter().map(|p| p.pid));
        let l = if chain.is_empty() {
            // not Linux: the child is the engine
            Some(format!("engine {pid} (stop it with: kill {pid})"))
        } else {
            line(&chain)
        };
        if let Some(l) = l {
            text.push_str(&l);
            text.push('\n');
        }
    }
    // an engine program whose wrapper ended
    for p in known_alive(&all) {
        if !named.contains(&p.pid) && (p.name.starts_with("hydo-") || p.name.starts_with("hvm")) {
            let stop = if p.uid == 0 { "sudo kill" } else { "kill" };
            text.push_str(&format!(
                "{} {} (its wrapper ended; stop it with: {stop} {})\n",
                p.name, p.pid, p.pid
            ));
        }
    }
    let first = !path.exists();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_ok() && first {
        eprintln!(
            "hydo: the pid of each engine program is in {} while it runs",
            path.display()
        );
    }
}

fn alive(p: &Proc) -> bool {
    let pid = p.pid;
    if cfg!(target_os = "linux") {
        return read_proc(pid).is_some_and(|q| !q.ended && q.start == p.start);
    }
    // elsewhere an ended child stays until it is waited for: reap it here (this process ends
    // after the stop), then kill(pid, 0) says whether a process is there (EPERM: another
    // user's, alive)
    let mut st = 0;
    // SAFETY: waitpid without blocking on a pid this process may have started; kill with
    // signal 0 only checks the pid
    unsafe {
        if libc::waitpid(pid as libc::pid_t, &mut st, libc::WNOHANG) == pid as libc::pid_t {
            return false;
        }
        libc::kill(pid as libc::pid_t, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// Set when a stop began: no engine starts after it ([`stopping`]).
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this process is stopping its engines.
pub fn stopping() -> bool {
    STOPPING.load(std::sync::atomic::Ordering::SeqCst)
}

/// While this process stops its engines, a thread that would start an engine, or that sees its
/// engine end, waits here until the stop ends the process: so no worker starts a new engine,
/// and no run counts as failed because the stop ended its engine (an exploration stopped in
/// its boot would report every seed as a failure).
pub fn park_if_stopping() {
    while stopping() {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stop every engine child and the processes under it: SIGTERM to each that this user may
/// signal (a `sudo` relays it to its root command), then up to `grace` for them to end, then
/// SIGKILL to what is left that this user may kill. Returns the processes that stayed, as
/// (name, pid).
pub fn stop_all(grace: Duration) -> Vec<(String, u32)> {
    STOPPING.store(true, std::sync::atomic::Ordering::SeqCst);
    let all = processes();
    remember(&all);
    let mut procs: Vec<Proc> = Vec::new();
    for pid in live() {
        let chain = tree(&all, pid);
        if chain.is_empty() {
            procs.push(Proc {
                pid,
                ppid: 0,
                uid: u32::MAX,
                name: "engine".into(),
                ended: false,
                start: 0,
            });
        }
        procs.extend(chain.into_iter().filter(|p| !p.ended));
    }
    for p in known_alive(&all) {
        if !procs.iter().any(|q| q.pid == p.pid) {
            procs.push(p);
        }
    }
    for p in &procs {
        // SAFETY: a signal to a pid of this process's engines; EPERM for a root process is fine
        unsafe {
            libc::kill(p.pid as libc::pid_t, libc::SIGTERM);
        }
    }
    let until = Instant::now() + grace;
    while Instant::now() < until && procs.iter().any(alive) {
        std::thread::sleep(Duration::from_millis(100));
    }
    for p in &procs {
        if alive(p) {
            // SAFETY: as above
            unsafe {
                libc::kill(p.pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until && procs.iter().any(alive) {
        std::thread::sleep(Duration::from_millis(100));
    }
    // a root engine that its sudo did not end: this process started it with sudo, so sudo may
    // end it too (without a password prompt: -n)
    let root: Vec<String> = procs
        .iter()
        .filter(|p| p.uid == 0 && alive(p))
        .map(|p| p.pid.to_string())
        .collect();
    if !root.is_empty() {
        let _ = std::process::Command::new("sudo")
            .args(["-n", "kill", "-KILL"])
            .args(&root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && procs.iter().any(alive) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    procs
        .iter()
        .filter(|p| alive(p))
        .map(|p| (p.name.clone(), p.pid))
        .collect()
}

/// The write end of the pipe the signal handler writes to (-1: not installed).
static PIPE_W: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    // no engine starts from now on, and the process waits for the stop when it exits
    STOPPING.store(true, std::sync::atomic::Ordering::SeqCst);
    let fd = PIPE_W.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        let b = sig as u8;
        // SAFETY: write(2) is async-signal-safe; one byte to the pipe of [`stop_on_signals`]
        unsafe {
            libc::write(fd, (&b as *const u8).cast(), 1);
        }
    }
}

/// On SIGINT or SIGTERM, stop the engine children ([`stop_all`]), say on stderr which ones
/// stopped and which stayed, and end this process by the same signal. A signal that this
/// process inherited as ignored (a command started with `nohup` or `&` in a script) stays
/// ignored. Call it once, before any engine starts.
pub fn stop_on_signals() {
    if PIPE_W.load(std::sync::atomic::Ordering::Relaxed) >= 0 {
        return;
    }
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: a pipe for the handler's byte
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let (r, w) = (fds[0], fds[1]);
    PIPE_W.store(w, std::sync::atomic::Ordering::Relaxed);
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: reading the current disposition, and installing a handler that only writes
        // one byte to a pipe
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut old) != 0 {
                continue;
            }
            if old.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
    // a worker that sees its engine end can end the program before the stop is done: its exit
    // waits for the stop, which ends the process by the signal
    // SAFETY: registering a plain function to run at exit
    unsafe {
        libc::atexit(wait_for_stop);
    }
    std::thread::spawn(move || {
        let mut b = 0u8;
        // SAFETY: reading the handler's byte from the pipe
        let n = unsafe { libc::read(r, (&mut b as *mut u8).cast(), 1) };
        if n != 1 {
            return;
        }
        let sig = b as libc::c_int;
        let had = live();
        if !had.is_empty() {
            let name = if sig == libc::SIGINT {
                "SIGINT"
            } else {
                "SIGTERM"
            };
            eprintln!(
                "hydo: {name}: stopping {} engine program(s) before this process ends",
                had.len()
            );
            let left = stop_all(Duration::from_secs(10));
            for (name, pid) in &left {
                eprintln!("hydo: {name} {pid} did not stop; stop it with: sudo kill -9 {pid}");
            }
            if left.is_empty() {
                eprintln!("hydo: {name}: the engine programs stopped");
            }
            refresh();
        }
        // end by the signal itself, so that the parent sees how this process ended
        // SAFETY: the default action back, then the signal to this process; _exit (no exit
        // handlers, which would wait for this stop) only if the signal did not end it
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::kill(libc::getpid(), sig);
            std::thread::sleep(Duration::from_secs(1));
            libc::_exit(128 + sig);
        }
    });
}

/// At exit during a stop: wait for the stop thread, which ends the process (at most 40 s).
extern "C" fn wait_for_stop() {
    if !stopping() {
        return;
    }
    let until = Instant::now() + Duration::from_secs(40);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, uid: u32, name: &str) -> Proc {
        Proc {
            pid,
            ppid,
            uid,
            name: name.into(),
            ended: false,
            start: 0,
        }
    }

    #[test]
    fn a_proc_entry_is_read_with_spaces_and_parentheses_in_its_name() {
        let stat = "336735 (hydo-hvx) S 336733 336328 336328 0 -1 4194560";
        let status = "Name:\thydo-hvx\nUid:\t0\t0\t0\t0\n";
        assert_eq!(
            parse_proc(336735, stat, status),
            Some(p(336735, 336733, 0, "hydo-hvx"))
        );
        let stat = "7 (a (b) c) Z 1 7 7";
        let status = "Uid:\t1000\t1000\t1000\t1000\n";
        let got = parse_proc(7, stat, status).unwrap();
        assert_eq!(got.name, "a (b) c");
        assert!(got.ended);
    }

    #[test]
    fn the_line_names_the_engine_under_its_wrappers() {
        let all = vec![
            p(1, 0, 0, "init"),
            p(336723, 336328, 1000, "flock"),
            p(336733, 336723, 1000, "sudo"),
            p(336735, 336733, 0, "hydo-hvx"),
            p(9, 1, 1000, "other"),
        ];
        let chain = tree(&all, 336723);
        assert_eq!(
            chain.iter().map(|p| p.pid).collect::<Vec<_>>(),
            [336723, 336733, 336735]
        );
        assert_eq!(
            line(&chain).unwrap(),
            "hydo-hvx 336735 (under sudo 336733, flock 336723; stop it with: sudo kill 336735)"
        );
        // an engine of this user, started directly
        let all = vec![p(5, 1, 501, "hydo-hvm")];
        assert_eq!(
            line(&tree(&all, 5)).unwrap(),
            "hydo-hvm 5 (stop it with: kill 5)"
        );
        // an ended chain has no line
        let mut z = p(5, 1, 501, "hydo-hvm");
        z.ended = true;
        assert_eq!(line(&[z]), None);
    }
}
