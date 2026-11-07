//! hydo-gui: the voter at the screen of compose-voting-browser (Milestone 166,
//! docs/design/gui-testing.md).
//!
//! It starts headless Chromium with `--remote-debugging-pipe` (the Chrome DevTools Protocol as
//! NUL-terminated JSON messages on file descriptors 3 and 4, so no TCP port, no WebSocket and no
//! client library), opens a page, clicks with real mouse events and reports what the page then
//! shows. Everything it waits for is bounded by a deadline in guest time.
//!
//!   hydo-gui vote FROM TO     voters FROM..TO-1 (voter<i> votes a on even i, b on odd i): set the
//!                             cookie voter_id=voter<i>, open http://vote/, click the button, read
//!                             the screen; one line per voter:
//!                               voter<i> clicked=<a|b|-> screen=<a|b|none> [why=<what was seen>]
//!                             screen=<x> means the page shows the vote for <x> as cast: button
//!                             <x> disabled with the check icon, the other button dimmed (the vote
//!                             app's own confirmation). $GUI_SHOTS (default /tmp/gui/shots, "-" for
//!                             none) gets voter<i>.png, a screenshot of what the voter saw.
//!   hydo-gui results A B      open http://result/ and wait (at most $GUI_WAIT s, default 10) until
//!                             the page shows A votes for a and B for b (total and percentages as
//!                             the result app renders them); A B = "- -": the database has no votes
//!                             table yet, so the page may show no total. Exit 0 when it does, 1
//!                             when it shows something else (or does not load), 2 on a usage error.
//!                             The last line says what the screen showed. Screenshot results.png.
//!   hydo-gui live             open http://result/ while people vote: exit 0 when the page shows a
//!                             vote total within $GUI_WAIT s, 1 when it does not load or shows
//!                             none. Screenshot live.png.
//!   hydo-gui probe            start the browser, print its version, stop it.
//!
//! $GUI_CHROME is the browser (default /headless-shell/headless-shell), $GUI_FLAGS extra flags.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

extern "C" {
    fn pipe2(fds: *mut i32, flags: i32) -> i32;
    fn dup2(old: i32, new: i32) -> i32;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn close(fd: i32) -> i32;
}
const O_CLOEXEC: i32 = 0o2_000_000;
const F_DUPFD_CLOEXEC: i32 = 1030;

/// The flags, chosen for a browser that runs the same way every time and does nothing on its
/// own: no GPU (software rendering), no background traffic, a fixed window and scale.
const FLAGS: &[&str] = &[
    "--headless",
    "--no-sandbox",
    "--disable-gpu",
    "--disable-dev-shm-usage",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-default-apps",
    "--disable-extensions",
    "--disable-sync",
    "--disable-breakpad",
    "--metrics-recording-only",
    "--mute-audio",
    "--password-store=basic",
    "--hide-scrollbars",
    "--window-size=640,400",
    "--force-device-scale-factor=1",
    "--lang=en-US",
    "--remote-debugging-pipe",
];

