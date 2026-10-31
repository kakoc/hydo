//! The plan the generator writes and the guest client reads (grpc/plan.json next to the
//! descriptor set grpc/desc.pb): which RPCs to drive, how to fill their requests (hints), how to
//! treat their outcomes. Plain JSON, so the client needs no YAML parser. The file's bytes are the
//! Go generator's (`json.MarshalIndent` of the same structs, keys in the same order).

use crate::gojson::{marshal_indent, J};
use serde_json::Value;
use std::collections::BTreeMap;

/// The plan format version this code writes and reads.
pub const FORMAT: i64 = 1;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub format: i64,
    /// host:port of the server (default for every RPC)
    pub target: String,
    /// per-call deadline (default for every RPC)
    pub deadline_ms: i64,
    /// default seed (HYDO_GRPC_SEED etc. override it)
    pub seed: u64,
    /// the numbered-steps mode (`hydo-grpc steps`)
    pub steps: Steps,
    /// the composer mode (`hydo-grpc drive <rpc>`)
    pub drive: Drive,
    pub defaults: Defaults,
    pub record: Record,
    pub rpcs: Vec<Rpc>,
    pub scenarios: Vec<Scenario>,
    /// the .proto files / reflection target the plan came from (None is JSON null)
    pub sources: Option<Vec<String>>,
}

/// `hydo-grpc steps FROM TO`: every step runs `calls` operations on each of `concurrency` workers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Steps {
    pub calls: i64,
    pub concurrency: i64,
}

/// `hydo-grpc drive <rpc>` (one composer command): `calls` operations per run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drive {
    pub calls: i64,
}

/// Where call records go: the console mode (off, brief, full) and the HYDO-HIST history name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    pub console: String,
    pub history: String,
}

/// The generator's value ranges where a field has no hint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Defaults {
    pub string_len: [i64; 2],
    pub bytes_len: [i64; 2],
    pub repeated_len: [i64; 2],
    pub map_len: [i64; 2],
    /// nested messages deeper than this are left unset
    pub depth: i64,
    /// a singular message field is set with this probability
    pub message_prob: f64,
    /// a proto3 `optional` / proto2 optional scalar is set with this probability
    pub optional_prob: f64,
    /// probability that a oneof is left empty
    pub oneof_none: f64,
}

/// The value ranges used where the hints say nothing.
pub fn default_defaults() -> Defaults {
    Defaults {
        string_len: [1, 10],
        bytes_len: [0, 16],
        repeated_len: [0, 3],
        map_len: [0, 2],
        depth: 4,
        message_prob: 0.85,
        optional_prob: 0.7,
        oneof_none: 0.0,
    }
}

/// One method to drive.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rpc {
    /// "pkg.Service/Method"
    pub name: String,
    /// "/pkg.Service/Method" (the HTTP/2 path)
    pub path: String,
    /// command name: "create" or "orders_create"
    pub short: String,
    pub input: String,
    pub output: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
    pub target: String,
    /// relative frequency; 0 = never drawn
    pub weight: i64,
    pub deadline_ms: i64,
    pub retry: Retry,
    /// request field -> HYDO_ACKED/HYDO_OUTCOMES
    pub outcome_id: String,
    /// gRPC code name -> ok|fail|info
    pub classify: BTreeMap<String, String>,
    /// request field path -> hint
    pub fields: BTreeMap<String, FieldHint>,
    /// oneof path -> allowed members (None is a YAML null)
    pub oneofs: BTreeMap<String, Option<Vec<String>>>,
    /// client-streaming: messages sent
    pub stream_len: [i64; 2],
    /// pools that must be non-empty (derived from `from`)
    pub needs: Vec<String>,
}

/// What to do after an unknown outcome.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Retry {
    /// extra attempts right away, with the SAME request (0 = none)
    pub attempts: i64,
    /// pause before each extra attempt
    pub backoff_ms: i64,
    /// `hydo-grpc retry` re-sends it after the faults heal ...
    pub at_end: bool,
    /// ... with up to this many attempts (default: 1 + attempts)
    pub end_tries: i64,
}

/// One request field's constraints. Values are kept as strings and parsed per field type.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldHint {
    pub value: Option<String>,
    pub choice: Vec<String>,
    pub range: Vec<String>,
    pub len: Vec<i64>,
    pub unique: bool,
    pub prefix: String,
    pub format: String,
    pub omit: bool,
    pub prob: Option<f64>,
    pub from: Option<From>,
}

