//! The storage faults of one service (docs/design/storage-faults.md; MANIFEST.md "Storage faults
//! of one service"): `G=` corrupts files in a service's storage directory, `L=` is a power loss of
//! that service alone. Both are driven from the host through the agent's existing commands
//! (`run` with the image's busybox, `read`, `mount`, `signal`), so they need no new agent, no
//! engine change and no kernel module: at a defined stop of the run the controller lists the
//! directory, decides from the token and the listing what to change (`hydo_schedule::Corruption`
//! `plan`, a pure function) and changes it, and the transcript records every command.
//!
//! - **Own file systems.** A service with `storage: {dir:, size_mib:}` has its directory on a
//!   journaled ext4 partition of its own (the data disk's logical partitions /dev/vda5, ...,
//!   made by hydo-build), mounted on the directory right after the data disk ([`mount_all`]),
//!   with the data disk's options and `errors=remount-ro`. A whole-guest power cut (`C=`, `cf=`)
//!   acts on it as on the rest of the disk.
//! - **`L=`, the power loss of one service** ([`power_loss`]): the service's process tree is
//!   stopped (every thread, as at the instant the power goes), the kernel is told that its file
//!   system failed (ext4's `trigger_fs_error` in sysfs, with `errors=remount-ro`: the journal is
//!   aborted and the file system marked shut down, so no more of its data or metadata reaches the
//!   disk), the tree is killed, the file system unmounted (the kernel drops the pages it had not
//!   written back) and mounted again (its journal replayed, as at a boot after a power loss).
//!   What the service wrote and the kernel had written back stays (a disk that kept every write
//!   it was given); what it had not is gone. The other services run on. The service starts again
//!   before step at+down.
//! - **The strict `L=`** (Milestone 245; [`strict_start`], [`links_up`]): before the first step
//!   the guest's periodic writeback is turned off and the service's file system mounted again
//!   without `auto_da_alloc`, so its disk receives only what it syncs; at the loss its bridge
//!   port is set down right after it stops (its peers hear nothing, also not the end of its
//!   connections) and up again before it starts.
//! - **`G=`, corruption** ([`corrupt`]): with `+down` the service is killed first and started
//!   again before step at+down; without it the files change under the running service. A list
//!   or a group's selector (Milestone 245) kills every service first, then corrupts each. The
//!   changes are written through the page cache with busybox `dd`, so they are subject to the
//!   same durability as any write (a later whole-guest power cut may take them away).
//! - **A `cf=` run** (Milestone 271, open item 127): its data disk swallows every request from
//!   the k-th flush on, and a mount or a read that the agent makes itself would then wait
//!   forever, with the agent, so that nothing could read the ids for the recovery boot. In such
//!   a run the mounts and the reads of these faults are made by busybox in a child process
//!   ([`in_child`]), which alone waits. Every other run sends the agent's own commands as before.

use super::{
    bb, busybox, call, die_oracle, env, log, run, secs, sh, svc_sig, xassert, Conversation, Ctx, F,
};
use crate::flow::Step;
use crate::plan::PlanStorage;
use hydo_agent_proto::msg::{Command, Mkdir, Mount, MountFlag, Read, Reply, Sig, Signal};
use hydo_schedule::{Corruption, Edit};

/// The assertion declarations of a schedule with G= or L= (type, id, message).
pub(super) const CORRUPT_DECL: (&str, &str, &str) = (
    "reachable",
    "fault.corrupted",
    "files in a service's storage were corrupted",
);
pub(super) const POWER_LOSS_DECL: (&str, &str, &str) = (
    "reachable",
    "fault.service_power_loss",
    "one service lost its power (its own file system shut down)",
);

/// The file the directory listing is written to (read back in chunks).
fn list_file(ctx: &Ctx) -> String {
    format!("{}/storage.list", ctx.plan.layout.dir)
}

/// The mount options of an own file system: the data disk's, and errors=remount-ro (what
/// `trigger_fs_error` needs to shut it down).
fn vol_data(ctx: &Ctx) -> String {
    match &ctx.mount_data {
        Some(d) if !d.is_empty() => format!("{d},errors=remount-ro"),
        _ => "errors=remount-ro".into(),
    }
}

