//! Objects a fake remembers (`save:` and `load:` on a route): a provider answers a later read of
//! an object with what it was created with (a payment's currency and amount, its status after a
//! capture). Without them a fake answers from the request only, and a system that reads back an
//! object it created gets fields the fake never stored (found onboarding a commerce backend,
//! hydo/onboard/examples/medusa: a refund read the payment's currency from a later read).
//!
//! ```yaml
//! routes:
//!   - name: intent_create
//!     method: POST
//!     path: /v1/payment_intents
//!     body: '{"id":"pi_{seq}","currency":"{form:currency}","amount":{form:amount},"status":"requires_capture"}'
//!     save: {key: 'pi_{seq}', fields: {currency: '{form:currency}', amount: '{form:amount}', status: requires_capture}}
//!   - name: intent_capture
//!     method: POST
//!     path: /v1/payment_intents/{id}/capture
//!     load: '{path:id}'
//!     save: {key: '{path:id}', fields: {status: succeeded}}
//!     body: '{"id":"{path:id}","currency":"{obj:currency}","amount":{obj:amount},"status":"{obj:status}"}'
//!   - name: intent_get
//!     method: GET
//!     path: /v1/payment_intents/{id}
//!     load: '{path:id}'
//!     body: '{"id":"{path:id}","currency":"{obj:currency}","amount":{obj:amount},"status":"{obj:status}"}'
//! ```
//!
//! - `save:` stores the rendered `fields` under the rendered `key` (per fake) when the request
//!   takes effect (`fx=1` in the record: an effectful route, not an idempotent replay, not an
//!   error rule without `effect=yes`), merging into an object already there (a status change).
//!   The templates are an answer's ({seq} {ref} {json:a.b} {form:x} {path:x} ...), plus
//!   `{obj:<field>}` of the object the route loaded.
//! - `load:` names the object a request is about. A missing object is answered 404
//!   (`{"error":"hydo fake: no object <key>"}`) and the request takes no effect (`out=-`,
//!   `obj=missing` in the record), as a provider refuses an unknown id.
//! - The answer's templates (body and headers) may use `{obj:<field>}`: the loaded object after
//!   the route's own save, or, on a route with only `save:`, the saved object (an idempotent
//!   replay sees what the first request stored). A field the object lacks is empty.
//! - Objects are part of the outside world, so a power cut carries them into the recovery boot
//!   (`w:0,o,<fake>,<key>,<field>,<value>` tokens, within the carried world's 3000 bytes).
//!
//! Without `save:` and `load:` nothing here runs and `{obj:...}` stays as written.

use crate::config::ReqView;
use crate::config::{render, Route};
use crate::goish::{esc, unesc};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Save {
    /// template of the object's key
    pub key: String,
    /// field -> template, in the manifest's order
    pub fields: Vec<(String, String)>,
}

impl Save {
    /// The `save` member of a route in world.json (null: none).
    pub fn from_value(v: &Value) -> Result<Option<Save>, String> {
        let o = match v {
            Value::Null => return Ok(None),
            Value::Object(o) => o,
            _ => return Err("route.save: an object".into()),
        };
        let key = match o.get("key") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("route.save.key: a string".into()),
        };
        let mut fields = Vec::new();
        match o.get("fields") {
            None | Some(Value::Null) => {}
            Some(Value::Object(f)) => {
                for (k, x) in f {
                    match x {
                        Value::String(s) => fields.push((k.clone(), s.clone())),
                        Value::Number(n) => fields.push((k.clone(), n.to_string())),
                        Value::Bool(b) => fields.push((k.clone(), b.to_string())),
                        _ => return Err(format!("route.save.fields.{k}: a string")),
                    }
                }
            }
            Some(_) => return Err("route.save.fields: an object".into()),
        }
        Ok(Some(Save { key, fields }))
    }
}

/// One object's fields.
pub type Obj = BTreeMap<String, Vec<u8>>;

/// Every fake's objects: (fake, key) -> fields.
pub type Objects = BTreeMap<(String, Vec<u8>), Obj>;

