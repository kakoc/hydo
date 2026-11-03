//! The rule set as a table: one schema change per row and its verdict under each kind of reader
//! ([`Readers::Current`]: the project format; [`Readers::Released`]: the results format and the
//! agent protocol). A verdict is `ok` (the change is an addition) or the place the check must
//! name. The crate documentation says why each rule is what it is.

use hydo_schema_compat::{check, Kind, Problem, Readers, Rules};
use serde_json::{json, Value};

fn base() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "records",
        "anyOf": [{"$ref": "#/$defs/Run"}, {"$ref": "#/$defs/Note"}],
        "$defs": {
            "Run": {
                "description": "A run.",
                "type": "object",
                "properties": {
                    "v": {"type": "integer", "const": 1},
                    "type": {"$ref": "#/$defs/RunType"},
                    "schedule": {"type": "string"},
                    "secs": {"type": ["number", "null"], "format": "double", "minimum": 0, "maximum": 100},
                    "count": {"type": "integer", "format": "uint64", "minimum": 0},
                    "mode": {"$ref": "#/$defs/Mode", "default": "fast"},
                    "run": {"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "env": {"type": "object", "additionalProperties": {"type": "string"}},
                    "name": {"type": "string", "pattern": "^[a-z]+$"},
                    "finding": {"$ref": "#/$defs/Finding"},
                    "parent": {"anyOf": [{"$ref": "#/$defs/Run"}, {"type": "null"}]}
                },
                "required": ["v", "type", "schedule"]
            },
            "RunType": {"type": "string", "enum": ["run"]},
            "Note": {
                "type": "object",
                "properties": {"type": {"type": "string", "const": "note"}, "text": {"type": "string"}},
                "required": ["type"]
            },
            "Mode": {"type": "string", "enum": ["fast", "slow"]},
            "Finding": {
                "type": "object",
                "additionalProperties": false,
                "properties": {"kind": {"type": "string"}, "id": {"type": "string"}},
                "required": ["kind"]
            },
            "Unused": {"type": "string"}
        }
    })
}

/// `Run`'s property `p`.
fn run<'a>(v: &'a mut Value, p: &str) -> &'a mut Value {
    &mut v["$defs"]["Run"]["properties"][p]
}

fn arr(v: &mut Value) -> &mut Vec<Value> {
    v.as_array_mut().unwrap()
}

fn obj(v: &mut Value) -> &mut serde_json::Map<String, Value> {
    v.as_object_mut().unwrap()
}

const OK: Option<&str> = None;

struct Row {
    change: &'static str,
    edit: fn(&mut Value),
    current: Option<&'static str>,
    released: Option<&'static str>,
}

macro_rules! rows {
    ($( $change:literal, |$v:ident| $edit:block, $cur:expr, $rel:expr; )*) => {
        vec![$( Row { change: $change, edit: |$v: &mut Value| $edit, current: $cur, released: $rel }, )*]
    };
}

const R: &str = "#/$defs/Run/properties";

