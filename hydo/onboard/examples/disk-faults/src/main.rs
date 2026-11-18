//! recs: a tiny record store for hydo's block faults (Milestone 313, docs/design/block-faults.md).
//! It keeps 16 keys in a file of fixed 4 KiB slots, and writes them with O_DIRECT, as a storage
//! engine does. It has two variants:
//!
//! - **careless** (the default) trusts its writes: one copy of each record, updated in place,
//!   no checksum, and the errors of `pwrite` and `fdatasync` are ignored;
//! - **careful** (`--careful`) checks them: two copies of each record (regions A and B), each
//!   with a CRC-32 over the whole slot, its key and its region; a record is acknowledged only
//!   after both copies were written and synced without an error, and a read takes the newest
//!   valid copy. It does a step's disk work in a child process and waits for it at most 5 s, so
//!   a disk that stops answering fails the step instead of stopping the store.
//!
//! Step `i` of the workload makes the record of step `i - 1` durable and acknowledges it, then
//! writes the record of step `i` (key `i % 16`, value `i + 1`), which stays in flight until the
//! next step. So at a power cut between two steps one write is in the disk's volatile cache.
//!
//! Commands (the directory is the service's storage):
//!
//! ```text
//! recs serve <dir> [--careful]          create the file once, then wait (the service)
//! recs ready <dir> [--careful]          exit 0 when the file is there
//! recs step <dir> <i> [--careful]       one workload step; prints the step it acknowledged
//! recs io <dir> <i> --careful           the careful step's disk work, in a child process
//! recs check <dir> <acked> [--careful]  every key reads back as acknowledged (or in flight)
//! ```

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, OpenOptionsExt};

const SLOT: usize = 4096;
const SLOTS: u64 = 64;
const KEYS: u64 = 16;
/// The keys' slots in the middle of a region, so that a misdirected write a few blocks away
/// stays in the file.
const FIRST: u64 = 24;
const MAGIC: &[u8; 4] = b"REC1";

#[cfg(target_arch = "aarch64")]
const O_DIRECT: i32 = 0o200000;
#[cfg(not(target_arch = "aarch64"))]
const O_DIRECT: i32 = 0o40000;

/// A 4 KiB buffer aligned for O_DIRECT.
struct Buf(*mut u8, std::alloc::Layout);

impl Buf {
    fn new() -> Buf {
        let l = std::alloc::Layout::from_size_align(SLOT, SLOT).unwrap();
        let p = unsafe { std::alloc::alloc_zeroed(l) };
        assert!(!p.is_null());
        Buf(p, l)
    }

    fn b(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.0, SLOT) }
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.0, self.1) };
    }
}

