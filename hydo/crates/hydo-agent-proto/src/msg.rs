//! The messages of agent protocol version 1: the host's commands, the agent's replies to them, its
//! refusals, and the events it reports on its own (a process exited, a probe ended, a log line, a
//! freeze fired). Each kind is one record type below; the line envelope (`v`, `id`, `op`, `seq`,
//! `re`, `ok`, `ev`) is added by [`crate::line`]. The JSON member of a field is its Rust name.
//!
//! Additions only: a new op, a new event, a new optional field or a new name in a closed set is
//! an addition of version 1. A new optional field of a COMMAND also needs a feature name in the
//! agent's hello ([`FEATURES`]), because the agent refuses unknown fields and the host must know
//! whether an older agent of the same version accepts it.

use crate::field::{names, record};

/// The protocol version this crate speaks.
pub const PROTO: u64 = 1;

/// Every protocol version the host side of this crate reads (it keeps every released one).
pub const HOST_PROTOCOLS: std::ops::RangeInclusive<u64> = 1..=1;

/// Optional capabilities an agent of version 1 can announce in its hello.
pub const FEATURES: &[&str] = &["freeze", "freeze-netns", "hostcall"];

/// What each feature of [`FEATURES`] gives, in words (for the host's message when an image's
/// agent lacks one).
pub fn feature_purpose(feature: &str) -> &'static str {
    match feature {
        "freeze" => "the packet trigger (F=)",
        "freeze-netns" => {
            "the packet trigger inside a service's network namespace (F= with network: bridge)"
        }
        "hostcall" => {
            "the host-call port: the agent's lines and the host's on an I/O port instead of the \
             serial line, when the engine offers it (hvx, M138)"
        }
        _ => "an optional capability",
    }
}

/// The longest host line the agent reads (the serial receive ring holds 4,096 bytes), without
/// its newline.
pub const MAX_HOST_LINE: usize = 4000;

names! {
    /// A signal the host can send to a process tree.
    Sig {
        /// SIGKILL
        Kill = "KILL",
        /// SIGTERM
        Term = "TERM",
        /// SIGSTOP
        Stop = "STOP",
        /// SIGCONT
        Cont = "CONT",
        /// SIGINT
        Int = "INT",
        /// SIGHUP
        Hup = "HUP",
        /// SIGQUIT
        Quit = "QUIT",
        /// SIGUSR1
        Usr1 = "USR1",
        /// SIGUSR2
        Usr2 = "USR2",
    }
}

names! {
    /// A mount flag (mount(2)'s MS_*).
    MountFlag {
        /// MS_RDONLY
        Ro = "ro",
        /// MS_NOSUID
        Nosuid = "nosuid",
        /// MS_NODEV
        Nodev = "nodev",
        /// MS_NOEXEC
        Noexec = "noexec",
        /// MS_NOATIME
        Noatime = "noatime",
        /// MS_RELATIME
        Relatime = "relatime",
        /// MS_SYNCHRONOUS
        Sync = "sync",
        /// MS_REMOUNT
        Remount = "remount",
        /// MS_BIND
        Bind = "bind",
    }
}

names! {
    /// The direction of a packet relative to the service that the freeze watches.
    Dir {
        /// A packet the service sends (to the target).
        Req = "req",
        /// A packet the target sends to the service.
        Resp = "resp",
    }
}

names! {
    /// How a probe ended.
    ProbeEnd {
        /// An attempt exited with status 0.
        Ok = "ok",
        /// The deadline passed (an attempt still running at the deadline is killed).
        Deadline = "deadline",
        /// The process named by `alive` was not running any more.
        Died = "died",
    }
}