fn table() -> Vec<Row> {
    rows![
        // nothing and notes
        "nothing changed", |_v| {}, OK, OK;
        "a description changed", |v| { v["$defs"]["Run"]["description"] = json!("A run, described anew."); }, OK, OK;
        "notes added (title, $comment, examples, deprecated)", |v| {
            let s = obj(run(v, "schedule"));
            s.insert("title".into(), json!("Schedule"));
            s.insert("$comment".into(), json!("x"));
            s.insert("examples".into(), json!(["run"]));
            s.insert("deprecated".into(), json!(true));
        }, OK, OK;
        // properties
        "a new optional property in an open object", |v| { *run(v, "point") = json!({"type": "string"}); }, OK, OK;
        "a new optional property in a closed object", |v| {
            v["$defs"]["Finding"]["properties"]["details"] = json!({"type": "object"});
        }, OK, OK;
        "a new required property", |v| {
            *run(v, "point") = json!({"type": "string"});
            arr(&mut v["$defs"]["Run"]["required"]).push(json!("point"));
        }, Some("#/$defs/Run/properties/point"), Some("#/$defs/Run/properties/point");
        "an optional property made required", |v| {
            arr(&mut v["$defs"]["Run"]["required"]).push(json!("secs"));
        }, Some("#/$defs/Run/properties/secs"), Some("#/$defs/Run/properties/secs");
        "a required property made optional", |v| {
            v["$defs"]["Run"]["required"] = json!(["v", "type"]);
        }, OK, Some("#/$defs/Run/properties/schedule");
        "a property removed", |v| { obj(&mut v["$defs"]["Run"]["properties"]).shift_remove("secs"); },
            Some("#/$defs/Run/properties/secs"), Some("#/$defs/Run/properties/secs");
        "a property renamed", |v| {
            let p = obj(&mut v["$defs"]["Run"]["properties"]);
            let s = p.shift_remove("secs").unwrap();
            p.insert("seconds".into(), s);
        }, Some("#/$defs/Run/properties/secs"), Some("#/$defs/Run/properties/secs");
        // type
        "a type changed", |v| { run(v, "schedule")["type"] = json!("integer"); },
            Some("#/$defs/Run/properties/schedule/type"), Some("#/$defs/Run/properties/schedule/type");
        "a type gained a kind (null)", |v| { run(v, "schedule")["type"] = json!(["string", "null"]); },
            OK, Some("#/$defs/Run/properties/schedule/type");
        "a type lost a kind (null)", |v| { run(v, "secs")["type"] = json!("number"); },
            Some("#/$defs/Run/properties/secs/type"), Some("#/$defs/Run/properties/secs/type");
        "integer became number", |v| { run(v, "count")["type"] = json!("number"); },
            OK, Some("#/$defs/Run/properties/count/type");
        "number became integer", |v| { run(v, "secs")["type"] = json!(["integer", "null"]); },
            Some("#/$defs/Run/properties/secs/type"), Some("#/$defs/Run/properties/secs/type");
        "a type where there was none", |v| { run(v, "run")["type"] = json!("string"); },
            Some("#/$defs/Run/properties/run/type"), Some("#/$defs/Run/properties/run/type");
        "a type gone", |v| { obj(run(v, "schedule")).shift_remove("type"); },
            OK, Some("#/$defs/Run/properties/schedule/type");
        // enum
        "an enum gained a value", |v| { arr(&mut v["$defs"]["Mode"]["enum"]).push(json!("medium")); },
            OK, Some("#/$defs/Mode/enum");
        "a record type's discriminator gained a value", |v| { arr(&mut v["$defs"]["RunType"]["enum"]).push(json!("run2")); },
            OK, Some("#/$defs/RunType/enum");
        "an enum lost a value", |v| { v["$defs"]["Mode"]["enum"] = json!(["fast"]); },
            Some("#/$defs/Mode/enum"), Some("#/$defs/Mode/enum");
        "an enum where there was none", |v| { run(v, "schedule")["enum"] = json!(["a", "b"]); },
            Some("#/$defs/Run/properties/schedule/enum"), Some("#/$defs/Run/properties/schedule/enum");
        // choices inside a value
        "a variant appended to a value's anyOf", |v| { arr(&mut run(v, "run")["anyOf"]).push(json!({"type": "integer"})); },
            OK, Some("#/$defs/Run/properties/run/anyOf");
        "a variant inserted before the old ones", |v| { arr(&mut run(v, "run")["anyOf"]).insert(0, json!({"type": "integer"})); },
            Some("#/$defs/Run/properties/run/anyOf/0"), Some("#/$defs/Run/properties/run/anyOf/0");
        "a variant removed", |v| { arr(&mut run(v, "run")["anyOf"]).pop(); },
            Some("#/$defs/Run/properties/run/anyOf/1"), Some("#/$defs/Run/properties/run/anyOf/1");
        "the variants reordered", |v| { arr(&mut run(v, "run")["anyOf"]).reverse(); },
            Some("#/$defs/Run/properties/run/anyOf/0"), Some("#/$defs/Run/properties/run/anyOf/0");
        "a choice where there was none", |v| { *run(v, "schedule") = json!({"type": "string", "anyOf": [{"pattern": "^a"}]}); },
            Some("#/$defs/Run/properties/schedule/anyOf"), Some("#/$defs/Run/properties/schedule/anyOf");
        "a choice gone", |v| { *run(v, "run") = json!({"type": "string"}); },
            Some("#/$defs/Run/properties/run/anyOf"), Some("#/$defs/Run/properties/run/anyOf");
        // the root's alternatives (record types, messages)
        "a record type appended", |v| {
            v["$defs"]["Shrink"] = json!({"type": "object", "properties": {"type": {"const": "shrink"}}});
            arr(&mut v["anyOf"]).push(json!({"$ref": "#/$defs/Shrink"}));
        }, OK, OK;
        "a record type inserted before the old ones", |v| {
            v["$defs"]["Shrink"] = json!({"type": "object", "properties": {"type": {"const": "shrink"}}});
            arr(&mut v["anyOf"]).insert(0, json!({"$ref": "#/$defs/Shrink"}));
        }, OK, OK;
        "the record types reordered", |v| { arr(&mut v["anyOf"]).reverse(); }, OK, OK;
        "a record type removed", |v| { arr(&mut v["anyOf"]).pop(); }, Some("#/anyOf/1"), Some("#/anyOf/1");
        "a record type changed", |v| { v["$defs"]["Note"]["properties"]["text"]["type"] = json!("integer"); },
            Some("#/$defs/Note/properties/text/type"), Some("#/$defs/Note/properties/text/type");
        // references and definitions
        "a definition renamed (its references with it)", |v| {
            let d = obj(&mut v["$defs"]);
            let f = d.shift_remove("Finding").unwrap();
            d.insert("Problem".into(), f);
            *run(v, "finding") = json!({"$ref": "#/$defs/Problem"});
        }, OK, OK;
        "a definition nothing refers to changed", |v| { v["$defs"]["Unused"]["type"] = json!("integer"); }, OK, OK;
        "a definition nothing refers to removed", |v| { obj(&mut v["$defs"]).shift_remove("Unused"); }, OK, OK;
        "a new definition", |v| { v["$defs"]["Later"] = json!({"type": "integer"}); }, OK, OK;
        "a referenced definition changed", |v| { v["$defs"]["Finding"]["properties"]["kind"]["type"] = json!("integer"); },
            Some("#/$defs/Finding/properties/kind/type"), Some("#/$defs/Finding/properties/kind/type");
        "a referenced definition removed", |v| { obj(&mut v["$defs"]).shift_remove("Finding"); },
            Some("#/$defs/Run/properties/finding"), Some("#/$defs/Run/properties/finding");
        "a reference written out in place", |v| { *run(v, "finding") = base()["$defs"]["Finding"].clone(); },
            Some("#/$defs/Run/properties/finding"), Some("#/$defs/Run/properties/finding");
        "an inline schema made a reference", |v| {
            v["$defs"]["Tags"] = json!({"type": "array", "items": {"type": "string"}});
            *run(v, "tags") = json!({"$ref": "#/$defs/Tags"});
        }, Some("#/$defs/Run/properties/tags"), Some("#/$defs/Run/properties/tags");
        "a definition that refers to itself, changed deep down", |v| {
            v["$defs"]["Run"]["properties"]["parent"]["anyOf"][1]["type"] = json!("string");
        }, Some("#/$defs/Run/properties/parent/anyOf/1/type"), Some("#/$defs/Run/properties/parent/anyOf/1/type");
        // default
        "a default changed", |v| { run(v, "mode")["default"] = json!("slow"); },
            Some("#/$defs/Run/properties/mode/default"), Some("#/$defs/Run/properties/mode/default");
        "a default gone", |v| { obj(run(v, "mode")).shift_remove("default"); },
            Some("#/$defs/Run/properties/mode/default"), Some("#/$defs/Run/properties/mode/default");
        "a default on an optional property that had none", |v| { run(v, "secs")["default"] = json!(null); },
            Some("#/$defs/Run/properties/secs/default"), Some("#/$defs/Run/properties/secs/default");
        "a required property made optional with a default", |v| {
            run(v, "schedule")["default"] = json!("");
            v["$defs"]["Run"]["required"] = json!(["v", "type"]);
        }, OK, Some("#/$defs/Run/properties/schedule");
        // const, format, pattern, uniqueItems
        "a const changed", |v| { run(v, "v")["const"] = json!(2); },
            Some("#/$defs/Run/properties/v/const"), Some("#/$defs/Run/properties/v/const");
        "a const gone", |v| { obj(run(v, "v")).shift_remove("const"); }, OK, Some("#/$defs/Run/properties/v/const");
        "a format changed", |v| { run(v, "count")["format"] = json!("uint32"); },
            Some("#/$defs/Run/properties/count/format"), Some("#/$defs/Run/properties/count/format");
        "a format gone", |v| { obj(run(v, "count")).shift_remove("format"); }, OK, Some("#/$defs/Run/properties/count/format");
        "a pattern where there was none", |v| { run(v, "schedule")["pattern"] = json!("^a"); },
            Some("#/$defs/Run/properties/schedule/pattern"), Some("#/$defs/Run/properties/schedule/pattern");
        "a pattern changed", |v| { run(v, "name")["pattern"] = json!("^[a-z0-9]+$"); },
            Some("#/$defs/Run/properties/name/pattern"), Some("#/$defs/Run/properties/name/pattern");
        "a pattern gone", |v| { obj(run(v, "name")).shift_remove("pattern"); }, OK, Some("#/$defs/Run/properties/name/pattern");
        "items made unique", |v| { run(v, "tags")["uniqueItems"] = json!(true); },
            Some("#/$defs/Run/properties/tags/uniqueItems"), Some("#/$defs/Run/properties/tags/uniqueItems");
        // bounds
        "a minimum raised", |v| { run(v, "secs")["minimum"] = json!(1); },
            Some("#/$defs/Run/properties/secs/minimum"), Some("#/$defs/Run/properties/secs/minimum");
        "a minimum lowered", |v| { run(v, "secs")["minimum"] = json!(-1); }, OK, Some("#/$defs/Run/properties/secs/minimum");
        "a maximum lowered", |v| { run(v, "secs")["maximum"] = json!(50); },
            Some("#/$defs/Run/properties/secs/maximum"), Some("#/$defs/Run/properties/secs/maximum");
        "a maximum raised", |v| { run(v, "secs")["maximum"] = json!(200); }, OK, Some("#/$defs/Run/properties/secs/maximum");
        "a bound where there was none", |v| { run(v, "schedule")["maxLength"] = json!(10); },
            Some("#/$defs/Run/properties/schedule/maxLength"), Some("#/$defs/Run/properties/schedule/maxLength");
        "a bound gone", |v| { obj(run(v, "count")).shift_remove("minimum"); }, OK, Some("#/$defs/Run/properties/count/minimum");
        // objects, maps and arrays
        "a closed object opened", |v| { obj(&mut v["$defs"]["Finding"]).shift_remove("additionalProperties"); }, OK, OK;
        "a closed object opened to a map", |v| { v["$defs"]["Finding"]["additionalProperties"] = json!({"type": "string"}); }, OK, OK;
        "an open object closed", |v| { v["$defs"]["Run"]["additionalProperties"] = json!(false); },
            Some("#/$defs/Run/additionalProperties"), Some("#/$defs/Run/additionalProperties");
        "a map's values narrowed", |v| { run(v, "env")["additionalProperties"]["type"] = json!("integer"); },
            Some("#/$defs/Run/properties/env/additionalProperties/type"), Some("#/$defs/Run/properties/env/additionalProperties/type");
        "a map's values widened", |v| { run(v, "env")["additionalProperties"]["type"] = json!(["string", "null"]); },
            OK, Some("#/$defs/Run/properties/env/additionalProperties/type");
        "an object's other keys restricted", |v| { v["$defs"]["Note"]["additionalProperties"] = json!({"type": "string"}); },
            Some("#/$defs/Note/additionalProperties/type"), Some("#/$defs/Note/additionalProperties/type");
        "items changed", |v| { run(v, "tags")["items"]["type"] = json!("integer"); },
            Some("#/$defs/Run/properties/tags/items/type"), Some("#/$defs/Run/properties/tags/items/type");
        "items gone", |v| { obj(run(v, "tags")).shift_remove("items"); }, OK, Some("#/$defs/Run/properties/tags/items/type");
        "items that accept nothing", |v| { run(v, "tags")["items"] = json!(false); },
            Some("#/$defs/Run/properties/tags/items"), Some("#/$defs/Run/properties/tags/items");
        // keywords the check does not judge
        "a new keyword the check does not judge", |v| { v["$defs"]["Run"]["allOf"] = json!([{"required": ["name"]}]); },
            Some("#/$defs/Run/allOf"), Some("#/$defs/Run/allOf");
        "the draft changed", |v| { v["$schema"] = json!("http://json-schema.org/draft-07/schema#"); },
            Some("#/$schema"), Some("#/$schema");
    ]
}

