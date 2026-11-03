//! The additions-only rule of hydo's versioned formats, as one check of two JSON Schemas: the
//! schema of a format version as the last release shipped it (`hydo/schema/released/<format>-v1.json`)
//! and the schema generated from the types now (`hydo/schema/<format>-v1.json`). Every format's
//! schema test calls [`check`]: the project format (`hydo-manifest/tests/format.rs`), the results
//! format (`hydo-results/tests/schema.rs`) and the agent protocol
//! (`hydo-agent-proto/tests/schema.rs`). The table in `tests/rules.rs` is the specification: one
//! schema change per row, with the verdict under each kind of reader.
//!
//! # Who reads a document
//!
//! A format version promises two things, and a format chooses which of them it needs with
//! [`Readers`]:
//!
//! 1. **Old documents, new readers** (every format). A document that the released schema accepts
//!    is still accepted, with the same meaning. hydo reads a project written for an older hydo,
//!    `hydo report` reads an older exploration, a newer host reads an older agent's lines and a
//!    newer agent reads an older host's commands. A change that refuses such a document narrows
//!    the format; a change that keeps accepting it but reads it differently changes it. Both
//!    break every format.
//! 2. **New documents, old readers** ([`Readers::Released`]: the results format and the agent
//!    protocol). A reader built for the released schema reads what is written now: a CI job or an
//!    AI tool reading `result.json`, a released host reading a newer agent's lines. These readers
//!    ignore fields they do not know and record types (alternatives of the root) they do not know,
//!    and nothing else: a known field with a kind of value or a name they do not know is an error
//!    to them (`hydo_results::reader`, `hydo_agent_proto::AgentLine::decode`). So a change that
//!    lets a known place hold a value the released schema refuses widens the format, and widening
//!    breaks these formats. The project format does not promise this ([`Readers::Current`]): an
//!    older hydo refuses a key or a value it does not know, on purpose, and a project that needs a
//!    newer hydo pins it with `hydo_version:`; so widenings pass there.
//!
//! # The rules
//!
//! "Narrows" and "changes" fail always; "widens" fails only for [`Readers::Released`].
//!
//! - **Notes** (`description`, `title`, `$comment`, `examples`, `deprecated`, `readOnly`,
//!   `writeOnly`) may change freely: no reader acts on them.
//! - **A new optional property** passes, also in a closed object (`additionalProperties: false`)
//!   and for [`Readers::Released`]. No old document has the key, because its writer wrote only
//!   what the released schema lists (even where an object was open); readers of the released
//!   schema ignore an unknown key; and where a reader refuses unknown keys, the format has its own
//!   gate (an older hydo refuses the key and `hydo_version:` names the release a project needs; a
//!   host sends a new command field only to an agent whose hello offers its feature).
//! - **A new required property**, or a property that **became required**, narrows: an old
//!   document without it is refused.
//! - **A property that stops being required** widens: old documents still have it, but a reader
//!   of the released schema may get a document without it.
//! - **A removed or renamed property** changes the format, also in an open object: an old
//!   document that has it is refused (closed object) or read without that part of its meaning
//!   (open object).
//! - **`type`**: a kind gone narrows; a kind gained widens (`integer` counts as part of `number`);
//!   a `type` where there was none narrows, a `type` gone widens.
//! - **`enum`**: a value gone narrows; a value gained widens, except at a place the format names
//!   open with [`Rules::open_enum`] because its readers keep an unknown value as text (the agent's
//!   refusal code) or never receive one (a command field whose new value needs a hello feature).
//!   An `enum` where there was none narrows, an `enum` gone widens.
//! - **`anyOf` / `oneOf` inside a value**: each old variant keeps its place and stays compatible
//!   there, new variants may only be appended, and an appended variant widens. Order matters
//!   because a reader of an untagged choice (serde's `untagged`) takes the first variant that
//!   fits, so a variant inserted before an old one could read an old document differently;
//!   reordered, inserted or removed variants fail. A choice where there was none narrows; a
//!   choice that is gone changes the format.
//! - **The root's `anyOf` / `oneOf`** (the record types of a stream, the messages of a protocol):
//!   every old alternative must have a compatible alternative anywhere in the new list, and new
//!   alternatives pass anywhere and for every reader. The alternatives are told apart by their
//!   own discriminator (`type`, `op`, `ev`), so their order carries no meaning; readers of the
//!   released schema skip a record type they do not know, a released host keeps an unknown
//!   event as such, and a host sends a new op only to an agent whose hello lists it.
//! - **`$ref`**: references are followed into `$defs` by name, so a renamed definition passes
//!   when what it accepts is compatible, and a definition nothing refers to is not compared (it
//!   accepts no document). Definition names are the generator's (schemars names them after Rust
//!   types), not part of a format. A reference written out in place, or an inline schema turned
//!   into a reference, changes the format: the check compares references with references only.
//! - **`default`** may not change or disappear: it is the meaning of a document that leaves the
//!   key out. It may appear only on a property the released schema required, where no old
//!   document leaves the key out.
//! - **`const`, `format`, `pattern`, `multipleOf`**: a new or changed one narrows (the check does
//!   not judge whether `uint32` → `uint64` or a rewritten pattern only widens; such a change
//!   needs a new version), one gone widens. `uniqueItems: true` added narrows, gone widens.
//! - **Bounds** (`minimum`, `exclusiveMinimum`, `minLength`, `minItems`, `minProperties` and the
//!   `max…` ones): a tighter or new bound narrows, a looser or removed bound widens.
//! - **`additionalProperties`**: closing an object narrows; opening a closed one passes (an
//!   unknown key is what every reader ignores or what the format's gate handles, as for a new
//!   optional property); a map's value schema is compared like any other schema. `items` is
//!   compared like any other schema; `items` where there was none narrows.
//! - **Any other keyword** (`allOf`, `not`, `$schema`, `if`, …) must stay exactly as it was and
//!   may not appear or disappear: the check does not judge it, so it allows no change of it.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt;

