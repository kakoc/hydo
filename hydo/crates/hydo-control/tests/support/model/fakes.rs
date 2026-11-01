//! The shell model's part for `hydo/onboard/hydo-init-fakes.sh` (M89, M125): the extension's
//! functions transliterated from the shell text as blocking code, each called where its wrapper
//! runs (the start of start_all, set_knob's H branch, step_range, the start of restart_dead,
//! the end of failed_ids at a cut, the start of final_verdict) and in the recovery boot's oracle
//! loop. Its files ($H/world.started, pf.tokens, pf.on.<n>, pf.done.<n>, pf.healed, the id
//! files) are fields of [`Pf`].
//!
//! Where the controller decides something the shell leaves open (HANDOFF.md, "Fakes"), the model
//! checks the decision as written there:
//! - the 300 pings 0.1 s apart are one `probe` (every 100 ms, 30 s, while `fakes:world` lives);
//! - `grep -c`, `tail -5`, `head -1`, `wc` work on a `read` of the file;
//! - `ctl dump` and `ctl summary` write their output into $H/world.dump and $H/world.summary
//!   (processes under the handles `fakes:dump` and `fakes:summary`), which are read back;
//! - the dump is not appended to $HYDO_ACKED in the guest (nothing reads it after the cut); the
//!   report line still carries it, and the hand-over gives the `w:` words as the carried world;
//! - in the recovery boot the `w:` words never go into the id files (start_all's `sed` would
//!   remove them before anything reads them); world.carry is written in one piece.

use super::{bad, s, spawn, write, Item, Model, BB, H, R};
use hydo_agent_proto::msg::{Command, Probe, ProbeEnd, Spawn};
use hydo_control::Ending;
use std::collections::BTreeSet;

const WORLD: &str = "/bin/hydo-world";
const CONFIG: &str = "/etc/hydo/world.json";

/// The extension's files and variables.
#[derive(Default, Debug)]
pub struct Pf {
    /// $H/world.started
    started: bool,
    /// $H/pf.tokens: "$PF_N $from $to $1", as text
    tokens: Vec<[String; 4]>,
    /// $H/pf.on.<n>, $H/pf.done.<n>
    on: BTreeSet<String>,
    done: BTreeSet<String>,
    /// $H/pf.healed
    healed: bool,
    /// The recovery boot's $HYDO_ACKED and $HYDO_FAILED as the oracle loop wrote them.
    acked_file: Vec<String>,
    failed_file: Vec<String>,
}

/// The lines a tool sees in a file (a last line without its newline counts).
fn file_lines(t: &str) -> Vec<&str> {
    let mut v: Vec<&str> = t.split('\n').collect();
    if v.last() == Some(&"") {
        v.pop();
    }
    v
}

fn ctl(args: &[&str]) -> Vec<String> {
    let mut v = vec![s(WORLD), s("ctl")];
    for a in args {
        v.push(s(a));
    }
    v
}