names! {
    /// Why the agent refused a line or could not carry out a command.
    ErrCode {
        /// The line is not JSON of the protocol's value set (msg says where).
        BadJson = "bad-json",
        /// The line is JSON but not a command: not an object, or `v`, `id` or `op` missing or of
        /// the wrong kind.
        BadLine = "bad-line",
        /// The line's `v` is not a version this agent speaks.
        BadVersion = "bad-version",
        /// The op is not one this agent knows (the hello lists the ones it does).
        UnknownOp = "unknown-op",
        /// A field is unknown, missing, of the wrong kind or out of range (msg names it).
        BadField = "bad-field",
        /// The id is not above the last id the agent executed (a command typed again after a
        /// restore is not executed twice).
        DuplicateId = "duplicate-id",
        /// No process was ever started under this handle.
        NoSuchHandle = "no-such-handle",
        /// A process under this handle is still running.
        HandleBusy = "handle-busy",
        /// The process under this handle has exited.
        NotRunning = "not-running",
        /// The resource is in use (a freeze is already armed, a tail name exists).
        Busy = "busy",
        /// No such thing to act on (a tail name, an armed freeze).
        NotFound = "not-found",
        /// A system call failed (msg names it and the errno).
        OsError = "os-error",
        /// The file's bytes are not UTF-8 (ask for hex).
        NotUtf8 = "not-utf8",
        /// The input line is longer than the agent reads; it was discarded up to its newline.
        TooLong = "too-long",
    }
}

names! {
    /// Something the agent noticed that is not an answer to a command.
    WarnCode {
        /// The netfilter queue overflowed and dropped packets (the freeze's count is wrong from here).
        NetlinkEnobufs = "netlink-enobufs",
        /// A packet verdict could not be sent.
        VerdictFailed = "verdict-failed",
        /// A tailed file could not be read.
        TailFailed = "tail-failed",
        /// The freeze's n-th packet came, but its process is not running: nothing was frozen.
        FreezeNoProcess = "freeze-no-process",
    }
}

// ------------------------------------------------------------------ commands

record! {
    /// Resynchronise after a restore: the agent echoes the nonce. Sent after a CAN byte (0x18),
    /// which makes the agent drop a partly read line first.
    Sync {
        /// Any number; echoed in the reply.
        nonce: u64,
    }
}

record! {
    /// Start a long-running process (a service) under a handle. Its stdin is /dev/null, it gets a
    /// new session, its environment is exactly `env`. When it exits the agent reports an `exit`
    /// event; the handle can then be used again.
    Spawn {
        /// The handle the host names the process by (for signals and in the exit event).
        h: String,
        /// The program and its arguments; the program is looked up in the PATH of `env` when it
        /// has no slash.
        argv: Vec<String>,
        /// The environment, as NAME=VALUE strings (nothing is inherited from the agent).
        env: Option<Vec<String>>,
        /// The working directory (default /).
        cwd: Option<String>,
        /// Run as this user id.
        uid: Option<u32>,
        /// Run as this group id.
        gid: Option<u32>,
        /// Enter this network namespace first (a path such as /var/run/netns/a).
        netns: Option<String>,
        /// Write stdout to this file (default /dev/null).
        stdout: Option<String>,
        /// Write stderr to this file; the same path as stdout shares one file (default /dev/null).
        stderr: Option<String>,
        /// Append to the output files instead of truncating them.
        append: Option<bool>,
        /// Kill the process tree (SIGKILL) after this many ms of guest time; the exit event then
        /// says timed_out.
        kill_after_ms: Option<u64>,
    }
}

record! {
    /// Send a signal to the process under a handle, or to it and every descendant.
    Signal {
        /// The process's handle.
        h: String,
        /// The signal.
        sig: Sig,
        /// Also every descendant (the tree as /proc shows it at this moment).
        tree: bool,
        /// After STOP: wait until every thread of the tree is stopped (at most about 2 s of
        /// guest time), and say whether they all were.
        wait_stopped: Option<bool>,
    }
}

