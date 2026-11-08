//! HTTPS for fakes and proxies (`tls: {hosts: [...]}`, hydo/onboard/MANIFEST.md "HTTPS for fakes
//! and proxies"): the image's own test certificate authority, one certificate per endpoint for
//! its host names, and what the guest needs to trust them and to find the endpoint by name.
//!
//! Reproducible by construction: every key is derived from the manifest (the image's name, the
//! endpoint's name and its host names) with SHA-256, every signature is ECDSA P-256 with RFC 6979
//! nonces (deterministic), the validity is fixed (2000-01-01 to 2099-12-31, so any guest clock,
//! `guest.clock_start` and `T=` steps included, is inside it), and the DER is written here byte
//! by byte. The same manifest gives the same certificates on every machine; the keys protect
//! nothing outside the guest (only the guest trusts this CA).
//!
//! What goes into the image (nothing of it for a manifest without `tls:`):
//! - /etc/hydo/tls/ca.pem (the CA), `/etc/hydo/tls/<endpoint>.pem` (its certificate and the CA's)
//!   and `<endpoint>.key` (PKCS#8), named in /etc/hydo/world.json for hydo-world-tls;
//! - the CA appended to the guest's /etc/ssl/certs/ca-certificates.crt (made if missing, with
//!   /etc/ssl/cert.pem pointing at it), and to every image service's system bundle (oci.rs);
//! - each host name mapped to its endpoint's address in /etc/hosts (the guest's and every image
//!   service's own);
//! - the variables of `ENV` for programs whose runtime reads its own trust list.

use crate::fsx::{self, io_fail};
use crate::shell::NET_GW;
use crate::Fail;
use hydo_manifest::py;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// The guest's copy of the CA (what NODE_EXTRA_CA_CERTS names for programs of the guest root).
pub const GUEST_CA: &str = "/etc/hydo/tls/ca.pem";
/// The system bundle path most runtimes read first (Debian, Ubuntu, Alpine, Arch; Go; OpenSSL
/// builds whose directory is /etc/ssl).
pub const BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// An image service's copy of the CA, in the directory hydo keeps in every image root.
pub const IMAGE_CA: &str = "/.hydo/tls-ca.pem";
/// System bundles of the common distributions, appended to where an image has them.
pub const BUNDLES: &[&str] = &[
    "etc/ssl/certs/ca-certificates.crt",
    "etc/pki/tls/certs/ca-bundle.crt",
    "etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "etc/ssl/ca-bundle.pem",
    "etc/ssl/cert.pem",
];
/// Source directories of the distributions' bundle tools, given the CA when an image has them:
/// update-ca-certificates (Debian, Ubuntu, Alpine) and update-ca-trust (RHEL, Fedora).
pub const ANCHORS: &[(&str, &str)] = &[
    ("usr/local/share/ca-certificates", "hydo-test-ca.crt"),
    ("etc/pki/ca-trust/source/anchors", "hydo-test-ca.pem"),
];

/// The variables set for every program (unless the manifest sets them): OpenSSL, Go, Ruby,
/// Python's httpx and curl read SSL_CERT_FILE; Python's requests REQUESTS_CA_BUNDLE (it keeps
/// its own bundle otherwise); Node.js adds NODE_EXTRA_CA_CERTS to its built-in list; programs
/// built by Nix read NIX_SSL_CERT_FILE. `ca` is the CA alone, `bundle` the system bundle with it.
pub fn env(ca: &str, bundle: &str) -> Vec<(&'static str, String)> {
    vec![
        ("SSL_CERT_FILE", bundle.to_string()),
        ("REQUESTS_CA_BUNDLE", bundle.to_string()),
        ("NODE_EXTRA_CA_CERTS", ca.to_string()),
        ("NIX_SSL_CERT_FILE", bundle.to_string()),
    ]
}

/// One endpoint with `tls:`.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub name: String,
    pub hosts: Vec<String>,
    /// the address hydo-world listens on (the endpoint's addr:, else the bridge or loopback)
    pub addr: String,
    pub port: i64,
}