/// A field of an earlier call whose values feed this one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct From {
    /// full method name
    pub rpc: String,
    /// request | response
    pub side: String,
    /// field path in that message
    pub path: String,
    /// random (default) | latest
    pub pick: String,
    /// skip (default: the call is not made) | generate
    pub fallback: String,
}

impl From {
    /// The name of the value pool this reads.
    pub fn pool(&self) -> String {
        format!("{}.{}.{}", self.rpc, self.side, self.path)
    }
}

/// A fixed sequence of calls run by one worker, values flowing through `from`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub weight: i64,
    /// full method names
    pub calls: Vec<String>,
}

impl Rpc {
    /// The hinted paths in a fixed order.
    pub fn field_paths(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }
}

fn method_name(full: &str) -> &str {
    match full.rfind('/') {
        Some(i) => &full[i + 1..],
        None => full,
    }
}

impl Plan {
    /// The RPC by full name, short name, path or method name.
    pub fn find(&self, name: &str) -> Result<usize, String> {
        let mut hits = Vec::new();
        for (i, r) in self.rpcs.iter().enumerate() {
            if r.name == name || r.short == name || r.path == name {
                return Ok(i);
            }
            if method_name(&r.name) == name {
                hits.push(i);
            }
        }
        match hits.len() {
            1 => Ok(hits[0]),
            0 => Err(format!("no rpc {} in the plan", crate::gofmt::quote(name))),
            _ => Err(format!(
                "rpc {} is ambiguous ({}, {}, ...): use Service/Method",
                crate::gofmt::quote(name),
                self.rpcs[hits[0]].name,
                self.rpcs[hits[1]].name
            )),
        }
    }

    /// The plan as stable, indented JSON (`json.MarshalIndent` plus a newline).
    pub fn marshal(&self) -> String {
        let mut s = marshal_indent(&self.to_json());
        s.push('\n');
        s
    }

