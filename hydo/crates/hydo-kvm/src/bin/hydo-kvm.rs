//! `hydo-kvm`: the KVM engine as a program, with the Mac engine's protocol on stdin and stdout.
//!
//! ```text
//! hydo-kvm [--ram-mib N] --control boot serve|one <bzImage-kvm> <initramfs> <disk|-> <outdir> ["<schedule>"]
//! ```

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    {
        // the engine's options, read once here (hydo_config)
        let o = hydo_kvm::serve::KvmOptions::from_env(&hydo_config::Env::process());
        let io = hydo_kvm::serve::Io {
            out: Box::new(std::io::LineWriter::new(std::io::stdout())),
            input: Box::new(std::io::BufReader::new(std::io::stdin())),
            diag: Box::new(std::io::stderr()),
            echo: Some(Box::new(std::io::stderr())),
        };
        std::process::exit(hydo_kvm::serve::main_with(&args, &o, io));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        eprintln!("hydo-kvm: the KVM engine needs Linux (/dev/kvm)");
        std::process::exit(1);
    }
}