/// Who reads the documents of a format (see the crate documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readers {
    /// Only readers of the current schema: old documents must stay readable with the same
    /// meaning; a widening passes. The project format.
    Current,
    /// Also readers built for the released schema, which ignore unknown fields and unknown
    /// alternatives of the root and nothing else: a widening fails too. The results format and
    /// the agent protocol.
    Released,
}

/// The rules a format's schema test checks with: who reads, and where an enum may gain values.
#[derive(Debug, Clone)]
pub struct Rules {
    readers: Readers,
    open_enums: Vec<String>,
}

impl Rules {
    pub fn new(readers: Readers) -> Rules {
        Rules {
            readers,
            open_enums: Vec::new(),
        }
    }

    /// A place whose `enum` may gain values for [`Readers::Released`] too, because its readers
    /// accept a value they do not know. `place` is the place as problems name it
    /// (`#/$defs/error/properties/err`); a trailing `*` names every place it starts
    /// (`#/$defs/command.*`). A place that names no `enum` of the released schema is itself a
    /// problem, so the option cannot outlive the place it was written for.
    pub fn open_enum(mut self, place: &str) -> Rules {
        self.open_enums.push(place.to_string());
        self
    }

    fn open_at(&self, at: &str) -> Option<usize> {
        self.open_enums
            .iter()
            .position(|p| match p.strip_suffix('*') {
                Some(prefix) => at.starts_with(prefix),
                None => at == p,
            })
    }
}

/// How a change breaks the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A document the released schema accepts is refused now.
    Narrows,
    /// A document written now may hold a value a reader of the released schema refuses
    /// (reported for [`Readers::Released`] only).
    Widens,
    /// A document the released schema accepts is read with another meaning now, or the check
    /// cannot tell that it is not.
    Changes,
    /// The rules name a place the released schema does not have.
    UnknownPlace,
}

/// One way the new schema breaks the rule against the released one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Where, as a JSON pointer into the released schema (`#/$defs/Run/properties/secs`).
    pub at: String,
    pub kind: Kind,
    pub what: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.what)
    }
}

