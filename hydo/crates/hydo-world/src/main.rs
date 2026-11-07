//! hydo-world: the outside world of a hydo project (hydo/onboard/MANIFEST.md, "Fake outside
//! services"). One process inside the guest serves every fake HTTP/gRPC service and every fault
//! proxy the manifest declares, records each request it receives (what the outside world saw),
//! sends webhooks back to the system under test, and applies protocol-level faults (`H=` tokens)
//! that the guest init switches on and off at workload steps.
//!
//! ```text
//! hydo-world serve <world.json> [--carry <file>]   run the world (the init starts it)
//! hydo-world ctl <command...>                      talk to it: ping | on <id> <token> | off <id>
//!                                                  | heal | dump | summary
//! hydo-world event <name> <ref>                    record an event of the system/workload
//! hydo-world check <kind> <target> [options]       evaluate a side-effect property
//! ```
//!
//! Everything is a pure function of the guest's execution: request order, sequence numbers and
//! fault decisions (a hash of the fault token and the request's index under it) never depend on
//! host time, so under hvx a schedule gives the same records on every run.

mod check;
mod config;
mod goish;
mod gojson;
mod grpc;
mod h2x;
mod http1;
mod objects;
mod rt;
mod rules;
mod serve;
#[cfg(feature = "tls")]
mod tls;
#[cfg(feature = "tls")]
mod tls_crypto;
mod sign;
mod world;

use std::os::unix::ffi::OsStringExt;

pub fn usage() -> ! {
    eprintln!("usage: hydo-world serve <world.json> [--carry FILE] | ctl <cmd...> | event <name> <ref> | check <kind> <target> [opts]");
    std::process::exit(2);
}

fn main() {
    let args: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
    if args.len() < 2 {
        usage();
    }
    let rest = &args[2..];
    let code = match args[1].as_slice() {
        b"serve" => {
            let s: Vec<String> = rest
                .iter()
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            #[cfg(feature = "tls")]
            tls::start_main(&s);
            serve::serve_main(&s)
        }
        b"ctl" => check::ctl_main(rest),
        b"event" => {
            if args.len() != 4 {
                usage();
            }
            check::ctl_main(&[
                b"event".to_vec(),
                goish::esc(&args[2]).into_bytes(),
                goish::esc(&args[3]).into_bytes(),
            ])
        }
        b"check" => check::check_main(rest),
        _ => usage(),
    };
    std::process::exit(code);
}