// ---- a small JSON reader and writer ---------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)] // a whole JSON value is kept, though only objects, strings and numbers are read
enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn get(&self, k: &str) -> Option<&J> {
        match self {
            J::Obj(v) => v.iter().find(|(n, _)| n == k).map(|(_, x)| x),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    fn num(&self) -> Option<f64> {
        match self {
            J::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn path(&self, p: &[&str]) -> Option<&J> {
        p.iter().try_fold(self, |j, k| j.get(k))
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn lit(&mut self, s: &str, v: J) -> Result<J, String> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            Err(format!("bad literal at {}", self.i))
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).ok_or("short \\u")?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).map_err(|e| e.to_string())?, 16)
            .map_err(|e| e.to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("bad escape")?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.b[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                u = 0x10000
                                    + ((u - 0xd800) << 10)
                                    + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            char::from_u32(u).unwrap_or('\u{fffd}')
                        }
                        _ => return Err("bad escape".into()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
    fn value(&mut self) -> Result<J, String> {
        self.ws();
        let c = *self.b.get(self.i).ok_or("unexpected end")?;
        match c {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(J::Obj(v));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err("expected a key".into());
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err("expected ':'".into());
                    }
                    self.i += 1;
                    let x = self.value()?;
                    v.push((k, x));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(J::Obj(v));
                        }
                        _ => return Err("expected ',' or '}'".into()),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(J::Arr(v));
                }
                loop {
                    v.push(self.value()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::Arr(v));
                        }
                        _ => return Err("expected ',' or ']'".into()),
                    }
                }
            }
            b'"' => self.string().map(J::Str),
            b't' => self.lit("true", J::Bool(true)),
            b'f' => self.lit("false", J::Bool(false)),
            b'n' => self.lit("null", J::Null),
            _ => {
                let s = self.i;
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[s..self.i])
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .map(J::Num)
                    .ok_or_else(|| format!("bad number at {s}"))
            }
        }
    }
}

fn parse(b: &[u8]) -> Result<J, String> {
    P { b, i: 0 }.value()
}

/// A JSON string literal.
fn q(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// One word for a report line: whitespace becomes '_', at most 80 characters.
fn word(s: &str) -> String {
    let w: String = s
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .take(80)
        .collect();
    if w.is_empty() {
        "-".into()
    } else {
        w
    }
}

fn b64decode(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let (mut out, mut acc, mut n) = (Vec::with_capacity(s.len() * 3 / 4), 0u32, 0);
    for v in s.bytes().filter_map(val) {
        acc = (acc << 6) | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
        }
    }
    out
}

// ---- the browser over the pipe --------------------------------------------------------------

struct Cdp {
    child: Child,
    w: File,
    rx: Receiver<J>,
    id: u64,
    events: VecDeque<J>,
    session: String,
    profile: String,
}

fn dup_high(fd: i32) -> i32 {
    // SAFETY: fcntl on a descriptor this process owns; the result is checked by the caller.
    let n = unsafe { fcntl(fd, F_DUPFD_CLOEXEC, 10) };
    // SAFETY: fd is one end of a pipe made above and no longer used after this.
    unsafe { close(fd) };
    n
}