/// Every way `new` breaks the additions-only rule against `old` under `rules` (empty: `new` is
/// an addition to `old`).
pub fn check(old: &Value, new: &Value, rules: &Rules) -> Vec<Problem> {
    let empty = Map::new();
    let mut cx = Cx {
        rules,
        old_defs: defs(old, &empty),
        new_defs: defs(new, &empty),
        seen: BTreeSet::new(),
        out: Vec::new(),
        open_seen: BTreeSet::new(),
        trial: false,
    };
    cx.cmp(old, new, "#", false);
    for (i, p) in rules.open_enums.iter().enumerate() {
        if !cx.open_seen.contains(&i) {
            cx.out.push(Problem {
                at: p.clone(),
                kind: Kind::UnknownPlace,
                what: "named open by the rules, but the released schema has no enum there".into(),
            });
        }
    }
    cx.out
}

const NOTES: &[&str] = &[
    "description",
    "title",
    "$comment",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
];
const ASSERTS: &[&str] = &["const", "format", "pattern", "multipleOf"];
const LOWER: &[&str] = &[
    "minimum",
    "exclusiveMinimum",
    "minLength",
    "minItems",
    "minProperties",
];
const UPPER: &[&str] = &[
    "maximum",
    "exclusiveMaximum",
    "maxLength",
    "maxItems",
    "maxProperties",
];
/// Keywords with their own rule above; every other keyword must stay as it was.
const JUDGED: &[&str] = &[
    "$defs",
    "$ref",
    "type",
    "enum",
    "uniqueItems",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "anyOf",
    "oneOf",
    "default",
];

fn defs<'a>(root: &'a Value, empty: &'a Map<String, Value>) -> &'a Map<String, Value> {
    root.get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(empty)
}

fn ref_name(r: &str) -> Option<&str> {
    r.strip_prefix("#/$defs/")
}

fn ref_of(v: &Value) -> Option<&str> {
    v.get("$ref").and_then(Value::as_str)
}

fn strings(v: Option<&Value>) -> BTreeSet<String> {
    match v {
        Some(Value::String(s)) => [s.clone()].into(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => BTreeSet::new(),
    }
}

fn list(s: &BTreeSet<String>) -> String {
    s.iter().cloned().collect::<Vec<_>>().join(", ")
}

/// Whether the kinds `set` include the kind `k` (`number` includes `integer`).
fn covers(set: &BTreeSet<String>, k: &str) -> bool {
    set.contains(k) || (k == "integer" && set.contains("number"))
}

struct Cx<'a> {
    rules: &'a Rules,
    old_defs: &'a Map<String, Value>,
    new_defs: &'a Map<String, Value>,
    /// Pairs of definitions compared or being compared (a definition may refer to itself).
    seen: BTreeSet<(String, String)>,
    out: Vec<Problem>,
    /// The open places an `enum` of the released schema was found at.
    open_seen: BTreeSet<usize>,
    /// Only whether there is a problem counts: stop at the first.
    trial: bool,
}