/// Milestone 271 (open item 127): whether this conversation's data disk can lose power under
/// the storage faults' commands. In a `cf=` schedule's run the disk swallows every request from
/// its k-th flush on. When that flush was the agent's own (the journal replay of an `L=`'s
/// mount, or a `G=`'s read of a byte), the agent waited on the dark disk forever and answered
/// nothing, so the power cut could not read the ids, and the run ended in an ERROR without its
/// recovery boot. Such a run makes the mounts and reads in a child process, which alone waits;
/// the agent answers on, and the power cut comes at the next stop. Every other conversation
/// sends the agent's own `mount` and `read`, so its transcript and its values stay.
fn in_child(ctx: &Ctx) -> bool {
    matches!(ctx.conv, Conversation::Run) && ctx.run.cf
}

/// A mount: the agent's own `mount`, or busybox `mount` in a child process ([`in_child`]);
/// Err(the words for the verdict) when it fails, as busybox says them in both cases.
fn mount(fx: &mut F<'_>, ctx: &Ctx, m: Mount) -> Step<Result<(), String>> {
    if in_child(ctx) {
        return mount_in_child(fx, ctx, &m);
    }
    let (src, dst) = (m.src.clone(), m.dst.clone());
    let id = fx.send(Command::Mount(m))?;
    Ok(match fx.reply(id)? {
        Ok(_) => Ok(()),
        Err(e) => Err(match e.errno {
            Some(n) => busybox::mount_error(&src, &dst, n, ctx.plan.layout.strerror),
            None => e.msg,
        }),
    })
}

/// `m` as busybox `mount -t <type> -o <flags and data> <src> <dst>` in a child process;
/// Err(busybox's words) when it fails.
fn mount_in_child(fx: &mut F<'_>, ctx: &Ctx, m: &Mount) -> Step<Result<(), String>> {
    let mut opts: Vec<String> = m
        .flags
        .iter()
        .flatten()
        .map(|f| f.as_str().to_string())
        .collect();
    opts.extend(m.data.iter().filter(|d| !d.is_empty()).cloned());
    let mut argv = vec![bb(ctx), "mount".into(), "-t".into(), m.fstype.clone()];
    if !opts.is_empty() {
        argv.push("-o".into());
        argv.push(opts.join(","));
    }
    argv.push(m.src.clone());
    argv.push(m.dst.clone());
    let r = run(fx, argv, Vec::new(), Some(512))?;
    Ok(if r.status == 0 {
        Ok(())
    } else {
        Err(match r.out.trim() {
            "" => format!("mount: mounting {} on {} failed", m.src, m.dst),
            out => out.to_string(),
        })
    })
}

/// One byte of a file as two hex digits: the agent's own `read`, or busybox `dd` and `od` in a
/// child process ([`in_child`]); None when it cannot be read.
fn read_byte(fx: &mut F<'_>, ctx: &Ctx, path: &str, off: u64) -> Step<Option<String>> {
    if in_child(ctx) {
        let b = bb(ctx);
        let r = run(
            fx,
            sh(
                ctx,
                format!(
                    "{b} dd if={} bs=1 skip={off} count=1 2>/dev/null | {b} od -An -tx1",
                    q(path)
                ),
            ),
            Vec::new(),
            Some(64),
        )?;
        let v = r.out.trim().to_string();
        return Ok((r.status == 0 && v.len() == 2).then_some(v));
    }
    let r = call(
        fx,
        Command::Read(Read {
            path: path.into(),
            offset: Some(off),
            max: 1,
            hex: Some(true),
        }),
    )?;
    Ok(match r {
        Ok(Reply::Read(r)) => Some(r.data.trim().to_string()),
        _ => None,
    })
}

/// Mount one own file system on its directory; Err(the words for the verdict) when it fails.
fn mount_one(fx: &mut F<'_>, ctx: &Ctx, st: &PlanStorage, dev: &str) -> Step<Result<(), String>> {
    mount(
        fx,
        ctx,
        Mount {
            src: dev.into(),
            dst: st.dir.clone(),
            fstype: ctx.plan.layout.data_fstype.clone(),
            flags: (!ctx.mount_flags.is_empty()).then(|| ctx.mount_flags.clone()),
            data: Some(vol_data(ctx)),
        },
    )
}

