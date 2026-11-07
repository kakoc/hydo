//! The TLS front of `tls:` fakes and proxies (hydo/onboard/MANIFEST.md, "HTTPS for fakes and
//! proxies"), built only into the program hydo-world-tls (the cargo feature `tls`).
//!
//! A fake or proxy with `tls: {hosts: [...]}` keeps its plain listener at `addr:port`, and gets a
//! TLS listener at `addr:<tls port>` (443 by default) that holds a certificate for its host names,
//! signed by the image's own test CA (hydo-build writes the CA, each certificate and its key
//! under /etc/hydo/tls/ and names them in world.json). Endpoints that share an address and a TLS
//! port share one listener and are told apart by the name the client asks for (SNI). Each
//! accepted connection is decrypted here and carried over a plain TCP connection to the
//! endpoint's own listener, so routes, records, `H=` rules and HTTP/2 (gRPC) work exactly as in
//! plain text: ALPN offers h2 and http/1.1, and the plain listener recognises HTTP/2 by its
//! preface. A reset the plain side gives (an `H=...:reset` rule) resets the client's connection.
//!
//! What differs from plain text: a request's `from` in the record is hydo-world's own address
//! (the plain connection starts here), not the client's.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde_json::Value;
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

/// One endpoint's TLS side, as world.json names it.
struct Front {
    name: String,
    hosts: Vec<String>,
    /// the endpoint's plain listener
    plain: String,
    key: Arc<CertifiedKey>,
}

/// The certificates of one TLS listener, chosen by SNI (the first endpoint's without SNI or for
/// an unknown name).
#[derive(Debug)]
struct BySni {
    names: BTreeMap<String, usize>,
    keys: Vec<Arc<CertifiedKey>>,
}

impl ResolvesServerCert for BySni {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let i = hello
            .server_name()
            .and_then(|n| self.names.get(&n.to_ascii_lowercase()).copied())
            .unwrap_or(0);
        self.keys.get(i).cloned()
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn front(e: &Value) -> Result<Option<(String, Front)>, String> {
    let Some(t) = e.get("tls").filter(|t| t.is_object()) else {
        return Ok(None);
    };
    let name = s(&e["name"]);
    let addr = s(&e["addr"]);
    let port = s(&e["port"]);
    let tport = t.get("port").map(s).unwrap_or_else(|| "443".into());
    let hosts: Vec<String> = t
        .get("hosts")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|h| s(h).to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let (cf, kf) = (s(&t["cert"]), s(&t["key"]));
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&cf)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("{name}: tls cert {cf}: {e}"))?;
    let key =
        PrivateKeyDer::from_pem_file(&kf).map_err(|e| format!("{name}: tls key {kf}: {e}"))?;
    let sk = crate::tls_crypto::provider()
        .key_provider
        .load_private_key(key)
        .map_err(|e| format!("{name}: tls key {kf}: {e}"))?;
    let lis = crate::goish::join_host_port(&addr, &tport);
    Ok(Some((
        lis,
        Front {
            name,
            hosts,
            plain: crate::goish::join_host_port(&addr, &port),
            key: Arc::new(CertifiedKey::new(certs, sk)),
        },
    )))
}

/// `hydo-world serve <world.json>` in hydo-world-tls: bind every TLS listener the configuration
/// names before the plain world starts (so "ctl ping" answering still means ready), and serve
/// them on a thread of their own. A configuration or bind error ends the program as the plain
/// listeners' errors do.
pub fn start_main(args: &[String]) {
    let Some(cfg_path) = args.first() else { return };
    if let Err(e) = start(cfg_path) {
        eprintln!("hydo-world: {e}");
        std::process::exit(1);
    }
}

type Listener = (
    std::net::TcpListener,
    TlsAcceptor,
    Arc<BTreeMap<String, usize>>,
    Arc<Vec<String>>,
);

