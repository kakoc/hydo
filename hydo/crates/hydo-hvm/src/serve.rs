//! hv/hvm/explore/serve.c (`hvmx`) in Rust: the explorer's run engine on the Mac. It boots a
//! manifest-built arm64 image to its snapshot points S0 ("root mounted") and S1 ("provisioned"),
//! or, for an agent image, under lock-step control from its first instruction; serves `RUN`,
//! `CTL`, `RESUME` and `QUIT`; runs `one` schedule from a fresh boot; and speaks hvx's lock-step
//! protocol (M119, M121, M126, M130). The C file's header has the whole protocol; every function
//! here keeps its C name, and every output line its C text.
//!
//! [`main_with`] is the program's `main` over any streams and any environment, so hydo-engine
//! can run it on a thread of its own process; [`Engine`] is the typed core the protocol drives.

use crate::fnv::{fnv, fnv_u64, mix64, OFFSET};
use crate::layout::*;
use crate::machine::{Mach, Pct, Tjit};
use crate::mem::Mem;
use crate::virtio::{self, F_DROP, F_ERROR, F_UNFLUSHED};
use crate::vmm::{now_s, VSnap, VmOpts, Vmm, NSR, SR};
use crate::{catch_fatal, fatal, say, Diag};
use hydo_hvx::cstr::{self, find, Scan};
use std::io::{BufRead, Read, Write};
use std::sync::{Arc, Mutex};

const S0_MARKER: &[u8] = b"hydo-explore-ready-S0";
const S1_MARKER: &[u8] = b"hydo-explore-ready-S1";
const DONE_MARKER: &[u8] = b"hydo-explore-workload-done";
const FINAL_MARKER: &[u8] = b"hydo-stack-marker: workload complete";
const ACKED_PREFIX: &[u8] = b"hydo-explore-acked:";
const FAILED_PREFIX: &[u8] = b"hydo-explore-failed:";
/// hydo-agent's framed lines: an agent image booted without --control boot
const AGENT_PREFIX: &[u8] = b"HYDO-AG ";

const CTL_MAX_SENDS: usize = 1024;
const CTL_MAX_SNAPS: usize = 8;
/// the UART receive queue
const CTL_SEND_MAX: usize = 4096;
/// the cadence without rb=: hvx's run calls return every 2^16 retired exits
const CTL_RB_DEFAULT: u64 = 65536;
const RAM_MIN_MIB: u64 = 256;
const RAM_MAX_MIB: u64 = 4096;

/// Where the engine's lines go and where the host's come from.
pub struct Io {
    /// stdout: the protocol (each line flushed)
    pub out: Box<dyn Write + Send>,
    /// stdin: requests, and the host's lines in a controlled run
    pub input: Box<dyn BufRead + Send>,
    /// stderr: diagnostics
    pub diag: Box<dyn Write + Send>,
    /// where HVM_ECHO copies the console (the program: stdout)
    pub echo: Option<Box<dyn Write + Send>>,
}

/// The environment the engine reads (getenv in the C engine), as a lookup.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A run from the workload's start on: what run_rest needs, and what a SNAP keeps (`struct runst`).
#[derive(Clone, Default, Debug)]
struct RunSt {
    tag: String,
    t0: f64,
    from: usize,
    cut: bool,
    rewound: bool,
    pct_n: u32,
    wx0: u64,
    tjinfo: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cs {
    Pending,
    Typed,
    Acked,
    Retype,
}

#[derive(Clone, Debug)]
struct CtlSend {
    id: u64,
    at: u64,
    sent: u64,
    state: Cs,
    buf: Vec<u8>,
}

/// What a snapshot carries besides the VM (`struct ctl_book`).
#[derive(Clone, Default, Debug)]
struct Book {
    nstop: u64,
    nev: u64,
    next_id: u64,
    ntyped: u64,
    inh: u64,
    /// the exit count the cadence counts from
    cad0: u64,
    /// console offset up to which lines were reported as EVENTs
    evfrom: usize,
    s: Vec<CtlSend>,
}

/// A snapshot (`struct xsnap`): RAM in an unlinked file, mapped.
#[derive(Default)]
struct XSnap {
    v: VSnap,
    m: Mach,
    ram: Option<Mem>,
    vio: Option<virtio::Save>,
    con: Vec<u8>,
}

#[derive(Default)]
struct CtlSnap {
    name: String,
    x: XSnap,
    rs: RunSt,
    k: Book,
    rb: u64,
    used: bool,
    rw: bool,
}

/// The lock-step state (`C`).
#[derive(Default)]
struct Ctl {
    on: bool,
    /// 1 = the phase's start stop is due, 2 = a RESUME's
    start: i32,
    ended: i32,
    /// --control boot one: SNAP S1 taken in the boot phase (M121)
    boot_s1: bool,
    /// M126: DISKOFF/CUT given (1), then acted on (2)
    wdone: i32,
    cut: bool,
    tag: String,
    phase: &'static str,
    at: u64,
    rb: u64,
    last_x: u64,
    k: Book,
    endv: String,
    rs: RunSt,
}

/// The stop at the first exit after any of several markers.
#[derive(Default)]
struct Hk {
    m: Vec<&'static [u8]>,
    hit: i32,
    from: usize,
    scan: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum S0Ref {
    Main,
    Slot(usize),
}

/// Settings read once from the environment.
#[derive(Clone, Debug)]
pub struct Opts {
    pub vm: VmOpts,
    pub ram_mib: u64,
    pub run_exits: u64,
    pub spin_auto: bool,
    pub snap_dir: String,
    pub diag: bool,
    pub diag_chunks: Option<String>,
    pub check_restore: bool,
    pub no_mlock: bool,
}

/// Everything but the VM (so the run loop's stop hook can borrow it while the VM runs).
struct Host {
    out: Box<dyn Write + Send>,
    input: Box<dyn BufRead + Send>,
    diag: Diag,
    outdir: String,
    o: Opts,
    c: Ctl,
    hk: Hk,
    snaps: Vec<CtlSnap>,
    g_ctl_one: bool,
    g_agent: bool,
    s0: XSnap,
    s1: XSnap,
    g_s0: S0Ref,
    spin_hit: bool,
    initrd_has_tc: bool,
    g_boot_rs: RunSt,
}

/// The engine: one VM and the explorer's state around it.
pub struct Engine {
    vm: Vmm,
    h: Host,
}

/// C's `snprintf(buf, cap, ...)`: at most `cap - 1` bytes.
fn ccap(mut b: Vec<u8>, cap: usize) -> Vec<u8> {
    b.truncate(cap - 1);
    b
}
fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
fn has(hay: &[u8], n: &[u8]) -> bool {
    find(hay, n).is_some()
}

impl Host {
    fn out_line(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
        let _ = self.out.write_all(b"\n");
        let _ = self.out.flush();
    }
    fn out_bytes(&mut self, b: &[u8]) {
        let _ = self.out.write_all(b);
        let _ = self.out.flush();
    }
    fn say(&self, s: &str) {
        say(&self.diag, s);
    }
    fn result_err(&mut self, tag: &str, v: &str) {
        self.out_line(&format!("RESULT {tag} fp=0 wfp=0 secs=0 verdict={v}"));
    }

    /// `marker_hook`: one of the markers appeared since `from`.
    fn marker_hook(&mut self, vm: &Vmm) -> bool {
        let len = vm.m.console_len as usize;
        for (i, m) in self.hk.m.iter().enumerate() {
            let ml = m.len();
            let mut st = self.hk.scan.saturating_sub(ml);
            if st < self.hk.from {
                st = self.hk.from;
            }
            if len >= st + ml && has(&vm.console[st..len], m) {
                self.hk.hit = i as i32;
                return true;
            }
        }
        self.hk.scan = len;
        false
    }

    fn snap_find(&self, name: &str) -> Option<usize> {
        self.snaps.iter().position(|s| s.used && s.name == name)
    }

