//! One test per mapping of `hydo fake from-openapi` (Milestone 249). The goldens on two real
//! public specs are in tests/openapi.rs.

use super::*;
use serde_json::json;

fn gen(spec: &str, o: Opts) -> Result<Generated, String> {
    let mut o = o;
    if o.command.is_empty() {
        o.command = "hydo fake from-openapi spec.yaml".into();
    }
    generate_bytes(spec.as_bytes(), &o)
}

fn routes(g: &Generated) -> Vec<Value> {
    g.fake["routes"].as_array().unwrap().clone()
}

fn route<'a>(rs: &'a [Value], name: &str) -> &'a Value {
    rs.iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no route {name} in {rs:?}"))
}

const HEAD: &str = "openapi: 3.0.3\ninfo: {title: Payments API, version: '1.0'}\n";

#[test]
fn yaml_core_schema_anchors_and_merge_keys() {
    let v = load_spec(
        "a: &x {k: 1, s: 'yes', n: ~, b: true, f: 1.5, h: 0x1f}\nb:\n  <<: *x\n  k: 2\n200: ok\nq: \"200\"\nno: no\n",
    )
    .unwrap();
    assert_eq!(
        v,
        json!({"a": {"k": 1, "s": "yes", "n": null, "b": true, "f": 1.5, "h": 31},
               "b": {"k": 2, "s": "yes", "n": null, "b": true, "f": 1.5, "h": 31},
               "200": "ok", "q": "200", "no": "no"})
    );
    assert_eq!(load_spec("{\"a\": [1]}").unwrap(), json!({"a": [1]}));
    assert!(load_spec("a: 1\n---\nb: 2\n").is_err());
}

#[test]
fn names_from_operation_ids_and_paths() {
    assert_eq!(snake("createTransfer"), "create_transfer");
    assert_eq!(snake("get-transfers"), "get_transfers");
    assert_eq!(snake("listHTTPRoutes"), "list_http_routes");
    assert_eq!(snake("v1.pets.list"), "v1_pets_list");
    assert_eq!(snake("2fa"), "op_2fa");
    assert_eq!(name_of_path("get", "/pets/{petId}"), "get_pets_by_pet_id");
    assert_eq!(
        default_name(&json!({"info": {"title": "Swagger Petstore"}})),
        "swagger_petstore"
    );
}

