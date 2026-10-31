//! The JSON Schema (draft 2020-12) of protocol version 1, generated from the types: every
//! command (`command.<op>`), every reply (`reply.<op>`), the refusal (`error`) and every event
//! (`event.<name>`) is a definition, and the root is the choice of all of them. A command's
//! definition is closed (`additionalProperties: false`, as the agent refuses unknown fields);
//! the agent's lines are open (the host ignores fields it does not know).
//!
//! `tests/schema.rs` compares it with the committed `hydo/schema/agent-v1.json` and checks that
//! it only grew from `hydo/schema/released/agent-v1.json`.

use crate::field::{obj, s, Elem};
use crate::json::{Obj, Value};
use crate::msg::{Command, ErrCode, Event, Reply, PROTO};

fn int_u64(desc: &str) -> Value {
    with(u64::elem_schema(), desc)
}

fn with(mut v: Value, desc: &str) -> Value {
    if let Value::Obj(o) = &mut v {
        o.insert("description".into(), s(desc));
    }
    v
}

fn konst(kind: &str, c: Value, desc: &str) -> Value {
    obj([("type", s(kind)), ("const", c), ("description", s(desc))])
}

fn def(
    desc: String,
    mut props: Obj,
    mut req: Vec<Value>,
    env: Vec<(&str, Value, bool)>,
    closed: bool,
) -> Value {
    for (k, v, required) in env {
        props.insert(k.into(), v);
        if required {
            req.push(s(k));
        }
    }
    req.sort_by_key(|v| v.to_canonical());
    let mut o = Obj::new();
    o.insert("type".into(), s("object"));
    o.insert("description".into(), s(&desc));
    o.insert("properties".into(), Value::Obj(props));
    o.insert("required".into(), Value::Arr(req));
    if closed {
        o.insert("additionalProperties".into(), Value::Bool(false));
    }
    Value::Obj(o)
}

fn v_prop() -> Value {
    konst(
        "integer",
        Value::Int(PROTO as i128),
        "The protocol version of the line.",
    )
}

/// The schema as a value.
pub fn schema() -> Value {
    let mut defs = Obj::new();
    let mut alts = Vec::new();
    let mut add = |name: String, d: Value, defs: &mut Obj| {
        alts.push(obj([("$ref", s(&format!("#/$defs/{name}")))]));
        defs.insert(name, d);
    };
    for (op, doc, p, r) in Command::schema_parts() {
        let env = vec![
            ("v", v_prop(), true),
            (
                "id",
                int_u64("The command's id; every command has a higher id than the one before."),
                true,
            ),
            ("op", konst("string", s(op), "The command."), true),
        ];
        add(
            format!("command.{op}"),
            def(format!("Host to agent: {doc}"), p, r, env, true),
            &mut defs,
        );
    }
    for (op, doc, p, r) in Reply::schema_parts() {
        let env = vec![
            ("v", v_prop(), true),
            (
                "seq",
                int_u64("The agent line's number, from 0 (the hello)."),
                true,
            ),
            ("re", int_u64("The id of the command this answers."), true),
            (
                "op",
                konst("string", s(op), "The command this answers."),
                true,
            ),
            (
                "ok",
                konst("boolean", Value::Bool(true), "The command was carried out."),
                true,
            ),
        ];
        add(
            format!("reply.{op}"),
            def(
                format!("Agent to host, the answer to {op}: {doc}"),
                p,
                r,
                env,
                false,
            ),
            &mut defs,
        );
    }
    let env = vec![
        ("v", v_prop(), true),
        (
            "seq",
            int_u64("The agent line's number, from 0 (the hello)."),
            true,
        ),
        (
            "re",
            int_u64("The id of the refused command, when the agent could read it."),
            false,
        ),
        (
            "op",
            with(
                obj([("type", s("string"))]),
                "The refused command's op, when the agent could read it.",
            ),
            false,
        ),
        (
            "ok",
            konst(
                "boolean",
                Value::Bool(false),
                "The command was not carried out.",
            ),
            true,
        ),
        ("err", with(ErrCode::elem_schema(), "Why."), true),
        (
            "msg",
            with(obj([("type", s("string"))]), "The reason in words."),
            true,
        ),
        (
            "errno",
            with(i64::elem_schema(), "The errno of a failed system call."),
            false,
        ),
    ];
    add(
        "error".into(),
        def(
            "Agent to host: a line or command the agent refused or could not carry out.".into(),
            Obj::new(),
            vec![],
            env,
            false,
        ),
        &mut defs,
    );
    for (ev, doc, p, r) in Event::schema_parts() {
        let env = vec![
            ("v", v_prop(), true),
            (
                "seq",
                int_u64("The agent line's number, from 0 (the hello)."),
                true,
            ),
            ("ev", konst("string", s(ev), "The event."), true),
        ];
        add(
            format!("event.{ev}"),
            def(format!("Agent to host, an event: {doc}"), p, r, env, false),
            &mut defs,
        );
    }
    let mut root = Obj::new();
    root.insert(
        "$schema".into(),
        s("https://json-schema.org/draft/2020-12/schema"),
    );
    root.insert("title".into(), s("hydo agent protocol, version 1"));
    root.insert(
        "description".into(),
        s("One line of the conversation between hydo's host and hydo-agent, the guest's first process. \
           Host lines are canonical JSON followed by a newline on the serial input; agent lines are framed \
           on the console as `HYDO-AG <len> <crc32> <json>`. Generated from the crate hydo-agent-proto; \
           version 1 only grows (new ops, events, optional fields and names)."),
    );
    root.insert("oneOf".into(), Value::Arr(alts));
    root.insert("$defs".into(), Value::Obj(defs));
    Value::Obj(root)
}

/// The committed form of the schema (pretty, with a final newline).
pub fn schema_text() -> String {
    schema().to_pretty()
}