impl Cx<'_> {
    fn push(&mut self, at: &str, kind: Kind, what: impl Into<String>) {
        if kind == Kind::Widens && self.rules.readers == Readers::Current {
            return;
        }
        self.out.push(Problem {
            at: at.to_string(),
            kind,
            what: what.into(),
        });
    }

    /// Whether `n` is compatible with `o`, without reporting.
    fn fits(&mut self, o: &Value, n: &Value, at: &str) -> bool {
        let mut sub = Cx {
            rules: self.rules,
            old_defs: self.old_defs,
            new_defs: self.new_defs,
            seen: self.seen.clone(),
            out: Vec::new(),
            open_seen: BTreeSet::new(),
            trial: true,
        };
        sub.cmp(o, n, at, false);
        self.open_seen.extend(sub.open_seen);
        if !sub.out.is_empty() {
            return false;
        }
        // the definitions a successful trial compared are compatible: not again
        self.seen.extend(sub.seen);
        true
    }

    /// Compares the schema `o` of the released version with `n` at `at`; `required` says whether
    /// `o` is a property its object required.
    fn cmp(&mut self, o: &Value, n: &Value, at: &str, required: bool) {
        if self.trial && !self.out.is_empty() {
            return;
        }
        let empty = Map::new();
        match (o, n) {
            (Value::Bool(false), Value::Bool(false)) => return,
            (Value::Bool(false), _) => {
                return self.push(at, Kind::Widens, "accepted nothing, accepts values now")
            }
            (_, Value::Bool(false)) => return self.push(at, Kind::Narrows, "accepts nothing now"),
            _ => {}
        }
        let (om, nm) = match (o, n) {
            (Value::Object(a), Value::Object(b)) => (a, b),
            (Value::Object(a), Value::Bool(true)) => (a, &empty),
            (Value::Bool(true), Value::Object(b)) => (&empty, b),
            (Value::Bool(true), Value::Bool(true)) => return,
            _ => {
                if o != n {
                    self.push(at, Kind::Changes, format!("was {o}, is {n}"));
                }
                return;
            }
        };
        if !self.reference(om, nm, at) {
            return;
        }
        self.kinds(om, nm, at);
        self.values(om, nm, at);
        self.asserts(om, nm, at);
        self.properties(om, nm, at);
        self.additional(om, nm, at);
        let t = Value::Bool(true);
        if om.contains_key("items") || nm.contains_key("items") {
            let (a, b) = (om.get("items").unwrap_or(&t), nm.get("items").unwrap_or(&t));
            self.cmp(a, b, &format!("{at}/items"), false);
        }
        self.choices(om, nm, at);
        self.default(om, nm, at, required);
        self.others(om, nm, at);
    }

    /// Compares the references of two schemas; false when one is a reference and the other is
    /// not, so that the rest of them is not compared.
    fn reference(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) -> bool {
        let (r, nr) = match (o.get("$ref"), n.get("$ref")) {
            (None, None) => return true,
            (Some(r), None) => {
                self.push(
                    at,
                    Kind::Changes,
                    format!("was a reference to {r}, is written out now"),
                );
                return false;
            }
            (None, Some(r)) => {
                self.push(at, Kind::Changes, format!("became a reference to {r}"));
                return false;
            }
            (Some(r), Some(nr)) => (r.as_str().unwrap_or(""), nr.as_str().unwrap_or("")),
        };
        let (Some(on), Some(nn)) = (ref_name(r), ref_name(nr)) else {
            if r != nr {
                self.push(at, Kind::Changes, format!("the reference {r} became {nr}"));
            }
            return true;
        };
        if !self.seen.insert((on.to_string(), nn.to_string())) {
            return true;
        }
        let (od, nd) = (self.old_defs, self.new_defs);
        match (od.get(on), nd.get(nn)) {
            (Some(a), Some(b)) => self.cmp(a, b, &format!("#/$defs/{on}"), false),
            (Some(_), None) => self.push(
                at,
                Kind::Narrows,
                format!("refers to {nr}, which the schema does not define"),
            ),
            (None, _) => self.push(
                at,
                Kind::Changes,
                format!("the released schema refers to {r}, which it does not define"),
            ),
        }
        true
    }

    fn kinds(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) {
        let at = format!("{at}/type");
        match (o.get("type"), n.get("type")) {
            (None, None) => {}
            (None, Some(b)) => self.push(
                &at,
                Kind::Narrows,
                format!("restricted to {}", list(&strings(Some(b)))),
            ),
            (Some(a), None) => self.push(
                &at,
                Kind::Widens,
                format!("any kind of value now (was {})", list(&strings(Some(a)))),
            ),
            (Some(a), Some(b)) => {
                let (a, b) = (strings(Some(a)), strings(Some(b)));
                let lost: BTreeSet<String> = a.iter().filter(|k| !covers(&b, k)).cloned().collect();
                let gained: BTreeSet<String> =
                    b.iter().filter(|k| !covers(&a, k)).cloned().collect();
                if !lost.is_empty() {
                    self.push(
                        &at,
                        Kind::Narrows,
                        format!("the kinds {} are refused now", list(&lost)),
                    );
                }
                if !gained.is_empty() {
                    self.push(
                        &at,
                        Kind::Widens,
                        format!("gained the kinds {}", list(&gained)),
                    );
                }
            }
        }
    }

    fn values(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) {
        let open = self.rules.open_at(at);
        let here = format!("{at}/enum");
        match (o.get("enum"), n.get("enum")) {
            (None, None) => {}
            (None, Some(_)) => self.push(&here, Kind::Narrows, "restricted to a set of values"),
            (Some(_), None) => {
                if let Some(i) = open {
                    self.open_seen.insert(i);
                }
                self.push(&here, Kind::Widens, "no longer a set of values")
            }
            (Some(a), Some(b)) => {
                if let Some(i) = open {
                    self.open_seen.insert(i);
                }
                let (a, b) = (
                    a.as_array().map_or(&[][..], Vec::as_slice),
                    b.as_array().map_or(&[][..], Vec::as_slice),
                );
                for x in a.iter().filter(|x| !b.contains(x)) {
                    self.push(&here, Kind::Narrows, format!("the value {x} is gone"));
                }
                let gained: Vec<String> = b
                    .iter()
                    .filter(|x| !a.contains(x))
                    .map(Value::to_string)
                    .collect();
                if !gained.is_empty() && open.is_none() {
                    self.push(
                        &here,
                        Kind::Widens,
                        format!("gained the values {}", gained.join(", ")),
                    );
                }
            }
        }
    }

    fn asserts(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) {
        for k in ASSERTS {
            let here = format!("{at}/{k}");
            match (o.get(*k), n.get(*k)) {
                (Some(a), Some(b)) if a == b => {}
                (None, None) => {}
                (Some(a), None) => self.push(&here, Kind::Widens, format!("{a} is gone")),
                (None, Some(b)) => self.push(&here, Kind::Narrows, format!("a new {k} {b}")),
                (Some(a), Some(b)) => self.push(&here, Kind::Narrows, format!("was {a}, is {b}")),
            }
        }
        let unique = |m: &Map<String, Value>| m.get("uniqueItems") == Some(&Value::Bool(true));
        match (unique(o), unique(n)) {
            (false, true) => self.push(
                &format!("{at}/uniqueItems"),
                Kind::Narrows,
                "items must be unique now",
            ),
            (true, false) => self.push(
                &format!("{at}/uniqueItems"),
                Kind::Widens,
                "items need not be unique now",
            ),
            _ => {}
        }
        for (keys, lower) in [(LOWER, true), (UPPER, false)] {
            for k in keys {
                let here = format!("{at}/{k}");
                match (o.get(*k), n.get(*k)) {
                    (None, None) => {}
                    (Some(a), Some(b)) if a == b => {}
                    (None, Some(b)) => self.push(&here, Kind::Narrows, format!("a new bound {b}")),
                    (Some(a), None) => {
                        self.push(&here, Kind::Widens, format!("the bound {a} is gone"))
                    }
                    (Some(a), Some(b)) => match (a.as_f64(), b.as_f64()) {
                        (Some(x), Some(y)) if (y < x) == lower => {
                            self.push(&here, Kind::Widens, format!("loosened from {a} to {b}"))
                        }
                        _ => self.push(&here, Kind::Narrows, format!("tightened from {a} to {b}")),
                    },
                }
            }
        }
    }

    fn properties(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) {
        let empty = Map::new();
        let op = o
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let np = n
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let (or, nr) = (strings(o.get("required")), strings(n.get("required")));
        for (p, os) in op {
            let here = format!("{at}/properties/{p}");
            match np.get(p) {
                Some(ns) => self.cmp(os, ns, &here, or.contains(p)),
                None => self.push(&here, Kind::Changes, "the key is gone"),
            }
        }
        for p in np
            .keys()
            .filter(|p| !op.contains_key(*p) && nr.contains(*p))
        {
            self.push(
                &format!("{at}/properties/{p}"),
                Kind::Narrows,
                "a new key that is required",
            );
        }
        for p in nr.difference(&or) {
            if op.contains_key(p) || !np.contains_key(p) {
                self.push(
                    &format!("{at}/properties/{p}"),
                    Kind::Narrows,
                    "became required",
                );
            }
        }
        for p in or.difference(&nr) {
            if !op.contains_key(p) || np.contains_key(p) {
                self.push(
                    &format!("{at}/properties/{p}"),
                    Kind::Widens,
                    "is no longer required",
                );
            }
        }
    }

    fn additional(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) {
        let closed = Value::Bool(false);
        let open = Value::Bool(true);
        let (a, b) = (o.get("additionalProperties"), n.get("additionalProperties"));
        if a == Some(&closed) {
            // closed before: still closed, or opened (allowed, see the rules)
            return;
        }
        let here = format!("{at}/additionalProperties");
        if b == Some(&closed) {
            return self.push(
                &here,
                Kind::Narrows,
                "the object is closed now: a key it does not list is refused",
            );
        }
        if a.is_some() || b.is_some() {
            self.cmp(a.unwrap_or(&open), b.unwrap_or(&open), &here, false);
        }
    }

    fn choices(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) {
        for k in ["anyOf", "oneOf"] {
            let here = format!("{at}/{k}");
            let (a, b) = match (o.get(k), n.get(k)) {
                (None, None) => continue,
                (None, Some(_)) => {
                    self.push(&here, Kind::Narrows, "a new choice");
                    continue;
                }
                (Some(_), None) => {
                    self.push(&here, Kind::Changes, "the choice is gone");
                    continue;
                }
                (Some(a), Some(b)) => (
                    a.as_array().map_or(&[][..], Vec::as_slice),
                    b.as_array().map_or(&[][..], Vec::as_slice),
                ),
            };
            if at == "#" {
                // the root: alternatives are found by what they accept, new ones pass anywhere
                for (i, x) in a.iter().enumerate() {
                    let alt = format!("{here}/{i}");
                    // the likely counterpart first: the same definition, else the same place
                    let same = ref_of(x)
                        .and_then(|r| b.iter().position(|y| ref_of(y) == Some(r)))
                        .or((i < b.len()).then_some(i));
                    let order = same
                        .into_iter()
                        .chain((0..b.len()).filter(|&j| Some(j) != same));
                    if order.into_iter().any(|j| self.fits(x, &b[j], &alt)) {
                        continue;
                    }
                    let same = same.map(|j| &b[j]);
                    match same {
                        Some(y) => self.cmp(x, y, &alt, false),
                        None => self.push(&alt, Kind::Narrows, "the alternative is gone"),
                    }
                }
                continue;
            }
            for (i, x) in a.iter().enumerate() {
                let var = format!("{here}/{i}");
                if b.get(i).is_some_and(|y| self.fits(x, y, &var)) {
                    continue;
                }
                let moved = (0..b.len()).find(|&j| j != i && self.fits(x, &b[j], &var));
                match (moved, b.get(i)) {
                    (Some(j), _) => self.push(
                        &var,
                        Kind::Changes,
                        format!(
                            "the variant moved to place {j}: a reader that takes the first \
                             variant that fits may read an old value as another one now"
                        ),
                    ),
                    (None, Some(y)) => self.cmp(x, y, &var, false),
                    (None, None) => self.push(&var, Kind::Narrows, "the variant is gone"),
                }
            }
            if b.len() > a.len() {
                self.push(
                    &here,
                    Kind::Widens,
                    format!("gained {} variants at its end", b.len() - a.len()),
                );
            }
        }
    }

    fn default(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str, req: bool) {
        let here = format!("{at}/default");
        match (o.get("default"), n.get("default")) {
            (None, None) => {}
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => self.push(
                &here,
                Kind::Changes,
                format!("was {a}, is {b}: a document without the key means something else now"),
            ),
            (Some(a), None) => self.push(
                &here,
                Kind::Changes,
                format!("{a} is gone: a document without the key means something else now"),
            ),
            (None, Some(b)) => {
                if !req {
                    self.push(
                        &here,
                        Kind::Changes,
                        format!(
                            "{b} where there was none: a document without the key may mean \
                             something else now"
                        ),
                    )
                }
            }
        }
    }

    fn others(&mut self, o: &Map<String, Value>, n: &Map<String, Value>, at: &str) {
        let judged = |k: &str| {
            NOTES.contains(&k)
                || JUDGED.contains(&k)
                || ASSERTS.contains(&k)
                || LOWER.contains(&k)
                || UPPER.contains(&k)
        };
        for (k, a) in o.iter().filter(|(k, _)| !judged(k)) {
            let here = format!("{at}/{k}");
            match n.get(k) {
                Some(b) if a == b => {}
                Some(b) => self.push(&here, Kind::Changes, format!("was {a}, is {b}")),
                None => self.push(&here, Kind::Changes, format!("{a} is gone")),
            }
        }
        for k in n.keys().filter(|k| !judged(k) && !o.contains_key(*k)) {
            self.push(
                &format!("{at}/{k}"),
                Kind::Changes,
                "a new keyword the check does not judge",
            );
        }
    }
}
