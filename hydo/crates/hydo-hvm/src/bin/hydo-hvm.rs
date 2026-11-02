//! `hydo-hvm`: the Mac run engine with hvmx's command line and protocol (M143), and hvm's boot
//! probes after the word `hvm`.
//!
//! ```text
//! hydo-hvm [--ram-mib N] [--control [boot]] serve|one <Image> <initramfs> <outdir> ["<schedule>"]
//! hydo-hvm hvm run|print|branch <Image> <initrd> [markers...]
//! ```
//!
//! The program needs the hypervisor entitlement:
//! `codesign --force --entitlements hv/hvm/hv.entitlements -s - hydo-hvm`.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        if args.get(1).map(String::as_str) == Some("hvm") {
            let mut a = args.clone();
            a.remove(1);
            let get = |k: &str| std::env::var(k).ok();
            std::process::exit(hydo_hvm::hvm_main::main_with(
                &a,
                &get,
                Box::new(std::io::stdout()),
                Box::new(std::io::stderr()),
            ));
        }
        std::process::exit(hydo_hvm::serve::main_stdio(&args));
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        let _ = args;
        eprintln!("hydo-hvm: the Mac engine needs macOS on Apple Silicon (Hypervisor.framework)");
        std::process::exit(1);
    }
}
