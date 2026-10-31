//! hydo's generated gRPC drivers, the guest side (MANIFEST.md "gRPC services"): the plan
//! (`spec`), the seeded value generator (`valuegen`), outcomes, call records, the state shared by
//! one run's processes, the protobuf JSON writer, a small HTTP/2 client, and the client that ties
//! them together (`client`). The program `hydo-grpc` runs inside a hydo guest; the generator
//! (`hydo-grpc-gen`) uses the same plan, generator and transport on the host.
//!
//! Ported from the Go generator (hydo/grpcgen, Milestone 91; docs/HISTORY.md, Milestone 113):
//! the same plan bytes, request streams, outcomes, records and exit codes.

pub mod b64;
pub mod client;
pub mod gofmt;
pub mod gojson;
pub mod jsonin;
pub mod outcome;
pub mod pjson;
pub mod record;
pub mod rng;
pub mod spec;
pub mod state;
pub mod transport;
pub mod valuegen;
pub mod wire;

/// Go's wording of an I/O error ("no such file or directory"), for messages that Go formatted
/// from a *PathError.
pub fn io_msg(e: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => return "no such file or directory".into(),
        K::PermissionDenied => return "permission denied".into(),
        K::ConnectionRefused => return "connection refused".into(),
        K::ConnectionReset => return "connection reset by peer".into(),
        _ => {}
    }
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => {
            let t = &s[..i];
            let mut c = t.chars();
            match c.next() {
                Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
        None => s,
    }
}