record! {
    /// Run a command to its end and report its exit status and output in a `done` event (stdout
    /// and stderr share one pipe; stdin is /dev/null).
    Run {
        /// The program and its arguments.
        argv: Vec<String>,
        /// The environment, as NAME=VALUE strings.
        env: Option<Vec<String>>,
        /// The working directory (default /).
        cwd: Option<String>,
        /// Run as this user id.
        uid: Option<u32>,
        /// Run as this group id.
        gid: Option<u32>,
        /// Enter this network namespace first.
        netns: Option<String>,
        /// Kill the command's tree after this many ms of guest time.
        kill_after_ms: Option<u64>,
        /// Keep at most this many bytes of output (default 2048, at most 3000).
        capture: Option<u32>,
    }
}

record! {
    /// Run a command again and again until it exits 0 or a deadline in guest time passes, and
    /// report the attempts in a `probe` event (readiness checks, eventually-polls).
    Probe {
        /// The program and its arguments.
        argv: Vec<String>,
        /// The environment, as NAME=VALUE strings.
        env: Option<Vec<String>>,
        /// The working directory (default /).
        cwd: Option<String>,
        /// Run as this user id.
        uid: Option<u32>,
        /// Run as this group id.
        gid: Option<u32>,
        /// Enter this network namespace first.
        netns: Option<String>,
        /// Start attempt k at k * every_ms after the command (or when attempt k-1 ended, if later).
        every_ms: u64,
        /// Give up this many ms after the command; an attempt still running then is killed.
        deadline_ms: u64,
        /// Give up as soon as the process under this handle is not running.
        alive: Option<String>,
    }
}

record! {
    /// Write a file (created with its parent directories).
    Write {
        /// The file.
        path: String,
        /// The text to write.
        data: String,
        /// Append instead of replacing.
        append: Option<bool>,
        /// The file mode for a new file (default 0644).
        mode: Option<u32>,
    }
}

record! {
    /// Read part of a file.
    Read {
        /// The file.
        path: String,
        /// Start at this byte (default 0).
        offset: Option<u64>,
        /// Read at most this many bytes (at most 3000).
        max: u32,
        /// Answer in lower-case hex instead of text.
        hex: Option<bool>,
    }
}

record! {
    /// Make a directory and its parents.
    Mkdir {
        /// The directory.
        path: String,
        /// The mode of directories made (default 0755).
        mode: Option<u32>,
    }
}

record! {
    /// Mount a file system (mount(2)); the mount point is made first.
    Mount {
        /// The source (a device, or none).
        src: String,
        /// The mount point.
        dst: String,
        /// The file system type.
        fstype: String,
        /// The flags.
        flags: Option<Vec<MountFlag>>,
        /// The file system's own options (mount(2)'s data, for example data=journal).
        data: Option<String>,
    }
}

record! {
    /// sync(2): write every file system's dirty data to its disk.
    Syncfs {}
}

record! {
    /// Set the realtime clock: to `sec` (seconds since 1970), or by `step` seconds from now.
    /// Exactly one of the two.
    ClockSet {
        /// The new time, seconds since 1970 (nanoseconds 0).
        sec: Option<i64>,
        /// Seconds to add to the current time (negative: backwards).
        step: Option<i64>,
    }
}

record! {
    /// Read the realtime and the monotonic clock.
    ClockGet {}
}

record! {
    /// The number of packets queued in a qdisc: runs `<tc> -s qdisc show dev <dev>` and reads the
    /// backlog of the qdisc with that handle (the drain of a lifted netem fault).
    Qlen {
        /// The image's pinned tc.
        tc: String,
        /// The device (for example lo).
        dev: String,
        /// The qdisc handle as tc prints it (for example 40:).
        handle: String,
    }
}

record! {
    /// Report every new complete line of a file as a `line` event, from its current end (or from
    /// its start). A file that shrinks is read again from its start. The file need not exist yet;
    /// its directory must.
    Tail {
        /// The name the events carry.
        name: String,
        /// The file.
        path: String,
        /// Start at the beginning of the file instead of its end.
        from_start: Option<bool>,
    }
}

record! {
    /// Stop a tail; a partial last line is not reported.
    Untail {
        /// The tail's name.
        name: String,
    }
}

