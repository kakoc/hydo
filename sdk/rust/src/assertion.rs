//! Assertions, the catalog registry and the record writer.

use crate::json::{write_map, write_str, Details, Value};
use crate::sink;
use std::borrow::Cow;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering::*};
use std::sync::{Mutex, MutexGuard};

/// An assertion type as it appears on the wire (`"t"`). Only the SDK's macros name it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Always,
    AlwaysOrUnreachable,
    Sometimes,
    Reachable,
    Unreachable,
    Eventually,
}

impl Type {
    pub const fn as_str(self) -> &'static str {
        match self {
            Type::Always => "always",
            Type::AlwaysOrUnreachable => "always_or_unreachable",
            Type::Sometimes => "sometimes",
            Type::Reachable => "reachable",
            Type::Unreachable => "unreachable",
            Type::Eventually => "eventually",
        }
    }
}

/// Where a static assertion was written; the sort key of the catalog. `wire` is set for the
/// call-site macros, whose records carry `"loc":"<file>:<line>"`.
#[derive(Debug)]
pub(crate) struct Loc {
    pub file: &'static str,
    pub line: u32,
    pub col: u32,
    pub wire: bool,
}

/// One declared property. Create it with the call-site macros (`hydo_always!` ...), with
/// [`declare!`](crate::declare), or at run time with [`always`](crate::always) and friends.
pub struct Assertion {
    pub(crate) t: Type,
    pub(crate) id: Cow<'static, str>,
    pub(crate) msg: Cow<'static, str>,
    pub(crate) loc: Option<Loc>,
    /// for a BUGGIFY point: the point's own id (the assertion id is `buggify.<id>`)
    pub(crate) bg_id: Option<Cow<'static, str>>,
    pub(crate) hits: AtomicU64,
    seen: [AtomicBool; 2],
    registered: AtomicBool,
    seq: AtomicU32,
    next: AtomicPtr<Assertion>,
}

impl std::fmt::Debug for Assertion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assertion").field("t", &self.t).field("id", &self.id).field("msg", &self.msg).finish()
    }
}

