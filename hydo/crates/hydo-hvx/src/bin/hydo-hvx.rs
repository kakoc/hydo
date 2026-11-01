//! `hydo-hvx`: the hvx run engine with hvx_explore's command line and protocol (M132).
//!
//! ```text
//! hydo-hvx [--ram-mib N] [--vcpus N] [--cpu v2|v3] [--control [boot]] serve|one <bzImage> <initramfs> <disk.img> <outdir> ["<schedule>"]
//! ```

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    std::process::exit(hydo_hvx::engine::main_stdio(&args));
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        eprintln!("hydo-hvx: the hvx engine needs Linux and the hvx kernel module (/dev/hvx)");
        std::process::exit(1);
    }
}