    // ---------------- lock-step control ----------------
    fn ctl_begin(&mut self, tag: &str) {
        self.c = Ctl {
            on: true,
            start: 1,
            tag: lossy(&ccap(tag.as_bytes().to_vec(), 64)),
            phase: "workload",
            rb: CTL_RB_DEFAULT,
            ..Ctl::default()
        };
    }
    fn ctl_finish(&mut self) {
        self.c.k = Book::default();
        self.c.on = false;
    }
    fn ctl_verdict(&self, vm: &Vmm, r: i32, from: usize) -> String {
        if r == -3 && !self.c.endv.is_empty() {
            self.c.endv.clone()
        } else {
            last_verdict(vm, from)
        }
    }
    /// type one send now (at the current stop)
    fn ctl_type(&mut self, vm: &mut Vmm, i: usize, print: bool) {
        let retype = self.c.k.s[i].state == Cs::Retype;
        let room = vm.rx_room();
        let len = self.c.k.s[i].buf.len() as u32;
        let id = self.c.k.s[i].id;
        if len > room {
            self.say(&format!(
                "hvmx: {}: send {id}: {} of {len} bytes do not fit in the receive queue",
                self.c.tag,
                len - room
            ));
        }
        for j in 0..(len.min(room) as usize) {
            vm.rx_push(self.c.k.s[i].buf[j]);
        }
        self.c.k.s[i].state = Cs::Typed;
        if !retype {
            let at = self.c.at;
            self.c.k.s[i].sent = at;
            self.c.k.ntyped += 1;
            let base = if self.c.k.inh != 0 { self.c.k.inh } else { OFFSET };
            let mut h = fnv_u64(at, base);
            h = fnv(&len.to_le_bytes(), h);
            h = fnv(&self.c.k.s[i].buf, h);
            self.c.k.inh = h;
        }
        if print {
            let s = format!(
                "SENT {} id={id} at={}{}",
                self.c.tag,
                self.c.at,
                if retype { " retype=1" } else { "" }
            );
            self.out_line(&s);
        }
    }
    /// every send due at this stop: retypes first (id order), then (at, id) order
    fn ctl_deliver(&mut self, vm: &mut Vmm) {
        for i in 0..self.c.k.s.len() {
            if self.c.k.s[i].state == Cs::Retype {
                self.ctl_type(vm, i, true);
            }
        }
        loop {
            let mut best: Option<usize> = None;
            for i in 0..self.c.k.s.len() {
                let s = &self.c.k.s[i];
                if s.state != Cs::Pending || s.at > self.c.at {
                    continue;
                }
                match best {
                    None => best = Some(i),
                    Some(b) => {
                        let bs = &self.c.k.s[b];
                        if s.at < bs.at || (s.at == bs.at && s.id < bs.id) {
                            best = Some(i);
                        }
                    }
                }
            }
            let Some(b) = best else { return };
            self.ctl_type(vm, b, true);
        }
    }
    fn ctl_due(&self) -> bool {
        self.c
            .k
            .s
            .iter()
            .any(|s| s.state == Cs::Retype || (s.state == Cs::Pending && s.at <= self.c.at))
    }
    /// a phase change to a new VM: queued input belongs to the old one
    fn ctl_drop_pending(&mut self) {
        for i in 0..self.c.k.s.len() {
            if matches!(self.c.k.s[i].state, Cs::Pending | Cs::Retype) {
                self.c.k.s[i].state = Cs::Acked;
                let s = format!("DROP {} id={}", self.c.tag, self.c.k.s[i].id);
                self.out_line(&s);
            }
        }
    }
    fn ctl_snap_cmd(&mut self, vm: &mut Vmm, arg: &[u8]) {
        let mut sc = Scan::new(arg);
        let name_b = sc.word(63).map(|w| w.to_vec());
        let mode_b = if name_b.is_some() {
            sc.word(15).map(|w| w.to_vec())
        } else {
            None
        };
        let n = sc.n;
        let name = lossy(name_b.as_deref().unwrap_or(b""));
        if n < 1 || (n == 2 && mode_b.as_deref() != Some(b"retype")) {
            self.out_line("ERR snap bad request");
            return;
        }
        if self.c.phase != "workload" && self.c.phase != "boot" {
            self.out_line(&format!(
                "ERR snap {name} only in the workload or boot phase"
            ));
            return;
        }
        if vm.tj.on || self.c.rs.pct_n != 0 {
            self.out_line(&format!("ERR snap {name} not for tj/pct schedules"));
            return;
        }
        if self.c.wdone != 0 {
            self.out_line(&format!("ERR snap {name} after DISKOFF/CUT"));
            return;
        }
        let typed: Vec<String> = self
            .c
            .k
            .s
            .iter()
            .filter(|s| s.state == Cs::Typed)
            .map(|s| s.id.to_string())
            .collect();
        let mut ids = typed.join(",");
        let il = ids.len();
        if il >= 512 {
            ids.truncate(507);
            ids.push_str(",...");
        }
        if il != 0 && n < 2 {
            self.out_line(&format!("ERR snap {name} unacked={ids}"));
            return;
        }
        let slot = self
            .snap_find(&name)
            .or_else(|| self.snaps.iter().position(|s| !s.used));
        let Some(slot) = slot else {
            self.out_line(&format!(
                "ERR snap {name} no free slot ({CTL_MAX_SNAPS} names)"
            ));
            return;
        };
        {
            let cs = &mut self.snaps[slot];
            if cs.used {
                // replaced: keep the RAM mapping, drop the rest
                cs.x.con.clear();
                cs.x.vio = None;
                cs.k = Book::default();
            }
        }
        let snap_dir = self.o.snap_dir.clone();
        xsnap_take(vm, &mut self.snaps[slot].x, "ctl", &snap_dir);
        let mut k = self.c.k.clone();
        for s in &mut k.s {
            if s.state == Cs::Typed {
                s.state = Cs::Retype;
            }
        }
        let cs = &mut self.snaps[slot];
        cs.k = k;
        cs.rs = self.c.rs.clone();
        cs.rb = self.c.rb;
        cs.used = true;
        cs.rw = vm.sp.rw_on;
        cs.name = lossy(&ccap(name.as_bytes().to_vec(), 64));
        let at = self.c.at;
        self.out_line(&format!(
            "OK snap {name} at={at} retype={}",
            if il != 0 { ids.as_str() } else { "-" }
        ));
        if self.g_agent && self.g_ctl_one && self.c.phase == "boot" && name == "S1" {
            self.c.boot_s1 = true; // M121: the workload starts here once the host says CONT
        }
    }

