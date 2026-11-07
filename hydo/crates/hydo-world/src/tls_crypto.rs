//! The TLS front's cryptography (hydo-world-tls only): a rustls `CryptoProvider` made of pure
//! Rust crates (RustCrypto's p256, aes-gcm, hmac and sha2), so hydo-world-tls, like hydo-world,
//! builds as one static program with no C compiler, from a Mac or a Linux machine alike.
//!
//! TLS 1.3 only, with what every TLS 1.3 client must offer: the cipher suite
//! TLS_AES_128_GCM_SHA256, the group secp256r1 (a client that sent only an X25519 share is asked
//! for a P-256 one with a HelloRetryRequest) and ECDSA P-256 SHA-256 signatures, which the
//! certificates hydo-build makes carry. Randomness (the key exchange) comes from the guest's
//! kernel (getrandom), which hydo seeds deterministically like everything else in the guest.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::Aes128Gcm;
use rustls::crypto::cipher::{
    make_tls13_aad, AeadKey, InboundOpaqueMessage, InboundPlainMessage, Iv, MessageDecrypter,
    MessageEncrypter, Nonce, OutboundOpaqueMessage, OutboundPlainMessage, PrefixedPayload,
    Tls13AeadAlgorithm, UnsupportedOperationError,
};
use rustls::crypto::tls13::HkdfUsingHmac;
use rustls::crypto::{
    hash, hmac, ActiveKeyExchange, CipherSuiteCommon, CryptoProvider, GetRandomFailed, KeyProvider,
    SecureRandom, SharedSecret, SupportedKxGroup, WebPkiSupportedAlgorithms,
};
use rustls::pki_types::PrivateKeyDer;
use rustls::sign::{Signer, SigningKey};
use rustls::{
    CipherSuite, ConnectionTrafficSecrets, ContentType, Error, NamedGroup, ProtocolVersion,
    SignatureAlgorithm, SignatureScheme, SupportedCipherSuite, Tls13CipherSuite,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// The provider: one cipher suite, one group, the guest's randomness, P-256 keys.
pub fn provider() -> CryptoProvider {
    CryptoProvider {
        cipher_suites: vec![SupportedCipherSuite::Tls13(&AES_128_GCM_SHA256)],
        kx_groups: vec![&P256],
        // only a server that asks for no client certificate uses this provider
        signature_verification_algorithms: WebPkiSupportedAlgorithms {
            all: &[],
            mapping: &[],
        },
        secure_random: &GuestRandom,
        key_provider: &P256Keys,
    }
}

static AES_128_GCM_SHA256: Tls13CipherSuite = Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_AES_128_GCM_SHA256,
        hash_provider: &Sha256Hash,
        confidentiality_limit: 1 << 24,
    },
    hkdf_provider: &HkdfUsingHmac(&Sha256Hmac),
    aead_alg: &Aes128GcmAead,
    quic: None,
};

// ------------------------------------------------------------------ randomness

#[derive(Debug)]
struct GuestRandom;

impl SecureRandom for GuestRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        let mut done = 0;
        while done < buf.len() {
            // SAFETY: the pointer and length name the unfilled rest of `buf`.
            let n = unsafe {
                libc::getrandom(
                    buf[done..].as_mut_ptr() as *mut libc::c_void,
                    buf.len() - done,
                    0,
                )
            };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(GetRandomFailed);
            }
            done += n as usize;
        }
        Ok(())
    }
}

fn random32() -> Result<[u8; 32], Error> {
    let mut b = [0u8; 32];
    GuestRandom
        .fill(&mut b)
        .map_err(|_| Error::FailedToGetRandomBytes)?;
    Ok(b)
}

// ------------------------------------------------------------------ SHA-256 and HMAC

struct Sha256Hash;

impl hash::Hash for Sha256Hash {
    fn start(&self) -> Box<dyn hash::Context> {
        Box::new(Sha256Ctx(Sha256::new()))
    }
    fn hash(&self, data: &[u8]) -> hash::Output {
        hash::Output::new(&Sha256::digest(data)[..])
    }
    fn output_len(&self) -> usize {
        32
    }
    fn algorithm(&self) -> hash::HashAlgorithm {
        hash::HashAlgorithm::SHA256
    }
}

struct Sha256Ctx(Sha256);

impl hash::Context for Sha256Ctx {
    fn fork_finish(&self) -> hash::Output {
        hash::Output::new(&self.0.clone().finalize()[..])
    }
    fn fork(&self) -> Box<dyn hash::Context> {
        Box::new(Sha256Ctx(self.0.clone()))
    }
    fn finish(self: Box<Self>) -> hash::Output {
        hash::Output::new(&self.0.finalize()[..])
    }
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}

struct Sha256Hmac;

impl hmac::Hmac for Sha256Hmac {
    fn with_key(&self, key: &[u8]) -> Box<dyn hmac::Key> {
        use ::hmac::Mac;
        Box::new(HmacKey(
            <::hmac::Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length"),
        ))
    }
    fn hash_output_len(&self) -> usize {
        32
    }
}

struct HmacKey(::hmac::Hmac<Sha256>);

impl hmac::Key for HmacKey {
    fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> hmac::Tag {
        use ::hmac::Mac;
        let mut m = self.0.clone();
        m.update(first);
        for x in middle {
            m.update(x);
        }
        m.update(last);
        hmac::Tag::new(&m.finalize().into_bytes()[..])
    }
    fn tag_len(&self) -> usize {
        32
    }
}

