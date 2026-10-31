//! How a Rust field maps to a JSON member: its encoding, its decoding and its schema, from one
//! trait, so that the codec and the generated schema cannot disagree.

use crate::json::Value;

/// A field kind of a protocol record.
pub trait Field: Sized {
    /// Whether the member must be present (every kind except `Option`).
    const REQUIRED: bool = true;
    /// The member's value; `None` leaves the member out.
    fn to_json(&self) -> Option<Value>;
    /// Reads the member (`None`: absent).
    fn from_json(v: Option<Value>) -> Result<Self, String>;
    /// The member's JSON Schema.
    fn schema() -> Value;
}

/// A kind that can be an array element.
pub trait Elem: Sized {
    fn to_elem(&self) -> Value;
    fn from_elem(v: Value) -> Result<Self, String>;
    fn elem_schema() -> Value;
}

pub(crate) fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub(crate) fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

fn need<T>(v: Option<Value>, f: impl FnOnce(Value) -> Result<T, String>) -> Result<T, String> {
    match v {
        None => Err("missing".into()),
        Some(v) => f(v),
    }
}

impl Elem for String {
    fn to_elem(&self) -> Value {
        Value::Str(self.clone())
    }
    fn from_elem(v: Value) -> Result<Self, String> {
        match v {
            Value::Str(x) => Ok(x),
            v => Err(format!("{} where a string was expected", v.kind())),
        }
    }
    fn elem_schema() -> Value {
        obj([("type", s("string"))])
    }
}

impl Elem for bool {
    fn to_elem(&self) -> Value {
        Value::Bool(*self)
    }
    fn from_elem(v: Value) -> Result<Self, String> {
        match v {
            Value::Bool(x) => Ok(x),
            v => Err(format!("{} where a boolean was expected", v.kind())),
        }
    }
    fn elem_schema() -> Value {
        obj([("type", s("boolean"))])
    }
}

macro_rules! int_elem {
    ($t:ty, $min:expr, $max:expr, $schema:expr) => {
        impl Elem for $t {
            fn to_elem(&self) -> Value {
                Value::Int(*self as i128)
            }
            fn from_elem(v: Value) -> Result<Self, String> {
                match v {
                    Value::Int(n) if n >= $min as i128 && n <= $max as i128 => Ok(n as $t),
                    Value::Int(n) => Err(format!("{n} is out of range")),
                    v => Err(format!("{} where an integer was expected", v.kind())),
                }
            }
            fn elem_schema() -> Value {
                $schema
            }
        }
    };
}

int_elem!(
    u64,
    0u64,
    u64::MAX,
    obj([("type", s("integer")), ("minimum", Value::Int(0))])
);
int_elem!(
    u32,
    0u32,
    u32::MAX,
    obj([
        ("type", s("integer")),
        ("minimum", Value::Int(0)),
        ("maximum", Value::Int(u32::MAX as i128))
    ])
);
int_elem!(
    i64,
    i64::MIN,
    i64::MAX,
    obj([
        ("type", s("integer")),
        ("minimum", Value::Int(i64::MIN as i128)),
        ("maximum", Value::Int(i64::MAX as i128))
    ])
);

macro_rules! scalar_field {
    ($($t:ty),*) => {$(
        impl Field for $t {
            fn to_json(&self) -> Option<Value> {
                Some(self.to_elem())
            }
            fn from_json(v: Option<Value>) -> Result<Self, String> {
                need(v, <$t as Elem>::from_elem)
            }
            fn schema() -> Value {
                <$t as Elem>::elem_schema()
            }
        }
    )*};
}

scalar_field!(String, bool, u64, u32, i64);

impl<T: Elem> Field for Vec<T> {
    fn to_json(&self) -> Option<Value> {
        Some(Value::Arr(self.iter().map(Elem::to_elem).collect()))
    }
    fn from_json(v: Option<Value>) -> Result<Self, String> {
        need(v, |v| match v {
            Value::Arr(a) => a
                .into_iter()
                .enumerate()
                .map(|(i, x)| T::from_elem(x).map_err(|e| format!("item {i}: {e}")))
                .collect(),
            v => Err(format!("{} where an array was expected", v.kind())),
        })
    }
    fn schema() -> Value {
        obj([("type", s("array")), ("items", T::elem_schema())])
    }
}

impl<T: Field> Field for Option<T> {
    const REQUIRED: bool = false;
    fn to_json(&self) -> Option<Value> {
        self.as_ref().and_then(Field::to_json)
    }
    fn from_json(v: Option<Value>) -> Result<Self, String> {
        match v {
            None => Ok(None),
            Some(v) => T::from_json(Some(v)).map(Some),
        }
    }
    fn schema() -> Value {
        T::schema()
    }
}