/// The route keeps or reads objects.
pub fn uses(r: &Route) -> bool {
    r.save.is_some() || !r.load.is_empty()
}

/// `load:`: the object a request names (None: the route loads none). Err(key): it names one
/// that is not there.
pub fn lookup(
    objs: &Objects,
    fake: &str,
    r: &Route,
    ctx: &[(&str, &[u8])],
    v: &ReqView,
) -> Result<Option<Obj>, Vec<u8>> {
    if r.load.is_empty() {
        return Ok(None);
    }
    let key = render(&r.load, ctx, Some(v));
    match objs.get(&(fake.to_string(), key.clone())) {
        Some(o) => Ok(Some(o.clone())),
        None => Err(key),
    }
}

/// `save:` of a request that took effect (`fx`): merge the rendered fields into the object at
/// the rendered key. Returns the object the answer shows: the loaded one (`cur`) after the save,
/// or, on a route that loads none, the saved one (also for a replay, which saves nothing).
pub fn save(
    objs: &mut Objects,
    fake: &str,
    r: &Route,
    fx: bool,
    cur: Option<Obj>,
    ctx: &[(&str, &[u8])],
    v: &ReqView,
) -> Option<Obj> {
    let Some(s) = &r.save else { return cur };
    let empty = Obj::new();
    let with = cur.as_ref().unwrap_or(&empty);
    let key = subst(render(&s.key, ctx, Some(v)), with);
    let id = (fake.to_string(), key);
    if fx {
        let vals: Vec<(String, Vec<u8>)> = s
            .fields
            .iter()
            .map(|(k, t)| (k.clone(), subst(render(t, ctx, Some(v)), with)))
            .collect();
        let o = objs.entry(id.clone()).or_default();
        for (k, x) in vals {
            o.insert(k, x);
        }
    }
    if r.load.is_empty() {
        return objs.get(&id).cloned();
    }
    // the loaded object, with this request's own changes when it saved to the same key
    match (cur, objs.get(&id)) {
        (Some(_), Some(o)) if fx => Some(o.clone()),
        (c, _) => c,
    }
}

