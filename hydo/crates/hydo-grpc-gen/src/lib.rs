//! hydo-grpc-gen, the host side of hydo's generated gRPC drivers (MANIFEST.md "gRPC services"):
//! it reads a project's service definitions (.proto files, a compiled descriptor set, or a running
//! server's reflection service) and its hints, checks the hints against the schema, and writes
//! the schema, the plan, one composer command per RPC, a starter section and the guest client.
//!
//! Ported from the Go generator (hydo/grpcgen, Milestone 91; docs/HISTORY.md, Milestone 113): the
//! same desc.pb, plan.json, workload files and starter section, byte for byte.

pub mod desc;
pub mod gen;
pub mod load;
pub mod reflect;
pub mod yaml;

/// Runs the generator with `argv` (without the program name) and returns the exit status,
/// printing errors as the Go program did ("hydo-grpc-gen: ...").
pub fn main_with(argv: &[String]) -> i32 {
    match gen::run(argv) {
        Ok(summary) => {
            print!("{summary}");
            0
        }
        Err(gen::Fail(msg, 0)) => {
            eprintln!("{msg}");
            0
        }
        Err(gen::Fail(msg, 2)) => {
            eprintln!("{msg}");
            2
        }
        Err(gen::Fail(msg, code)) => {
            eprintln!("hydo-grpc-gen: {msg}");
            code
        }
    }
}