fn start(cfg_path: &str) -> Result<(), String> {
    let text = std::fs::read(cfg_path).map_err(|e| format!("{cfg_path}: {e}"))?;
    let v: Value = serde_json::from_slice(&text).map_err(|e| format!("{cfg_path}: {e}"))?;
    let mut by_lis: BTreeMap<String, Vec<Front>> = BTreeMap::new();
    for kind in ["fakes", "proxies"] {
        for e in v.get(kind).and_then(Value::as_array).into_iter().flatten() {
            if let Some((lis, f)) = front(e)? {
                by_lis.entry(lis).or_default().push(f);
            }
        }
    }
    let mut ready: Vec<Listener> = Vec::new();
    for (lis, fronts) in by_lis {
        let mut names = BTreeMap::new();
        let mut keys = Vec::new();
        let mut plains = Vec::new();
        for (i, f) in fronts.iter().enumerate() {
            for h in &f.hosts {
                names.entry(h.clone()).or_insert(i);
            }
            keys.push(f.key.clone());
            plains.push(f.plain.clone());
            eprintln!(
                "hydo-world: {} tls on {lis} ({}) -> {}",
                f.name,
                f.hosts.join(" "),
                f.plain
            );
        }
        let res = BySni {
            names: names.clone(),
            keys,
        };
        let mut sc =
            rustls::ServerConfig::builder_with_provider(Arc::new(crate::tls_crypto::provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(|e| format!("tls {lis}: {e}"))?
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(res));
        sc.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let ln = std::net::TcpListener::bind(&lis)
            .and_then(|l| l.set_nonblocking(true).map(|()| l))
            .map_err(|e| {
                format!(
                    "tls: listen tcp {lis}: bind: {}",
                    crate::goish::errno_text(&e)
                )
            })?;
        ready.push((
            ln,
            TlsAcceptor::from(Arc::new(sc)),
            Arc::new(names),
            Arc::new(plains),
        ));
    }
    if ready.is_empty() {
        return Ok(());
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tls: {e}"))?;
    std::thread::spawn(move || {
        rt.block_on(async move {
            for (ln, acc, names, plains) in ready {
                match TcpListener::from_std(ln) {
                    Ok(ln) => {
                        tokio::spawn(accept_loop(ln, acc, names, plains));
                    }
                    Err(e) => eprintln!("hydo-world: tls: {e}"),
                }
            }
            std::future::pending::<()>().await;
        })
    });
    Ok(())
}

async fn accept_loop(
    ln: TcpListener,
    acc: TlsAcceptor,
    names: Arc<BTreeMap<String, usize>>,
    plains: Arc<Vec<String>>,
) {
    loop {
        let Ok((c, _)) = ln.accept().await else {
            return;
        };
        crate::rt::tune(c.as_raw_fd());
        let (acc, names, plains) = (acc.clone(), names.clone(), plains.clone());
        tokio::spawn(async move { conn(c, acc, &names, &plains).await });
    }
}

/// SO_LINGER 0: closing the socket sends a reset, as the plain side did.
fn reset(fd: i32) {
    let l = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: setsockopt with a valid fd and a correctly sized linger struct.
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            &l as *const libc::linger as *const libc::c_void,
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

async fn conn(c: TcpStream, acc: TlsAcceptor, names: &BTreeMap<String, usize>, plains: &[String]) {
    let Ok(mut tls) = acc.accept(c).await else {
        return;
    };
    let i = tls
        .get_ref()
        .1
        .server_name()
        .and_then(|n| names.get(&n.to_ascii_lowercase()).copied())
        .unwrap_or(0);
    let Some(to) = plains.get(i) else { return };
    let Ok(mut up) = TcpStream::connect(to.as_str()).await else {
        reset(tls.get_ref().0.as_raw_fd());
        return;
    };
    crate::rt::tune(up.as_raw_fd());
    if let Err(e) = tokio::io::copy_bidirectional(&mut tls, &mut up).await {
        if e.kind() == std::io::ErrorKind::ConnectionReset {
            reset(tls.get_ref().0.as_raw_fd());
        }
    }
}