/// After the data disk is mounted: every storage directory made, every own file system mounted
/// on it. `verdict` is the data disk's (a boot's ERROR, a recovery's VIOLATION).
pub(super) fn mount_all(fx: &mut F<'_>, ctx: &Ctx, verdict: &str) -> Step<()> {
    for st in &ctx.plan.storage {
        super::post(
            fx,
            Command::Mkdir(Mkdir {
                path: st.dir.clone(),
                mode: None,
            }),
        )?;
        let Some(dev) = st.dev(&ctx.plan.layout) else {
            continue;
        };
        if let Err(words) = mount_one(fx, ctx, st, &dev)? {
            return die_oracle(fx, format!("{verdict} ({words})"));
        }
        log(
            fx,
            format!("mounted {dev} on {} ({}'s own file system)", st.dir, st.svc),
        );
    }
    Ok(())
}

/// Single-quoted for sh.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The regular files under `dir` with their sizes (paths relative to `dir`).
fn list(fx: &mut F<'_>, ctx: &Ctx, dir: &str) -> Step<Vec<(String, u64)>> {
    let b = bb(ctx);
    let out = list_file(ctx);
    let text = format!(
        "{b} find {d} -type f | while IFS= read -r f; do printf '%s %s\\n' \"$({b} wc -c < \"$f\")\" \"${{f#{dq}/}}\"; done > {o}",
        d = q(dir),
        dq = dir,
        o = q(&out),
    );
    run(fx, sh(ctx, text), Vec::new(), None)?;
    let listing = super::read_all(fx, &out)?;
    let mut files = Vec::new();
    for line in listing.lines() {
        let line = line.trim_start();
        if let Some((n, p)) = line.split_once(' ') {
            if let Ok(n) = n.trim().parse::<u64>() {
                files.push((p.to_string(), n));
            }
        }
    }
    Ok(files)
}

/// G=: one file of the service's storage changed as the token says. Logs what was done; a
/// service without storage, or a directory with no file to change, changes nothing.
pub(super) fn corrupt(fx: &mut F<'_>, ctx: &Ctx, svc: &str, how: &Corruption, p: i64) -> Step<()> {
    let Some(st) = ctx.plan.storage_of(svc) else {
        log(fx, format!("G: {svc} has no storage, ignored"));
        return Ok(());
    };
    let files = list(fx, ctx, &st.dir)?;
    let Some((rel, edits)) = how.plan(&files) else {
        log(
            fx,
            format!(
                "G: no file to change in {} ({} files), nothing corrupted",
                st.dir,
                files.len()
            ),
        );
        return Ok(());
    };
    let path = format!("{}/{rel}", st.dir);
    // Milestone 313: a latent sector error under the file (bad, rot) is the engine's
    if let [Edit::Sectors { off, len, rot }] = edits.as_slice() {
        let why = super::sectors::arm(fx, ctx, svc, &path, (*off, *len, *rot))?;
        if !why.is_empty() {
            log(
                fx,
                format!("G: no bad sectors under {svc}'s {path} before step {p}: {why}"),
            );
        }
        return Ok(());
    }
    let b = bb(ctx);
    let mut done: Vec<String> = Vec::new();
    for e in &edits {
        match e {
            // (one Sectors edit is the whole plan of bad and rot, above)
            Edit::Sectors { .. } => {}
            Edit::Flip { off, bit } => {
                let Some(r) = read_byte(fx, ctx, &path, *off)? else {
                    done.push(format!("byte {off} unreadable"));
                    continue;
                };
                let Ok(v) = u8::from_str_radix(&r, 16) else {
                    done.push(format!("byte {off} unreadable"));
                    continue;
                };
                let w = v ^ (1u8 << bit);
                run(
                    fx,
                    sh(
                        ctx,
                        format!(
                            "printf '\\{w:03o}' | {b} dd of={} bs=1 seek={off} count=1 conv=notrunc 2>/dev/null",
                            q(&path)
                        ),
                    ),
                    Vec::new(),
                    None,
                )?;
                done.push(format!("bit {bit} of byte {off} ({v:02x}->{w:02x})"));
            }
            Edit::Zero { off, len } => {
                run(
                    fx,
                    sh(
                        ctx,
                        format!(
                            "{b} dd if=/dev/zero of={} bs=1 seek={off} count={len} conv=notrunc 2>/dev/null",
                            q(&path)
                        ),
                    ),
                    Vec::new(),
                    None,
                )?;
                done.push(format!("{len} bytes from {off} zeroed"));
            }
            Edit::Trunc { size } => {
                run(
                    fx,
                    sh(
                        ctx,
                        format!(
                            "{b} dd if=/dev/null of={} bs=1 seek={size} count=0 2>/dev/null",
                            q(&path)
                        ),
                    ),
                    Vec::new(),
                    None,
                )?;
                done.push(format!("cut to {size} bytes"));
            }
        }
    }
    log(
        fx,
        format!(
            "CORRUPTED {svc}'s {path} before step {p}: {}",
            done.join(", ")
        ),
    );
    let (t, id, msg) = CORRUPT_DECL;
    xassert(fx, t, id, msg);
    Ok(())
}

