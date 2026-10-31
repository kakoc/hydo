//! NET-NAMESPACES (docs/HISTORY.md M124): `network: bridge`, every service in its own network
//! namespace on the bridge hydo0, as `hydo/onboard/hydo-init-netns.sh` sets it up.
//!
//! - [`setup`] is ns_setup and its log line, at the place where /sbin/init reaches the extension:
//!   after the init's start (the run directory, `lo`, the phase line) and before main
//!   (declare_all, S0), so every conversation resumed from S0 or S1 has the network. The pinned
//!   `ip` runs through `run`, in the same order with the same arguments; `nsenter -n<file>` is the
//!   agent's `netns` field.
//! - [`of`]/[`put`]: start_svc's setup, command and readiness check run in the service's
//!   namespace (hydo-init-netns.sh's start_svc puts `nsenter -n<file>` into `$BB`).
//! - [`veth`], [`ns_file`]: the devices NET-FAULTS puts netem on (netfault.rs).
//!
//! AMBIGUOUS (HANDOFF.md, M124): the shell writes `$H/netns.log` and prints it in its log line;
//! the controller keeps that text on the host (the outputs of the commands that wrote to it and
//! the `<svc>=<addr>@vh<i>` lines) and does not write the file. `ns_v6off`'s builtins (`[ -d ]`,
//! `echo 1 >`) run as one `sh -c` with the same text. On failure the last 8 lines of that text
//! are notes (the shell's `tail -8`).

use super::*;
use crate::plan::NET_GW;

/// The namespace file of a service (`$NSD/<svc>`).
pub(super) fn ns_file(ctx: &Ctx, svc: &str) -> String {
    format!("{}/ns/{svc}", ctx.plan.layout.dir)
}

/// The namespace a service's own commands run in: with network: bridge, its file.
pub(super) fn of(ctx: &Ctx, svc: &str) -> Option<String> {
    ctx.plan.bridge.then(|| ns_file(ctx, svc))
}

/// NS_VETH_<svc>: the service's bridge port vh<i>, i = its place in HYDO_SERVICES from 1.
pub(super) fn veth(ctx: &Ctx, svc: &str) -> String {
    match ctx.plan.services.iter().position(|s| s.name == svc) {
        Some(i) => format!("vh{}", i + 1),
        None => String::new(),
    }
}

/// A command run in a network namespace (None: the guest's own).
pub(super) fn put(cmd: Command, ns: &Option<String>) -> Command {
    match cmd {
        Command::Spawn(mut s) => {
            s.netns = ns.clone();
            Command::Spawn(s)
        }
        Command::Run(mut r) => {
            r.netns = ns.clone();
            Command::Run(r)
        }
        Command::Probe(mut p) => {
            p.netns = ns.clone();
            Command::Probe(p)
        }
        c => c,
    }
}

/// ns_mac: `printf "%s:%02x:%02x:%02x:%02x"` of a prefix and the address's four numbers.
pub(super) fn mac(addr: &str, prefix: &str) -> String {
    let f: Vec<&str> = addr.split('.').collect();
    let n = |i: usize| -> u64 {
        f.get(i)
            .map(|x| netfault::awk_num(x).max(0.0) as u64)
            .unwrap_or(0)
    };
    format!(
        "{prefix}:{:02x}:{:02x}:{:02x}:{:02x}",
        n(0),
        n(1),
        n(2),
        n(3)
    )
}

