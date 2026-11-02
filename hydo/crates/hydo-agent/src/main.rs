//! hydo-agent: the guest's first process under host control (docs/design/guest-control.md).
//!
//! - As PID 1 it mounts /dev (when /dev/null is missing), /proc, /sys and /tmp, runs
//!   /bin/hvx_seedrng when the image has it, takes its console (`hydo.tty=` on the kernel command
//!   line, default /dev/ttyS0) in raw mode without echo, says hello and serves the host's commands.
//! - `hydo-agent serve [--root DIR]` serves on stdin and stdout as a normal process (the native
//!   tests); paths the agent opens itself are taken under DIR.
//! - Run as `hydo_fc` (its file name), or as `hydo-agent fc <args>`, it is the freeze helper of
//!   today's images with hydo_fc.c's command line and output (step A3's proof).
//! - `hydo-agent --version` prints its version and protocol.
//! - M138: when the kernel command line offers the host-call port (`hydo.hc=1`) and the port
//!   answers, PID 1 sends its lines there and reads the host's lines from there (`hc`); the
//!   console then only carries a doorbell byte from the host.
//!
//! The protocol is the crate hydo-agent-proto; the agent adds no decision of its own.

#[cfg(target_os = "linux")]
mod agent;
#[cfg(target_os = "linux")]
mod fc;
#[cfg(target_os = "linux")]
mod hc;
#[cfg(target_os = "linux")]
mod nfq;
#[cfg(target_os = "linux")]
mod sys;
#[cfg(target_os = "linux")]
mod tree;

const USAGE: &str = "usage: hydo-agent                      (as the guest's PID 1)\n       \
                     hydo-agent serve [--root DIR]  (on stdin and stdout)\n       \
                     hydo-agent fc <hydo_fc arguments>\n       \
                     hydo-agent --version";

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let name = args
        .first()
        .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
        .unwrap_or_default();
    if name == "hydo_fc" {
        std::process::exit(fc::main(&args[1..]));
    }
    if sys::getpid() == 1 {
        agent::pid1_setup();
        let path = agent::console_path();
        let fd = loop {
            match agent::open_console(&path) {
                Ok(fd) => break fd,
                // nothing to serve on: try again rather than end PID 1 (a kernel panic)
                Err(_) => sys::sleep_us(1_000_000),
            }
        };
        let io = agent::Io {
            input: fd,
            output: fd,
            root: None,
            pid1: true,
            port: hc::Port::open(),
        };
        run(io);
        // the console ended: PID 1 must not exit
        loop {
            sys::sleep_us(1_000_000_000);
        }
    }
    match args.get(1).map(String::as_str) {
        Some("--version") => println!(
            "hydo-agent {} (agent protocol {})",
            agent::VERSION,
            hydo_agent_proto::PROTO
        ),
        Some("fc") => std::process::exit(fc::main(&args[2..])),
        Some("serve") => {
            let root = match args.get(2).map(String::as_str) {
                None => None,
                Some("--root") if args.len() == 4 => {
                    let r = std::fs::canonicalize(&args[3])
                        .unwrap_or_else(|_| std::path::PathBuf::from(&args[3]));
                    if let Err(e) = std::env::set_current_dir(&r) {
                        eprintln!("hydo-agent: --root {}: {e}", r.display());
                        std::process::exit(2);
                    }
                    Some(r)
                }
                _ => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            run(agent::Io {
                input: 0,
                output: 1,
                root,
                pid1: false,
                port: None,
            });
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "linux")]
fn run(io: agent::Io) {
    match agent::Agent::new(io) {
        Ok(mut a) => {
            a.hello();
            a.serve();
        }
        Err(e) => eprintln!("hydo-agent: cannot start: {e}"),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("hydo-agent runs on Linux only (it is the guest's first process)\n{USAGE}");
    std::process::exit(2);
}