/// Every fake and proxy with `tls:`, in manifest order (fakes first).
pub fn endpoints(m: &Value) -> Vec<Endpoint> {
    let dflt = if crate::shell::has_netns(m) {
        NET_GW
    } else {
        "127.0.0.1"
    };
    let mut out = Vec::new();
    for kind in ["fakes", "proxies"] {
        for e in m.get(kind).and_then(Value::as_array).into_iter().flatten() {
            let Some(t) = e.get("tls").filter(|t| t.is_object()) else {
                continue;
            };
            out.push(Endpoint {
                name: py::str_of(&e["name"]),
                hosts: t
                    .get("hosts")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|h| py::str_of(h).to_ascii_lowercase())
                            .collect()
                    })
                    .unwrap_or_default(),
                addr: e
                    .get("addr")
                    .map(py::str_of)
                    .unwrap_or_else(|| dflt.to_string()),
                port: t.get("port").and_then(py::int_of).unwrap_or(443) as i64,
            });
        }
    }
    out
}

pub fn has_tls(m: &Value) -> bool {
    !endpoints(m).is_empty()
}

/// world.json's `tls` entry of one endpoint (hydo-world-tls reads it).
pub fn world_entry(e: &Value) -> Option<Value> {
    let t = e.get("tls").filter(|t| t.is_object())?;
    let name = py::str_of(&e["name"]);
    let mut o = serde_json::Map::new();
    o.insert(
        "cert".into(),
        Value::String(format!("/etc/hydo/tls/{name}.pem")),
    );
    o.insert(
        "hosts".into(),
        Value::Array(
            t.get("hosts")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|h| Value::String(py::str_of(h).to_ascii_lowercase()))
                        .collect()
                })
                .unwrap_or_default(),
        ),
    );
    o.insert(
        "key".into(),
        Value::String(format!("/etc/hydo/tls/{name}.key")),
    );
    o.insert(
        "port".into(),
        Value::from(t.get("port").and_then(py::int_of).unwrap_or(443) as i64),
    );
    Some(Value::Object(o))
}

/// The /etc/hosts lines that send each host name to its endpoint.
pub fn host_lines(eps: &[Endpoint]) -> String {
    eps.iter()
        .flat_map(|e| e.hosts.iter().map(move |h| format!("{} {h}\n", e.addr)))
        .collect()
}

/// The CA and one certificate per endpoint.
pub struct Pki {
    pub ca_pem: String,
    /// (endpoint, its certificate followed by the CA's, its PKCS#8 key)
    pub leaves: Vec<(String, String, String)>,
}

/// The certificates of an image named `image` for these endpoints.
pub fn make(image: &str, eps: &[Endpoint]) -> Pki {
    let ca_key = key_from(&["hydo tls ca v1", image]);
    let ca_name = name(&format!("hydo test CA ({image})"));
    let ca_ski = key_id(&ca_key);
    let ca_der = certificate(
        &ca_key,
        &ca_key,
        &serial(&["ca", image]),
        &ca_name,
        &ca_name,
        &[
            ext(OID_BASIC_CONSTRAINTS, true, &seq(&[tlv(0x01, &[0xff])])),
            ext(OID_KEY_USAGE, true, &[0x03, 0x02, 0x01, 0x86]),
            ext(OID_SKI, false, &tlv(0x04, &ca_ski)),
        ],
    );
    let ca_pem = pem("CERTIFICATE", &ca_der);
    let mut leaves = Vec::new();
    for e in eps {
        let hosts = e.hosts.join("\n");
        let k = key_from(&["hydo tls leaf v1", image, &e.name, &hosts]);
        let cn = e.hosts.first().cloned().unwrap_or_else(|| e.name.clone());
        let san: Vec<Vec<u8>> = e.hosts.iter().map(|h| tlv(0x82, h.as_bytes())).collect();
        let der = certificate(
            &k,
            &ca_key,
            &serial(&["leaf", image, &e.name, &hosts]),
            &ca_name,
            &name(&cn),
            &[
                ext(OID_BASIC_CONSTRAINTS, true, &seq(&[])),
                ext(OID_KEY_USAGE, true, &[0x03, 0x02, 0x07, 0x80]),
                ext(OID_EXT_KEY_USAGE, false, &seq(&[oid(OID_SERVER_AUTH)])),
                ext(OID_SAN, false, &seq(&san)),
                ext(OID_SKI, false, &tlv(0x04, &key_id(&k))),
                ext(OID_AKI, false, &seq(&[tlv(0x80, &ca_ski)])),
            ],
        );
        let chain = pem("CERTIFICATE", &der) + &ca_pem;
        leaves.push((e.name.clone(), chain, pem("PRIVATE KEY", &pkcs8(&k))));
    }
    Pki { ca_pem, leaves }
}