    /// one stop: report, then serve host lines until CONT (false) or END/EOF (true)
    fn ctl_stop(&mut self, vm: &mut Vmm, why: &str) -> bool {
        let mut p = self.c.k.evfrom;
        let len = vm.m.console_len as usize;
        for q in p..len {
            if vm.console[q] != b'\n' {
                continue;
            }
            let mut e = q;
            if e > p && vm.console[e - 1] == b'\r' {
                e -= 1;
            }
            self.c.k.nev += 1;
            let mut line = format!("EVENT {} at={} n={} ", self.c.tag, self.c.at, self.c.k.nev).into_bytes();
            ctl_esc(&mut line, &vm.console[p..e]);
            line.push(b'\n');
            self.out_bytes(&line);
            p = q + 1;
        }
        self.c.k.evfrom = p;
        self.ctl_deliver(vm);
        self.c.k.nstop += 1;
        let s = format!(
            "STOP {} at={} n={} phase={} why={why}",
            self.c.tag, self.c.at, self.c.k.nstop, self.c.phase
        );
        self.out_line(&s);
        loop {
            let mut ln = Vec::new();
            match self.input.read_until(b'\n', &mut ln) {
                Ok(0) | Err(_) => {
                    self.c.endv = "ERROR control channel closed".into();
                    return true;
                }
                Ok(_) => {}
            }
            while matches!(ln.last(), Some(b'\n' | b'\r')) {
                ln.pop();
            }
            if let Some(z) = ln.iter().position(|&c| c == 0) {
                ln.truncate(z); // a C string ends at its first NUL
            }
            if ln == b"CONT" {
                return false;
            }
            if ln.starts_with(b"END") && (ln.len() == 3 || ln[3] == b' ') {
                let v = if ln.len() > 3 { &ln[4..] } else { &b""[..] };
                self.c.endv = lossy(&ccap(v.to_vec(), 256));
                return true;
            }
            if let Some(rest) = ln.strip_prefix(b"SEND at=") {
                let (at, used) = cstr::strtoull(rest, 10);
                let body = rest.get(used + 1..).map(ctl_unesc);
                let ok = used != 0 && rest.get(used) == Some(&b' ');
                let Some(Some(bytes)) = body.filter(|_| ok) else {
                    self.out_line("ERR send bad request");
                    continue;
                };
                let len = bytes.len();
                if len == 0 || len > CTL_SEND_MAX {
                    self.out_line(&format!("ERR send length {len} not in [1, {CTL_SEND_MAX}]"));
                    continue;
                }
                if self.c.k.s.len() >= CTL_MAX_SENDS {
                    self.out_line(&format!("ERR send more than {CTL_MAX_SENDS} sends in one run"));
                    continue;
                }
                self.c.k.next_id += 1;
                let id = self.c.k.next_id;
                self.c.k.s.push(CtlSend {
                    id,
                    at,
                    sent: 0,
                    state: Cs::Pending,
                    buf: bytes,
                });
                if at <= self.c.at {
                    let i = self.c.k.s.len() - 1;
                    self.ctl_type(vm, i, false);
                    let s = format!("OK send id={id} sent={}", self.c.at);
                    self.out_line(&s);
                } else {
                    self.out_line(&format!("OK send id={id} queued={at}"));
                }
            } else if let Some(rest) = ln.strip_prefix(b"ACK ") {
                let id = cstr::strtoull(rest, 10).0;
                let mut msg = format!("ERR ack {id} unknown");
                if let Some(s) = self.c.k.s.iter_mut().find(|s| s.id == id) {
                    if s.state == Cs::Pending {
                        msg = format!("ERR ack {id} not typed yet");
                    } else {
                        s.state = Cs::Acked;
                        msg = format!("OK ack {id}");
                    }
                }
                self.out_line(&msg);
            } else if let Some(rest) = ln.strip_prefix(b"SNAP ") {
                let rest = rest.to_vec();
                self.ctl_snap_cmd(vm, &rest);
            } else if ln == b"DISKOFF" || ln == b"CUT" {
                let cut = ln[0] == b'C';
                let no = if !self.g_agent {
                    Some("only for an agent image (--control boot)")
                } else if self.c.phase != "workload" {
                    Some("only in the workload phase")
                } else if self.c.wdone != 0 {
                    Some("after DISKOFF/CUT")
                } else if cut && self.snap_find("S0").is_none() {
                    Some("without a snapshot S0")
                } else {
                    None
                };
                if let Some(no) = no {
                    self.out_line(&format!("ERR {} {no}", if cut { "cut" } else { "diskoff" }));
                    continue;
                }
                self.c.wdone = 1;
                self.c.cut = cut;
                if cut {
                    let s = format!("OK cut at={}", self.c.at);
                    self.out_line(&s);
                    return false;
                }
                let s = format!("OK diskoff at={} cut=0", self.c.at);
                self.out_line(&s);
            } else {
                let mut b = b"ERR unknown command ".to_vec();
                ctl_esc(&mut b, &ln);
                b.push(b'\n');
                self.out_bytes(&b);
            }
        }
    }

    /// hvm_stop_hook at every loop top of a lock-step phase
    fn ctl_hook(&mut self, vm: &mut Vmm) -> bool {
        let x = vm.m.exits;
        if x == self.c.last_x {
            return false; // a kick or an observer exit: not a new exit
        }
        self.c.last_x = x;
        let hit = self.marker_hook(vm);
        self.c.at = x;
        let why = if hit {
            Some("marker")
        } else if self.ctl_due() {
            Some("send")
        } else if (x.wrapping_sub(self.c.k.cad0)) % self.c.rb == 0 {
            Some("cadence")
        } else {
            None
        };
        if let Some(w) = why {
            if self.ctl_stop(vm, w) {
                self.c.ended = 1;
                return true;
            }
        }
        if self.c.boot_s1 {
            self.c.ended = 2;
            return true;
        }
        if self.c.wdone == 1 {
            self.c.wdone = 2;
            self.c.ended = 3;
            return true;
        }
        hit
    }
}

/// `ctl_esc`: \\ \n \r \t \xHH.
fn ctl_esc(o: &mut Vec<u8>, p: &[u8]) {
    for &c in p {
        match c {
            b'\\' => o.extend_from_slice(b"\\\\"),
            b'\n' => o.extend_from_slice(b"\\n"),
            b'\r' => o.extend_from_slice(b"\\r"),
            b'\t' => o.extend_from_slice(b"\\t"),
            c if !(0x20..=0x7e).contains(&c) => o.extend_from_slice(format!("\\x{c:02x}").as_bytes()),
            c => o.push(c),
        }
    }
}
/// the inverse of ctl_esc; None on a malformed escape
fn ctl_unesc(s: &[u8]) -> Option<Vec<u8>> {
    let mut o = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'\\' {
            o.push(s[i]);
            i += 1;
            continue;
        }
        i += 1;
        match s.get(i) {
            Some(b'\\') => o.push(b'\\'),
            Some(b'n') => o.push(b'\n'),
            Some(b'r') => o.push(b'\r'),
            Some(b't') => o.push(b'\t'),
            Some(b'x')
                if s.get(i + 1).is_some_and(|c| c.is_ascii_hexdigit())
                    && s.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit()) =>
            {
                o.push(u8::from_str_radix(std::str::from_utf8(&s[i + 1..i + 3]).unwrap(), 16).unwrap());
                i += 2;
            }
            _ => return None,
        }
        i += 1;
    }
    Some(o)
}

fn last_verdict(vm: &Vmm, from: usize) -> String {
    let len = vm.m.console_len as usize;
    let c = &vm.console[..len];
    let mut at: Option<usize> = None;
    let mut i = from;
    while i + 12 <= len {
        if &c[i..i + 12] == b"HYDO-ORACLE:" {
            at = Some(i);
        }
        i += 1;
    }
    let Some(at) = at else {
        return "ERROR no verdict".into();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while at + i < len && c[at + i] != b'\n' && c[at + i] != b'\r' && i + 1 < 300 {
        out.push(c[at + i]);
        i += 1;
    }
    lossy(&out)
}

fn stop_name(s: i32) -> &'static str {
    match s {
        STOP_HALT => "guest halted",
        STOP_IDLE => "guest idle with no timer",
        STOP_LIMIT => "exit budget exhausted",
        STOP_SPIN => "SPIN (guest loops without exits; run invalid)",
        _ => "?",
    }
}

/// the host part's disk policy, as hvx's parse_sched; None if it is not one
fn parse_disk(host: &[u8]) -> Option<(u64, u32, u32)> {
    let mut d: Vec<u8> = b"none".to_vec();
    if let Some(p) = find(host, b"disk=") {
        if let Some(w) = Scan::new(&host[p + 5..]).word(63) {
            d = w.to_vec();
        }
    }
    let seed = find(host, b"dseed=")
        .map(|p| cstr::strtoull(&host[p + 6..], 16).0)
        .unwrap_or(0);
    if d == b"none" {
        return Some((seed, 0, 0));
    }
    if let Some(r) = d.strip_prefix(b"ioerr:") {
        return Some((seed, F_ERROR, cstr::atoi(r) as u32));
    }
    if d == b"cut" {
        return Some((seed, F_UNFLUSHED, 100));
    }
    if let Some(r) = d.strip_prefix(b"lying:") {
        return Some((seed, F_DROP, cstr::atoi(r) as u32));
    }
    None
}

/// the host part's tick jitter (left off: run_once turns it on); None if malformed
fn parse_tj(host: &[u8]) -> Option<Tjit> {
    let mut t = Tjit::default();
    if let Some(p) = find(host, b"tjx=") {
        let mut p = p + 4;
        while p < host.len() && host[p] != b' ' && t.n_explicit < TJ_MAX_EXPLICIT {
            let (k, u) = cstr::strtoull(&host[p..], 10);
            let e = p + u;
            if host.get(e) != Some(&b':') {
                return None;
            }
            let (d, u2) = cstr::strtoull(&host[e + 1..], 10);
            let e2 = e + 1 + u2;
            t.ek[t.n_explicit] = k;
            t.ed[t.n_explicit] = d as u32;
            t.n_explicit += 1;
            p = if host.get(e2) == Some(&b',') { e2 + 1 } else { e2 };
        }
        if p < host.len() && host[p] != b' ' {
            return None;
        }
        return Some(t);
    }
    let Some(p) = find(host, b"tj=") else {
        return Some(t);
    };
    t.seed = cstr::strtoull(&host[p + 3..], 16).0;
    let num = |key: &[u8], dflt: u64| {
        find(host, key)
            .map(|p| cstr::strtoull(&host[p + key.len()..], 10).0)
            .unwrap_or(dflt)
    };
    t.permille = num(b"tjpm=", 10) as u32;
    t.max_delay = num(b"tjmax=", 1000) as u32;
    t.lo = num(b"tjlo=", 0);
    t.hi = num(b"tjhi=", 0);
    if t.permille == 0 || t.max_delay == 0 {
        t = Tjit::default();
    }
    Some(t)
}
fn tj_wanted(t: &Tjit) -> bool {
    t.n_explicit != 0 || (t.permille != 0 && t.max_delay != 0)
}