impl Model<'_> {
    fn has_world(&self) -> bool {
        self.plan.world.is_some()
    }

    /// A console line of the shell that is not a hydo-ob: log line (tail's output).
    pub(super) fn note_raw(&mut self, line: &str) -> R<()> {
        match self.next_main(line)? {
            Item::Note { line: l, .. } if l == line => Ok(()),
            other => bad(format!(
                "the shell prints {line:?} for people\n  the controller: {other:?}"
            )),
        }
    }

    /// pf_world_start [carry file]
    fn pf_world_start(&mut self, carry: &str) -> R<()> {
        // [ -f $H/world.started ] && return 0; : > $H/world.started
        if self.pf.started {
            return Ok(());
        }
        self.pf.started = true;
        // $BB sh -c "exec $HYDO_WORLD serve /etc/hydo/world.json ${1:+--carry $1}" > $H/world.out 2>&1 &
        let plus = if carry.is_empty() {
            String::new()
        } else {
            format!("--carry {carry}")
        };
        let line = format!("exec {WORLD} serve {CONFIG} {plus}");
        let env = self.env(&[]);
        let out = format!("{H}/world.out");
        let id = self.send(spawn(
            "fakes:world",
            vec![s(BB), s("sh"), s("-c"), line],
            env.clone(),
            &out,
            false,
        ))?;
        // while [ $n -lt 300 ]; do ctl ping && up; kill -0 $pid || break; sleep 0.1; done
        let mut up = false;
        if self.reply(id)?.is_ok() {
            let pid = self.send(Command::Probe(Probe {
                argv: ctl(&["ping"]),
                env: Some(env),
                cwd: None,
                uid: None,
                gid: None,
                netns: None,
                every_ms: 100,
                deadline_ms: 300 * 100,
                alive: Some(s("fakes:world")),
            }))?;
            if self.reply(pid)?.is_ok() {
                let p = self.probes.get(&pid);
                self.waited("the world's ping probe", p.map(|x| x.0))?;
                up = p.unwrap().1.why == ProbeEnd::Ok;
            }
        }
        let text = self.read_all(&out)?;
        if up {
            // log "fake world up: $($BB grep -c '^hydo-world: ' $H/world.out) endpoints (uptime=..)"
            let mut n = 0;
            for l in file_lines(&text) {
                if l.len() >= 12 && &l[..12] == "hydo-world: " {
                    n += 1;
                }
            }
            return self.note(format!("fake world up: {n} endpoints"));
        }
        // $BB tail -5 $H/world.out; die_oracle "..."
        let ls = file_lines(&text);
        let from = if ls.len() > 5 { ls.len() - 5 } else { 0 };
        for l in ls[from..].iter().copied() {
            self.note_raw(l)?;
        }
        self.die(s(
            "HYDO-ORACLE: ERROR the fake outside world (hydo-world) did not start",
        ))
    }

    /// start_all's wrapper (before the services): the carried world in the recovery boot.
    pub(super) fn pf_start_all(&mut self, recovery: bool) -> R<()> {
        if !self.has_world() {
            return Ok(());
        }
        let mut c = String::new();
        // case "$PHASE:$CMD" in serve:oracle*)
        if recovery {
            // $BB grep -h '^w:' $HYDO_ACKED $HYDO_FAILED > $H/world.carry
            let mut carry = String::new();
            let mut n = 0;
            for l in self.pf.acked_file.iter().chain(self.pf.failed_file.iter()) {
                if l.starts_with("w:") {
                    carry.push_str(l);
                    carry.push('\n');
                    n += 1;
                }
            }
            let file = format!("{H}/world.carry");
            self.send(write(&file, &carry, false))?;
            // $BB sed -i '/^w:/d' $HYDO_ACKED $HYDO_FAILED
            self.pf.acked_file.retain(|l| !l.starts_with("w:"));
            self.pf.failed_file.retain(|l| !l.starts_with("w:"));
            self.note(format!(
                "recovery boot: the fake world starts from {n} carried records"
            ))?;
            c = file;
        }
        self.pf_world_start(&c)
    }

    /// The recovery boot's oracle loop: `dst=$HYDO_ACKED; for id in ${CMD#oracle}; do if [ "$id"
    /// = failed ]; then dst=$HYDO_FAILED; else echo "$id" >> $dst; fi; done`.
    pub(super) fn pf_oracle_loop(&mut self, cmd: &str) -> R<()> {
        let rest = cmd.strip_prefix("oracle").unwrap_or(cmd);
        let mut dst_failed = false;
        for id in rest.split_whitespace() {
            if id == "failed" {
                dst_failed = true;
            } else if dst_failed {
                self.pf.failed_file.push(s(id));
            } else {
                self.pf.acked_file.push(s(id));
            }
        }
        // the decision: the w: words are not written (start_all moves them out at once)
        for (f, list) in [
            ("acked", self.pf.acked_file.clone()),
            ("failed", self.pf.failed_file.clone()),
        ] {
            let text: String = list
                .iter()
                .filter(|l| !l.starts_with("w:"))
                // hydo-init-clock.sh's `sed -i '/^clock=/d'` (M122: never written either)
                .filter(|l| self.plan.clock_after_cut.is_none() || !l.starts_with("clock="))
                .map(|l| format!("{l}\n"))
                .collect();
            if !text.is_empty() {
                self.send(write(&format!("{H}/{f}"), &text, true))?;
            }
        }
        Ok(())
    }

    /// pf_add <value of H=>
    pub(super) fn pf_add(&mut self, v: &str) -> R<()> {
        // tgt=${1%%@*}; rest=${1#*@}; rng=${rest%%:*}; from=${rng%-*}; to=${rng#*-}
        let at = v.find('@');
        let tgt = &v[..at.unwrap_or(v.len())];
        let rest = match at {
            Some(i) => &v[i + 1..],
            None => v,
        };
        let rng = &rest[..rest.find(':').unwrap_or(rest.len())];
        let from = &rng[..rng.rfind('-').unwrap_or(rng.len())];
        let to = match rng.find('-') {
            Some(i) => &rng[i + 1..],
            None => rng,
        };
        // case "$from$to" in ""|*[!0-9]*) die_oracle "... (window)";; esac
        let both = format!("{from}{to}");
        if both.is_empty() || both.chars().any(|c| !c.is_ascii_digit()) {
            return self.die(format!(
                "HYDO-ORACLE: ERROR bad protocol fault H={v} (window)"
            ));
        }
        // [ "$rest" != "$1" ] && [ "${rest#*:}" != "$rest" ] || die_oracle
        if rest == v || !rest.contains(':') {
            return self.die(format!("HYDO-ORACLE: ERROR bad protocol fault H={v}"));
        }
        // pf_known "$tgt" || { log "H: no fake or proxy $tgt, ignored"; return 0; }
        let known = self
            .plan
            .world
            .as_ref()
            .is_some_and(|w| w.targets.iter().any(|t| t == tgt));
        if !known {
            return self.note(format!("H: no fake or proxy {tgt}, ignored"));
        }
        if self.pf.tokens.is_empty() {
            self.report(s("HYDO-ASSERT-DECL {\"t\":\"reachable\",\"id\":\"fault.protocol\",\"msg\":\"a protocol fault (H=) was switched on\"}"))?;
        }
        let n = (self.pf.tokens.len() + 1).to_string();
        self.pf.tokens.push([n.clone(), s(from), s(to), s(v)]);
        self.note(format!("protocol fault {n}: H={v}"))
    }

    /// pf_point <step>
    fn pf_point(&mut self, step: i64) -> R<()> {
        let here = step.to_string();
        for [id, from, to, tok] in self.pf.tokens.clone() {
            if to == here && self.pf.on.contains(&id) {
                let e = self.env(&[]);
                self.run(ctl(&["off", &id]), e, None)?;
                self.pf.on.remove(&id);
                self.note(format!("protocol fault {id} off before step {here}"))?;
            }
            let lt = from.parse::<i64>().unwrap() < to.parse::<i64>().unwrap();
            if from == here && lt && !self.pf.done.contains(&id) {
                let e = self.env(&[]);
                let (rc, out) = self.run(ctl(&["on", &id, &tok]), e, None)?;
                if rc != 0 {
                    // $(...) drops the trailing newlines
                    let mut o = out.as_str();
                    while let Some(x) = o.strip_suffix('\n') {
                        o = x;
                    }
                    return self.die(format!("HYDO-ORACLE: ERROR protocol fault H={tok}: {o}"));
                }
                self.pf.on.insert(id.clone());
                self.pf.done.insert(id.clone());
                self.note(format!(
                    "PROTOCOL FAULT {id} on before step {here}: H={tok}"
                ))?;
                self.report(s("HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.protocol\",\"ok\":true,\"msg\":\"a protocol fault (H=) was switched on\"}"))?;
            }
        }
        Ok(())
    }

    /// step_range's wrapper: [$1,$2) split at the H= window boundaries inside it.
    pub(super) fn pf_step_range(&mut self, a: i64, b: i64) -> R<()> {
        // [ -f $H/pf.tokens ] || { _pf_orig_step_range "$@"; return; }
        if self.pf.tokens.is_empty() {
            return self.step_range_orig(a, b);
        }
        self.pf_point(a)?;
        let mut cuts: BTreeSet<i64> = BTreeSet::new();
        for [_, from, to, _] in &self.pf.tokens {
            for x in [from, to] {
                let v: i64 = x.parse().unwrap();
                if v > a && v < b {
                    cuts.insert(v);
                }
            }
        }
        let mut prev = a;
        for p in cuts {
            self.step_range_orig(prev, p)?;
            self.pf_point(p)?;
            prev = p;
        }
        self.step_range_orig(prev, b)
    }

    /// restart_dead's wrapper: every protocol fault off, once.
    pub(super) fn pf_restart_dead(&mut self) -> R<()> {
        if !self.has_world() || self.pf.tokens.is_empty() || self.pf.healed {
            return Ok(());
        }
        self.pf.healed = true;
        let e = self.env(&[]);
        let (rc, _) = self.run(ctl(&["heal"]), e, None)?;
        if rc == 0 {
            self.note(s("protocol faults healed"))?;
        }
        Ok(())
    }

    /// failed_ids's wrapper at a cut: the world's dump, as text ("" without a world).
    pub(super) fn pf_dump(&mut self) -> R<String> {
        if !self.has_world() {
            return Ok(String::new());
        }
        let e = self.env(&[]);
        let f = format!("{H}/world.dump");
        // $HYDO_WORLD ctl dump > $H/world.dump 2>&1 || { log "...$(head -1 ...)"; echo "w:trunc" > ...; }
        let rc = self.spawn_wait("fakes:dump", ctl(&["dump"]), e, &f, false)?;
        let mut d = self.read_all(&f)?;
        if rc != 0 {
            let head = d.lines().next().unwrap_or("").to_string();
            self.note(format!("world dump failed: {head}"))?;
            d = s("w:trunc\n");
        }
        let nl = d.bytes().filter(|b| *b == b'\n').count();
        self.note(format!(
            "fake world frozen at the power cut: {nl} records carried ({} bytes)",
            d.len()
        ))?;
        Ok(d)
    }

    /// What the engine is handed at the cut: the words of the acked line, the world's records
    /// apart (the decision).
    pub(super) fn pf_hand_over(&self, acked: &str, failed: &str) -> Ending {
        let mut ids = Vec::new();
        let mut world = Vec::new();
        for w in acked.split_whitespace() {
            if self.has_world() && w.starts_with("w:") {
                world.push(s(w));
            } else {
                ids.push(s(w));
            }
        }
        Ending::PowerCut {
            acked: ids,
            failed: failed.split_whitespace().map(s).collect(),
            world,
        }
    }

    /// final_verdict's wrapper: `$HYDO_WORLD ctl summary 2> /dev/null | $BB sed 's/^/hydo-world: /'`.
    pub(super) fn pf_final_verdict(&mut self) -> R<()> {
        if !self.has_world() {
            return Ok(());
        }
        let f = format!("{H}/world.summary");
        let id = self.send(Command::Spawn(Spawn {
            h: s("fakes:summary"),
            argv: ctl(&["summary"]),
            env: Some(self.env(&[])),
            cwd: None,
            uid: None,
            gid: None,
            netns: None,
            stdout: Some(f.clone()),
            stderr: None,
            append: None,
            kill_after_ms: None,
        }))?;
        if self.reply(id)?.is_ok() {
            let e = self.exits.get(&id);
            self.waited("the end of ctl summary", e.map(|x| x.0))?;
        }
        let t = self.read_all(&f)?;
        // stdout of final_verdict: into $H/el.verdict when hydo-init-errlog.sh's final_verdict
        // wraps this one (it is spliced after this file), else on the console
        for l in file_lines(&t) {
            self.assert_out(format!("hydo-world: {l}"))?;
        }
        Ok(())
    }
}