/// Write the CA, the endpoints' certificates and keys, the guest's trust bundle and the host
/// names into the staged guest root `r`.
pub fn stage_root(r: &Path, eps: &[Endpoint], pki: &Pki) -> Result<(), Fail> {
    let d = r.join("etc/hydo/tls");
    fsx::makedirs(&d)?;
    fsx::write_text(&d.join("ca.pem"), &pki.ca_pem)?;
    for (n, chain, key) in &pki.leaves {
        fsx::write_text(&d.join(format!("{n}.pem")), chain)?;
        fsx::write_text(&d.join(format!("{n}.key")), key)?;
        fsx::chmod(&d.join(format!("{n}.key")), 0o600)?;
    }
    let ssl = r.join("etc/ssl/certs");
    fsx::makedirs(&ssl)?;
    append_pem(&r.join(BUNDLE.trim_start_matches('/')), &pki.ca_pem)?;
    let cert_pem = r.join("etc/ssl/cert.pem");
    if !crate::fsx::lexists(&cert_pem) {
        std::os::unix::fs::symlink("certs/ca-certificates.crt", &cert_pem)
            .map_err(|e| io_fail("cannot link", &cert_pem, e))?;
    }
    let hosts = r.join("etc/hosts");
    let mut h = std::fs::read_to_string(&hosts).unwrap_or_default();
    if !h.is_empty() && !h.ends_with('\n') {
        h.push('\n');
    }
    h.push_str(&host_lines(eps));
    fsx::write_text(&hosts, &h)
}

/// Append a PEM block to a bundle file (made if missing), on a line of its own.
pub fn append_pem(p: &Path, block: &str) -> Result<(), Fail> {
    let mut t = std::fs::read(p).unwrap_or_default();
    if !t.is_empty() && !t.ends_with(b"\n") {
        t.push(b'\n');
    }
    t.extend_from_slice(block.as_bytes());
    if let Some(d) = p.parent() {
        fsx::makedirs(d)?;
    }
    std::fs::write(p, &t).map_err(|e| io_fail("cannot write", p, e))
}

/// The manifest's top-level `env:` with the trust variables added (the manifest's own win).
pub fn add_env(m: &mut Value) {
    let o = m.as_object_mut().expect("the manifest is a mapping");
    let e = o
        .entry("env")
        .or_insert_with(|| Value::Object(Default::default()));
    if !e.is_object() {
        *e = Value::Object(Default::default());
    }
    let e = e.as_object_mut().expect("env is a mapping");
    for (k, v) in env(GUEST_CA, BUNDLE) {
        e.entry(k).or_insert(Value::String(v));
    }
}

// ------------------------------------------------------------------ keys

fn key_from(parts: &[&str]) -> SigningKey {
    for n in 0u32.. {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p.as_bytes());
            h.update([0]);
        }
        h.update(n.to_be_bytes());
        if let Ok(k) = SigningKey::from_slice(&h.finalize()) {
            return k;
        }
    }
    unreachable!("a SHA-256 output is a valid P-256 scalar almost always")
}