    pub fn to_json(&self) -> J {
        let mut o = vec![
            ("format".into(), J::Int(self.format)),
            ("target".into(), J::Str(self.target.clone())),
            ("deadline_ms".into(), J::Int(self.deadline_ms)),
            ("seed".into(), J::Uint(self.seed)),
            (
                "steps".into(),
                J::Obj(vec![
                    ("calls".into(), J::Int(self.steps.calls)),
                    ("concurrency".into(), J::Int(self.steps.concurrency)),
                ]),
            ),
            (
                "drive".into(),
                J::Obj(vec![("calls".into(), J::Int(self.drive.calls))]),
            ),
        ];
        let d = &self.defaults;
        let pair = |p: [i64; 2]| J::Arr(vec![J::Int(p[0]), J::Int(p[1])]);
        o.push((
            "defaults".into(),
            J::Obj(vec![
                ("string_len".into(), pair(d.string_len)),
                ("bytes_len".into(), pair(d.bytes_len)),
                ("repeated_len".into(), pair(d.repeated_len)),
                ("map_len".into(), pair(d.map_len)),
                ("depth".into(), J::Int(d.depth)),
                ("message_prob".into(), J::Float(d.message_prob)),
                ("optional_prob".into(), J::Float(d.optional_prob)),
                ("oneof_none".into(), J::Float(d.oneof_none)),
            ]),
        ));
        o.push((
            "record".into(),
            J::Obj(vec![
                ("console".into(), J::Str(self.record.console.clone())),
                ("history".into(), J::Str(self.record.history.clone())),
            ]),
        ));
        o.push((
            "rpcs".into(),
            J::Arr(self.rpcs.iter().map(rpc_json).collect()),
        ));
        if !self.scenarios.is_empty() {
            o.push((
                "scenarios".into(),
                J::Arr(
                    self.scenarios
                        .iter()
                        .map(|s| {
                            J::Obj(vec![
                                ("name".into(), J::Str(s.name.clone())),
                                ("weight".into(), J::Int(s.weight)),
                                ("calls".into(), strs(&s.calls)),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
        o.push((
            "sources".into(),
            match &self.sources {
                None => J::Null,
                Some(v) => strs(v),
            },
        ));
        J::Obj(o)
    }

    /// Reads a plan (json.Unmarshal into the Go structs: missing keys and nulls are zero values,
    /// unknown keys are ignored).
    pub fn from_json(text: &str) -> Result<Plan, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut p = Plan {
            format: int(&v["format"]),
            target: string(&v["target"]),
            deadline_ms: int(&v["deadline_ms"]),
            seed: v["seed"].as_u64().unwrap_or(0),
            steps: Steps {
                calls: int(&v["steps"]["calls"]),
                concurrency: int(&v["steps"]["concurrency"]),
            },
            drive: Drive {
                calls: int(&v["drive"]["calls"]),
            },
            ..Default::default()
        };
        let d = &v["defaults"];
        p.defaults = Defaults {
            string_len: pair(&d["string_len"]),
            bytes_len: pair(&d["bytes_len"]),
            repeated_len: pair(&d["repeated_len"]),
            map_len: pair(&d["map_len"]),
            depth: int(&d["depth"]),
            message_prob: d["message_prob"].as_f64().unwrap_or(0.0),
            optional_prob: d["optional_prob"].as_f64().unwrap_or(0.0),
            oneof_none: d["oneof_none"].as_f64().unwrap_or(0.0),
        };
        p.record = Record {
            console: string(&v["record"]["console"]),
            history: string(&v["record"]["history"]),
        };
        for r in v["rpcs"].as_array().into_iter().flatten() {
            p.rpcs.push(rpc_from(r));
        }
        for s in v["scenarios"].as_array().into_iter().flatten() {
            p.scenarios.push(Scenario {
                name: string(&s["name"]),
                weight: int(&s["weight"]),
                calls: strings(&s["calls"]),
            });
        }
        if let Some(a) = v["sources"].as_array() {
            p.sources = Some(a.iter().map(string).collect());
        }
        Ok(p)
    }

    /// Loads a plan file.
    pub fn load(path: &str) -> Result<Plan, String> {
        let b = std::fs::read_to_string(path)
            .map_err(|e| format!("open {}: {}", path, crate::io_msg(&e)))?;
        let p = Plan::from_json(&b).map_err(|e| format!("{}: {}", path, e))?;
        if p.format != FORMAT {
            return Err(format!(
                "{}: plan format {}, this client reads {}",
                path, p.format, FORMAT
            ));
        }
        Ok(p)
    }
}

fn strs(v: &[String]) -> J {
    J::Arr(v.iter().map(|s| J::Str(s.clone())).collect())
}

fn rpc_json(r: &Rpc) -> J {
    let mut o: Vec<(String, J)> = vec![
        ("name".into(), J::Str(r.name.clone())),
        ("path".into(), J::Str(r.path.clone())),
        ("short".into(), J::Str(r.short.clone())),
        ("input".into(), J::Str(r.input.clone())),
        ("output".into(), J::Str(r.output.clone())),
    ];
    if r.client_streaming {
        o.push(("client_streaming".into(), J::Bool(true)));
    }
    if r.server_streaming {
        o.push(("server_streaming".into(), J::Bool(true)));
    }
    if !r.target.is_empty() {
        o.push(("target".into(), J::Str(r.target.clone())));
    }
    o.push(("weight".into(), J::Int(r.weight)));
    o.push(("deadline_ms".into(), J::Int(r.deadline_ms)));
    o.push((
        "retry".into(),
        J::Obj(vec![
            ("attempts".into(), J::Int(r.retry.attempts)),
            ("backoff_ms".into(), J::Int(r.retry.backoff_ms)),
            ("at_end".into(), J::Bool(r.retry.at_end)),
            ("end_tries".into(), J::Int(r.retry.end_tries)),
        ]),
    ));
    if !r.outcome_id.is_empty() {
        o.push(("outcome_id".into(), J::Str(r.outcome_id.clone())));
    }
    if !r.classify.is_empty() {
        o.push((
            "classify".into(),
            J::Obj(
                r.classify
                    .iter()
                    .map(|(k, v)| (k.clone(), J::Str(v.clone())))
                    .collect(),
            ),
        ));
    }
    if !r.fields.is_empty() {
        o.push((
            "fields".into(),
            J::Obj(
                r.fields
                    .iter()
                    .map(|(k, h)| (k.clone(), hint_json(h)))
                    .collect(),
            ),
        ));
    }
    if !r.oneofs.is_empty() {
        o.push((
            "oneofs".into(),
            J::Obj(
                r.oneofs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_ref().map(|v| strs(v)).unwrap_or(J::Null)))
                    .collect(),
            ),
        ));
    }
    o.push((
        "stream_len".into(),
        J::Arr(vec![J::Int(r.stream_len[0]), J::Int(r.stream_len[1])]),
    ));
    if !r.needs.is_empty() {
        o.push(("needs".into(), strs(&r.needs)));
    }
    J::Obj(o)
}

fn hint_json(h: &FieldHint) -> J {
    let mut o: Vec<(String, J)> = Vec::new();
    if let Some(v) = &h.value {
        o.push(("value".into(), J::Str(v.clone())));
    }
    if !h.choice.is_empty() {
        o.push(("choice".into(), strs(&h.choice)));
    }
    if !h.range.is_empty() {
        o.push(("range".into(), strs(&h.range)));
    }
    if !h.len.is_empty() {
        o.push((
            "len".into(),
            J::Arr(h.len.iter().map(|&x| J::Int(x)).collect()),
        ));
    }
    if h.unique {
        o.push(("unique".into(), J::Bool(true)));
    }
    if !h.prefix.is_empty() {
        o.push(("prefix".into(), J::Str(h.prefix.clone())));
    }
    if !h.format.is_empty() {
        o.push(("format".into(), J::Str(h.format.clone())));
    }
    if h.omit {
        o.push(("omit".into(), J::Bool(true)));
    }
    if let Some(p) = h.prob {
        o.push(("prob".into(), J::Float(p)));
    }
    if let Some(f) = &h.from {
        o.push((
            "from".into(),
            J::Obj(vec![
                ("rpc".into(), J::Str(f.rpc.clone())),
                ("side".into(), J::Str(f.side.clone())),
                ("path".into(), J::Str(f.path.clone())),
                ("pick".into(), J::Str(f.pick.clone())),
                ("fallback".into(), J::Str(f.fallback.clone())),
            ]),
        ));
    }
    J::Obj(o)
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or(0)
}

fn string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().map(string).collect()
}

fn pair(v: &Value) -> [i64; 2] {
    [int(&v[0]), int(&v[1])]
}

fn rpc_from(r: &Value) -> Rpc {
    let mut out = Rpc {
        name: string(&r["name"]),
        path: string(&r["path"]),
        short: string(&r["short"]),
        input: string(&r["input"]),
        output: string(&r["output"]),
        client_streaming: r["client_streaming"].as_bool().unwrap_or(false),
        server_streaming: r["server_streaming"].as_bool().unwrap_or(false),
        target: string(&r["target"]),
        weight: int(&r["weight"]),
        deadline_ms: int(&r["deadline_ms"]),
        retry: Retry {
            attempts: int(&r["retry"]["attempts"]),
            backoff_ms: int(&r["retry"]["backoff_ms"]),
            at_end: r["retry"]["at_end"].as_bool().unwrap_or(false),
            end_tries: int(&r["retry"]["end_tries"]),
        },
        outcome_id: string(&r["outcome_id"]),
        stream_len: pair(&r["stream_len"]),
        needs: strings(&r["needs"]),
        ..Default::default()
    };
    if let Some(m) = r["classify"].as_object() {
        for (k, v) in m {
            out.classify.insert(k.clone(), string(v));
        }
    }
    if let Some(m) = r["oneofs"].as_object() {
        for (k, v) in m {
            out.oneofs
                .insert(k.clone(), v.as_array().map(|_| strings(v)));
        }
    }
    if let Some(m) = r["fields"].as_object() {
        for (k, h) in m {
            out.fields.insert(
                k.clone(),
                FieldHint {
                    value: h["value"].as_str().map(str::to_string),
                    choice: strings(&h["choice"]),
                    range: strings(&h["range"]),
                    len: h["len"].as_array().into_iter().flatten().map(int).collect(),
                    unique: h["unique"].as_bool().unwrap_or(false),
                    prefix: string(&h["prefix"]),
                    format: string(&h["format"]),
                    omit: h["omit"].as_bool().unwrap_or(false),
                    prob: h["prob"].as_f64(),
                    from: h["from"].as_object().map(|_| From {
                        rpc: string(&h["from"]["rpc"]),
                        side: string(&h["from"]["side"]),
                        path: string(&h["from"]["path"]),
                        pick: string(&h["from"]["pick"]),
                        fallback: string(&h["from"]["fallback"]),
                    }),
                },
            );
        }
    }
    out
}
