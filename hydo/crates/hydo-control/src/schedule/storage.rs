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
//! - **`G=`, corruption** ([`corrupt`]): with `+down` the service is killed first and started
//!   again before step at+down; without it the files change under the running service. The
//!   changes are written through the page cache with busybox `dd`, so they are subject to the
//!   same durability as any write (a later whole-guest power cut may take them away).

use super::{bb, busybox, call, die_oracle, log, run, sh, svc_sig, xassert, Ctx, F};
use crate::flow::Step;
use crate::plan::PlanStorage;
use hydo_agent_proto::msg::{Command, Mkdir, Mount, Read, Reply, Sig, Signal};
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

/// Mount one own file system on its directory; Err(the words for the verdict) when it fails.
fn mount_one(fx: &mut F<'_>, ctx: &Ctx, st: &PlanStorage, dev: &str) -> Step<Result<(), String>> {
    let l = &ctx.plan.layout;
    let id = fx.send(Command::Mount(Mount {
        src: dev.into(),
        dst: st.dir.clone(),
        fstype: l.data_fstype.clone(),
        flags: (!ctx.mount_flags.is_empty()).then(|| ctx.mount_flags.clone()),
        data: Some(vol_data(ctx)),
    }))?;
    Ok(match fx.reply(id)? {
        Ok(_) => Ok(()),
        Err(e) => Err(match e.errno {
            Some(n) => busybox::mount_error(dev, &st.dir, n, l.strerror),
            None => e.msg,
        }),
    })
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
    let b = bb(ctx);
    let mut done: Vec<String> = Vec::new();
    for e in &edits {
        match e {
            Edit::Flip { off, bit } => {
                let r = call(
                    fx,
                    Command::Read(Read {
                        path: path.clone(),
                        offset: Some(*off),
                        max: 1,
                        hex: Some(true),
                    }),
                )?;
                let Ok(Reply::Read(r)) = r else {
                    done.push(format!("byte {off} unreadable"));
                    continue;
                };
                let Ok(v) = u8::from_str_radix(r.data.trim(), 16) else {
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

/// L=: the power loss of one service (the module comment). Ok(()) also when the service has no
/// own file system (logged, nothing done); a file system that does not come back is the
/// verdict `HYDO-ORACLE: VIOLATION <svc> storage unmountable after its power loss (...)`.
pub(super) fn power_loss(fx: &mut F<'_>, ctx: &Ctx, svc: &str, p: i64) -> Step<bool> {
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
        return Ok(false);
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
    let name = dev.rsplit('/').next().unwrap_or(&dev).to_string();
    let b = bb(ctx);
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
                    "n=0; while {b} grep -q '^{dev} ' /proc/mounts; do for m in $({b} awk '$1 == \"{dev}\" {{ print $2 }}' /proc/mounts | {b} sort -r); do {b} umount $m 2>/dev/null; done; n=$((n+1)); [ $n -ge 100 ] && {{ {b} grep '^{dev} ' /proc/mounts; exit 1; }}; {b} grep -q '^{dev} ' /proc/mounts && {b} usleep 50000; done; {b} rm -f /tmp/hydo/oci-mnt.{svc}.done"
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
    log(
        fx,
        format!(
            "POWER LOSS of {svc} before step {p} ({dev} shut down and mounted again; what was not written back is lost)"
        ),
    );
    let (t, id, msg) = POWER_LOSS_DECL;
    xassert(fx, t, id, msg);
    Ok(true)
}