/// Replace every `{obj:<field>}` in rendered bytes by the object's field (empty when absent).
pub fn subst(b: Vec<u8>, o: &Obj) -> Vec<u8> {
    const OPEN: &[u8] = b"{obj:";
    if !b.windows(OPEN.len()).any(|w| w == OPEN) {
        return b;
    }
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(OPEN) {
            let s = i + OPEN.len();
            let mut k = s;
            while k < b.len() && !b"{}\"".contains(&b[k]) {
                k += 1;
            }
            if k > s && b.get(k) == Some(&b'}') {
                let name = String::from_utf8_lossy(&b[s..k]);
                if let Some(x) = o.get(name.as_ref()) {
                    out.extend_from_slice(x);
                }
                i = k + 1;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// The answer to a request that names a missing object.
pub fn missing_body(key: &[u8]) -> Vec<u8> {
    let k = serde_json::to_string(&String::from_utf8_lossy(key)).unwrap_or_default();
    let k = k.trim_matches('"');
    format!(r#"{{"error":"hydo fake: no object {k}"}}"#).into_bytes()
}

/// The carried world's tokens of every object, one per field.
pub fn carry(objs: &Objects) -> Vec<String> {
    let mut out = Vec::new();
    for ((fake, key), o) in objs {
        for (k, x) in o {
            out.push(format!(
                "w:0,o,{},{},{},{}",
                esc(fake.as_bytes()),
                esc(key),
                esc(k.as_bytes()),
                esc(x)
            ));
        }
    }
    out
}

/// One carried `o` token's fields (fake, key, field, value), as escaped.
pub fn load_carried(objs: &mut Objects, f: &[&[u8]]) {
    if f.len() != 4 {
        return;
    }
    let fake = String::from_utf8_lossy(&unesc(f[0])).into_owned();
    let field = String::from_utf8_lossy(&unesc(f[2])).into_owned();
    objs.entry((fake, unesc(f[1])))
        .or_default()
        .insert(field, unesc(f[3]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(save: Option<Save>, load: &str) -> Route {
        Route {
            name: "r".into(),
            save,
            load: load.into(),
            ..Default::default()
        }
    }

    fn form(body: &str, path: &str, vars: &[(&str, &str)]) -> ReqView {
        ReqView {
            path: path.as_bytes().to_vec(),
            header: vec![(
                "Content-Type".into(),
                b"application/x-www-form-urlencoded".to_vec(),
            )],
            body: body.as_bytes().to_vec(),
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
                .collect(),
            ..Default::default()
        }
    }

    /// Create, read, update and read again; a replay and a missing id; the carried tokens.
    #[test]
    fn create_read_update() {
        let mut objs = Objects::new();
        let create = route(
            Some(Save {
                key: "pi_{seq}".into(),
                fields: vec![
                    ("currency".into(), "{form:currency}".into()),
                    ("status".into(), "requires_capture".into()),
                ],
            }),
            "",
        );
        let v = form("amount=100&currency=eur", "/v1/payment_intents", &[]);
        let ctx: [(&str, &[u8]); 1] = [("seq", b"3")];
        let o = save(&mut objs, "stripe", &create, true, None, &ctx, &v).unwrap();
        assert_eq!(o["currency"], b"eur");
        // an idempotent replay saves nothing and shows what the first one stored
        let v2 = form("amount=100&currency=usd", "/v1/payment_intents", &[]);
        let o = save(&mut objs, "stripe", &create, false, None, &ctx, &v2).unwrap();
        assert_eq!(o["currency"], b"eur");
        // capture: load, then save the new status into the same object
        let capture = route(
            Some(Save {
                key: "{path:id}".into(),
                fields: vec![("status".into(), "succeeded".into())],
            }),
            "{path:id}",
        );
        let vc = form("", "/v1/payment_intents/pi_3/capture", &[("id", "pi_3")]);
        let cur = lookup(&objs, "stripe", &capture, &[], &vc).unwrap();
        let o = save(&mut objs, "stripe", &capture, true, cur, &[], &vc).unwrap();
        assert_eq!(
            (&o["status"][..], &o["currency"][..]),
            (&b"succeeded"[..], &b"eur"[..])
        );
        let get = route(None, "{path:id}");
        let o = lookup(&objs, "stripe", &get, &[], &vc).unwrap().unwrap();
        assert_eq!(
            subst(
                br#"{"c":"{obj:currency}","s":"{obj:status}","x":"{obj:none}"}"#.to_vec(),
                &o
            ),
            br#"{"c":"eur","s":"succeeded","x":""}"#
        );
        // an unknown id, and another fake's objects
        let vm = form("", "/v1/payment_intents/pi_9", &[("id", "pi_9")]);
        assert_eq!(
            lookup(&objs, "stripe", &get, &[], &vm),
            Err(b"pi_9".to_vec())
        );
        assert!(lookup(&objs, "other", &get, &[], &vc).is_err());
        assert_eq!(
            missing_body(b"pi_9"),
            br#"{"error":"hydo fake: no object pi_9"}"#
        );
        // carried and loaded back
        let toks = carry(&objs);
        assert_eq!(
            toks,
            vec![
                "w:0,o,stripe,pi_3,currency,eur".to_string(),
                "w:0,o,stripe,pi_3,status,succeeded".to_string()
            ]
        );
        let mut back = Objects::new();
        for t in &toks {
            let f: Vec<&[u8]> = t.as_bytes()[2..].split(|&c| c == b',').collect();
            load_carried(&mut back, &f[2..]);
        }
        assert_eq!(back, objs);
    }

    #[test]
    fn subst_leaves_other_braces() {
        let o: Obj = [("a".to_string(), b"1".to_vec())].into_iter().collect();
        assert_eq!(
            subst(b"{obj:a}{obj:}{x}{obj:a".to_vec(), &o),
            b"1{obj:}{x}{obj:a"
        );
    }
}