record! {
    /// Arm the packet trigger (hydo_fc, the F= fault): read the packets the image's iptables rules
    /// put into netfilter queue `queue`, count those with TCP payload in direction `dir` that
    /// contain `pattern`, and at the n-th hold it, stop the process tree of `h` (SIGSTOP, then
    /// wait until every thread is stopped), report `froze`, and only then let the packet through.
    /// After dur_ms of guest time the tree is continued and `resumed` reported. Every other
    /// packet is accepted at once. n = 0 never fires (a trace in `log`).
    Freeze {
        /// The process to freeze.
        h: String,
        /// The netfilter queue number.
        queue: u32,
        /// A packet is a request when its source address is this IPv4 address.
        sip: Option<String>,
        /// Otherwise: a request when its destination port is this port.
        port: Option<u32>,
        /// Otherwise: a request when its source port is this port (the service's own port, the
        /// peer being the workload's client).
        own_port: Option<u32>,
        /// Which direction counts.
        dir: Dir,
        /// Fire at the n-th counted packet.
        n: u64,
        /// Count only packets whose payload contains these bytes (%xx escapes any byte).
        pattern: Option<String>,
        /// How long the tree stays stopped, in ms of guest time.
        dur_ms: u64,
        /// Write a trace line per payload packet to this file (hydo_fc's log format).
        log: Option<String>,
        /// Bind the netfilter queue inside this network namespace (a path such as
        /// /tmp/hydo/ns/a): netfilter queues are per namespace, so the trigger reads the packets
        /// that namespace's rules queue, as hydo_fc does under `nsenter -n<file>`. Only the
        /// queue's socket is made there; the agent stays in its own namespace. Needs the feature
        /// `freeze-netns`.
        netns: Option<String>,
    }
}

record! {
    /// Disarm the packet trigger: close the queue (the image's rules must be removed first, or
    /// their packets bypass it). A freeze still in force is NOT continued; the host decides.
    Unfreeze {}
}

// ------------------------------------------------------------------ replies (ok)

record! {
    /// The nonce of the sync.
    SyncOk {
        /// The nonce.
        nonce: u64,
    }
}

record! {
    /// The process was started.
    SpawnOk {}
}

record! {
    /// The signal was sent.
    SignalOk {
        /// How many processes were signalled.
        procs: u32,
        /// With wait_stopped: whether every thread stopped in time.
        stopped: Option<bool>,
    }
}

record! {
    /// The command was started; a `done` event follows.
    RunOk {}
}

record! {
    /// The probe was started; a `probe` event follows.
    ProbeOk {}
}

record! {
    /// The file was written.
    WriteOk {
        /// Bytes written.
        bytes: u64,
    }
}

record! {
    /// The bytes read.
    ReadOk {
        /// The bytes, as text or hex.
        data: String,
        /// The file's size.
        size: u64,
        /// Whether the read reached the end of the file.
        eof: bool,
    }
}

record! {
    /// The directory exists.
    MkdirOk {}
}

record! {
    /// The file system is mounted.
    MountOk {}
}

record! {
    /// sync(2) returned.
    SyncfsOk {}
}

record! {
    /// The clock was set.
    ClockSetOk {
        /// The realtime clock after the change, seconds since 1970.
        sec: i64,
    }
}

record! {
    /// The clocks.
    ClockGetOk {
        /// Realtime, seconds since 1970.
        sec: i64,
        /// Realtime, nanoseconds.
        nsec: u32,
        /// CLOCK_MONOTONIC in ms (guest time since boot).
        mono_ms: u64,
    }
}

record! {
    /// The queue length.
    QlenOk {
        /// Packets queued; absent when there is no qdisc with that handle.
        packets: Option<u64>,
    }
}

record! {
    /// The tail is on.
    TailOk {}
}

record! {
    /// The tail is off.
    UntailOk {
        /// Lines it reported.
        lines: u64,
    }
}

record! {
    /// The queue is bound; packets are counted from now.
    FreezeOk {}
}