fn point(k: &SigningKey) -> Vec<u8> {
    k.verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec()
}

/// The key identifier: the first 20 bytes of the SHA-256 of the public point.
fn key_id(k: &SigningKey) -> Vec<u8> {
    Sha256::digest(point(k))[..20].to_vec()
}

/// A positive 16-byte serial number.
fn serial(parts: &[&str]) -> Vec<u8> {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    let mut s = h.finalize()[..16].to_vec();
    s[0] &= 0x7f;
    if s[0] == 0 {
        s[0] = 1;
    }
    s
}

// ------------------------------------------------------------------ DER

const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_O: &[u8] = &[0x55, 0x04, 0x0a];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_EXT_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
const OID_SKI: &[u8] = &[0x55, 0x1d, 0x0e];
const OID_AKI: &[u8] = &[0x55, 0x1d, 0x23];
const OID_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = content.len();
    if n < 0x80 {
        v.push(n as u8);
    } else {
        let b: Vec<u8> = n
            .to_be_bytes()
            .into_iter()
            .skip_while(|x| *x == 0)
            .collect();
        v.push(0x80 | b.len() as u8);
        v.extend(b);
    }
    v.extend_from_slice(content);
    v
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn oid(o: &[u8]) -> Vec<u8> {
    tlv(0x06, o)
}

/// An INTEGER from big-endian unsigned bytes.
fn uint(b: &[u8]) -> Vec<u8> {
    let mut b: Vec<u8> = b.iter().copied().skip_while(|x| *x == 0).collect();
    if b.is_empty() || b[0] & 0x80 != 0 {
        b.insert(0, 0);
    }
    tlv(0x02, &b)
}

fn bits(b: &[u8]) -> Vec<u8> {
    let mut v = vec![0];
    v.extend_from_slice(b);
    tlv(0x03, &v)
}

/// A Name: an organisation that says what this is, and the common name.
fn name(cn: &str) -> Vec<u8> {
    let rdn = |o: &[u8], v: &str| tlv(0x31, &seq(&[oid(o), tlv(0x0c, v.as_bytes())]));
    seq(&[rdn(OID_O, "hydo (test only)"), rdn(OID_CN, cn)])
}

fn ext(o: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let mut p = vec![oid(o)];
    if critical {
        p.push(tlv(0x01, &[0xff]));
    }
    p.push(tlv(0x04, value));
    seq(&p)
}

fn spki(k: &SigningKey) -> Vec<u8> {
    seq(&[
        seq(&[oid(OID_EC_PUBLIC_KEY), oid(OID_P256)]),
        bits(&point(k)),
    ])
}

fn certificate(
    subject_key: &SigningKey,
    issuer_key: &SigningKey,
    serial: &[u8],
    issuer: &[u8],
    subject: &[u8],
    exts: &[Vec<u8>],
) -> Vec<u8> {
    let alg = seq(&[oid(OID_ECDSA_SHA256)]);
    let validity = seq(&[
        tlv(0x17, b"000101000000Z"),   // UTCTime 2000-01-01 00:00:00
        tlv(0x18, b"20991231235959Z"), // GeneralizedTime 2099-12-31 23:59:59
    ]);
    let tbs = seq(&[
        tlv(0xa0, &uint(&[2])),
        uint(serial),
        alg.clone(),
        issuer.to_vec(),
        validity,
        subject.to_vec(),
        spki(subject_key),
        tlv(0xa3, &seq(exts)),
    ]);
    let sig: Signature = issuer_key.sign(&tbs);
    seq(&[tbs, alg, bits(sig.to_der().as_bytes())])
}