/// Does the (gzip'd cpio) initramfs carry a tc? Its compiled manifest says HYDO_TC='<path>' or HYDO_TC=''.
pub fn scan_has_tc(gz: &[u8]) -> bool {
    const KEY: &[u8] = b"\nHYDO_TC='/";
    const NONE: &[u8] = b"\nHYDO_TC=''";
    let mut z = flate2::read::GzDecoder::new(gz);
    let mut buf = vec![0u8; 1 << 16];
    let mut win: Vec<u8> = Vec::with_capacity(buf.len() + 16);
    loop {
        let n = match z.read(&mut buf) {
            Ok(n) => n,
            Err(_) => 0,
        };
        win.extend_from_slice(&buf[..n]);
        if has(&win, KEY) {
            return true;
        }
        if has(&win, NONE) || n == 0 {
            return false;
        }
        let keep = win.len().min(15);
        let cut = win.len() - keep;
        win.drain(..cut);
    }
}

/// The host tokens this engine applies; the others are refused: Some(why, unknown to every engine).
fn host_token_problem(host: &[u8]) -> Option<(String, bool)> {
    const OK: &[&str] = &[
        "disk=", "dseed=", "tj=", "tjpm=", "tjmax=", "tjlo=", "tjhi=", "tjx=", "pct=", "pctn=", "pctk=",
    ];
    const HVX_ONLY: &[&str] = &[
        "rb=", "pp=", "pptsc=", "xt=", "xtr=", "xtpm=", "xtlo=", "xthi=", "xtsc=", "xtx=", "cf=", "q=",
        "qs=", "qb=", "qbs=", "qm=",
    ];
    let words = ccap(host.to_vec(), 1024);
    for w in words
        .split(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        .filter(|w| !w.is_empty())
    {
        let good = w == b"spin=rewind" || OK.iter().any(|p| w.starts_with(p.as_bytes()));
        if good {
            continue;
        }
        let hvx = w == b"cfn" || HVX_ONLY.iter().any(|p| w.starts_with(p.as_bytes()));
        let t = lossy(&w[..w.len().min(60)]);
        let msg = if hvx {
            format!("host token {t} needs hvx (the PMU clock, vCPU turns or a power cut at a disk FLUSH)")
        } else {
            format!("unknown host token {t}")
        };
        return Some((lossy(&ccap(msg.into_bytes(), 200)), !hvx));
    }
    None
}

fn append_line(vm: &Vmm, from: usize, prefix: &[u8], out: &mut Vec<u8>, n: usize) {
    let len = vm.m.console_len as usize;
    if len <= from {
        return;
    }
    let c = &vm.console[from..len];
    if let Some(at) = find(c, prefix) {
        for &b in &c[at + prefix.len()..] {
            if b == b'\n' || b == b'\r' || out.len() + 2 >= n {
                break;
            }
            out.push(b);
        }
    }
}
/// the recovery boot's command, as hvx's: "oracle <acked> [failed <ids>]\n" (3584 bytes at most)
fn oracle_cmd(vm: &Vmm, from: usize) -> Vec<u8> {
    const N: usize = 3584;
    let mut out = b"oracle".to_vec();
    append_line(vm, from, ACKED_PREFIX, &mut out, N);
    let len = vm.m.console_len as usize;
    if len > from && has(&vm.console[from..len], FAILED_PREFIX) && out.len() + 8 < N {
        out.extend_from_slice(b" failed");
        append_line(vm, from, FAILED_PREFIX, &mut out, N);
    }
    out.push(b'\n');
    out
}

/// `xsnap_take`
fn xsnap_take(vm: &Vmm, s: &mut XSnap, name: &str, snap_dir: &str) {
    if s.ram.is_none() {
        match Mem::file_backed(snap_dir, name, vm.ram_size as usize) {
            Ok(m) => s.ram = Some(m),
            Err(e) => fatal(1, &format!("{e}")),
        }
    }
    s.v = vm.vtake();
    s.m = vm.m;
    s.ram
        .as_mut()
        .unwrap()
        .bytes_mut()
        .copy_from_slice(vm.ram.bytes());
    if vm.vio.attached() {
        s.vio = Some(vm.vio.save());
    }
    s.con = vm.console[..vm.m.console_len as usize].to_vec();
}

/// `xsnap_restore`: a fresh VM, not just a fresh vCPU.
fn xsnap_restore(vm: &mut Vmm, s: &XSnap, check: bool) {
    vm.fresh_vm();
    vm.spin_reset();
    vm.ram
        .bytes_mut()
        .copy_from_slice(s.ram.as_ref().expect("a snapshot").bytes());
    let (p, n) = (vm.ram.ptr(), vm.ram.len());
    crate::hv::icache_invalidate(p, n);
    vm.vput(&s.v);
    vm.m = s.m;
    let n = vm.m.console_len as usize;
    vm.console[..n].copy_from_slice(&s.con[..n]);
    if let Some(v) = &s.vio {
        vm.vio.restore(v);
    }
    vm.rx_head = 0;
    vm.rx_tail = 0;
    if check {
        let r = vm.vtake();
        for i in 0..31 {
            if r.x[i] != s.v.x[i] {
                vm.say(&format!("hvmx: restore: x{i} {:x} != {:x}", r.x[i], s.v.x[i]));
            }
        }
        if r.pc != s.v.pc || r.cpsr != s.v.cpsr || r.fpcr != s.v.fpcr || r.fpsr != s.v.fpsr {
            vm.say("hvmx: restore: pc/cpsr/fpcr/fpsr differ");
        }
        if r.q != s.v.q {
            vm.say("hvmx: restore: SIMD registers differ");
        }
        for i in 0..NSR {
            if r.ok[i] != s.v.ok[i] || r.sr[i] != s.v.sr[i] {
                vm.say(&format!(
                    "hvmx: restore: sysreg {i} (0x{:x}) ok {}/{} {:x} != {:x}",
                    SR[i], r.ok[i], s.v.ok[i], r.sr[i], s.v.sr[i]
                ));
            }
        }
    }
}

impl Engine {
    /// HVMX_DIAG=1: per-component hashes of the end state into <outdir>/<tag>.diag
    fn diag(&self, tag: &str, phase: &str) {
        if !self.h.o.diag {
            return;
        }
        let vm = &self.vm;
        let path = format!("{}/{tag}.diag", self.h.outdir);
        let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
            return;
        };
        let s = vm.vtake();
        let mut t = String::new();
        use std::fmt::Write as _;
        let cl = vm.m.console_len;
        let _ = writeln!(t, "{phase} console {:016x} len {cl}", fnv(&vm.console[..cl as usize], OFFSET));
        for i in 0..31 {
            let _ = writeln!(t, "{phase} x{i} {:016x}", s.x[i]);
        }
        let _ = writeln!(t, "{phase} pc {:016x} cpsr {:016x} fpcr {:x} fpsr {:x}", s.pc, s.cpsr, s.fpcr, s.fpsr);
        for i in 0..32 {
            let _ = writeln!(t, "{phase} q{i} {:016x}", fnv(&s.q[i].0, OFFSET));
        }
        for i in 0..NSR {
            let _ = writeln!(t, "{phase} sr{i} {} {:016x}", s.ok[i], s.sr[i]);
        }
        let _ = writeln!(
            t,
            "{phase} mach {:016x} exits {} vtime {}",
            fnv(&vm.m.without_stats().bytes(), OFFSET),
            vm.m.exits,
            vm.m.vtime
        );
        let ram = vm.ram.bytes();
        let mut o = 0usize;
        while (o as u64) < vm.ram_size {
            let _ = writeln!(t, "{phase} ram@{o:08x} {:016x}", mix64(&ram[o..o + (1 << 20)], OFFSET));
            o += 1 << 20;
        }
        let _ = f.write_all(t.as_bytes());
        if let Some(ch) = &self.h.o.diag_chunks {
            for c in ch.split(',').filter(|c| !c.is_empty()) {
                let o = cstr::strtoull0(c.as_bytes());
                let p = format!("{}/{tag}.{phase}.{o:x}.bin", self.h.outdir);
                let o = o as usize;
                if o + (1 << 20) <= ram.len() {
                    let _ = std::fs::write(p, &ram[o..o + (1 << 20)]);
                }
            }
        }
    }

    fn save_log(&self, tag: &str, from: usize) {
        let path = format!("{}/{tag}.log", self.h.outdir);
        let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
            return;
        };
        let len = self.vm.m.console_len as usize;
        if len > from {
            let _ = f.write_all(&self.vm.console[from..len]);
        }
    }

    /// returns the index of the marker reached, or -1 (and the stop reason in `why`)
    fn run_to(&mut self, m: &[&'static [u8]], from: usize, why: &mut i32) -> i32 {
        if self.h.c.on {
            return self.ctl_run_to(m, from, why);
        }
        self.h.hk = Hk {
            m: m.to_vec(),
            hit: -1,
            from,
            scan: from,
        };
        self.vm.max_exits = self.vm.m.exits + self.h.o.run_exits;
        let h = &mut self.h;
        let s = self.vm.run_until(None, &mut |vm| h.marker_hook(vm));
        *why = s;
        if s == STOP_MARKER {
            self.h.hk.hit
        } else {
            -1
        }
    }

    /// run_to under lock-step: the phase's start (or resume) stop, then stops at markers, due
    /// sends and the cadence; -3 when the host ended the run (END)
    fn ctl_run_to(&mut self, m: &[&'static [u8]], from: usize, why: &mut i32) -> i32 {
        self.h.hk = Hk {
            m: m.to_vec(),
            hit: -1,
            from,
            scan: from,
        };
        if self.h.c.start != 0 {
            let w = if self.h.c.start == 2 { "resume" } else { "start" };
            if self.h.c.start == 2 {
                self.h.c.k.nstop = self.h.c.k.nstop.wrapping_sub(1); // the snapshot's own stop, again
            } else {
                self.h.c.k.cad0 = self.vm.m.exits;
            }
            self.h.c.start = 0;
            self.h.c.at = self.vm.m.exits;
            if self.h.ctl_stop(&mut self.vm, w) {
                return -3;
            }
            if self.h.c.boot_s1 {
                return -4;
            }
            if self.h.c.wdone == 1 {
                self.h.c.wdone = 2;
                return -7;
            }
        }
        let len = self.vm.m.console_len as usize;
        for (i, mk) in m.iter().enumerate() {
            if len > from && has(&self.vm.console[from..len], mk) {
                return i as i32;
            }
        }
        self.h.c.last_x = self.vm.m.exits;
        self.h.c.ended = 0;
        self.vm.max_exits = self.vm.m.exits + self.h.o.run_exits;
        let h = &mut self.h;
        let s = self.vm.run_until(None, &mut |vm| h.ctl_hook(vm));
        match self.h.c.ended {
            2 => return -4,
            3 => return -7,
            0 => {}
            _ => return -3,
        }
        if s == STOP_MARKER {
            return self.h.hk.hit;
        }
        self.h.c.at = self.vm.m.exits;
        if self.h.ctl_stop(&mut self.vm, "exit") {
            return -3;
        }
        *why = s;
        -1
    }

    /// The lock-step part of a schedule: its rb=<n> taken out (the cadence; 0 = none), and
    /// whether its run is in the rewind mode. Prints the error RESULT and returns None for a bad rb=.
    fn ctl_sched(&mut self, tag: &str, s: &mut Vec<u8>) -> Option<(u64, bool)> {
        let mut bar = s.iter().position(|&c| c == b'|');
        let rw = self.vm.sp.rw_on || bar.is_some_and(|b| has(&s[..b], b"spin=rewind"));
        let mut rb = 0;
        let mut w = 0usize;
        while let Some(b) = bar {
            let Some(k) = find(&s[w..], b"rb=") else { break };
            let at = w + k;
            if at >= b {
                break;
            }
            if at != 0 && s[at - 1] != b' ' {
                w = at + 3;
                continue;
            }
            let (v, used) = cstr::strtoull(&s[at + 3..], 10);
            let e = at + 3 + used;
            if used == 0 || !matches!(s.get(e), Some(b' ' | b'|')) {
                self.h.out_line(&format!(
                    "RESULT {tag} fp=0 wfp=0 secs=0 verdict=ERROR bad schedule: rb= takes a number of exits"
                ));
                return None;
            }
            rb = v;
            s.drain(at..e);
            w = at;
            bar = s.iter().position(|&c| c == b'|');
        }
        Some((rb, rw))
    }

    /// CTL <tag> <schedule> (after the S1 restore) and `--control one`; an agent image's CTL
    /// restores the host's S1 (`cs`) here and continues its book.
    fn ctl_request(&mut self, tag: &str, sched: &[u8], cs: Option<usize>) {
        let mut s = ccap(sched.to_vec(), 4096);
        let base = self.vm.sp.rw_on;
        let Some((rb, rw)) = self.ctl_sched(tag, &mut s) else {
            return;
        };
        if let Some(i) = cs {
            let check = self.h.o.check_restore;
            xsnap_restore(&mut self.vm, &self.h.snaps[i].x, check);
        }
        self.h.ctl_begin(tag);
        if let Some(i) = cs {
            self.h.c.k = self.h.snaps[i].k.clone();
            self.h.c.start = 2; // the first stop is S1's own again
        }
        if rb != 0 {
            self.h.c.rb = rb;
        }
        self.h.spin_hit = false;
        self.vm.sp.rw_on = rw; // M130: the run's phases in the rewind mode
        self.run_once(tag, &s, rw);
        self.vm.sp.rw_on = base;
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    /// the boot under control: -3 at the host's END, -4 (--control boot one) at the stop where
    /// the host took S1 and said CONT, -1 when the guest stopped (`why`)
    fn agent_boot(&mut self, tag: &str, rb: u64, why: &mut i32) -> i32 {
        self.h.ctl_begin(tag);
        self.h.c.phase = "boot";
        if rb != 0 {
            self.h.c.rb = rb;
        }
        self.h.g_boot_rs = RunSt {
            tag: tag.to_string(),
            t0: now_s(),
            ..RunSt::default()
        };
        self.h.c.rs = self.h.g_boot_rs.clone();
        self.run_to(&[], 0, why)
    }
    fn agent_boot_result(&mut self, tag: &str, r: i32, why: i32, t0: f64) {
        let mut wfp = 0;
        let verdict = if r == -3 {
            wfp = self.vm.xfp();
            self.h.ctl_verdict(&self.vm, r, 0)
        } else {
            format!("ERROR boot: {}", stop_name(why))
        };
        self.save_log(tag, 0);
        let k = &self.h.c.k;
        let s = format!(
            "RESULT {tag} fp={:016x} wfp={wfp:016x} secs={:.1} stops={} typed={} inh={:016x} verdict={verdict}",
            fnv_u64(wfp, wfp),
            now_s() - t0,
            k.nstop,
            k.ntyped,
            k.inh
        );
        self.h.out_line(&s);
        self.h.ctl_finish();
    }
    /// RESUME <tag> <snapshot>: continue a controlled run from a SNAP in a fresh VM
    fn ctl_resume(&mut self, tag: &str, name: &str) {
        let Some(i) = self.h.snaps.iter().rposition(|s| s.used && s.name == name) else {
            self.h.result_err(tag, &format!("ERROR no snapshot {name}"));
            return;
        };
        let check = self.h.o.check_restore;
        xsnap_restore(&mut self.vm, &self.h.snaps[i].x, check);
        self.h.ctl_begin(tag);
        self.h.c.k = self.h.snaps[i].k.clone();
        self.h.c.start = 2;
        self.h.c.rb = self.h.snaps[i].rb;
        let mut rs = self.h.snaps[i].rs.clone();
        rs.tag = lossy(&ccap(tag.as_bytes().to_vec(), 64));
        rs.t0 = now_s();
        self.h.spin_hit = false;
        let base = self.vm.sp.rw_on;
        self.vm.sp.rw_on = self.h.snaps[i].rw;
        self.run_rest(rs);
        self.vm.sp.rw_on = base;
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    fn unsupported(&self, host: &[u8], guest: &[u8]) -> Option<&'static str> {
        let att = self.vm.vio.attached();
        if let Some(d) = find(host, b"disk=") {
            if !att && !host[d..].starts_with(b"disk=none") {
                return Some("disk faults and power cuts need the data disk (HVM_DISK)");
            }
        }
        if !att && has(guest, b" C=") {
            return Some("power cuts need the data disk (HVM_DISK)");
        }
        if !self.h.initrd_has_tc && (has(guest, b" N=") || has(guest, b" O=")) {
            return Some("network faults need tc on the arm64 root");
        }
        None
    }

    /// One schedule: arm the host part, type the guest part, run it (`run_once`).
    fn run_once(&mut self, tag: &str, sched: &[u8], rewound: bool) {
        let t0 = now_s();
        let Some(bar) = sched.iter().position(|&c| c == b'|') else {
            self.h.result_err(tag, "ERROR bad schedule");
            return;
        };
        let host = ccap(sched[..bar].to_vec(), 1024);
        let mut guest = b" ".to_vec();
        guest.extend_from_slice(&sched[bar + 1..]);
        let mut guest = ccap(guest, 1024);
        while matches!(guest.last(), Some(b'\n' | b' ')) {
            guest.pop();
        }
        if let Some((hw, unknown)) = host_token_problem(&host) {
            let p = if unknown { "ERROR bad schedule: " } else { "UNSUPPORTED on hvm: " };
            self.h.result_err(tag, &format!("{p}{hw}"));
            return;
        }
        if let Some(u) = self.unsupported(&host, &guest) {
            self.h.result_err(tag, &format!("UNSUPPORTED on hvm: {u}"));
            return;
        }
        let Some((dseed, dcl, dpr)) = parse_disk(&host) else {
            self.h.result_err(tag, "ERROR bad disk token");
            return;
        };
        let Some(tj) = parse_tj(&host) else {
            self.h.result_err(tag, "ERROR bad tick-jitter token");
            return;
        };
        let pct_seed = find(&host, b"pct=")
            .map(|p| cstr::strtoull(&host[p + 4..], 16).0)
            .unwrap_or(0);
        let mut pct_n = find(&host, b"pctn=")
            .map(|p| cstr::strtoull(&host[p + 5..], 10).0 as u32)
            .unwrap_or(0);
        let pct_k = find(&host, b"pctk=")
            .map(|p| cstr::strtoull(&host[p + 5..], 10).0)
            .unwrap_or(20000);
        if find(&host, b"pct=").is_none() {
            pct_n = 0;
        }
        let cut = has(&guest, b" C=");
        let from = self.vm.m.console_len as usize;
        if self.h.c.on && !self.h.g_agent {
            self.h.c.k.evfrom = from; // lock-step: EVENTs start with the workload's console
        }
        self.vm.trace_line(&format!("RUN {tag}"));
        self.vm.vio.set_fault(dseed, dcl, dpr);
        let mut rs = RunSt {
            tag: lossy(&ccap(tag.as_bytes().to_vec(), 64)),
            t0,
            from,
            cut,
            rewound,
            pct_n,
            ..RunSt::default()
        };
        if tj_wanted(&tj) {
            // the jitter covers the workload only, like the disk fault
            self.vm.tj = tj;
            self.vm.tj.on = true;
            rs.tjinfo = format!(" tjk={}", self.vm.m.vtime / TJ_PERIOD);
        }
        rs.wx0 = self.vm.m.exits;
        if pct_n != 0 {
            let x = self.vm.m.exits;
            self.vm.pct = Pct::set(pct_seed, pct_n, x, x.wrapping_add(pct_k));
        }
        let mut line = guest.get(1..).unwrap_or(&[]).to_vec();
        line.push(b'\n');
        let line = ccap(line, 1100);
        if !(self.h.c.on && (guest.is_empty() || self.h.g_agent)) {
            self.vm.put_input(&line);
        }
        self.run_rest(rs);
    }

    /// the rest of a run, from the workload's start: a RUN goes on here, a RESUME starts here
    fn run_rest(&mut self, rs: RunSt) {
        let wk: &[&'static [u8]] = if self.h.g_agent { &[] } else { &[DONE_MARKER, FINAL_MARKER] };
        let fin: &[&'static [u8]] = if self.h.g_agent { &[] } else { &[FINAL_MARKER] };
        let tag = rs.tag.clone();
        let mut tjinfo = rs.tjinfo.clone();
        let t0 = rs.t0;
        let (mut wfp, mut ffp) = (0u64, 0u64);
        let wx0 = rs.wx0;
        let pct_n = rs.pct_n;
        let mut cut = rs.cut;
        let rewound = rs.rewound;
        let mut why = 0;
        let mut from = rs.from;
        self.h.c.rs = rs; // lock-step: what a SNAP in this run carries
        let mut r = self.run_to(wk, from, &mut why);
        if r == -7 {
            r = 0;
            cut |= self.h.c.cut; // M126: the host's DISKOFF/CUT is its workload marker
        }
        let cap = |s: &mut String| {
            if s.len() > 159 {
                s.truncate(159)
            }
        };
        if pct_n != 0 {
            tjinfo.push_str(&format!(
                " pctf={} pctw={}",
                self.vm.pct.fired,
                self.vm.m.exits.wrapping_sub(wx0)
            ));
            cap(&mut tjinfo);
            self.vm.pct = Pct::default();
        }
        if self.vm.tj.on {
            tjinfo.push_str(&format!(
                "-{} tjn={}",
                self.vm.m.vtime / TJ_PERIOD,
                self.vm.tj.applied
            ));
            cap(&mut tjinfo);
            self.vm.tj = Tjit::default();
        }
        let mut log_tag = tag.clone();
        let verdict: String;
        'out: {
            if r < 0 && r != -3 {
                self.h.spin_hit |= why == STOP_SPIN;
                verdict = format!("ERROR workload: {}", stop_name(why));
                break 'out;
            }
            wfp = self.vm.xfp();
            self.diag(&tag, "w");
            if r == 1 || r == -3 {
                verdict = self.h.ctl_verdict(&self.vm, r, from);
                ffp = wfp;
                break 'out;
            }
            if !cut {
                // same boot: faults off, heal, check
                self.vm.vio.set_fault(0, 0, 0);
                if !self.h.g_agent {
                    self.vm.put_input(b"go\n");
                }
                if self.h.c.on {
                    self.h.c.phase = "check";
                    self.h.c.start = 1;
                }
                r = self.run_to(fin, from, &mut why);
                if r < 0 && r != -3 {
                    self.h.spin_hit |= why == STOP_SPIN;
                    verdict = format!("ERROR check: {}", stop_name(why));
                    break 'out;
                }
                ffp = self.vm.xfp();
                self.diag(&tag, "f");
                verdict = self.h.ctl_verdict(&self.vm, r, from);
                break 'out;
            }
            // power cut: only the flushed disk bytes survive; recover from S0
            let orc = oracle_cmd(&self.vm, from);
            let durable = self.vm.vio.disk.clone();
            self.save_log(&tag, from);
            let check = self.h.o.check_restore;
            match self.h.g_s0 {
                S0Ref::Main => xsnap_restore(&mut self.vm, &self.h.s0, check),
                S0Ref::Slot(i) => xsnap_restore(&mut self.vm, &self.h.snaps[i].x, check),
            }
            if let (Some(d), Some(du)) = (&mut self.vm.vio.disk, durable) {
                d.copy_from_slice(&du);
            }
            self.vm.vio.set_fault(0, 0, 0);
            from = self.vm.m.console_len as usize;
            if !self.h.g_agent {
                self.vm.put_input(&orc);
            }
            if self.h.c.on {
                // a new VM with its own clock: queued input does not carry over
                self.h.ctl_drop_pending();
                self.h.c.phase = "recovery";
                self.h.c.start = 1;
                self.h.c.k.evfrom = from;
            }
            r = self.run_to(fin, from, &mut why);
            if r < 0 && r != -3 {
                self.h.spin_hit |= why == STOP_SPIN;
                verdict = format!("ERROR recovery: {}", stop_name(why));
            } else {
                ffp = self.vm.xfp();
                self.diag(&tag, "f");
                verdict = self.h.ctl_verdict(&self.vm, r, from);
            }
            log_tag = format!("{tag}.recovery");
        }
        self.save_log(&log_tag, from);
        if self.h.spin_hit && !rewound && self.h.o.spin_auto && !self.h.c.on {
            return; // not reported: run_sched re-runs it with rewind
        }
        if rewound {
            self.h.say(&format!("hvmx: {tag}: reported from a rewind-mode run"));
        }
        if self.h.c.on {
            let k = &self.h.c.k;
            tjinfo.push_str(&format!(
                " stops={} typed={} inh={:016x}",
                k.nstop, k.ntyped, k.inh
            ));
            cap(&mut tjinfo);
        }
        let s = format!(
            "RESULT {tag} fp={:016x} wfp={wfp:016x} secs={:.1}{tjinfo} verdict={verdict}",
            fnv_u64(ffp, wfp),
            now_s() - t0
        );
        self.h.out_line(&s);
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    /// one schedule from the restored S1 (serve) or from the provisioned boot (one)
    fn run_sched(&mut self, tag: &str, sched: &[u8], have_s1: bool) {
        let forced = sched
            .iter()
            .position(|&c| c == b'|')
            .is_some_and(|b| has(&sched[..b], b"spin=rewind"));
        let base = self.vm.sp.rw_on;
        self.h.spin_hit = false;
        if forced && !self.vm.sp.rw_on {
            self.vm.sp.rw_on = true;
        }
        let rw = self.vm.sp.rw_on;
        self.run_once(tag, sched, rw);
        if self.h.spin_hit && !self.vm.sp.rw_on && self.h.o.spin_auto {
            self.h.say(&format!(
                "hvmx: {tag} ended as SPIN; running it again with rewind (counter loops)"
            ));
            if !have_s1 || self.h.s1.ram.is_none() {
                fatal(3, "hvmx: no S1 snapshot to re-run from");
            }
            let check = self.h.o.check_restore;
            xsnap_restore(&mut self.vm, &self.h.s1, check);
            self.vm.sp.rw_on = true;
            self.h.spin_hit = false;
            self.run_once(tag, sched, true);
        }
        self.vm.sp.rw_on = base;
    }

    fn boot_to_s1(&mut self, take_s1: bool) -> Result<(), ()> {
        let mut why = 0;
        let r0 = self.run_to(&[S0_MARKER, FINAL_MARKER, AGENT_PREFIX], 0, &mut why);
        if r0 == 2 {
            self.h.say("hvmx: the guest's first process is hydo-agent (an agent image, /etc/hydo/control): boot it with --control boot");
            return Err(());
        }
        if r0 != 0 {
            self.h.say(&format!("hvmx: no S0 marker ({})", stop_name(why)));
            let n = self.vm.m.console_len as usize;
            self.h.say(&lossy(&self.vm.console[..n]));
            return Err(());
        }
        let dir = self.h.o.snap_dir.clone();
        xsnap_take(&self.vm, &mut self.h.s0, "s0", &dir);
        self.h.say(&format!(
            "hvmx: S0 at {} console bytes, exit {}, state {:016x}",
            self.vm.m.console_len,
            self.vm.m.exits,
            self.vm.xfp()
        ));
        let f = self.vm.m.console_len as usize;
        self.vm.put_input(b"provision\n");
        if self.run_to(&[S1_MARKER, FINAL_MARKER], f, &mut why) != 0 {
            self.h.say(&format!("hvmx: no S1 marker ({})", stop_name(why)));
            let n = self.vm.m.console_len as usize;
            self.h.say(&lossy(&self.vm.console[f..n]));
            return Err(());
        }
        self.h.say(&format!(
            "hvmx: S1 at {} console bytes, exit {}, state {:016x}",
            self.vm.m.console_len,
            self.vm.m.exits,
            self.vm.xfp()
        ));
        if take_s1 {
            xsnap_take(&self.vm, &mut self.h.s1, "s1", &dir);
        }
        Ok(())
    }

    /// The serve loop of a shell image (RUN, CTL, RESUME, BRANCH, QUIT).
    fn serve_loop(&mut self, agent: bool) {
        loop {
            let mut ln = Vec::new();
            match self.h.input.read_until(b'\n', &mut ln) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if ln.starts_with(b"QUIT") {
                return;
            }
            if ln.starts_with(b"BRANCH ") {
                self.h.out_line("BRANCHEND ? unsupported on hvm");
                continue;
            }
            if ln.starts_with(b"RESUME ") {
                let mut sc = Scan::new(&ln);
                sc.lit("RESUME").ws();
                let tag = sc.word(63).map(lossy);
                let name = sc.word(63).map(lossy);
                let (Some(tag), Some(name)) = (tag, name) else {
                    self.h.result_err("?", "ERROR bad request");
                    continue;
                };
                self.ctl_resume(&tag, &name);
                continue;
            }
            let ctl = ln.starts_with(b"CTL ");
            let rq = if ctl { &ln[4..] } else { &ln[..] };
            let mut sc = Scan::new(rq);
            if !ctl {
                sc.lit("RUN");
            }
            let tag = sc.word(63).map(lossy);
            sc.ws();
            let off = sc.pos;
            let Some(tag) = tag.filter(|_| off != 0) else {
                self.h.result_err("?", "ERROR bad request");
                continue;
            };
            let sched = rq[off..].to_vec();
            if agent {
                let cs = self.h.snap_find("S1");
                let no = if !ctl {
                    Some("an agent image runs under control only (CTL, RESUME)")
                } else if cs.is_none() {
                    Some("no snapshot S1 (the boot took none)")
                } else if !agent_guest_ok(&sched) {
                    Some("an agent image's schedule has the guest part \"run\" only")
                } else {
                    None
                };
                if let Some(no) = no {
                    self.h.result_err(&tag, &format!("ERROR {no}"));
                    continue;
                }
                self.ctl_request(&tag, &sched, cs);
                continue;
            }
            let check = self.h.o.check_restore;
            xsnap_restore(&mut self.vm, &self.h.s1, check);
            if ctl {
                self.ctl_request(&tag, &sched, None);
            } else {
                self.run_sched(&tag, &sched, true);
            }
        }
    }
}

/// An agent image's schedule has no guest tokens: its guest part is "run" (or empty).
fn agent_guest_ok(s: &[u8]) -> bool {
    let Some(bar) = s.iter().position(|&c| c == b'|') else {
        return true; // run_once refuses it
    };
    let mut g = &s[bar + 1..];
    while g.first() == Some(&b' ') {
        g = &g[1..];
    }
    while matches!(g.last(), Some(b' ' | b'\n' | b'\r')) {
        g = &g[..g.len() - 1];
    }
    g.is_empty() || g == b"run"
}

fn ram_mib_parse(v: &str, what: &str, diag: &Diag) -> Option<u64> {
    let (n, used) = cstr::strtoull(v.as_bytes(), 10);
    if v.is_empty() || used != v.len() || !(RAM_MIN_MIB..=RAM_MAX_MIB).contains(&n) {
        say(
            diag,
            &format!(
                "hvmx: {what}={v}: guest RAM must be a whole number of MiB in [{RAM_MIN_MIB}, {RAM_MAX_MIB}]"
            ),
        );
        return None;
    }
    Some(n)
}

/// The settings from the environment (hvm.c's and serve.c's getenv calls).
pub fn opts_from(get: Lookup, hvmx: bool) -> VmOpts {
    let f = |k: &str| get(k);
    let spin_rewind = f("HVM_SPIN_REWIND");
    let mut virt = virtio::Config {
        disk: f("HVM_DISK"),
        disk_out: f("HVM_DISK_OUT"),
        fault: f("HVM_FAULT"),
        fault_classes: f("HVM_FAULT_CLASSES"),
        fault_prob: f("HVM_FAULT_PROB"),
        fault_maxdelay: f("HVM_FAULT_MAXDELAY"),
        net: f("HVM_NET").is_some(),
        net_rx: f("HVM_NET_RX"),
        net_rx_pace: f("HVM_NET_RX_PACE"),
        net_tx_out: f("HVM_NET_TX_OUT"),
        net_fault: f("HVM_NET_FAULT"),
        net_fault_classes: f("HVM_NET_FAULT_CLASSES"),
        net_fault_prob: f("HVM_NET_FAULT_PROB"),
    };
    if hvmx && virt.disk.is_some() && virt.fault.is_none() {
        // negotiate FLUSH: flush-aware class at probability 0
        virt.fault = Some("0".into());
        virt.fault_classes = Some("unflushed".into());
        virt.fault_prob = Some("0".into());
    }
    let soft = f("HVM_SOFT_IRQ").map(|v| cstr::atoi(v.as_bytes()) != 0);
    VmOpts {
        ram_size: 0,
        step: f("HVM_STEP")
            .map(|v| cstr::strtoull0(v.as_bytes()))
            .unwrap_or(64),
        echo: f("HVM_ECHO").is_some(),
        trace: f("HVM_TRACE"),
        trace_watch: f("HVM_TRACE_WATCH"),
        trace_sysreg: f("HVM_TRACE_SYSREG").is_some(),
        spin_secs: f("HVM_SPIN_SECS").map(|v| cstr::atof(&v)).unwrap_or(10.0),
        kick_s: f("HVM_SPIN_MS")
            .map(|v| cstr::atof(&v) / 1000.0)
            .unwrap_or(0.005),
        spin_observe: f("HVM_SPIN_OBSERVE")
            .map(|v| cstr::atoi(v.as_bytes()) != 0)
            .unwrap_or(true),
        rw_on: spin_rewind.as_deref() == Some("1"),
        splog: f("HVM_SPLOG"),
        soft_irq: soft.unwrap_or(hvmx),
        bootargs: f("HVM_BOOTARGS").unwrap_or_else(|| BOOTARGS.to_string()),
        virtio: virt,
    }
}

/// hvmx's `main` over the given streams, with the environment from `get`; the exit status.
pub fn main_with(args: &[String], get: Lookup, io: Io) -> i32 {
    let diag: Diag = Arc::new(Mutex::new(io.diag));
    let d2 = diag.clone();
    let Io {
        out, input, echo, ..
    } = io;
    catch_fatal(&d2, move || hvmx_main(args, get, out, input, echo, diag))
}

/// `main` of the program over the process's own streams and environment.
pub fn main_stdio(args: &[String]) -> i32 {
    let io = Io {
        out: Box::new(std::io::LineWriter::new(std::io::stdout())),
        input: Box::new(std::io::BufReader::new(std::io::stdin())),
        diag: Box::new(std::io::stderr()),
        echo: Some(Box::new(std::io::stdout())),
    };
    let get = |k: &str| std::env::var(k).ok();
    main_with(args, &get, io)
}

fn hvmx_main(
    args: &[String],
    get: Lookup,
    out: Box<dyn Write + Send>,
    input: Box<dyn BufRead + Send>,
    echo: Option<Box<dyn Write + Send>>,
    diag: Diag,
) -> i32 {
    let mut a: Vec<String> = args.to_vec();
    let mut ram_mib = 1024;
    if let Some(v) = get("HVM_MEM_MB") {
        match ram_mib_parse(&v, "HVM_MEM_MB", &diag) {
            Some(n) => ram_mib = n,
            None => return 2,
        }
    }
    while a.len() > 2 && a[1] == "--ram-mib" {
        match ram_mib_parse(&a[2], "--ram-mib", &diag) {
            Some(n) => ram_mib = n,
            None => return 2,
        }
        a.drain(1..3);
    }
    let mut ctl_one = false;
    let mut agent = false;
    if a.len() > 2 && a[1] == "--control" && a[2] == "boot" {
        agent = true;
        a.drain(1..3);
    } else if a.len() > 1 && a[1] == "--control" {
        ctl_one = true;
        a.remove(1);
    }
    let argc = a.len();
    let usage = argc < 5
        || (a[1] != "serve" && a[1] != "one")
        || (a[1] == "one" && argc < 6)
        || (ctl_one && a[1] != "one");
    if usage {
        say(&diag, "usage: hvmx [--ram-mib N] serve <Image> <initramfs> <outdir>\n       hvmx [--ram-mib N] [--control] one <Image> <initramfs> <outdir> \"<schedule>\"\n       hvmx [--ram-mib N] --control boot serve|one <Image> <initramfs> <outdir> [\"<schedule>\"]");
        return 2;
    }
    let mode = a[1].clone();
    let kernel = std::fs::read(&a[2]).unwrap_or_else(|e| fatal(1, &format!("{}: {e}", a[2])));
    let initrd = std::fs::read(&a[3]).unwrap_or_else(|e| fatal(1, &format!("{}: {e}", a[3])));
    let initrd_has_tc = scan_has_tc(&initrd);
    let outdir = a[4].clone();
    let _ = std::fs::create_dir(&outdir);
    let mut vo = opts_from(get, true);
    vo.ram_size = ram_mib << 20;
    if ram_mib != 1024 {
        say(&diag, &format!("hvmx: guest RAM {ram_mib} MiB"));
    }
    let spin_rewind = get("HVM_SPIN_REWIND");
    if vo.rw_on && !vo.spin_observe {
        say(&diag, "hvm: HVM_SPIN_REWIND=1 needs the observer (HVM_SPIN_OBSERVE)");
        return 2;
    }
    vo.bootargs = format!(
        "{} {}",
        vo.bootargs,
        if agent { "quiet loglevel=1" } else { "hydo_phase=serve" }
    );
    let o = Opts {
        vm: vo.clone(),
        ram_mib,
        run_exits: get("HVM_RUN_EXITS")
            .map(|v| cstr::strtoull0(v.as_bytes()))
            .unwrap_or(50_000_000),
        spin_auto: spin_rewind.is_none() || spin_rewind.as_deref() == Some("auto"),
        snap_dir: get("HVM_SNAP_DIR").unwrap_or_else(|| "/tmp/hvm-snap".into()),
        diag: get("HVMX_DIAG").is_some(),
        diag_chunks: get("HVMX_DIAG_CHUNKS"),
        check_restore: get("HVMX_CHECK_RESTORE").is_some(),
        no_mlock: get("HVMX_NO_MLOCK").is_some(),
    };
    let vm = Vmm::new(&vo, &kernel, &initrd, diag.clone(), echo, !o.no_mlock);
    drop(kernel);
    drop(initrd);
    let t0 = now_s();
    let mut e = Engine {
        vm,
        h: Host {
            out,
            input,
            diag,
            outdir,
            o,
            c: Ctl::default(),
            hk: Hk::default(),
            snaps: (0..CTL_MAX_SNAPS).map(|_| CtlSnap::default()).collect(),
            g_ctl_one: false,
            g_agent: agent,
            s0: XSnap::default(),
            s1: XSnap::default(),
            g_s0: S0Ref::Main,
            spin_hit: false,
            initrd_has_tc,
            g_boot_rs: RunSt::default(),
        },
    };
    e.vm.max_exits = 200_000_000;
    if agent {
        // M121: the boot under control; S0 and S1 are the host's SNAPs
        let tag = if mode != "one" { "boot" } else { "one" };
        let mut s = ccap(
            a.get(5).map(|x| x.as_bytes().to_vec()).unwrap_or_else(|| b"|".to_vec()),
            4096,
        );
        let Some((rb, rw)) = e.ctl_sched(tag, &mut s) else {
            return 2;
        };
        if !agent_guest_ok(&s) {
            e.h.say("hvmx: an agent image's schedule has the guest part \"run\" only");
            return 2;
        }
        e.h.g_ctl_one = mode == "one";
        let mut why = 0;
        let r = e.agent_boot(tag, rb, &mut why);
        if r != -4 {
            e.agent_boot_result(tag, r, why, t0);
        }
        if e.h.g_ctl_one {
            if r == -4 {
                // the workload from the stop where S1 was taken, as a CTL from S1
                if let Some(i) = e.h.snap_find("S0") {
                    e.h.g_s0 = S0Ref::Slot(i);
                }
                e.h.c.boot_s1 = false;
                e.h.c.phase = "workload";
                e.h.spin_hit = false;
                e.vm.sp.rw_on = rw; // M130: the rewind mode from S1's stop on
                e.run_once(tag, &s, rw);
                if e.h.c.on {
                    e.h.ctl_finish();
                }
            }
            return 0;
        }
        if let Some(i) = e.h.snap_find("S0") {
            e.h.g_s0 = S0Ref::Slot(i);
        }
        e.h.out_line(&format!("READY boot_secs={:.1}", now_s() - t0));
        e.serve_loop(true);
        return 0;
    }
    if mode == "one" {
        let spin_auto = e.h.o.spin_auto;
        if e.boot_to_s1(spin_auto).is_err() {
            return 1;
        }
        e.h.say(&format!(
            "hvmx: provisioned in {:.1} s (fresh boot, no snapshot restore)",
            now_s() - t0
        ));
        let sched = a[5].as_bytes().to_vec();
        if ctl_one {
            e.ctl_request("one", &sched, None);
        } else {
            e.run_sched("one", &sched, spin_auto);
        }
        return 0;
    }
    if e.boot_to_s1(true).is_err() {
        return 1;
    }
    e.h.out_line(&format!("READY boot_secs={:.1}", now_s() - t0));
    e.serve_loop(false);
    0
}