/// The kernel's name of a partition (`/dev/vda6` -> `vda6`).
fn dev_name(dev: &str) -> String {
    dev.rsplit('/').next().unwrap_or(dev).to_string()
}

/// The sectors written to a partition since the guest's boot (the seventh number of
/// `/sys/class/block/<name>/stat`), None when the file cannot be read.
fn sectors_written(fx: &mut F<'_>, ctx: &Ctx, name: &str) -> Step<Option<u64>> {
    let b = bb(ctx);
    let r = run(
        fx,
        sh(ctx, format!("{b} cat /sys/class/block/{name}/stat")),
        Vec::new(),
        Some(512),
    )?;
    Ok(r.out
        .split_whitespace()
        .nth(6)
        .and_then(|x| x.parse::<u64>().ok()))
}

/// A strict `L=` (Milestone 245), before the workload's first step: from now on Linux writes no
/// dirty page back on its own, so the service's disk receives only what it syncs.
///
/// - `vm.dirty_writeback_centisecs` is set to 0: the kernel's flusher threads no longer write
///   dirty pages back after 30 s. The setting is the guest's, so every service's unsynced
///   writes stay in the page cache too: they see no difference while they run (their reads hit
///   the cache), but a whole-guest power cut in the same run takes more of them away.
/// - The service's own file system is mounted again without ext4's `auto_da_alloc`, which writes
///   a file's data back when a program replaces the file by a rename or truncates it to zero.
///
/// What still reaches the disk without a sync: the journal's metadata every 5 s, and dirty data
/// beyond the kernel's dirty thresholds (`vm.dirty_background_ratio`). The power loss logs both
/// the dirty pages and the sectors written to the partition, so a run shows what was written.
/// Returns the partition's sectors written so far, None when the service has no own file system
/// (the power loss then logs it and does nothing).
pub(super) fn strict_start(fx: &mut F<'_>, ctx: &Ctx, svc: &str) -> Step<Option<u64>> {
    let l = &ctx.plan.layout;
    let Some((st, dev)) = ctx
        .plan
        .storage_of(svc)
        .and_then(|s| s.dev(l).map(|d| (s, d)))
    else {
        return Ok(None);
    };
    let b = bb(ctx);
    let r = run(
        fx,
        sh(
            ctx,
            format!(
                "w=/proc/sys/vm/dirty_writeback_centisecs; {b} cat $w; echo 0 > $w && {b} cat $w"
            ),
        ),
        Vec::new(),
        Some(512),
    )?;
    if r.status != 0 {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR strict power loss of {svc}: the kernel's periodic writeback \
                 could not be turned off ({})",
                r.out.trim()
            ),
        );
    }
    let was = r.out.split_whitespace().next().unwrap_or("?").to_string();
    let mut flags = ctx.mount_flags.clone();
    flags.push(MountFlag::Remount);
    let again = Mount {
        src: dev.clone(),
        dst: st.dir.clone(),
        fstype: l.data_fstype.clone(),
        flags: Some(flags),
        data: Some(format!("{},noauto_da_alloc", vol_data(ctx))),
    };
    // the agent's refusal is the words outside a cf= run, as before Milestone 271
    let mounted = if in_child(ctx) {
        mount_in_child(fx, ctx, &again)?
    } else {
        let id = fx.send(Command::Mount(again))?;
        fx.reply(id)?.map(|_| ()).map_err(|e| e.msg)
    };
    if let Err(words) = mounted {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR strict power loss of {svc}: {dev} could not be mounted \
                 again without auto_da_alloc ({words})"
            ),
        );
    }
    let name = dev_name(&dev);
    let sectors = sectors_written(fx, ctx, &name)?.unwrap_or(0);
    log(
        fx,
        format!(
            "STRICT power loss of {svc} armed: no periodic writeback (vm.dirty_writeback_centisecs \
             {was} -> 0), {dev} without auto_da_alloc, {sectors} sectors written to it so far"
        ),
    );
    Ok(Some(sectors))
}