impl Assertion {
    pub(crate) const fn raw(t: Type, id: Cow<'static, str>, msg: Cow<'static, str>, loc: Option<Loc>, bg_id: Option<Cow<'static, str>>) -> Self {
        Assertion {
            t,
            id,
            msg,
            loc,
            bg_id,
            hits: AtomicU64::new(0),
            seen: [AtomicBool::new(false), AtomicBool::new(false)],
            registered: AtomicBool::new(false),
            seq: AtomicU32::new(0),
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// A property that must hold every time it is evaluated. A `static` made this way is in
    /// the catalog only once it is [`register`](crate::register)ed (or first evaluated);
    /// [`declare!`](crate::declare) does that before `main`.
    pub const fn always(id: &'static str, msg: &'static str) -> Self {
        Self::raw(Type::Always, Cow::Borrowed(id), Cow::Borrowed(msg), None, None)
    }
    /// As [`always`](Self::always), but never being evaluated is also fine.
    pub const fn always_or_unreachable(id: &'static str, msg: &'static str) -> Self {
        Self::raw(Type::AlwaysOrUnreachable, Cow::Borrowed(id), Cow::Borrowed(msg), None, None)
    }
    /// A property that must be true at least once across an exploration.
    pub const fn sometimes(id: &'static str, msg: &'static str) -> Self {
        Self::raw(Type::Sometimes, Cow::Borrowed(id), Cow::Borrowed(msg), None, None)
    }
    /// A code location that must be reached at least once.
    pub const fn reachable(id: &'static str, msg: &'static str) -> Self {
        Self::raw(Type::Reachable, Cow::Borrowed(id), Cow::Borrowed(msg), None, None)
    }
    /// A code location that must never be reached.
    pub const fn unreachable(id: &'static str, msg: &'static str) -> Self {
        Self::raw(Type::Unreachable, Cow::Borrowed(id), Cow::Borrowed(msg), None, None)
    }

    #[doc(hidden)]
    pub const fn __decl(t: Type, id: &'static str, msg: &'static str, file: &'static str, line: u32, col: u32) -> Self {
        Self::raw(t, Cow::Borrowed(id), Cow::Borrowed(msg), Some(Loc { file, line, col, wire: false }), None)
    }
    #[doc(hidden)]
    pub const fn __site(t: Type, id: &'static str, msg: &'static str, file: &'static str, line: u32, col: u32) -> Self {
        Self::raw(t, Cow::Borrowed(id), Cow::Borrowed(msg), Some(Loc { file, line, col, wire: true }), None)
    }

    /// The property's id, as its records carry it.
    pub fn id(&self) -> &str { &self.id }
    /// The property's message for people.
    pub fn msg(&self) -> &str {
        &self.msg
    }

    /// Evaluates an always / always_or_unreachable / sometimes assertion and returns `cond`.
    /// For sometimes only a true outcome is recorded. Each outcome is written once per process.
    pub fn check(&'static self, cond: bool, details: impl Into<Details>) -> bool {
        if self.t == Type::Sometimes && !cond {
            return cond;
        }
        self.record(cond, details.into());
        cond
    }

    /// Marks a reachable (recorded `ok:true`) or unreachable (recorded `ok:false`: reaching
    /// it is a violation) location as reached.
    pub fn hit(&'static self, details: impl Into<Details>) {
        self.record(self.t != Type::Unreachable, details.into());
    }

    /// Numeric comparison: evaluates `left <op> right` (op one of `<`, `<=`, `==`, `!=`, `>=`,
    /// `>`; any other op is false) as a [`check`](Self::check), with `left`, `op` and `right`
    /// added to the details. The Go SDK's `Cmp` takes `int64`s; any ordered value works here.
    pub fn cmp<T: PartialOrd + Into<Value>>(&'static self, left: T, op: &str, right: T, details: impl Into<Details>) -> bool {
        let ok = match op {
            "<" => left < right,
            "<=" => left <= right,
            "==" => left == right,
            "!=" => left != right,
            ">=" => left >= right,
            ">" => left > right,
            _ => false,
        };
        let mut d = details.into();
        d.insert("left", left);
        d.insert("op", op);
        d.insert("right", right);
        self.check(ok, d)
    }

    pub(crate) fn record(&'static self, ok: bool, d: Details) {
        let mut g = lock();
        if !self.registered.swap(true, AcqRel) {
            g.runtime.push(self); // a static first used without registration (or no constructors)
        }
        let i = ok as usize;
        if self.seen[i].swap(true, AcqRel) {
            return;
        }
        if !sink::active() {
            return;
        }
        let mut s = String::from("HYDO-ASSERT {\"t\":");
        write_str(&mut s, self.t.as_str());
        s.push_str(",\"id\":");
        write_str(&mut s, &self.id);
        s.push_str(if ok { ",\"ok\":true,\"msg\":" } else { ",\"ok\":false,\"msg\":" });
        write_str(&mut s, &self.msg);
        if !d.is_empty() {
            s.push_str(",\"details\":");
            if write_map(&mut s, &d).is_err() {
                return; // a NaN or an infinity: Go's json.Marshal fails and writes nothing
            }
        }
        self.push_loc(&mut s);
        s.push_str("}\n");
        sink::write_line(s.as_bytes());
    }

    fn push_loc(&self, s: &mut String) {
        if let Some(l) = &self.loc {
            if l.wire {
                s.push_str(",\"loc\":");
                write_str(s, &format!("{}:{}", l.file, l.line));
            }
        }
    }

    fn decl_line(&self) -> String {
        let mut s = String::from("HYDO-ASSERT-DECL {\"t\":");
        write_str(&mut s, self.t.as_str());
        s.push_str(",\"id\":");
        write_str(&mut s, &self.id);
        s.push_str(",\"msg\":");
        write_str(&mut s, &self.msg);
        self.push_loc(&mut s);
        s.push_str("}\n");
        s
    }
}

// ---- registry ----
//
// Static assertions (call-site macros, declare!) are pushed before main by a constructor
// (.init_array / __mod_init_func) onto a lock-free intrusive list, so registration never
// allocates or locks. The catalog sorts them by where they were written (file, line, column,
// then declaration order inside one declare!), which is a pure function of the source, and
// lists after them the assertions registered at run time, in registration order.

static STATICS: AtomicPtr<Assertion> = AtomicPtr::new(ptr::null_mut());
static SEQ: AtomicU32 = AtomicU32::new(0);

pub(crate) struct Global {
    pub runtime: Vec<&'static Assertion>,
    pub bg: crate::buggify::BgState,
}

static LOCK: Mutex<Global> = Mutex::new(Global { runtime: Vec::new(), bg: crate::buggify::BgState::new() });

pub(crate) fn lock() -> MutexGuard<'static, Global> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[doc(hidden)]
pub fn __register_static(a: &'static Assertion) {
    if a.registered.swap(true, AcqRel) {
        return;
    }
    a.seq.store(SEQ.fetch_add(1, AcqRel), Release);
    let mut head = STATICS.load(Acquire);
    loop {
        a.next.store(head, Release);
        match STATICS.compare_exchange_weak(head, a as *const _ as *mut _, AcqRel, Acquire) {
            Ok(_) => return,
            Err(h) => head = h,
        }
    }
}

/// Puts `a` into the catalog (run-time registration order). Needed only for a `static`
/// built with [`Assertion::always`] and friends outside [`declare!`](crate::declare).
pub fn register(a: &'static Assertion) {
    let mut g = lock();
    if !a.registered.swap(true, AcqRel) {
        g.runtime.push(a);
    }
}

/// Every registered assertion in catalog order. Caller holds LOCK.
pub(crate) fn all_registered(g: &Global) -> Vec<&'static Assertion> {
    let mut st = Vec::new();
    let mut p = STATICS.load(Acquire);
    while !p.is_null() {
        // SAFETY: only &'static Assertions are ever pushed onto the list
        let a: &'static Assertion = unsafe { &*p };
        st.push(a);
        p = a.next.load(Acquire);
    }
    st.sort_by(|a, b| {
        let ka = a.loc.as_ref().map(|l| (l.file, l.line, l.col));
        let kb = b.loc.as_ref().map(|l| (l.file, l.line, l.col));
        ka.cmp(&kb).then(a.seq.load(Acquire).cmp(&b.seq.load(Acquire)))
    });
    st.extend(g.runtime.iter().copied());
    st
}

pub(crate) fn declare_runtime(a: Assertion) -> &'static Assertion {
    let a: &'static Assertion = Box::leak(Box::new(a));
    register(a);
    a
}