#[test]
fn one_route_per_operation_with_the_base_path_and_literal_segments_first() {
    let spec = format!(
        "{HEAD}servers: [{{url: 'https://api.example.com/{{v}}', variables: {{v: {{default: v2}}}}}}]
paths:
  /things/{{id}}:
    get:
      operationId: getThing
      parameters: [{{name: id, in: path, required: true, schema: {{type: string}}}}]
      responses: {{'200': {{description: ok, content: {{application/json: {{schema: {{type: object, properties: {{id: {{type: string}}, n: {{type: integer}}}}}}}}}}}}}}
  /things/search:
    get:
      responses: {{'200': {{description: ok}}}}
"
    );
    let g = gen(&spec, Opts::default()).unwrap();
    let rs = routes(&g);
    assert_eq!(rs.len(), 2);
    // /things/search before /things/{id}: hydo-world takes the first route that fits
    assert_eq!(rs[0]["name"], "get_things_search");
    assert_eq!(rs[0]["path"], "/v2/things/search");
    assert_eq!(rs[0]["method"], "GET");
    assert_eq!(rs[1]["name"], "get_thing");
    assert_eq!(rs[1]["path"], "/v2/things/{id}");
    assert_eq!(rs[1]["body"], r#"{"id":"{path:id}","n":0}"#);
    assert_eq!(g.fake["name"], "payments_api");
    assert_eq!(g.fake["port"], 9001);
    assert!(g.refused.is_empty());
}

#[test]
fn the_success_answer_from_examples_then_the_schema() {
    let spec = format!(
        "{HEAD}paths:
  /a:
    get:
      responses:
        '201': {{description: x, content: {{application/json: {{example: {{z: 1, y: [a]}}}}}}}}
        '200': {{description: x, content: {{application/json: {{examples: {{one: {{value: {{w: 2}}}}}}}}}}}}
  /b:
    get:
      responses:
        2XX:
          description: x
          content:
            application/json:
              schema:
                type: object
                required: [kind, at, uid, mail, count, ok, tags, deep]
                properties:
                  kind: {{type: string, enum: [card, bank]}}
                  at: {{type: string, format: date-time}}
                  uid: {{type: string, format: uuid}}
                  mail: {{type: string, format: email}}
                  count: {{type: integer, minimum: 5}}
                  ok: {{type: boolean}}
                  tags: {{type: array, items: {{type: string, minLength: 8}}}}
                  maybe: {{type: [string, 'null'], examples: [hello]}}
                  fixed: {{const: 7}}
                  secret: {{type: string, writeOnly: true}}
                  deep:
                    type: object
                    properties:
                      inner: {{type: object, properties: {{x: {{type: string}}}}, required: [x]}}
                      opt: {{type: object, properties: {{y: {{type: string}}}}}}
  /c:
    delete:
      responses: {{'204': {{description: gone}}, default: {{description: err, content: {{application/json: {{example: {{error: x}}}}}}}}}}
  /d:
    get:
      responses: {{default: {{description: any, content: {{text/plain: {{example: pong}}}}}}}}
"
    );
    let g = gen(&spec, Opts::default()).unwrap();
    let rs = routes(&g);
    let a = route(&rs, "get_a");
    assert_eq!(a["status"], 200);
    assert_eq!(a["body"], r#"{"w":2}"#);
    let b = route(&rs, "get_b");
    assert_eq!(b["status"], 200);
    assert_eq!(
        b["body"],
        r#"{"kind":"card","at":"2026-01-01T00:00:00Z","uid":"00000000-0000-4000-8000-000000000000","mail":"user@example.com","count":5,"ok":false,"tags":["stringxx"],"maybe":"hello","fixed":7,"deep":{"inner":{"x":"string"},"opt":{}}}"#
    );
    let c = route(&rs, "delete_c");
    assert_eq!(c["status"], 204);
    assert!(c.get("body").is_none());
    assert_eq!(c["errors"], json!({"default": r#"{"error":"x"}"#}));
    let d = route(&rs, "get_d");
    assert_eq!(d["status"], 200);
    assert_eq!(d["body"], "pong");
    assert_eq!(d["headers"], json!({"Content-Type": "text/plain"}));
    assert!(d.get("errors").is_none());
}

#[test]
fn error_answers_by_status_class_and_default() {
    let spec = format!(
        "{HEAD}paths:
  /p:
    post:
      responses:
        '200': {{description: ok, content: {{application/json: {{example: {{id: p1}}}}}}}}
        '409': {{$ref: '#/components/responses/Conflict'}}
        4XX: {{description: client, content: {{application/json: {{schema: {{$ref: '#/components/schemas/Err'}}}}}}}}
        '503': {{description: down}}
        '302': {{description: moved}}
        default: {{description: other, content: {{application/json: {{example: {{code: other}}}}}}}}
components:
  responses:
    Conflict: {{description: c, content: {{application/json: {{example: {{code: conflict}}}}}}}}
  schemas:
    Err: {{type: object, required: [code], properties: {{code: {{type: string, example: bad}}}}}}
"
    );
    let g = gen(&spec, Opts::default()).unwrap();
    let r = &routes(&g)[0];
    assert_eq!(
        r["errors"],
        json!({"409": r#"{"code":"conflict"}"#, "4XX": r#"{"code":"bad"}"#, "503": "", "default": r#"{"code":"other"}"#})
    );
}

#[test]
fn templates_where_the_schema_allows_them() {
    let spec = format!(
        "{HEAD}paths:
  /v1/charges:
    post:
      operationId: createCharge
      requestBody:
        content:
          application/x-www-form-urlencoded:
            schema:
              type: object
              required: [amount, currency]
              properties: {{amount: {{type: integer}}, currency: {{type: string}}, note: {{type: string}}}}
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  id: {{type: string, example: ch_3Mtw}}
                  amount: {{type: integer}}
                  currency: {{type: string}}
                  note: {{type: string}}
  /v1/orders:
    post:
      parameters: [{{name: shop, in: query, required: true, schema: {{type: string}}}}]
      requestBody:
        content:
          application/json:
            schema: {{type: object, required: [total], properties: {{total: {{type: number}}}}}}
      responses:
        '201':
          description: ok
          content:
            application/json:
              schema: {{type: object, properties: {{id: {{type: integer}}, total: {{type: number}}, shop: {{type: string}}}}}}
  /v1/users:
    post:
      responses:
        '201':
          description: ok
          content:
            application/json:
              schema: {{type: object, properties: {{id: {{type: string, format: uuid}}}}}}
"
    );
    let g = gen(&spec, Opts::default()).unwrap();
    let rs = routes(&g);
    assert_eq!(
        route(&rs, "create_charge")["body"],
        r#"{"id":"ch_{seq}","amount":{form:amount},"currency":"{form:currency}","note":"string"}"#
    );
    assert_eq!(
        route(&rs, "post_v1_orders")["body"],
        r#"{"id":{seq},"total":{json:total},"shop":"{query:shop}"}"#
    );
    assert_eq!(route(&rs, "post_v1_users")["body"], r#"{"id":"{uuid}"}"#);
    // no save:/load: without --stateful
    assert!(rs
        .iter()
        .all(|r| r.get("save").is_none() && r.get("load").is_none()));
}

const STATEFUL: &str = "paths:
  /transfers:
    post:
      operationId: createTransfer
      parameters: [{$ref: '#/components/parameters/Idem'}]
      requestBody:
        content:
          application/json:
            schema: {$ref: '#/components/schemas/NewTransfer'}
      responses:
        '201': {description: ok, content: {application/json: {schema: {$ref: '#/components/schemas/Transfer'}}}}
        '422': {description: bad, content: {application/json: {example: {error: invalid}}}}
    get:
      operationId: listTransfers
      responses:
        '200': {description: ok, content: {application/json: {schema: {type: array, items: {$ref: '#/components/schemas/Transfer'}}}}}
  /transfers/{id}:
    parameters: [{name: id, in: path, required: true, schema: {type: string}}]
    get:
      operationId: getTransfer
      responses:
        '200': {description: ok, content: {application/json: {schema: {$ref: '#/components/schemas/Transfer'}}}}
        '404': {description: none, content: {application/json: {example: {error: not_found}}}}
    put:
      operationId: replaceTransfer
      requestBody:
        content:
          application/json:
            schema: {$ref: '#/components/schemas/NewTransfer'}
      responses:
        '200': {description: ok, content: {application/json: {schema: {$ref: '#/components/schemas/Transfer'}}}}
    delete:
      operationId: deleteTransfer
      responses: {'204': {description: gone}}
  /transfers/{transferId}/cancel:
    post:
      operationId: cancelTransfer
      parameters:
        - {name: transferId, in: path, required: true, schema: {type: string}}
        - {name: Idempotency-Key, in: header, schema: {type: string}}
      responses:
        '200': {description: ok, content: {application/json: {schema: {$ref: '#/components/schemas/Transfer'}}}}
components:
  parameters:
    Idem: {name: Idempotency-Key, in: header, required: false, schema: {type: string}}
  schemas:
    NewTransfer:
      type: object
      required: [amount, account]
      properties: {amount: {type: integer}, account: {type: string}, memo: {type: string}}
    Transfer:
      type: object
      properties:
        id: {type: string, example: tr_1}
        amount: {type: integer}
        account: {type: string}
        status: {type: string, enum: [pending, done]}
";

#[test]
fn stateful_create_then_read_pairs_and_the_idempotency_header() {
    let spec = format!("{HEAD}{STATEFUL}");
    let g = gen(
        &spec,
        Opts {
            stateful: true,
            name: Some("bank".into()),
            port: Some(9101),
            ..Opts::default()
        },
    )
    .unwrap();
    assert_eq!(g.fake["name"], "bank");
    assert_eq!(g.fake["port"], 9101);
    assert_eq!(g.fake["idempotency"], "header:Idempotency-Key");
    let rs = routes(&g);
    let c = route(&rs, "create_transfer");
    assert_eq!(c["status"], 201);
    assert_eq!(c["idempotent"], true);
    assert_eq!(
        c["body"],
        r#"{"id":"tr_{seq}","amount":{json:amount},"account":"{json:account}","status":"pending"}"#
    );
    assert_eq!(
        c["save"],
        json!({"key": "tr_{seq}", "fields": {"account": "{json:account}", "amount": "{json:amount}"}})
    );
    assert_eq!(c["errors"], json!({"422": r#"{"error":"invalid"}"#}));
    let list = route(&rs, "list_transfers");
    assert!(list.get("load").is_none() && list.get("save").is_none());
    assert!(list.get("idempotent").is_none());
    let get = route(&rs, "get_transfer");
    assert_eq!(get["load"], "{path:id}");
    assert_eq!(
        get["body"],
        r#"{"id":"{path:id}","amount":{obj:amount},"account":"{obj:account}","status":"pending"}"#
    );
    assert!(get.get("save").is_none());
    let put = route(&rs, "replace_transfer");
    assert_eq!(put["load"], "{path:id}");
    assert_eq!(
        put["save"],
        json!({"key": "{path:id}", "fields": {"account": "{json:account}", "amount": "{json:amount}"}})
    );
    assert_eq!(route(&rs, "delete_transfer")["load"], "{path:id}");
    let cancel = route(&rs, "cancel_transfer");
    assert_eq!(cancel["load"], "{path:transferId}");
    assert_eq!(cancel["idempotent"], true);
    // an action answers the resource it is about (the schema GET /transfers/{id} answers)
    assert_eq!(
        cancel["body"],
        r#"{"id":"{path:transferId}","amount":{obj:amount},"account":"{obj:account}","status":"pending"}"#
    );
}

#[test]
fn only_the_named_operations() {
    let spec = format!("{HEAD}{STATEFUL}");
    let g = gen(
        &spec,
        Opts {
            ops: Some(vec!["createTransfer".into(), "get_transfer".into()]),
            ..Opts::default()
        },
    )
    .unwrap();
    let names: Vec<_> = routes(&g).iter().map(|r| r["name"].clone()).collect();
    assert_eq!(names, [json!("create_transfer"), json!("get_transfer")]);
    let g = gen(
        &spec,
        Opts {
            ops: Some(vec!["createTransfer".into(), "nope".into()]),
            ..Opts::default()
        },
    )
    .unwrap();
    assert_eq!(g.refused_named, ["nope: no such operation in the spec"]);
}

#[test]
fn discriminators_and_one_of_without_one() {
    let spec = format!(
        "{HEAD}paths:
  /pay:
    get:
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                oneOf: [{{$ref: '#/components/schemas/Card'}}, {{$ref: '#/components/schemas/Bank'}}]
                discriminator: {{propertyName: method, mapping: {{bank: '#/components/schemas/Bank', card: '#/components/schemas/Card'}}}}
  /nullable:
    get:
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema: {{type: object, properties: {{a: {{oneOf: [{{type: 'null'}}, {{type: integer}}]}}}}}}
  /optional:
    get:
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  a: {{type: string}}
                  b: {{oneOf: [{{type: string}}, {{type: integer}}]}}
  /ambiguous:
    get:
      operationId: getAmbiguous
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema: {{oneOf: [{{type: string}}, {{type: integer}}]}}
components:
  schemas:
    Card: {{type: object, required: [last4], properties: {{method: {{type: string}}, last4: {{type: string, example: '4242'}}}}}}
    Bank: {{type: object, required: [iban], properties: {{method: {{type: string}}, iban: {{type: string}}}}}}
"
    );
    let g = gen(&spec, Opts::default()).unwrap();
    let rs = routes(&g);
    assert_eq!(
        route(&rs, "get_pay")["body"],
        r#"{"method":"bank","iban":"string"}"#
    );
    assert_eq!(route(&rs, "get_nullable")["body"], r#"{"a":0}"#);
    // an optional field it cannot make is left out
    assert_eq!(route(&rs, "get_optional")["body"], r#"{"a":"string"}"#);
    assert_eq!(
        g.refused,
        ["getAmbiguous (GET /ambiguous): a oneOf without a discriminator (2 alternatives), so no one answer is right"]
    );
}

#[test]
fn refusals_name_the_operation_and_the_reason() {
    let spec = format!(
        "{HEAD}paths:
  /ok:
    get:
      responses: {{'200': {{description: ok}}}}
  /hooks:
    post:
      operationId: subscribe
      callbacks: {{onEvent: {{'{{$request.body#/url}}': {{post: {{responses: {{'200': {{description: ok}}}}}}}}}}}}
      responses: {{'201': {{description: ok}}}}
  /upload:
    post:
      operationId: upload
      requestBody: {{content: {{application/octet-stream: {{schema: {{type: string, format: binary}}}}}}}}
      responses: {{'201': {{description: ok}}}}
  /form:
    post:
      operationId: uploadForm
      requestBody: {{content: {{multipart/form-data: {{schema: {{type: object}}}}}}}}
      responses: {{'201': {{description: ok}}}}
  /pdf:
    get:
      operationId: getPdf
      responses: {{'200': {{description: ok, content: {{application/pdf: {{schema: {{type: string, format: binary}}}}}}}}}}
  /xml:
    get:
      operationId: getXml
      responses: {{'200': {{description: ok, content: {{application/xml: {{schema: {{type: object}}}}}}}}}}
  /ext:
    get:
      operationId: getExt
      responses: {{'200': {{$ref: 'other.yaml#/components/responses/R'}}}}
  /files/{{name}}.json:
    get:
      operationId: getFile
      responses: {{'200': {{description: ok}}}}
  /none:
    get:
      operationId: getNone
      responses: {{'404': {{description: none}}}}
"
    );
    let g = gen(&spec, Opts::default()).unwrap();
    assert_eq!(routes(&g).len(), 1);
    assert_eq!(
        g.refused,
        [
            "subscribe (POST /hooks): it has callbacks: (the provider calls the system back); give the route a webhook: by hand",
            "upload (POST /upload): the request body is binary (application/octet-stream)",
            "uploadForm (POST /form): the request body is multipart/form-data (an upload), which hydo's fakes do not parse",
            "getPdf (GET /pdf): the answer is binary (application/pdf)",
            "getXml (GET /xml): the answer is application/xml, not JSON, form-encoded or text",
            "getExt (GET /ext): a $ref to another file (other.yaml#/components/responses/R) is not followed",
            "getFile (GET /files/{name}.json): a path parameter inside the segment {name}.json; hydo's routes match whole segments",
            "getNone (GET /none): it has no 2xx or default answer",
        ]
    );
    let only = gen(
        &spec,
        Opts {
            ops: Some(vec!["upload".into(), "get_ok".into()]),
            ..Opts::default()
        },
    )
    .unwrap();
    assert_eq!(only.refused_named, ["upload: not mapped (above)"]);
    // nothing to map at all
    let e = gen(
        &spec,
        Opts {
            ops: Some(vec!["upload".into()]),
            ..Opts::default()
        },
    )
    .unwrap_err();
    assert!(
        e.starts_with("no operation could be mapped:\n  upload (POST /upload)"),
        "{e}"
    );
}

#[test]
fn swagger_and_unknown_versions_are_refused() {
    let e = gen("swagger: '2.0'\npaths: {}\n", Opts::default()).unwrap_err();
    assert!(e.contains("Swagger 2.0"), "{e}");
    let e = gen("openapi: 4.0.0\npaths: {}\n", Opts::default()).unwrap_err();
    assert_eq!(
        e,
        "openapi: 4.0.0: only OpenAPI 3.0 and 3.1 descriptions are read"
    );
    let e = gen(
        "openapi: 3.1.0\npaths: {/a: {get: {responses: {'200': {description: x}}}}}\n",
        Opts {
            name: Some("Bad-Name".into()),
            ..Opts::default()
        },
    )
    .unwrap_err();
    assert!(e.contains("[a-z][a-z0-9_]*"), "{e}");
}

#[test]
fn recursive_schemas_stop_and_large_answers_shrink() {
    let mut props = String::new();
    for i in 0..400 {
        props.push_str(&format!(
            "        f{i}: {{type: object, properties: {{x: {{type: string, minLength: 40}}}}}}\n"
        ));
    }
    let spec = format!(
        "{HEAD}paths:
  /tree:
    get:
      responses:
        '200': {{description: ok, content: {{application/json: {{schema: {{$ref: '#/components/schemas/Node'}}}}}}}}
  /big:
    get:
      responses:
        '200': {{description: ok, content: {{application/json: {{schema: {{$ref: '#/components/schemas/Big'}}}}}}}}
components:
  schemas:
    Node:
      type: object
      required: [name, child]
      properties: {{name: {{type: string}}, child: {{$ref: '#/components/schemas/Node'}}}}
    Big:
      type: object
      properties:
{props}"
    );
    let g = gen(&spec, Opts::default()).unwrap();
    let rs = routes(&g);
    assert_eq!(
        route(&rs, "get_tree")["body"],
        r#"{"name":"string","child":null}"#
    );
    let big = route(&rs, "get_big")["body"].as_str().unwrap().to_string();
    assert!(big.len() < 16 * 1024, "{}", big.len());
    assert!(big.starts_with(r#"{"f0":{},"f1":{}"#), "{}", &big[..40]);
}

#[test]
fn tls_hosts_from_the_first_server() {
    let spec = format!(
        "{HEAD}servers: [{{url: 'https://api.pay.example/v1'}}]\npaths: {{/a: {{get: {{responses: {{'200': {{description: x}}}}}}}}}}\n"
    );
    let g = gen(
        &spec,
        Opts {
            tls: true,
            ..Opts::default()
        },
    )
    .unwrap();
    assert_eq!(g.fake["tls"], json!({"hosts": ["api.pay.example"]}));
    assert_eq!(routes(&g)[0]["path"], "/v1/a");
    let g = gen(
        &spec,
        Opts {
            base_path: Some("/".into()),
            ..Opts::default()
        },
    )
    .unwrap();
    assert_eq!(routes(&g)[0]["path"], "/a");
    assert!(g
        .header
        .iter()
        .any(|h| h.contains("tls: {hosts: [api.pay.example]}")));
}

#[test]
fn the_yaml_reads_back_and_quotes_what_it_must() {
    let spec = format!(
        "{HEAD}paths:
  /a:
    get:
      summary: \"it's a 'quoted' # summary: with colons\"
      responses:
        '200': {{description: x, content: {{application/json: {{example: {{text: \"it's #1: yes\", n: true}}}}}}}}
"
    );
    // generate_bytes checks that the YAML reads back as the same fake
    let g = gen(&spec, Opts::default()).unwrap();
    let y = file_yaml(&g);
    assert!(
        y.contains("    description: 'it''s a ''quoted'' # summary: with colons'\n"),
        "{y}"
    );
    assert!(
        y.contains(r#"    body: '{"text":"it''s #1: yes","n":true}'"#),
        "{y}"
    );
}

#[test]
fn added_to_a_manifest() {
    let spec =
        format!("{HEAD}paths: {{/a: {{get: {{responses: {{'200': {{description: x}}}}}}}}}}\n");
    let g = gen(
        &spec,
        Opts {
            name: Some("pay".into()),
            ..Opts::default()
        },
    )
    .unwrap();
    // into a fakes: block whose items are at column 0, before the next key and its comment
    let m = "name: x\nfakes:\n- name: other\n  port: 9000\n  routes:\n  - {name: r, path: /}\n\n# the services\nservices:\n  - {name: s, cmd: sleep 1}\n";
    let t = add_to(m, &g).unwrap();
    assert!(
        t.contains("  - {name: r, path: /}\n# Generated by `hydo fake from-openapi spec.yaml`\n"),
        "{t}"
    );
    assert!(t.contains("\n- name: pay\n  port: 9001\n"), "{t}");
    assert!(
        t.ends_with("\n\n# the services\nservices:\n  - {name: s, cmd: sleep 1}\n"),
        "{t}"
    );
    // no fakes: yet
    let t = add_to("name: x\nservices: []", &g).unwrap();
    assert!(
        t.starts_with("name: x\nservices: []\n\nfakes:\n  # Generated"),
        "{t}"
    );
    // fakes: [] is written as a block
    let t = add_to("name: x\nfakes: []  # none yet\nservices: []\n", &g).unwrap();
    assert!(t.starts_with("name: x\nfakes:\n  # Generated"), "{t}");
    assert!(t.ends_with("\nservices: []\n"), "{t}");
    // a flow list is not edited
    let e = add_to("fakes: [{name: a}]\n", &g).unwrap_err();
    assert!(e.contains("inline"), "{e}");
}