/// The service's links on the bridge cut or given back (network: bridge): its bridge port
/// `vh<i>` set down or up. Down, nothing the service's namespace sends leaves it and nothing
/// reaches it, so its peers hear nothing, as from a machine without power; its kernel's
/// answers to them (the end of its connections when it is killed) are lost too. Ok(false)
/// without network: bridge (logged).
fn links(fx: &mut F<'_>, ctx: &Ctx, svc: &str, up: bool) -> Step<bool> {
    if !ctx.plan.bridge || ctx.plan.layout.ip.is_empty() {
        if !up {
            log(
                fx,
                format!(
                    "L: {svc}'s links are not cut (the manifest has no network: bridge, so the \
                     services share one network)"
                ),
            );
        }
        return Ok(false);
    }
    let vh = super::netns::veth(ctx, svc);
    let mut argv: Vec<String> = ctx
        .plan
        .layout
        .ip
        .split_whitespace()
        .map(str::to_string)
        .collect();
    argv.extend(
        ["link", "set", vh.as_str(), if up { "up" } else { "down" }]
            .iter()
            .map(|x| x.to_string()),
    );
    let r = run(fx, argv, Vec::new(), Some(512))?;
    if r.status != 0 {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR power loss of {svc}: its bridge port {vh} could not be set {} \
                 ({})",
                if up { "up" } else { "down" },
                r.out.trim()
            ),
        );
    }
    Ok(true)
}

/// `N=<svc>@<at>~<off>:<dur>` (Milestone 393): `off` ms after the step starts, the service's
/// bridge port goes down; `dur` ms later it comes up again, while the step runs. With
/// `browser:` the service is the browser: its page is offline, and its requests to the server
/// and to the fakes fail or wait until the link is back. Without network: bridge it is logged and
/// nothing happens.
pub(super) fn link_cut_timer(fx: &mut F<'_>, ctx: &Ctx, svc: &str, off: u64, dur: u64) -> Step<()> {
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![bb(ctx), "sleep".into(), secs(off, 6)],
        e.clone(),
        None,
    )?;
    if !links(fx, ctx, svc, false)? {
        return Ok(());
    }
    log(fx, format!("LINK of {svc} cut (timer, for {dur}ms)"));
    xassert(
        fx,
        "reachable",
        "fault.link_cut",
        "a service's network link was cut while a step ran",
    );
    run(fx, vec![bb(ctx), "sleep".into(), secs(dur, 6)], e, None)?;
    links(fx, ctx, svc, true)?;
    log(fx, format!("LINK of {svc} back (timer)"));
    Ok(())
}

/// A strict power loss's service is back: its links first (before it starts again).
pub(super) fn links_up(fx: &mut F<'_>, ctx: &Ctx, svc: &str, when: &str) -> Step<()> {
    if links(fx, ctx, svc, true)? {
        log(fx, format!("LINKS of {svc} back {when}"));
    }
    Ok(())
}