fn crc32(b: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &x in b {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// The slot of the record of (key, value) in `region` (careless: region 0, no checksum).
fn record(key: u64, value: u64, region: u32, careful: bool, out: &mut [u8]) {
    out[..4].copy_from_slice(MAGIC);
    out[4..8].copy_from_slice(&(key as u32).to_le_bytes());
    out[8..16].copy_from_slice(&value.to_le_bytes());
    out[16..20].copy_from_slice(&region.to_le_bytes());
    out[20..24].fill(0);
    for (i, b) in out[24..].iter_mut().enumerate() {
        *b = ((value * 131 + key * 7 + i as u64) % 251) as u8;
    }
    if careful {
        let c = crc32(out);
        out[20..24].copy_from_slice(&c.to_le_bytes());
    }
}

fn file_len(careful: bool) -> u64 {
    SLOTS * SLOT as u64 * if careful { 2 } else { 1 }
}

fn slot_off(key: u64, region: u32) -> u64 {
    (region as u64 * SLOTS + FIRST + key) * SLOT as u64
}

fn path(dir: &str) -> String {
    format!("{dir}/records.dat")
}

fn open(dir: &str) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(O_DIRECT)
        .open(path(dir))
        .unwrap_or_else(|e| die(&format!("open {}: {e}", path(dir))))
}

fn die(m: &str) -> ! {
    eprintln!("recs: {m}");
    std::process::exit(1)
}

fn serve(dir: &str, careful: bool) {
    let p = path(dir);
    let want = file_len(careful);
    if std::fs::metadata(&p).map(|m| m.len()).ok() != Some(want) {
        let tmp = format!("{p}.new");
        let mut f = File::create(&tmp).unwrap_or_else(|e| die(&format!("{tmp}: {e}")));
        f.write_all(&vec![0u8; want as usize]).unwrap();
        f.sync_all().unwrap();
        std::fs::rename(&tmp, &p).unwrap();
        File::open(dir).unwrap().sync_all().unwrap();
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// One copy written and synced; Err on any error (the careful path).
fn put(f: &File, key: u64, value: u64, region: u32) -> std::io::Result<()> {
    let mut b = Buf::new();
    record(key, value, region, true, b.b());
    f.write_all_at(b.b(), slot_off(key, region))?;
    Ok(())
}

fn step(dir: &str, i: u64, careful: bool) {
    let f = open(dir);
    if !careful {
        // the careless store: sync, acknowledge whatever happened, write the next one in place
        let _ = f.sync_data();
        if i > 0 {
            println!("{}", i - 1);
        }
        let mut b = Buf::new();
        record(i % KEYS, i + 1, 0, false, b.b());
        let _ = f.write_all_at(b.b(), slot_off(i % KEYS, 0));
        return;
    }
    // the careful store does the disk's work in a child process and waits for it at most
    // DEADLINE: a process that waits on a disk request that never completes cannot be ended,
    // so the step reports the disk as stuck and goes on without an acknowledgement
    drop(f);
    let me = std::env::current_exe().unwrap_or_else(|e| die(&format!("own path: {e}")));
    let mut child = std::process::Command::new(me)
        .args(["io", dir, &i.to_string(), "--careful"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| die(&format!("spawn: {e}")));
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                if st.code() == Some(0) && i > 0 {
                    println!("{}", i - 1);
                }
                return;
            }
            Ok(None) if t0.elapsed() < DEADLINE => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => {
                eprintln!(
                    "recs: step {i}: the disk did not answer within {} s; step {} is not \
                     acknowledged",
                    DEADLINE.as_secs(),
                    i.saturating_sub(1)
                );
                std::process::exit(1);
            }
        }
    }
}

/// How long a careful step waits for the disk.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// The careful step's disk work (in a child process): the record in flight again in A, synced,
/// then B, synced; exit 0 only when every call succeeded (a failed fsync is not cleared by a
/// later one, so the whole is tried again); then the next record in A.
fn io(dir: &str, i: u64) {
    let f = open(dir);
    let mut done = i == 0;
    if i > 0 {
        let (k, v) = ((i - 1) % KEYS, i);
        for _ in 0..3 {
            let r = put(&f, k, v, 0)
                .and_then(|_| f.sync_data())
                .and_then(|_| put(&f, k, v, 1))
                .and_then(|_| f.sync_data());
            match r {
                Ok(()) => {
                    done = true;
                    break;
                }
                Err(e) => eprintln!("recs: step {}: {e}; trying again", i - 1),
            }
        }
        if !done {
            eprintln!("recs: step {} not acknowledged: the disk failed it three times", i - 1);
        }
    }
    if let Err(e) = put(&f, i % KEYS, i + 1, 0) {
        eprintln!("recs: step {i}: {e} (written again in the next step)");
    }
    std::process::exit(if done { 0 } else { 2 });
}

/// What a client reads for `key`: Ok(None) for no value, Ok(Some(slot bytes)), or Err(why).
fn read(f: &File, key: u64, careful: bool) -> Result<Option<Vec<u8>>, String> {
    let get = |region: u32| -> Result<Vec<u8>, String> {
        let mut b = Buf::new();
        f.read_exact_at(b.b(), slot_off(key, region))
            .map_err(|e| format!("read of region {region}: {e}"))?;
        Ok(b.b().to_vec())
    };
    if !careful {
        // the careless store trusts what it reads
        let s = get(0)?;
        return Ok(if s.iter().all(|x| *x == 0) { None } else { Some(s) });
    }
    let mut best: Option<(u64, Vec<u8>)> = None;
    let mut empty = 0;
    let mut why = Vec::new();
    for region in [0u32, 1] {
        let s = match get(region) {
            Ok(s) => s,
            Err(e) => {
                why.push(e);
                continue;
            }
        };
        if s.iter().all(|x| *x == 0) {
            empty += 1;
            continue;
        }
        let mut z = s.clone();
        z[20..24].fill(0);
        let crc = u32::from_le_bytes(s[20..24].try_into().unwrap());
        let k = u32::from_le_bytes(s[4..8].try_into().unwrap()) as u64;
        let r = u32::from_le_bytes(s[16..20].try_into().unwrap());
        if &s[..4] != MAGIC || crc32(&z) != crc || k != key || r != region {
            why.push(format!("region {region}: a damaged or misplaced copy"));
            continue;
        }
        let v = u64::from_le_bytes(s[8..16].try_into().unwrap());
        if best.as_ref().is_none_or(|(bv, _)| v > *bv) {
            best = Some((v, s));
        }
    }
    match best {
        Some((_, s)) => Ok(Some(s)),
        None if empty > 0 => Ok(None),
        None => Err(format!("both copies bad ({})", why.join("; "))),
    }
}

fn check(dir: &str, acked: &str, careful: bool) {
    let acked: Vec<u64> = std::fs::read_to_string(acked)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .collect();
    let f = open(dir);
    let mut bad = Vec::new();
    for key in 0..KEYS {
        // the acknowledged value, and the values of the writes after it (in flight at the end)
        let a = acked.iter().filter(|s| *s % KEYS == key).max().copied();
        let mut ok: Vec<Option<u64>> = vec![a.map(|s| s + 1)];
        let from = a.map_or(key, |s| s + KEYS);
        // (a write of any later step may be in flight: a careful step that was not acknowledged
        // has still written its record)
        let to = from + KEYS * 64;
        let mut s = from;
        while s <= to {
            ok.push(Some(s + 1));
            s += KEYS;
        }
        let got = match read(&f, key, careful) {
            Ok(g) => g,
            Err(e) => {
                bad.push(format!("key {key}: {e}"));
                continue;
            }
        };
        let fits = ok.iter().any(|v| match (v, &got) {
            (None, None) => true,
            (Some(v), Some(g)) => {
                let mut want = vec![0u8; SLOT];
                record(key, *v, 0, careful, &mut want);
                let (mut g, mut w) = (g.clone(), want);
                // a copy of region B differs in its region and its checksum only
                g[16..24].fill(0);
                w[16..24].fill(0);
                g == w
            }
            _ => false,
        });
        if !fits {
            let what = match &got {
                None => "no value".to_string(),
                Some(g) => format!(
                    "a record whose header says value {} but whose bytes are neither that \
                     value's nor another allowed one's",
                    u64::from_le_bytes(g[8..16].try_into().unwrap())
                ),
            };
            bad.push(format!(
                "key {key}: acknowledged value {}, reads {what}",
                a.map_or("none".into(), |s| (s + 1).to_string())
            ));
        }
    }
    if bad.is_empty() {
        println!("recs: all {KEYS} keys read back as acknowledged");
        return;
    }
    for b in &bad {
        println!("recs: {b}");
    }
    println!("recs: {} of {KEYS} keys read back wrong", bad.len());
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let careful = args.iter().any(|a| a == "--careful");
    let a: Vec<&str> = args
        .iter()
        .filter(|a| *a != "--careful")
        .map(String::as_str)
        .collect();
    match a.as_slice() {
        ["serve", dir] => serve(dir, careful),
        ["ready", dir] => {
            if std::fs::metadata(path(dir)).map(|m| m.len()).ok() != Some(file_len(careful)) {
                std::process::exit(1);
            }
        }
        ["step", dir, i] => step(dir, i.parse().unwrap_or_else(|_| die("step <i>")), careful),
        ["io", dir, i] => io(dir, i.parse().unwrap_or_else(|_| die("io <i>"))),
        ["check", dir, acked] => check(dir, acked, careful),
        _ => die("usage: recs serve|ready <dir> | step <dir> <i> | check <dir> <acked> [--careful]"),
    }
}
