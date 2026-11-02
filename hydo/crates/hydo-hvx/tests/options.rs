//! The engine's switches from the environment (Milestone 140: the host-call port is on by
//! default and `HYDO_HVX_HOSTCALL=0` turns it off; in-place reuse stays opt-in).
#![cfg(target_os = "linux")]

use hydo_hvx::engine::Options;

fn opts(env: &[(&str, &str)]) -> Options {
    Options::from_env(&hydo_config::Env::from_pairs(env.iter().copied()))
}

#[test]
fn the_host_call_port_is_on_by_default() {
    assert!(opts(&[]).hostcall);
    assert!(opts(&[("HYDO_HVX_HOSTCALL", "1")]).hostcall);
    assert!(!opts(&[("HYDO_HVX_HOSTCALL", "0")]).hostcall);
}

#[test]
fn reuse_stays_opt_in() {
    assert!(!opts(&[]).reuse);
    assert!(opts(&[("HYDO_HVX_REUSE", "1")]).reuse);
    assert!(!opts(&[]).check_reset);
}