impl Cdp {
    fn launch(tag: &str) -> Result<Cdp, String> {
        let chrome =
            std::env::var("GUI_CHROME").unwrap_or_else(|_| "/headless-shell/headless-shell".into());
        let profile = format!("/tmp/gui/profile-{tag}-{}", std::process::id());
        fs::create_dir_all(&profile).map_err(|e| format!("{profile}: {e}"))?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("/tmp/gui/chrome-{tag}.log"))
            .map_err(|e| e.to_string())?;
        let (mut a, mut b) = ([0i32; 2], [0i32; 2]);
        // SAFETY: two arrays of two descriptors, as pipe2 expects.
        if unsafe { pipe2(a.as_mut_ptr(), O_CLOEXEC) } != 0
            || unsafe { pipe2(b.as_mut_ptr(), O_CLOEXEC) } != 0
        {
            return Err("pipe2 failed".into());
        }
        // the browser reads commands on fd 3 and writes answers on fd 4; first move both of its
        // ends above 9 so that the dup2 calls in the child cannot overwrite one with the other
        let (cr, cw) = (dup_high(a[0]), dup_high(b[1]));
        if cr < 0 || cw < 0 {
            return Err("fcntl failed".into());
        }
        let mut cmd = Command::new(&chrome);
        cmd.args(FLAGS).arg(format!("--user-data-dir={profile}"));
        if let Ok(extra) = std::env::var("GUI_FLAGS") {
            cmd.args(extra.split_whitespace());
        }
        cmd.arg("about:blank")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        // SAFETY: only async-signal-safe calls (dup2) between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if dup2(cr, 3) < 0 || dup2(cw, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| format!("{chrome}: {e}"))?;
        // SAFETY: the browser's ends are no longer needed here; ours become Files we own.
        let (w, mut r) = unsafe {
            close(cr);
            close(cw);
            (File::from_raw_fd(a[1]), File::from_raw_fd(b[0]))
        };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let (mut buf, mut chunk) = (Vec::new(), [0u8; 65536]);
            loop {
                let n = match r.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                while let Some(p) = buf.iter().position(|&c| c == 0) {
                    let msg: Vec<u8> = buf.drain(..=p).collect();
                    if let Ok(j) = parse(&msg[..p]) {
                        if tx.send(j).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut c = Cdp {
            child,
            w,
            rx,
            id: 0,
            events: VecDeque::new(),
            session: String::new(),
            profile,
        };
        let t = c.call_on(false, "Target.createTarget", r#"{"url":"about:blank"}"#, 30)?;
        let target = t
            .get("targetId")
            .and_then(J::str)
            .ok_or("no targetId")?
            .to_string();
        let s = c.call_on(
            false,
            "Target.attachToTarget",
            &format!(r#"{{"targetId":{},"flatten":true}}"#, q(&target)),
            30,
        )?;
        c.session = s
            .get("sessionId")
            .and_then(J::str)
            .ok_or("no sessionId")?
            .to_string();
        c.call("Page.enable", "{}")?;
        c.call(
            "Emulation.setDeviceMetricsOverride",
            r#"{"width":640,"height":400,"deviceScaleFactor":1,"mobile":false}"#,
        )?;
        Ok(c)
    }

    fn call_on(
        &mut self,
        session: bool,
        method: &str,
        params: &str,
        secs: u64,
    ) -> Result<J, String> {
        self.id += 1;
        let id = self.id;
        let sess = if session {
            format!(r#","sessionId":{}"#, q(&self.session))
        } else {
            String::new()
        };
        let msg = format!(
            r#"{{"id":{id},"method":{},"params":{params}{sess}}}"#,
            q(method)
        );
        self.w
            .write_all(msg.as_bytes())
            .and_then(|_| self.w.write_all(&[0]))
            .map_err(|e| format!("{method}: {e}"))?;
        let until = Instant::now() + Duration::from_secs(secs);
        loop {
            let m = self
                .next(until)
                .ok_or_else(|| format!("{method}: no answer in {secs} s"))?;
            if m.get("id").and_then(J::num) == Some(id as f64) {
                if let Some(e) = m.get("error") {
                    let t = e.get("message").and_then(J::str).unwrap_or("error");
                    return Err(format!("{method}: {t}"));
                }
                return Ok(m.get("result").cloned().unwrap_or(J::Null));
            }
            if m.get("method").is_some() {
                self.events.push_back(m);
            }
        }
    }

    fn call(&mut self, method: &str, params: &str) -> Result<J, String> {
        self.call_on(true, method, params, 30)
    }

    fn next(&mut self, until: Instant) -> Option<J> {
        let left = until.checked_duration_since(Instant::now())?;
        match self.rx.recv_timeout(left) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Forget every event seen so far (before an action whose own events are awaited).
    fn forget(&mut self) {
        while self.rx.try_recv().is_ok() {}
        self.events.clear();
    }

    fn wait_event(&mut self, method: &str, secs: u64) -> bool {
        let mine = |m: &J, s: &str| {
            m.get("method").and_then(J::str) == Some(method)
                && m.get("sessionId").and_then(J::str) == Some(s)
        };
        if let Some(p) = self.events.iter().position(|m| mine(m, &self.session)) {
            self.events.drain(..=p);
            return true;
        }
        let until = Instant::now() + Duration::from_secs(secs);
        while let Some(m) = self.next(until) {
            if mine(&m, &self.session) {
                return true;
            }
        }
        false
    }

    fn eval(&mut self, expr: &str) -> Result<String, String> {
        let r = self.call(
            "Runtime.evaluate",
            &format!(r#"{{"expression":{},"returnByValue":true}}"#, q(expr)),
        )?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(format!(
                "script: {}",
                e.path(&["exception", "description"])
                    .and_then(J::str)
                    .unwrap_or("exception")
            ));
        }
        Ok(r.path(&["result", "value"])
            .and_then(J::str)
            .unwrap_or("")
            .to_string())
    }

    /// Open a URL and wait for its load event; Err says what the browser said instead.
    fn open(&mut self, url: &str, secs: u64) -> Result<(), String> {
        self.forget();
        let r = self.call("Page.navigate", &format!(r#"{{"url":{}}}"#, q(url)))?;
        if let Some(e) = r.get("errorText").and_then(J::str) {
            if !e.is_empty() {
                return Err(e.to_string());
            }
        }
        if self.wait_event("Page.loadEventFired", secs) {
            Ok(())
        } else {
            Err(format!("no load event in {secs} s"))
        }
    }

    fn click(&mut self, x: i64, y: i64) -> Result<(), String> {
        for (t, extra) in [
            ("mouseMoved", ""),
            ("mousePressed", r#","button":"left","clickCount":1"#),
            ("mouseReleased", r#","button":"left","clickCount":1"#),
        ] {
            self.call(
                "Input.dispatchMouseEvent",
                &format!(r#"{{"type":"{t}","x":{x},"y":{y}{extra}}}"#),
            )?;
        }
        Ok(())
    }

    fn shot(&mut self, file: &str) {
        if let Ok(r) = self.call("Page.captureScreenshot", r#"{"format":"png"}"#) {
            if let Some(d) = r.get("data").and_then(J::str) {
                let _ = fs::write(file, b64decode(d));
            }
        }
    }

    fn close(mut self) {
        let _ = self.call_on(false, "Browser.close", "{}", 5);
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            if Instant::now() >= until {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_dir_all(&self.profile);
    }
}

// ---- what the voter does and sees -----------------------------------------------------------

/// The vote app's own confirmation (its template's script, which needs jQuery): the chosen button
/// disabled with the check icon, the other one at opacity 0.5.
const SCREEN_JS: &str = r#"(function(){var a=document.getElementById('a'),b=document.getElementById('b');
if(!a||!b){return 'none page='+document.title;}
function on(e,o){return e.disabled&&!!e.querySelector('i.fa-check-circle')&&getComputedStyle(o).opacity==='0.5';}
if(on(a,b)){return 'a';} if(on(b,a)){return 'b';}
return 'none form-unchanged';})()"#;

fn button_center(opt: &str) -> String {
    format!(
        "(function(){{var e=document.getElementById('{opt}');if(!e){{return '';}}var r=e.getBoundingClientRect();\
         return Math.round(r.left+r.width/2)+' '+Math.round(r.top+r.height/2);}})()"
    )
}

fn vote_one(c: &mut Cdp, i: u64, opt: &str, shots: &Option<String>) -> (String, String, String) {
    let v = format!("voter{i}");
    let none = |clicked: &str, why: String| (clicked.to_string(), "none".to_string(), why);
    if let Err(e) = c.call(
        "Network.setCookie",
        &format!(
            r#"{{"name":"voter_id","value":{},"url":"http://vote/"}}"#,
            q(&v)
        ),
    ) {
        return none("-", word(&e));
    }
    if let Err(e) = c.open("http://vote/", 15) {
        return none("-", format!("vote-page:{}", word(&e)));
    }
    let xy = match c.eval(&button_center(opt)) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            let t = c.eval("document.title").unwrap_or_default();
            return none("-", format!("no-button:{}", word(&t)));
        }
        Err(e) => return none("-", word(&e)),
    };
    let mut it = xy.split(' ').filter_map(|n| n.parse::<i64>().ok());
    let (x, y) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    c.forget();
    if let Err(e) = c.click(x, y) {
        return none(opt, word(&e));
    }
    if !c.wait_event("Page.loadEventFired", 15) {
        if let Some(d) = shots {
            c.shot(&format!("{d}/{v}.png"));
        }
        return none(opt, "no-answer-in-15s".into());
    }
    let seen = c.eval(SCREEN_JS).unwrap_or_else(|e| format!("none {e}"));
    if let Some(d) = shots {
        c.shot(&format!("{d}/{v}.png"));
    }
    let mut parts = seen.splitn(2, ' ');
    let screen = parts.next().unwrap_or("none").to_string();
    (
        opt.to_string(),
        screen,
        parts.next().map(word).unwrap_or_default(),
    )
}

fn vote(from: u64, to: u64) -> i32 {
    let shots = match std::env::var("GUI_SHOTS").unwrap_or_else(|_| "/tmp/gui/shots".into()) {
        s if s == "-" => None,
        s => {
            let _ = fs::create_dir_all(&s);
            Some(s)
        }
    };
    let mut c = match Cdp::launch(&format!("v{from}")) {
        Ok(c) => c,
        Err(e) => {
            for i in from..to {
                println!("voter{i} clicked=- screen=none why=browser:{}", word(&e));
            }
            return 0;
        }
    };
    for i in from..to {
        let opt = if i % 2 == 0 { "a" } else { "b" };
        let (clicked, screen, why) = vote_one(&mut c, i, opt, &shots);
        let why = if why.is_empty() {
            String::new()
        } else {
            format!(" why={why}")
        };
        println!("voter{i} clicked={clicked} screen={screen}{why}");
        let _ = std::io::stdout().flush();
    }
    c.close();
    0
}

/// What the result app renders for a votes and b votes (its index.html and app.js).
fn expected(ab: Option<(u64, u64)>) -> (Vec<String>, String, String) {
    match ab {
        None => (
            vec![String::new(), "No votes yet".into()],
            "50.0%".into(),
            "50.0%".into(),
        ),
        Some((a, b)) => {
            let t = a + b;
            let text = match t {
                0 => "No votes yet".to_string(),
                1 => "1 vote".to_string(),
                _ => format!("{t} votes"),
            };
            // Math.round(a / (a + b) * 100) with the same double arithmetic as the page
            let pa = if t > 0 {
                (a as f64 / t as f64 * 100.0 + 0.5).floor() as i64
            } else {
                50
            };
            (vec![text], format!("{pa}.0%"), format!("{}.0%", 100 - pa))
        }
    }
}

const RESULTS_JS: &str = r#"(function(){var r=document.getElementById('result');
if(!r){return 'nopage|'+document.title+'|';}
var s=document.querySelectorAll('.stat');
return r.innerText.trim()+'|'+(s[0]?s[0].innerText.trim():'')+'|'+(s[1]?s[1].innerText.trim():'');})()"#;

fn results(ab: Option<(u64, u64)>) -> i32 {
    let wait: u64 = std::env::var("GUI_WAIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let (texts, ea, eb) = expected(ab);
    let db = match ab {
        Some((a, b)) => format!("PostgreSQL counts a={a} b={b}"),
        None => "PostgreSQL has no votes table yet".into(),
    };
    let mut c = match Cdp::launch("r") {
        Ok(c) => c,
        Err(e) => {
            println!("the browser did not start: {e}");
            return 1;
        }
    };
    if let Err(e) = c.open("http://result/", wait) {
        c.shot("/tmp/gui/results.png");
        println!("the results page did not load ({e}); {db}");
        c.close();
        return 1;
    }
    let until = Instant::now() + Duration::from_secs(wait);
    let (rc, last) = loop {
        let last = c.eval(RESULTS_JS).unwrap_or_else(|e| format!("error|{e}|"));
        let f: Vec<&str> = last.split('|').collect();
        let (t, sa, sb) = (
            f.first().copied().unwrap_or(""),
            f.get(1).copied().unwrap_or(""),
            f.get(2).copied().unwrap_or(""),
        );
        if texts.iter().any(|x| x == t) && sa == ea && sb == eb {
            break (0, last);
        }
        if Instant::now() >= until {
            break (1, last);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    c.shot("/tmp/gui/results.png");
    let f: Vec<&str> = last.split('|').collect();
    let shown = if f.first() == Some(&"nopage") {
        format!(
            "a page without results (title {:?})",
            f.get(1).copied().unwrap_or("")
        )
    } else {
        format!(
            "{:?}, Cats {}, Dogs {}",
            f.first().copied().unwrap_or(""),
            f.get(1).copied().unwrap_or(""),
            f.get(2).copied().unwrap_or("")
        )
    };
    if rc == 0 {
        println!("the results page shows {shown}, as {db}");
    } else {
        println!(
            "the results page shows {shown} after {wait} s, but {db} ({:?}, Cats {ea}, Dogs {eb})",
            texts.last().cloned().unwrap_or_default()
        );
    }
    c.close();
    rc
}

/// Whether the results page shows a vote total at all ("No votes yet", "1 vote", "<n> votes").
fn shows_total(t: &str) -> bool {
    t == "No votes yet"
        || t == "1 vote"
        || t.strip_suffix(" votes")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

/// The results page while people vote: it loads and shows a total within $GUI_WAIT s.
fn live() -> i32 {
    let wait: u64 = std::env::var("GUI_WAIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let mut c = match Cdp::launch("l") {
        Ok(c) => c,
        Err(e) => {
            println!("the browser did not start: {e}");
            return 1;
        }
    };
    if let Err(e) = c.open("http://result/", wait) {
        c.shot("/tmp/gui/live.png");
        println!("the results page did not load ({e}) while PostgreSQL answers");
        c.close();
        return 1;
    }
    let until = Instant::now() + Duration::from_secs(wait);
    let (rc, last) = loop {
        let last = c.eval(RESULTS_JS).unwrap_or_else(|e| format!("error|{e}|"));
        if shows_total(last.split('|').next().unwrap_or("")) {
            break (0, last);
        }
        if Instant::now() >= until {
            break (1, last);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    c.shot("/tmp/gui/live.png");
    let t = last.split('|').next().unwrap_or("").to_string();
    if rc == 0 {
        println!("the results page shows {t:?}");
    } else {
        println!(
            "the results page shows no vote total after {wait} s ({t:?}) while PostgreSQL answers"
        );
    }
    c.close();
    rc
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let _ = fs::create_dir_all("/tmp/gui");
    let num = |i: usize| args.get(i).and_then(|s| s.parse::<u64>().ok());
    let rc = match args.get(1).map(String::as_str) {
        Some("vote") if num(2).is_some() && num(3).is_some() => {
            vote(num(2).unwrap_or(0), num(3).unwrap_or(0))
        }
        Some("results") if args.len() == 4 && args[2] == "-" && args[3] == "-" => results(None),
        Some("results") if num(2).is_some() && num(3).is_some() => {
            results(Some((num(2).unwrap_or(0), num(3).unwrap_or(0))))
        }
        Some("live") => live(),
        Some("probe") => match Cdp::launch("probe") {
            Ok(mut c) => {
                let v = c.call_on(false, "Browser.getVersion", "{}", 30);
                println!(
                    "{}",
                    v.ok()
                        .and_then(|v| v.get("product").and_then(J::str).map(String::from))
                        .unwrap_or_default()
                );
                c.close();
                0
            }
            Err(e) => {
                println!("the browser did not start: {e}");
                1
            }
        },
        _ => {
            eprintln!("usage: hydo-gui vote FROM TO | results A B | results - - | live | probe");
            2
        }
    };
    std::process::exit(rc);
}