record! {
    /// The trigger is off.
    UnfreezeOk {
        /// Whether it had fired.
        fired: bool,
        /// Whether the tree had been continued.
        resumed: bool,
        /// Packets counted (in the counted direction, with the pattern).
        counted: u64,
    }
}

// ------------------------------------------------------------------ events

record! {
    /// The agent's first line. The host sends only ops and optional fields it lists.
    Hello {
        /// The protocol version (the highest this agent speaks).
        proto: u64,
        /// The agent's version.
        agent: String,
        /// Every op it executes.
        ops: Vec<String>,
        /// Optional capabilities (see FEATURES).
        features: Vec<String>,
        /// Whether it runs as the guest's first process.
        pid1: bool,
    }
}

record! {
    /// A process started by spawn exited.
    Exit {
        /// Its handle.
        h: String,
        /// The id of the spawn command.
        id: u64,
        /// The exit status as a shell reports it: the code, or 128 + the signal number.
        status: u32,
        /// The signal that ended it, if one did.
        sig: Option<u32>,
        /// Whether kill_after_ms ended it.
        timed_out: Option<bool>,
    }
}

record! {
    /// A run command ended.
    Done {
        /// The id of the run command.
        id: u64,
        /// The exit status (code, or 128 + signal).
        status: u32,
        /// The signal that ended it, if one did.
        sig: Option<u32>,
        /// Whether kill_after_ms ended it.
        timed_out: Option<bool>,
        /// The output kept (invalid UTF-8 replaced by U+FFFD).
        out: String,
        /// The output's whole length in bytes.
        out_len: u64,
    }
}

record! {
    /// A probe ended.
    ProbeDone {
        /// The id of the probe command.
        id: u64,
        /// Whether an attempt succeeded.
        ok: bool,
        /// How it ended.
        why: ProbeEnd,
        /// Attempts started.
        tries: u64,
        /// The last attempt's exit status, if one ended.
        status: Option<u32>,
    }
}

record! {
    /// A new line of a tailed file.
    Line {
        /// The tail's name.
        name: String,
        /// The line's number in the file, from 1 (counted from where the tail started).
        n: u64,
        /// The line without its newline (invalid UTF-8 replaced by U+FFFD; at most 2,000 bytes).
        text: String,
        /// Whether the line was longer and was cut.
        cut: Option<bool>,
    }
}

record! {
    /// The packet trigger fired: the process tree is stopped and the packet let through.
    Froze {
        /// The id of the freeze command.
        id: u64,
        /// The frozen process's handle.
        h: String,
        /// Processes stopped.
        procs: u32,
        /// Whether every thread stopped in time.
        all_stopped: bool,
        /// The counted direction.
        dir: Dir,
        /// The packet's number among the counted ones (= n).
        count: u64,
        /// The packet's own direction.
        kdir: Dir,
        /// Its number among the payload packets of its direction.
        k: u64,
        /// Source port.
        sport: u32,
        /// Destination port.
        dport: u32,
        /// Payload length.
        len: u32,
        /// The first 60 payload bytes, printable ASCII kept, others as '.'.
        snippet: String,
    }
}

record! {
    /// The frozen tree was continued after dur_ms.
    Resumed {
        /// The id of the freeze command.
        id: u64,
        /// Processes continued.
        procs: u32,
    }
}

record! {
    /// A CAN byte discarded a partly read input line.
    Dropped {
        /// The bytes discarded.
        bytes: u64,
    }
}

record! {
    /// Something went wrong outside a command.
    Warn {
        /// What.
        code: WarnCode,
        /// In words.
        msg: String,
    }
}

