//! Signed webhooks (`webhook.sign`): many providers sign every webhook delivery with an HMAC of
//! a timestamp and the body under an endpoint secret, and the receiving system refuses a delivery
//! without a valid signature. A fake that cannot sign cannot heal the system's books through its
//! webhook endpoint (found onboarding a commerce backend, hydo/onboard/examples/medusa).
//!
//! ```yaml
//! webhook:
//!   url: http://10.89.0.3:9000/hooks/payments
//!   body: '{"type":"payment_intent.succeeded","id":"pi_{seq}"}'
//!   sign:
//!     header: Webhook-Signature   # required: the header that carries it
//!     secret: whsec_test          # required: the endpoint secret (its bytes are the HMAC key)
//!     payload: '{ts}.{body}'      # what is signed (default); {ts} {body} {seq} {ref} {idem} {fake} {route}
//!     value: 't={ts},v1={hmac}'   # the header's value (default); {hmac} is the signature
//!     encoding: hex               # hex (default) or base64 (standard, padded)
//! ```
//!
//! HMAC-SHA256 only. `{ts}` is the delivery attempt's guest time in Unix seconds, so a retried
//! delivery is signed again with its own time, as providers do; under hydo the guest clock is
//! deterministic, so the signatures are too.

use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sign {
    pub header: String,
    pub secret: String,
    pub payload: String,
    pub value: String,
    /// "hex" or "base64"
    pub encoding: String,
}

pub const DEFAULT_PAYLOAD: &str = "{ts}.{body}";
pub const DEFAULT_VALUE: &str = "t={ts},v1={hmac}";

impl Sign {
    /// The `sign` member of world.json's webhook (null: none), with the defaults.
    pub fn from_value(v: &Value) -> Result<Option<Sign>, String> {
        let o = match v {
            Value::Null => return Ok(None),
            Value::Object(o) => o,
            _ => return Err("webhook.sign: an object".into()),
        };
        let get = |k: &str| -> Result<String, String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(String::new()),
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(format!("webhook.sign.{k}: a string")),
            }
        };
        let mut s = Sign {
            header: get("header")?,
            secret: get("secret")?,
            payload: get("payload")?,
            value: get("value")?,
            encoding: get("encoding")?,
        };
        if s.payload.is_empty() {
            s.payload = DEFAULT_PAYLOAD.into();
        }
        if s.value.is_empty() {
            s.value = DEFAULT_VALUE.into();
        }
        if s.encoding.is_empty() {
            s.encoding = "hex".into();
        }
        Ok(Some(s))
    }

    /// The header's name and value for one delivery attempt of `body` at Unix second `ts`;
    /// `ctx` holds the webhook's own template values ({seq} {ref} {idem} {fake} {route}).
    pub fn header(&self, ts: i64, body: &[u8], ctx: &[(&str, &[u8])]) -> (String, Vec<u8>) {
        let ts = ts.to_string();
        let mut vars: Vec<(&str, &[u8])> = vec![("ts", ts.as_bytes()), ("body", body)];
        vars.extend_from_slice(ctx);
        let payload = fill(&self.payload, &vars);
        let mac = hmac_sha256(self.secret.as_bytes(), &payload);
        let enc = if self.encoding == "base64" {
            base64(&mac)
        } else {
            hex(&mac)
        };
        vars.push(("hmac", enc.as_bytes()));
        (self.header.clone(), fill(&self.value, &vars))
    }
}

/// Replace every `{name}` of `vars` in `t` (other braces stay as they are).
fn fill(t: &str, vars: &[(&str, &[u8])]) -> Vec<u8> {
    let b = t.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    'outer: while i < b.len() {
        if b[i] == b'{' {
            for (k, v) in vars {
                let n = k.len();
                if b[i + 1..].starts_with(k.as_bytes()) && b.get(i + 1 + n) == Some(&b'}') {
                    out.extend_from_slice(v);
                    i += n + 2;
                    continue 'outer;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|x| x ^ 0x36));
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|x| x ^ 0x5c));
    outer.update(ih);
    outer.finalize().into()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Standard base64 with padding.
fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4231 test case 2 and a key longer than the block.
    #[test]
    fn hmac_vectors() {
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    /// The default scheme: `t=<ts>,v1=<hex HMAC of "<ts>.<body>">`; a custom one with base64 and
    /// the webhook's own values.
    #[test]
    fn signature_header() {
        let s = Sign::from_value(
            &serde_json::json!({"header": "Stripe-Signature", "secret": "whsec_x"}),
        )
        .unwrap()
        .unwrap();
        let (h, v) = s.header(1767225600, b"{\"a\":1}", &[]);
        assert_eq!(h, "Stripe-Signature");
        let mac = hex(&hmac_sha256(b"whsec_x", b"1767225600.{\"a\":1}"));
        assert_eq!(
            String::from_utf8(v).unwrap(),
            format!("t=1767225600,v1={mac}")
        );
        let s = Sign::from_value(&serde_json::json!({
            "header": "webhook-signature", "secret": "k", "encoding": "base64",
            "payload": "{seq}.{ts}.{body}", "value": "v1,{hmac}"
        }))
        .unwrap()
        .unwrap();
        let (_, v) = s.header(5, b"b", &[("seq", b"7")]);
        assert_eq!(
            String::from_utf8(v).unwrap(),
            format!("v1,{}", base64(&hmac_sha256(b"k", b"7.5.b")))
        );
        assert_eq!(Sign::from_value(&Value::Null).unwrap(), None);
    }
}