// ------------------------------------------------------------------ AES-128-GCM records

struct Aes128GcmAead;

impl Tls13AeadAlgorithm for Aes128GcmAead {
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        Box::new(Sealer {
            c: Aes128Gcm::new_from_slice(key.as_ref()).expect("a 16-byte key"),
            iv,
        })
    }
    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        Box::new(Opener {
            c: Aes128Gcm::new_from_slice(key.as_ref()).expect("a 16-byte key"),
            iv,
        })
    }
    fn key_len(&self) -> usize {
        16
    }
    fn extract_keys(
        &self,
        key: AeadKey,
        iv: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(ConnectionTrafficSecrets::Aes128Gcm { key, iv })
    }
}

const TAG: usize = 16;

struct Sealer {
    c: Aes128Gcm,
    iv: Iv,
}

impl MessageEncrypter for Sealer {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total = self.encrypted_payload_len(msg.payload.len());
        let mut p = PrefixedPayload::with_capacity(total);
        p.extend_from_chunks(&msg.payload);
        p.extend_from_slice(&msg.typ.to_array());
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls13_aad(total);
        let tag = self
            .c
            .encrypt_in_place_detached(&nonce.into(), &aad, p.as_mut())
            .map_err(|_| Error::EncryptError)?;
        p.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(
            ContentType::ApplicationData,
            ProtocolVersion::TLSv1_2,
            p,
        ))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + 1 + TAG
    }
}

struct Opener {
    c: Aes128Gcm,
    iv: Iv,
}

impl MessageDecrypter for Opener {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let p = &mut msg.payload;
        if p.len() < TAG {
            return Err(Error::DecryptError);
        }
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls13_aad(p.len());
        let n = p.len() - TAG;
        let tag = aes_gcm::Tag::clone_from_slice(&p[n..]);
        self.c
            .decrypt_in_place_detached(&nonce.into(), &aad, &mut p[..n], &tag)
            .map_err(|_| Error::DecryptError)?;
        p.truncate(n);
        msg.into_tls13_unpadded_message()
    }
}

// ------------------------------------------------------------------ the P-256 key exchange

#[derive(Debug)]
struct P256Group;
static P256: P256Group = P256Group;

impl SupportedKxGroup for P256Group {
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        loop {
            if let Ok(sk) = p256::SecretKey::from_slice(&random32()?) {
                use p256::elliptic_curve::sec1::ToEncodedPoint;
                let pk = sk.public_key().to_encoded_point(false).as_bytes().to_vec();
                return Ok(Box::new(P256Share { sk, pk }));
            }
        }
    }
    fn name(&self) -> NamedGroup {
        NamedGroup::secp256r1
    }
}

struct P256Share {
    sk: p256::SecretKey,
    pk: Vec<u8>,
}

impl ActiveKeyExchange for P256Share {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        // TLS sends uncompressed points only (RFC 8446, the key share of secp256r1)
        if peer.first() != Some(&4) {
            return Err(Error::General(
                "secp256r1 key share is not an uncompressed point".into(),
            ));
        }
        let pp = p256::PublicKey::from_sec1_bytes(peer)
            .map_err(|_| Error::General("invalid secp256r1 key share".into()))?;
        let s = p256::ecdh::diffie_hellman(self.sk.to_nonzero_scalar(), pp.as_affine());
        Ok(SharedSecret::from(&s.raw_secret_bytes()[..]))
    }
    fn pub_key(&self) -> &[u8] {
        &self.pk
    }
    fn group(&self) -> NamedGroup {
        NamedGroup::secp256r1
    }
}

// ------------------------------------------------------------------ P-256 certificate keys

#[derive(Debug)]
struct P256Keys;

impl KeyProvider for P256Keys {
    fn load_private_key(&self, key: PrivateKeyDer<'static>) -> Result<Arc<dyn SigningKey>, Error> {
        let d = key.secret_der();
        // the private scalar of a PKCS#8 (or SEC 1) P-256 key: ECPrivateKey's version 1 and the
        // OCTET STRING of 32 bytes that follows it
        let at = d
            .windows(5)
            .position(|w| w == [0x02, 0x01, 0x01, 0x04, 0x20])
            .filter(|i| i + 5 + 32 <= d.len())
            .ok_or_else(|| Error::General("not a P-256 private key".into()))?;
        let sk = p256::ecdsa::SigningKey::from_slice(&d[at + 5..at + 37])
            .map_err(|_| Error::General("invalid P-256 private key".into()))?;
        Ok(Arc::new(P256Key(Arc::new(sk))))
    }
}

#[derive(Debug)]
struct P256Key(Arc<p256::ecdsa::SigningKey>);

impl SigningKey for P256Key {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| Box::new(P256Signer(self.0.clone())) as Box<dyn Signer>)
    }
    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

#[derive(Debug)]
struct P256Signer(Arc<p256::ecdsa::SigningKey>);

impl Signer for P256Signer {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        use p256::ecdsa::signature::Signer as _;
        let sig: p256::ecdsa::Signature = self.0.sign(message);
        Ok(sig.to_der().as_bytes().to_vec())
    }
    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}