/// Defines a closed set of message kinds, each one record type, tagged by a name.
macro_rules! union {
    ($(#[doc = $ed:literal])* $name:ident { $( $(#[doc = $d:literal])* $v:ident = $s:literal ($rec:ident) ),* $(,)? }) => {
        $(#[doc = $ed])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $name { $( $(#[doc = $d])* $v($rec), )* }

        impl $name {
            /// Every name, in declaration order.
            pub const NAMES: &'static [&'static str] = &[$($s),*];

            /// This one's name (the `op` or `ev` member).
            pub fn name(&self) -> &'static str {
                match self { $( $name::$v(_) => $s, )* }
            }

            pub(crate) fn put(&self, m: &mut $crate::json::Obj) {
                match self { $( $name::$v(r) => r.put_fields(m), )* }
            }

            /// `None`: no kind of this name.
            pub(crate) fn take(name: &str, m: &mut $crate::json::Obj)
                -> Option<Result<Self, $crate::field::FieldError>> {
                match name { $( $s => Some($rec::take_fields(m).map($name::$v)), )* _ => None }
            }

            /// (name, description, properties, required) of every kind.
            pub(crate) fn schema_parts() -> Vec<(&'static str, String, $crate::json::Obj, Vec<$crate::json::Value>)> {
                let mut out = Vec::new();
                $( {
                    let mut p = $crate::json::Obj::new();
                    let mut r = Vec::new();
                    $rec::schema_fields(&mut p, &mut r);
                    out.push(($s, $rec::doc(), p, r));
                } )*
                out
            }
        }
    };
}

union! {
    /// A command from the host.
    Command {
        Sync = "sync" (Sync),
        Spawn = "spawn" (Spawn),
        Signal = "signal" (Signal),
        Run = "run" (Run),
        Probe = "probe" (Probe),
        Write = "write" (Write),
        Read = "read" (Read),
        Mkdir = "mkdir" (Mkdir),
        Mount = "mount" (Mount),
        Syncfs = "syncfs" (Syncfs),
        ClockSet = "clock_set" (ClockSet),
        ClockGet = "clock_get" (ClockGet),
        Qlen = "qlen" (Qlen),
        Tail = "tail" (Tail),
        Untail = "untail" (Untail),
        Freeze = "freeze" (Freeze),
        Unfreeze = "unfreeze" (Unfreeze),
    }
}

union! {
    /// The result of a command the agent carried out (the `op` member names the command).
    Reply {
        Sync = "sync" (SyncOk),
        Spawn = "spawn" (SpawnOk),
        Signal = "signal" (SignalOk),
        Run = "run" (RunOk),
        Probe = "probe" (ProbeOk),
        Write = "write" (WriteOk),
        Read = "read" (ReadOk),
        Mkdir = "mkdir" (MkdirOk),
        Mount = "mount" (MountOk),
        Syncfs = "syncfs" (SyncfsOk),
        ClockSet = "clock_set" (ClockSetOk),
        ClockGet = "clock_get" (ClockGetOk),
        Qlen = "qlen" (QlenOk),
        Tail = "tail" (TailOk),
        Untail = "untail" (UntailOk),
        Freeze = "freeze" (FreezeOk),
        Unfreeze = "unfreeze" (UnfreezeOk),
    }
}

union! {
    /// An event: something the agent reports on its own.
    Event {
        Hello = "hello" (Hello),
        Exit = "exit" (Exit),
        Done = "done" (Done),
        Probe = "probe" (ProbeDone),
        Line = "line" (Line),
        Froze = "froze" (Froze),
        Resumed = "resumed" (Resumed),
        Dropped = "dropped" (Dropped),
        Warn = "warn" (Warn),
    }
}

impl Command {
    /// The optional features (from the hello) the command needs, every one of them.
    pub fn features(&self) -> &'static [&'static str] {
        match self {
            Command::Freeze(f) if f.netns.is_some() => &["freeze", "freeze-netns"],
            Command::Freeze(_) | Command::Unfreeze(_) => &["freeze"],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_and_commands_pair_up() {
        assert_eq!(Command::NAMES, Reply::NAMES);
        let mut seen = std::collections::BTreeSet::new();
        for n in Command::NAMES.iter().chain(Event::NAMES) {
            assert!(
                n.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{n}"
            );
            seen.insert(*n);
        }
    }
}