fn run_table(readers: Readers, want: impl Fn(&Row) -> Option<&'static str>) -> Vec<String> {
    let rules = Rules::new(readers);
    let mut wrong = Vec::new();
    for row in table() {
        let mut new = base();
        (row.edit)(&mut new);
        let got = check(&base(), &new, &rules);
        let shown = got.iter().map(Problem::to_string).collect::<Vec<_>>();
        match want(&row) {
            None if got.is_empty() => {}
            None => wrong.push(format!(
                "{readers:?}: {}: want ok, got {shown:?}",
                row.change
            )),
            Some(at) if got.iter().any(|p| p.at == at) => {}
            Some(at) => wrong.push(format!(
                "{readers:?}: {}: want a problem at {at}, got {shown:?}",
                row.change
            )),
        }
    }
    wrong
}

#[test]
fn the_table_for_readers_of_the_current_schema() {
    let wrong = run_table(Readers::Current, |r| r.current);
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn the_table_for_readers_of_the_released_schema() {
    let wrong = run_table(Readers::Released, |r| r.released);
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn the_released_readers_refuse_everything_the_current_ones_refuse() {
    for row in table() {
        if row.current.is_some() {
            assert!(row.released.is_some(), "{}", row.change);
        }
    }
}

#[test]
fn an_open_place_lets_its_enum_gain_values_and_nothing_else() {
    let mut grown = base();
    arr(&mut grown["$defs"]["Mode"]["enum"]).push(json!("medium"));
    let rules = Rules::new(Readers::Released).open_enum("#/$defs/Mode");
    assert_eq!(check(&base(), &grown, &rules), vec![]);
    // a prefix names every place it starts
    let rules = Rules::new(Readers::Released).open_enum("#/$defs/Mo*");
    assert_eq!(check(&base(), &grown, &rules), vec![]);
    // a value gone still narrows, and the place's other keywords keep their rules
    let mut shrunk = base();
    shrunk["$defs"]["Mode"]["enum"] = json!(["fast"]);
    shrunk["$defs"]["Mode"]["type"] = json!(["string", "null"]);
    let got: Vec<String> = check(&base(), &shrunk, &rules)
        .iter()
        .map(Problem::to_string)
        .collect();
    assert_eq!(
        got,
        [
            "#/$defs/Mode/type: gained the kinds null",
            "#/$defs/Mode/enum: the value \"slow\" is gone"
        ]
    );
    // another enum is not open
    let mut other = base();
    arr(&mut other["$defs"]["RunType"]["enum"]).push(json!("run2"));
    assert_eq!(check(&base(), &other, &rules).len(), 1);
}

#[test]
fn an_open_place_that_names_no_enum_is_a_problem() {
    let rules = Rules::new(Readers::Released)
        .open_enum("#/$defs/Mode")
        .open_enum("#/$defs/Gone")
        .open_enum("#/$defs/Run/properties/schedule");
    let got = check(&base(), &base(), &rules);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.iter().all(|p| p.kind == Kind::UnknownPlace));
    assert_eq!(got[0].at, "#/$defs/Gone");
}

#[test]
fn problems_say_how_a_change_breaks_the_rule() {
    let mut new = base();
    obj(&mut new["$defs"]["Run"]["properties"]).shift_remove("secs");
    run(&mut new, "schedule")["type"] = json!(["string", "null"]);
    arr(&mut new["$defs"]["Run"]["required"]).push(json!("name"));
    let got = check(&base(), &new, &Rules::new(Readers::Released));
    let kinds: Vec<(String, Kind)> = got.iter().map(|p| (p.at.clone(), p.kind)).collect();
    assert_eq!(
        kinds,
        [
            (format!("{R}/schedule/type"), Kind::Widens),
            (format!("{R}/secs"), Kind::Changes),
            (format!("{R}/name"), Kind::Narrows),
        ]
    );
    assert_eq!(got[1].to_string(), format!("{R}/secs: the key is gone"));
    // the readers of the current schema do not hear about the widening
    let got = check(&base(), &new, &Rules::new(Readers::Current));
    assert_eq!(got.len(), 2);
}