/// The text of `///` lines, joined by spaces.
pub(crate) fn doc_text(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn with_doc(mut schema: Value, doc: String) -> Value {
    if !doc.is_empty() {
        if let Value::Obj(o) = &mut schema {
            o.insert("description".into(), Value::Str(doc));
        }
    }
    schema
}

/// A field that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    pub what: String,
}

/// Defines a record: a struct whose fields are JSON members.
macro_rules! record {
    ($(#[doc = $sd:literal])* $name:ident { $( $(#[doc = $d:literal])* $f:ident : $t:ty ),* $(,)? }) => {
        $(#[doc = $sd])*
        #[derive(Debug, Clone, PartialEq, Eq, Default)]
        pub struct $name { $( $(#[doc = $d])* pub $f: $t, )* }

        impl $name {
            /// The JSON member names, in declaration order.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($f)),*];

            #[allow(unused_variables)]
            pub(crate) fn put_fields(&self, m: &mut $crate::json::Obj) {
                $( if let Some(v) = $crate::field::Field::to_json(&self.$f) {
                    m.insert(stringify!($f).to_string(), v);
                } )*
            }

            #[allow(unused_variables)]
            pub(crate) fn take_fields(
                m: &mut $crate::json::Obj,
            ) -> Result<Self, $crate::field::FieldError> {
                Ok(Self { $( $f: <$t as $crate::field::Field>::from_json(m.remove(stringify!($f)))
                    .map_err(|what| $crate::field::FieldError { field: stringify!($f).to_string(), what })?, )* })
            }

            #[allow(unused_variables)]
            pub(crate) fn schema_fields(
                props: &mut $crate::json::Obj,
                req: &mut Vec<$crate::json::Value>,
            ) {
                $( {
                    let s = $crate::field::with_doc(
                        <$t as $crate::field::Field>::schema(),
                        $crate::field::doc_text(&[$($d),*]),
                    );
                    props.insert(stringify!($f).to_string(), s);
                    if <$t as $crate::field::Field>::REQUIRED {
                        req.push($crate::json::Value::Str(stringify!($f).to_string()));
                    }
                } )*
            }

            pub(crate) fn doc() -> String {
                $crate::field::doc_text(&[$($sd),*])
            }
        }
    };
}

/// Defines a closed set of names (a JSON string with an `enum` schema).
macro_rules! names {
    ($(#[doc = $sd:literal])* $name:ident { $(#[doc = $d0:literal])* $v0:ident = $s0:literal $(, $(#[doc = $d:literal])* $v:ident = $s:literal)* $(,)? }) => {
        $(#[doc = $sd])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub enum $name { $(#[doc = $d0])* #[default] $v0, $( $(#[doc = $d])* $v, )* }

        impl $name {
            /// Every name, in declaration order.
            pub const ALL: &'static [$name] = &[$name::$v0, $($name::$v),*];
            pub fn as_str(&self) -> &'static str {
                match self { $name::$v0 => $s0, $( $name::$v => $s, )* }
            }
            pub fn from_name(x: &str) -> Option<Self> {
                match x { $s0 => Some($name::$v0), $( $s => Some($name::$v), )* _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl $crate::field::Elem for $name {
            fn to_elem(&self) -> $crate::json::Value {
                $crate::json::Value::Str(self.as_str().to_string())
            }
            fn from_elem(v: $crate::json::Value) -> Result<Self, String> {
                match v {
                    $crate::json::Value::Str(x) => Self::from_name(&x)
                        .ok_or_else(|| format!("{x:?} is not one of {}", Self::ALL.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", "))),
                    v => Err(format!("{} where a string was expected", v.kind())),
                }
            }
            fn elem_schema() -> $crate::json::Value {
                $crate::field::obj([
                    ("type", $crate::field::s("string")),
                    ("enum", $crate::json::Value::Arr(Self::ALL.iter().map(|n| $crate::field::s(n.as_str())).collect())),
                ])
            }
        }

        impl $crate::field::Field for $name {
            fn to_json(&self) -> Option<$crate::json::Value> {
                Some($crate::field::Elem::to_elem(self))
            }
            fn from_json(v: Option<$crate::json::Value>) -> Result<Self, String> {
                match v {
                    None => Err("missing".into()),
                    Some(v) => <$name as $crate::field::Elem>::from_elem(v),
                }
            }
            fn schema() -> $crate::json::Value {
                <$name as $crate::field::Elem>::elem_schema()
            }
        }
    };
}

pub(crate) use names;
pub(crate) use record;