/// L=: the power loss of one service (the module comment). Gives (whether the service is down,
/// whether its links are cut); (false, false) also when the service has no own file system
/// (logged, nothing done); a file system that does not come back is the verdict
/// `HYDO-ORACLE: VIOLATION <svc> storage unmountable after its power loss (...)`. `strict`
/// (Milestone 245): the sectors [`strict_start`] found written; the service's links are cut
/// right after it stops, and the dirty pages and the partition's writes are logged. `when` says
/// when in the log lines (`before step 5`, or a reaction's `at the reaction A=...`, Milestone
/// 355).
pub(super) fn power_loss(
    fx: &mut F<'_>,
    ctx: &Ctx,
    svc: &str,
    when: &str,
    strict: Option<u64>,
) -> Step<(bool, bool)> {
    let l = &ctx.plan.layout;
    let Some((st, dev)) = ctx
        .plan
        .storage_of(svc)
        .and_then(|s| s.dev(l).map(|d| (s, d)))
    else {
        log(
            fx,
            format!("L: {svc} has no own file system (storage.size_mib), ignored"),
        );
        return Ok((false, false));
    };
    // the instant the power goes: every thread of the service stops
    super::post(
        fx,
        Command::Signal(Signal {
            h: svc.into(),
            sig: Sig::Stop,
            tree: true,
            wait_stopped: Some(true),
        }),
    )?;
    let name = dev_name(&dev);
    let b = bb(ctx);
    let mut cut = false;
    if let Some(before) = strict {
        // its machine is gone: no packet leaves or reaches it, also not the end of its
        // connections when its processes are killed below
        cut = links(fx, ctx, svc, false)?;
        let r = run(
            fx,
            sh(
                ctx,
                format!("{b} grep -E '^(Dirty|Writeback):' /proc/meminfo"),
            ),
            Vec::new(),
            Some(512),
        )?;
        let mem: Vec<String> = r
            .out
            .lines()
            .map(|x| x.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        let now = sectors_written(fx, ctx, &name)?.unwrap_or(0);
        log(
            fx,
            format!(
                "STRICT power loss of {svc} {when}: {} sectors written to {dev} since \
                 the run's start; the guest's {}{}",
                now.saturating_sub(before),
                mem.join(", "),
                if cut { "; its links cut" } else { "" }
            ),
        );
    }
    let r = run(
        fx,
        sh(
            ctx,
            format!("echo hydo-power-loss > /sys/fs/ext4/{name}/trigger_fs_error"),
        ),
        Vec::new(),
        Some(512),
    )?;
    if r.status != 0 {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR power loss of {svc}: its file system could not be shut down ({})",
                r.out.trim()
            ),
        );
    }
    svc_sig(fx, svc, Sig::Kill)?;
    fx.until(|h, f| !super::running(h, f, svc))?;
    // the dead tree's mount namespace may hold the file system for a moment; an image service
    // also has the directory bound into its root (`mounts:`), and that bind keeps the shut-down
    // file system alive, so mounting the device again failed with "Resource busy" (found by the
    // YugabyteDB onboarding): every mount of the device goes, the deepest first, and the
    // launcher's once-per-boot marker with them, so the restart binds the new mount again
    let r = run(
        fx,
        sh(
            ctx,
            // an image service's command is its launcher (plan.rs)
            if ctx
                .plan
                .service(svc)
                .is_some_and(|s| s.cmd == format!("/etc/hydo/oci/{svc}/run"))
            {
                format!(
                    "n=0; while {b} grep -q '^{dev} ' /proc/mounts; do for m in $({b} awk '$1 == \"{dev}\" {{ print $2 }}' /proc/mounts | {b} sort -r); do {b} umount $m 2>/dev/null; done; n=$((n+1)); [ $n -ge 100 ] && {{ {b} grep '^{dev} ' /proc/mounts; exit 1; }}; {b} grep -q '^{dev} ' /proc/mounts && {b} usleep 50000; done; {b} rm -f /tmp/hydo/oci-mnt.{svc}.done{}",
                    // Milestone 392: the roots of the service's versions bound it too
                    if ctx.plan.service(svc).is_some_and(|s| s.has_versions()) {
                        format!(" /tmp/hydo/oci-mnt.{svc}@*.done")
                    } else {
                        String::new()
                    }
                )
            } else {
                format!(
                    "n=0; until {b} umount {d} 2>/dev/null; do n=$((n+1)); [ $n -ge 100 ] && {{ {b} umount {d}; exit 1; }}; {b} usleep 50000; done",
                    d = q(&st.dir)
                )
            },
        ),
        Vec::new(),
        Some(512),
    )?;
    if r.status != 0 {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR power loss of {svc}: {} stays busy ({})",
                st.dir,
                r.out.trim()
            ),
        );
    }
    if let Err(words) = mount_one(fx, ctx, st, &dev)? {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: VIOLATION {svc} storage unmountable after its power loss ({words})"
            ),
        );
    }
    if strict.is_some() {
        log(
            fx,
            format!(
                "POWER LOSS of {svc} {when} ({dev} shut down and mounted again; what was not synced is lost{})",
                if cut { "; its links stay cut until it starts again" } else { "" }
            ),
        );
    } else {
        log(
            fx,
            format!(
                "POWER LOSS of {svc} {when} ({dev} shut down and mounted again; what was not written back is lost)"
            ),
        );
    }
    let (t, id, msg) = POWER_LOSS_DECL;
    xassert(fx, t, id, msg);
    Ok((true, cut))
}
