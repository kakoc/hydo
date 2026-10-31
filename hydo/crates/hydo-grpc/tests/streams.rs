//! `hydo-grpc gen <rpc> N` against the Go client's recorded answers
//! (../hydo-grpc-gen/tests/golden/streams.txt, written by the Go hydo-grpc of Milestone 91): the
//! same requests for the same seeds and hints, byte for byte, and the same exit status and
//! message where a request cannot be made (a `from` field with no earlier value). Four schemas,
//! 12 RPCs, 3 seeds, 100 to 300 requests each.

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn request_streams_like_go() {
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hydo-grpc-gen/tests/golden");
    let lines = std::fs::read_to_string(golden.join("streams.txt")).unwrap();
    let state = std::env::temp_dir().join(format!("hydo-grpc-streams-{}", std::process::id()));
    let mut n = 0;
    for l in lines.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let (name, rpc, seed, count, code, out_sha, err_sha) =
            (f[0], f[1], f[2], f[3], f[4], f[5], f[6]);
        let _ = std::fs::remove_dir_all(&state);
        let o = Command::new(env!("CARGO_BIN_EXE_hydo-grpc"))
            .env("HYDO_GRPC_CONSOLE", "off")
            .env_remove("HYDO_GRPC_SEED")
            .args([
                "--dir",
                golden
                    .join("schemas")
                    .join(name)
                    .join("grpc")
                    .to_str()
                    .unwrap(),
            ])
            .args([
                "--state",
                state.to_str().unwrap(),
                "--seed",
                seed,
                "gen",
                rpc,
                count,
            ])
            .output()
            .unwrap();
        let got = (
            o.status.code().unwrap_or(-1).to_string(),
            hex(&Sha256::digest(&o.stdout)),
            hex(&Sha256::digest(&o.stderr)),
        );
        assert_eq!(
            got,
            (code.to_string(), out_sha.to_string(), err_sha.to_string()),
            "{l}\nstderr: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        n += 1;
    }
    assert_eq!(n, 36);
    let _ = std::fs::remove_dir_all(&state);
}
