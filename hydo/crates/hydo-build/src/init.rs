//! /sbin/init for a manifest: hydo-init.sh unchanged, or with the init extensions the manifest
//! needs spliced in before its "main" section (renamed copies of the functions each extension
//! wraps, then the extension file). Port of hydo-build's `init_text`, `_splice`, `_fn_defs` and
//! `install_init`; the extensions stay shell files (they are guest files).

use crate::shell::{
    has_buggify, has_checks, has_clock_after_cut, has_errlog, has_eventually, has_freeze,
    has_links, has_netns, has_world,
};
use crate::{Ctx, Fail};
use regex::Regex;
use serde_json::Value;
use std::path::Path;

const LIVE_MARK: &str = "# ---------------------------------------------------------------- main\n";
const LIVE_WRAP: &[&str] = &["step_range", "start_periodic", "start_all", "finish"];
const FC_WRAP: &[&str] = &["set_knob", "step_range", "restart_dead", "stop_periodic"];
const CLK_WRAP: &[&str] = &["failed_ids", "mount_data"];
const BG_WRAP: &[&str] = &["set_knob"];
const HIST_WRAP: &[&str] = &["start_periodic", "finish"];
const PF_WRAP: &[&str] = &[
    "start_all",
    "set_knob",
    "step_range",
    "restart_dead",
    "failed_ids",
    "final_verdict",
];
const EL_WRAP: &[&str] = &[
    "mount_data",
    "step_range",
    "retry_workload",
    "failed_ids",
    "final_verdict",
    "die_oracle",
];
const NS_WRAP: &[&str] = &["start_svc"];
pub const LINKS_INIT: &str = "hydo-init-links.sh";

/// `_fn_defs(text, fn)`: every definition of shell function fn: one-line `fn() { ...; }` or a
/// block that ends with a `}` at the start of a line.
fn fn_defs(text: &str, f: &str) -> Vec<String> {
    let re = Regex::new(&format!(
        r"(?sm)^{}\(\) \{{(?:[^\n]*\}}\n|.*?^\}}\n)",
        regex::escape(f)
    ))
    .expect("function pattern");
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

fn marker_missing(why: &str) -> Fail {
    Fail::Manifest(format!(
        "hydo-init.sh: the '# ---- main' section marker is missing (needed for {why})"
    ))
}

fn read_ext(ctx: &Ctx, ext: &str) -> Result<String, Fail> {
    crate::read_text(&ctx.onboard.join(ext))
}

/// `_splice(base, fns, prefix, ext, why)`.
fn splice(
    ctx: &Ctx,
    base: &str,
    fns: &[&str],
    prefix: &str,
    ext: &str,
    why: &str,
) -> Result<String, Fail> {
    let i = base.find(LIVE_MARK).ok_or_else(|| marker_missing(why))?;
    let mut copies = String::new();
    for f in fns {
        let defs = fn_defs(&base[..i], f);
        let last = defs.last().ok_or_else(|| {
            Fail::Manifest(format!(
                "hydo-init.sh: function {f}() not found (needed for {why})"
            ))
        })?;
        copies.push_str(prefix);
        copies.push_str(last);
    }
    let txt = read_ext(ctx, ext)?;
    Ok(format!(
        "{}# copies of the init functions {} wraps\n{}\n{}{}",
        &base[..i],
        ext,
        copies,
        txt,
        &base[i..]
    ))
}

fn any_extension(m: &Value) -> Result<bool, Fail> {
    Ok(has_eventually(m)
        || has_links(m)
        || has_freeze(m)
        || has_clock_after_cut(m)?
        || has_buggify(m)
        || has_checks(m)
        || has_world(m)
        || has_netns(m)
        || has_errlog(m))
}

/// `init_text(m)`.
pub fn init_text(ctx: &Ctx, m: &Value) -> Result<String, Fail> {
    let mut base = read_ext(ctx, "hydo-init.sh")?;
    if has_links(m) {
        if base.matches(LIVE_MARK).count() != 1 {
            return Err(marker_missing("addr:"));
        }
        base = base.replace(
            LIVE_MARK,
            &format!(
                ". /etc/hydo/init-links.sh   # added by hydo-build: services with addr:\n{LIVE_MARK}"
            ),
        );
    }
    if has_eventually(m) {
        let i = base
            .find(LIVE_MARK)
            .ok_or_else(|| marker_missing("eventually invariants"))?;
        let mut copies = String::new();
        for f in LIVE_WRAP {
            let re = Regex::new(&format!(r"(?sm)^{}\(\) \{{.*?^\}}\n", regex::escape(f)))
                .expect("function pattern");
            let mo = re.find(&base).ok_or_else(|| {
                Fail::Manifest(format!(
                    "hydo-init.sh: function {f}() not found (needed for eventually invariants)"
                ))
            })?;
            copies.push_str("_live_orig_");
            copies.push_str(mo.as_str());
        }
        let live = read_ext(ctx, "hydo-liveness.sh")?;
        base = format!(
            "{}# copies of the init functions hydo-liveness.sh wraps\n{}\n{}{}",
            &base[..i],
            copies,
            live,
            &base[i..]
        );
    }
    if has_clock_after_cut(m)? {
        base = splice(
            ctx,
            &base,
            CLK_WRAP,
            "_clk_orig_",
            "hydo-init-clock.sh",
            "guest.clock_after_cut",
        )?;
    }
    if has_checks(m) {
        base = splice(
            ctx,
            &base,
            HIST_WRAP,
            "_hist_orig_",
            "hydo-init-history.sh",
            "checks:",
        )?;
    }
    if has_freeze(m) {
        base = splice(
            ctx,
            &base,
            FC_WRAP,
            "_fc_orig_",
            "hydo-init-freeze.sh",
            "faults: freeze",
        )?;
    }
    if has_buggify(m) {
        base = splice(
            ctx,
            &base,
            BG_WRAP,
            "_bg_orig_",
            "hydo-init-buggify.sh",
            "buggify: true",
        )?;
    }
    if has_world(m) {
        base = splice(
            ctx,
            &base,
            PF_WRAP,
            "_pf_orig_",
            "hydo-init-fakes.sh",
            "fakes:/proxies:",
        )?;
    }
    if has_errlog(m) {
        base = splice(
            ctx,
            &base,
            EL_WRAP,
            "_el_orig_",
            "hydo-init-errlog.sh",
            "errors:",
        )?;
    }
    if has_netns(m) {
        let mut fns: Vec<&str> = NS_WRAP.to_vec();
        if has_freeze(m) {
            fns.push("fc_arm");
        }
        base = splice(
            ctx,
            &base,
            &fns,
            "_ns_orig_",
            "hydo-init-netns.sh",
            "network: bridge",
        )?;
    }
    Ok(base)
}

/// `install_init(m, dst)`: write /sbin/init (and /etc/hydo/init-links.sh for addr:), mode 755.
pub fn install_init(ctx: &Ctx, m: &Value, root: &Path) -> Result<(), Fail> {
    let dst = root.join("sbin/init");
    if any_extension(m)? {
        crate::fsx::write_text(&dst, &init_text(ctx, m)?)?;
    } else {
        crate::fsx::copy_file(&ctx.onboard.join("hydo-init.sh"), &dst)?;
    }
    if has_links(m) {
        crate::fsx::copy_file(
            &ctx.onboard.join(LINKS_INIT),
            &root.join("etc/hydo/init-links.sh"),
        )?;
    }
    crate::fsx::chmod(&dst, 0o755)
}
