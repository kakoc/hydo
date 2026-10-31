//! `load.FromReflection`: asks a running server (gRPC server reflection, v1) for every service it
//! exposes and the files that define them, with their imports. Each question is one call of
//! `ServerReflectionInfo` (the Go generator asked them on one stream; the answers are the same).

use crate::desc::{self, canonical, file_info, FileBytes, Origin};
use hydo_grpc::outcome::code_name;
use hydo_grpc::transport::Channel;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::time::Instant;

const PATH: &str = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";

/// A ServerReflectionRequest with one string field of the message_request oneof.
fn request(field: u32, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    desc::put_len(&mut out, field, value.as_bytes());
    out
}

fn fields(b: &[u8]) -> Result<Vec<(u32, u8, Vec<u8>, u64)>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let key = desc::get_varint(b, &mut i)?;
        let (num, wt) = ((key >> 3) as u32, (key & 7) as u8);
        match wt {
            0 => {
                let v = desc::get_varint(b, &mut i)?;
                out.push((num, wt, Vec::new(), v));
            }
            1 => i += 8,
            5 => i += 4,
            2 => {
                let n = desc::get_varint(b, &mut i)? as usize;
                let s = b.get(i..i + n).ok_or("unexpected EOF")?;
                out.push((num, wt, s.to_vec(), 0));
                i += n;
            }
            _ => return Err("invalid wire type".into()),
        }
    }
    Ok(out)
}

enum Answer {
    Files(Vec<Vec<u8>>),
    Services(Vec<String>),
}

async fn ask(ch: &Channel, field: u32, value: &str, deadline: Instant) -> Result<Answer, String> {
    let (msgs, st) = ch.call(PATH, &[request(field, value)], true, deadline).await;
    let Some(m) = msgs.first() else {
        if st.code != 0 {
            return Err(format!("rpc error: code = {} desc = {}", go_code(st.code), st.msg));
        }
        return Err("EOF".into());
    };
    for (num, _, data, _) in fields(m)? {
        match num {
            4 => {
                let files = fields(&data)?.into_iter().filter(|f| f.0 == 1).map(|f| f.2).collect();
                return Ok(Answer::Files(files));
            }
            6 => {
                let mut names = Vec::new();
                for (n, _, svc, _) in fields(&data)? {
                    if n == 1 {
                        for (k, _, name, _) in fields(&svc)? {
                            if k == 1 {
                                names.push(String::from_utf8_lossy(&name).into_owned());
                            }
                        }
                    }
                }
                return Ok(Answer::Services(names));
            }
            7 => {
                let (mut code, mut msg) = (0u64, String::new());
                for (k, _, d, v) in fields(&data)? {
                    if k == 1 {
                        code = v;
                    } else if k == 2 {
                        msg = String::from_utf8_lossy(&d).into_owned();
                    }
                }
                return Err(format!("reflection error {}: {}", code as i32, msg));
            }
            _ => {}
        }
    }
    Err("reflection: an answer without a response".into())
}

/// grpc-go's codes.Code String (CamelCase).
fn go_code(c: u32) -> String {
    let n = code_name(c);
    if n == "CANCELLED" {
        return "Canceled".into();
    }
    n.split('_').map(|w| {
        let mut cs = w.chars();
        match cs.next() {
            Some(f) => f.to_string() + &cs.as_str().to_lowercase(),
            None => String::new(),
        }
    }).collect()
}

fn skip_service(name: &str) -> bool {
    name.starts_with("grpc.reflection.") || name.starts_with("grpc.health.")
}

/// Every service of the server at `target` and the files that define them.
pub fn from_reflection(target: &str, timeout: Duration) -> Result<Vec<FileBytes>, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let ch = Channel::new(target);
        let deadline = Instant::now() + timeout;
        let svcs = match ask(&ch, 7, "*", deadline).await {
            Ok(Answer::Services(s)) => s,
            Ok(_) => return Err(format!("reflection on {target}: an unexpected answer (is server reflection enabled?)")),
            Err(e) => return Err(format!("reflection on {target}: {e} (is server reflection enabled?)")),
        };
        let mut got: BTreeMap<String, FileBytes> = BTreeMap::new();
        let take = |got: &mut BTreeMap<String, FileBytes>, a: Answer| -> Result<(), String> {
            if let Answer::Files(fs) = a {
                for b in fs {
                    let f = file_info(b)?;
                    got.insert(f.name.clone(), f);
                }
            }
            Ok(())
        };
        for s in &svcs {
            if skip_service(s) {
                continue;
            }
            let a = ask(&ch, 4, s, deadline).await.map_err(|e| format!("reflection: service {s}: {e}"))?;
            take(&mut got, a)?;
        }
        loop {
            let mut missing: Vec<String> = Vec::new();
            for f in got.values() {
                for d in &f.deps {
                    if !got.contains_key(d) {
                        missing.push(d.clone());
                    }
                }
            }
            missing.sort();
            let mut changed = false;
            for d in missing {
                if got.contains_key(&d) {
                    continue;
                }
                let a = ask(&ch, 3, &d, deadline).await.map_err(|e| format!("reflection: file {d}: {e}"))?;
                take(&mut got, a)?;
                changed = true;
            }
            if !changed {
                break;
            }
        }
        canonical(got.into_values().collect(), Origin::Wire)
    })
}