/// Writes one `HYDO-ASSERT-DECL` line per registered assertion, in catalog order, so
/// properties that are never evaluated can still be reported. Call it once, early, from the
/// process that should carry the catalog into the console.
pub fn catalog() {
    let g = lock();
    let all = all_registered(&g);
    if !sink::active() {
        return;
    }
    for a in all {
        sink::write_line(a.decl_line().as_bytes());
    }
}

/// Writes the lifecycle line `HYDO-SETUP-COMPLETE`: the system is provisioned and ready,
/// faults may begin, and this is a good snapshot/branch point.
pub fn setup_complete() {
    sink::write_line(b"HYDO-SETUP-COMPLETE\n");
}

/// Declares an always property at run time (registered now, in the catalog in this order).
pub fn always(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Assertion {
    declare_runtime(Assertion::raw(Type::Always, id.into(), msg.into(), None, None))
}
/// Declares an always_or_unreachable property at run time.
pub fn always_or_unreachable(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Assertion {
    declare_runtime(Assertion::raw(Type::AlwaysOrUnreachable, id.into(), msg.into(), None, None))
}
/// Declares a sometimes property at run time.
pub fn sometimes(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Assertion {
    declare_runtime(Assertion::raw(Type::Sometimes, id.into(), msg.into(), None, None))
}
/// Declares a reachable location at run time.
pub fn reachable(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Assertion {
    declare_runtime(Assertion::raw(Type::Reachable, id.into(), msg.into(), None, None))
}
/// Declares an unreachable location at run time.
pub fn unreachable(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Assertion {
    declare_runtime(Assertion::raw(Type::Unreachable, id.into(), msg.into(), None, None))
}
