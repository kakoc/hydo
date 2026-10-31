//! What several client processes of one run share, kept in files under one directory (default
//! /tmp/hydo/grpc): named counters (operation ids, unique values, drive invocations, assertion
//! once-flags) and the value pools that `from` hints read. Every update is a short critical
//! section under flock, so concurrent composer drivers see one consistent sequence (deterministic
//! under hydo, which schedules the processes deterministically).

use crate::record;
use crate::rng::R;
use crate::spec::From;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

pub struct State {
    pub dir: PathBuf,
}

struct Locked(File);

impl Drop for Locked {
    fn drop(&mut self) {
        // SAFETY: flock on an open descriptor.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl State {
    /// Creates the directory (and its pool/) if needed.
    pub fn new(dir: &str) -> Result<State, String> {
        let d = PathBuf::from(dir);
        std::fs::create_dir_all(d.join("pool"))
            .map_err(|e| format!("mkdir {}: {}", d.join("pool").display(), crate::io_msg(&e)))?;
        Ok(State { dir: d })
    }

    fn locked(&self, name: &str) -> std::io::Result<Locked> {
        let f = OpenOptions::new().create(true).read(true).write(true).mode(0o644).open(self.dir.join(name))?;
        // SAFETY: flock on an open descriptor.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Locked(f))
    }

    /// The next value of a named counter (1, 2, ...).
    pub fn next(&self, name: &str) -> u64 {
        let r = (|| -> std::io::Result<u64> {
            let l = self.locked(name)?;
            let mut b = [0u8; 32];
            let k = l.0.read_at(&mut b, 0).unwrap_or(0);
            let n = std::str::from_utf8(&b[..k]).unwrap_or("").trim().parse::<u64>().unwrap_or(0) + 1;
            l.0.set_len(0)?;
            l.0.write_at(format!("{n}\n").as_bytes(), 0)?;
            Ok(n)
        })();
        match r {
            Ok(n) => n,
            Err(e) => {
                eprintln!("hydo-grpc: counter {}: {}", name, crate::io_msg(&e));
                0
            }
        }
    }

    fn pool_file(pool: &str) -> String {
        format!("pool/{}", pool.replace(['/', ' '], "_"))
    }

    /// Appends values to a pool.
    pub fn put(&self, pool: &str, values: &[String]) {
        if values.is_empty() {
            return;
        }
        let mut b = String::new();
        for v in values {
            b.push_str(&record::enc(v.as_bytes()));
            b.push('\n');
        }
        if let Ok(mut l) = self.locked(&Self::pool_file(pool)) {
            if l.0.seek(SeekFrom::End(0)).is_ok() {
                let _ = l.0.write_all(b.as_bytes());
            }
        }
    }

    /// One value of a pool: the newest (pick latest) or a random one.
    pub fn take(&self, f: &From, r: &mut R) -> Option<String> {
        let mut vals: Vec<String> = Vec::new();
        if let Ok(mut l) = self.locked(&Self::pool_file(&f.pool())) {
            let mut s = Vec::new();
            let _ = l.0.read_to_end(&mut s);
            // bufio.Scanner's lines: a trailing "\r" is dropped, no token after the last newline
            for line in String::from_utf8_lossy(&s).lines() {
                if let Ok(v) = record::dec(line) {
                    vals.push(String::from_utf8_lossy(&v).into_owned());
                }
            }
        }
        if vals.is_empty() {
            return None;
        }
        if f.pick == "latest" {
            return vals.pop();
        }
        let i = r.intn(vals.len() as i64) as usize;
        Some(vals.swap_remove(i))
    }
}