/// PKCS#8 of a P-256 key, with the public key inside (as OpenSSL writes it; ring needs it).
fn pkcs8(k: &SigningKey) -> Vec<u8> {
    let ec = seq(&[
        uint(&[1]),
        tlv(0x04, &k.to_bytes()),
        tlv(0xa1, &bits(&point(k))),
    ]);
    seq(&[
        uint(&[0]),
        seq(&[oid(OID_EC_PUBLIC_KEY), oid(OID_P256)]),
        tlv(0x04, &ec),
    ])
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(b: &[u8]) -> String {
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn pem(label: &str, der: &[u8]) -> String {
    let b = base64(der);
    let mut s = format!("-----BEGIN {label}-----\n");
    for l in b.as_bytes().chunks(64) {
        s.push_str(std::str::from_utf8(l).expect("base64 is ASCII"));
        s.push('\n');
    }
    s.push_str(&format!("-----END {label}-----\n"));
    s
}

/// The DER inside the first PEM block of `text` (tests and checks).
pub fn pem_der(text: &str) -> Vec<u8> {
    let body: String = text
        .lines()
        .skip_while(|l| !l.starts_with("-----BEGIN"))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END"))
        .collect();
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut n = 0;
    for c in body.bytes().filter(|c| *c != b'=') {
        let v = B64.iter().position(|x| *x == c).unwrap_or(0) as u32;
        acc = acc << 6 | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

    fn eps() -> Vec<Endpoint> {
        vec![Endpoint {
            name: "stripe".into(),
            hosts: vec!["api.stripe.com".into(), "files.stripe.com".into()],
            addr: NET_GW.into(),
            port: 443,
        }]
    }

    #[test]
    fn base64_round_trips() {
        for n in 0..40u8 {
            let b: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37).wrapping_add(n)).collect();
            assert_eq!(pem_der(&pem("X", &b)), b);
        }
        assert_eq!(base64(b"hydo"), "aHlkbw==");
    }

    /// The same manifest gives the same bytes; another image name gives another CA.
    #[test]
    fn reproducible() {
        let a = make("medusa", &eps());
        let b = make("medusa", &eps());
        assert_eq!(a.ca_pem, b.ca_pem);
        assert_eq!(a.leaves, b.leaves);
        assert_ne!(make("other", &eps()).ca_pem, a.ca_pem);
    }

    /// A client's certificate check (rustls-webpki, as rustls clients run it) accepts the leaf
    /// for each host name, with only the CA trusted, at any time a guest clock can show; and
    /// refuses another name.
    #[test]
    fn a_client_accepts_the_certificate() {
        let pki = make("medusa", &eps());
        let ca = CertificateDer::from(pem_der(&pki.ca_pem));
        let anchors = [webpki::anchor_from_trusted_cert(&ca).unwrap().to_owned()];
        let leaf = CertificateDer::from(pem_der(&pki.leaves[0].1));
        let ee = webpki::EndEntityCert::try_from(&leaf).unwrap();
        for secs in [978_307_200u64, 1_767_225_600, 4_000_000_000] {
            ee.verify_for_usage(
                &[webpki::ring::ECDSA_P256_SHA256],
                &anchors,
                &[],
                UnixTime::since_unix_epoch(std::time::Duration::from_secs(secs)),
                webpki::KeyUsage::server_auth(),
                None,
                None,
            )
            .unwrap();
        }
        for h in ["api.stripe.com", "files.stripe.com"] {
            ee.verify_is_valid_for_subject_name(&ServerName::try_from(h).unwrap())
                .unwrap();
        }
        assert!(ee
            .verify_is_valid_for_subject_name(&ServerName::try_from("stripe.com").unwrap())
            .is_err());
    }

    /// The key file is a PKCS#8 P-256 key that ring (a TLS server's usual loader) accepts, and
    /// it belongs to the certificate.
    #[test]
    fn the_key_loads() {
        let pki = make("medusa", &eps());
        let der = pem_der(&pki.leaves[0].2);
        let kp = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &der,
            &ring::rand::SystemRandom::new(),
        )
        .unwrap();
        use ring::signature::KeyPair;
        let cert = pem_der(&pki.leaves[0].1);
        let pk = kp.public_key().as_ref();
        assert!(cert.windows(pk.len()).any(|w| w == pk));
    }
}