fn ip_words(ctx: &Ctx) -> Vec<String> {
    ctx.plan
        .layout
        .ip
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// `$HYDO_IP <args>` (its output goes to netns.log); whether it exited 0.
fn ip(fx: &mut F<'_>, ctx: &Ctx, args: &[&str], log: &mut String) -> Step<bool> {
    let mut argv = ip_words(ctx);
    argv.extend(args.iter().map(|x| x.to_string()));
    let e = env(fx, ctx, &[]);
    let r = run(fx, argv, e, None)?;
    log.push_str(&r.out);
    Ok(r.status == 0)
}

/// ns_v6off: IPv6 off on a device, if the kernel has IPv6 (the answer is ignored).
fn v6off(fx: &mut F<'_>, ctx: &Ctx, dev: &str, log: &mut String) -> Step<()> {
    let e = env(fx, ctx, &[]);
    let r = run(
        fx,
        sh(
            ctx,
            format!(
                "[ -d /proc/sys/net/ipv6/conf/{dev} ] && echo 1 > /proc/sys/net/ipv6/conf/{dev}/disable_ipv6"
            ),
        ),
        e,
        None,
    )?;
    log.push_str(&r.out);
    Ok(())
}

/// ns_setup: the bridge, one namespace and veth pair per service; false where it `return 1`s.
fn ns_setup(fx: &mut F<'_>, ctx: &Ctx, log: &mut String) -> Step<bool> {
    let plan = &ctx.plan;
    netfault::insmods(fx, ctx, &["llc", "stp", "bridge", "veth"])?;
    post(
        fx,
        Command::Mkdir(Mkdir {
            path: format!("{}/ns", plan.layout.dir),
            mode: None,
        }),
    )?;
    let gw_mac = mac(NET_GW, "02:42");
    if !ip(
        fx,
        ctx,
        &[
            "link",
            "add",
            "hydo0",
            "address",
            &gw_mac,
            "type",
            "bridge",
            "forward_delay",
            "0",
            "stp_state",
            "0",
        ],
        log,
    )? {
        return Ok(false);
    }
    v6off(fx, ctx, "hydo0", log)?;
    let gw = format!("{NET_GW}/16");
    if !ip(fx, ctx, &["addr", "add", &gw, "dev", "hydo0"], log)? {
        return Ok(false);
    }
    for a in &plan.net_extra {
        let a32 = format!("{a}/32");
        if !ip(fx, ctx, &["addr", "add", &a32, "dev", "hydo0"], log)? {
            return Ok(false);
        }
    }
    if !ip(fx, ctx, &["link", "set", "hydo0", "up"], log)? {
        return Ok(false);
    }
    for (i, s) in plan.services.iter().enumerate() {
        let i = i + 1;
        let a = s.addr.clone();
        let f = ns_file(ctx, &s.name);
        // `: > $f && $BB unshare --net=$f true || return 1`
        if call(fx, write_cmd(&f, "", false))?.is_err() {
            return Ok(false);
        }
        let e = env(fx, ctx, &[]);
        let r = run(
            fx,
            vec![
                bb(ctx),
                "unshare".into(),
                format!("--net={f}"),
                "true".into(),
            ],
            e,
            None,
        )?;
        log.push_str(&r.out);
        if r.status != 0 {
            return Ok(false);
        }
        let vh = format!("vh{i}");
        let (m3, m2) = (mac(&a, "02:43"), mac(&a, "02:42"));
        if !ip(
            fx,
            ctx,
            &[
                "link", "add", &vh, "address", &m3, "type", "veth", "peer", "name", "eth0",
                "address", &m2, "netns", &f,
            ],
            log,
        )? {
            return Ok(false);
        }
        v6off(fx, ctx, &vh, log)?;
        if !ip(fx, ctx, &["link", "set", &vh, "master", "hydo0", "up"], log)? {
            return Ok(false);
        }
        let ipx = plan.layout.ip.clone();
        let text = format!(
            "[ -d /proc/sys/net/ipv6/conf/eth0 ] && echo 1 > /proc/sys/net/ipv6/conf/eth0/disable_ipv6\n      {ipx} link set lo up && {ipx} addr add {a}/16 dev eth0 && {ipx} link set eth0 up"
        );
        let e = env(fx, ctx, &[]);
        let r = run_in(fx, sh(ctx, text), e, None, Some(f.clone()))?;
        log.push_str(&r.out);
        if r.status != 0 {
            return Ok(false);
        }
        log.push_str(&format!("{}={a}@{vh}\n", s.name));
    }
    Ok(true)
}

/// The service network before S0 (network: bridge only), with its log line or the verdict.
pub(super) fn setup(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if !ctx.plan.bridge {
        return Ok(());
    }
    let mut text = String::new();
    if ns_setup(fx, ctx, &mut text)? {
        let extra = ctx.plan.net_extra.join(" ");
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!(" + {extra}")
        };
        log(
            fx,
            format!(
                "service network (network: bridge): hydo0 {NET_GW}/16{extra}; {}",
                text.replace('\n', " ")
            ),
        );
        return Ok(());
    }
    // `$BB tail -8 $H/netns.log`
    let ls = lines(&text);
    for l in &ls[ls.len().saturating_sub(8)..] {
        fx.note(l.to_string());
    }
    die_oracle(
        fx,
        "HYDO-ORACLE: ERROR service network setup failed (network: bridge)".into(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn mac_as_ns_mac_prints_it() {
        assert_eq!(super::mac("10.89.0.1", "02:42"), "02:42:0a:59:00:01");
        assert_eq!(super::mac("10.89.0.255", "02:43"), "02:43:0a:59:00:ff");
    }
}
