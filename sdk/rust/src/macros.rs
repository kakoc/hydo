//! Macros: call-site assertions that capture file and line,
//! `declare!` for a catalog of named statics, and `details!`.
//!
//! Every call-site macro defines a hidden `static` for its assertion and registers it before
//! `main` with a constructor (`.init_array` on ELF, `__mod_init_func` on Mach-O; std only, no
//! allocation), so the catalog lists it even if it is never evaluated. Its records carry
//! `"loc":"<file>:<line>"`. Without an id the message is the id; give one
//! with `"id" => "message"`.

/// Builds a [`Details`](crate::Details) object: `details!{"order": id, "got": got}`.
#[macro_export]
macro_rules! details {
    () => { $crate::Details::new() };
    ($($k:literal : $v:expr),+ $(,)?) => {{
        let mut __d = $crate::Details::new();
        $( __d.insert($k, $v); )+
        __d
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __details {
    () => { $crate::Details::new() };
    ({ $($k:literal : $v:expr),* $(,)? }) => { $crate::details!($($k : $v),*) };
    ($e:expr) => { $crate::Details::from($e) };
}

/// Runs `$body` once before `main` (std-only constructor). On targets without constructors
/// the body does not run; the assertion is then registered at its first evaluation.
#[doc(hidden)]
#[macro_export]
macro_rules! __ctor {
    ($body:block) => {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd", target_os = "netbsd",
                  target_os = "openbsd", target_os = "dragonfly", target_os = "illumos", target_os = "solaris"))]
        #[used]
        #[link_section = ".init_array"]
        static __HYDO_CTOR: extern "C" fn() = {
            extern "C" fn __hydo_ctor() $body
            __hydo_ctor
        };
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "tvos", target_os = "watchos"))]
        #[used]
        #[link_section = "__DATA,__mod_init_func"]
        static __HYDO_CTOR: extern "C" fn() = {
            extern "C" fn __hydo_ctor() $body
            __hydo_ctor
        };
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __site {
    ($kind:ident, $id:expr, $msg:expr) => {{
        static __HYDO_SITE: $crate::Assertion =
            $crate::Assertion::__site($crate::Type::$kind, $id, $msg, file!(), line!(), column!());
        $crate::__ctor!({ $crate::__register_static(&__HYDO_SITE); });
        &__HYDO_SITE
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __check_macro {
    ($kind:ident, $cond:expr, $id:literal => $msg:literal $(, $($d:tt)+)?) => {
        $crate::__site!($kind, $id, $msg).check($cond, $crate::__details!($($($d)+)?))
    };
    ($kind:ident, $cond:expr, $msg:literal $(, $($d:tt)+)?) => {
        $crate::__site!($kind, $msg, $msg).check($cond, $crate::__details!($($($d)+)?))
    };
}

/// `hydo_always!(cond, "message" [, {details}])` or `hydo_always!(cond, "id" => "message" ...)`:
/// cond must hold every time; returns cond.
#[macro_export]
macro_rules! hydo_always {
    ($($t:tt)+) => { $crate::__check_macro!(Always, $($t)+) };
}

/// As [`hydo_always!`], but never being evaluated is also fine.
#[macro_export]
macro_rules! hydo_always_or_unreachable {
    ($($t:tt)+) => { $crate::__check_macro!(AlwaysOrUnreachable, $($t)+) };
}

/// cond must be true at least once across an exploration (only a true outcome is recorded).
#[macro_export]
macro_rules! hydo_sometimes {
    ($($t:tt)+) => { $crate::__check_macro!(Sometimes, $($t)+) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __hit_macro {
    ($kind:ident, $id:literal => $msg:literal $(, $($d:tt)+)?) => {
        $crate::__site!($kind, $id, $msg).hit($crate::__details!($($($d)+)?))
    };
    ($kind:ident, $msg:literal $(, $($d:tt)+)?) => {
        $crate::__site!($kind, $msg, $msg).hit($crate::__details!($($($d)+)?))
    };
}

/// `hydo_reachable!("message" [, {details}])`: this line must be reached at least once.
#[macro_export]
macro_rules! hydo_reachable {
    ($($t:tt)+) => { $crate::__hit_macro!(Reachable, $($t)+) };
}

/// `hydo_unreachable!("message" [, {details}])`: this line must never be reached.
#[macro_export]
macro_rules! hydo_unreachable {
    ($($t:tt)+) => { $crate::__hit_macro!(Unreachable, $($t)+) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __cmp_macro {
    ($kind:ident, $op:literal, $l:expr, $r:expr, $id:literal => $msg:literal $(, $($d:tt)+)?) => {
        $crate::__site!($kind, $id, $msg).cmp($l, $op, $r, $crate::__details!($($($d)+)?))
    };
    ($kind:ident, $op:literal, $l:expr, $r:expr, $msg:literal $(, $($d:tt)+)?) => {
        $crate::__site!($kind, $msg, $msg).cmp($l, $op, $r, $crate::__details!($($($d)+)?))
    };
}

/// `hydo_always_cmp!(left, ">=", right, "message" [, {details}])`: a numeric comparison that
/// must always hold; `op` is one of `<`, `<=`, `==`, `!=`, `>=`, `>` (any other is false), and
/// the details carry `left`, `op` and `right`. Returns the comparison's result.
#[macro_export]
macro_rules! hydo_always_cmp {
    ($l:expr, $op:literal, $r:expr, $($t:tt)+) => { $crate::__cmp_macro!(Always, $op, $l, $r, $($t)+) };
}
/// `hydo_sometimes_cmp!(left, "<", right, "message" [, {details}])`: a numeric comparison that
/// must be true at least once (only a true outcome is recorded).
#[macro_export]
macro_rules! hydo_sometimes_cmp {
    ($l:expr, $op:literal, $r:expr, $($t:tt)+) => { $crate::__cmp_macro!(Sometimes, $op, $l, $r, $($t)+) };
}
/// `hydo_buggify!("id" [=> "message"] [, {details}])` -> bool: a fault point at this line.
/// Use as `if hydo_buggify!("kafka.lost_ack" => "the write lands but the reply is lost") { ... }`.
#[macro_export]
macro_rules! hydo_buggify {
    ($id:literal => $msg:literal $(, $($d:tt)+)?) => {{
        static __HYDO_POINT: $crate::Point = $crate::Point::__new(
            concat!("buggify.", $id), concat!("fault point fired: ", $msg), $id, file!(), line!(), column!(), true);
        $crate::__ctor!({ $crate::__register_static(__HYDO_POINT.__assertion()); });
        __HYDO_POINT.fire($crate::__details!($($($d)+)?))
    }};
    ($id:literal $(, $($d:tt)+)?) => { $crate::hydo_buggify!($id => $id $(, $($d)+)?) };
}

/// `hydo_eventually!(within, "id" [=> "message"])` -> `&'static Liveness`: an eventually
/// property declared at this line; `within` must be a constant expression
/// (`Duration::from_secs(10)`).
#[macro_export]
macro_rules! hydo_eventually {
    ($within:expr, $id:literal => $msg:literal) => {{
        static __HYDO_LIVE: $crate::Liveness = $crate::Liveness::__new($id, $msg, $within, file!(), line!(), column!(), true);
        $crate::__ctor!({ $crate::__register_static(__HYDO_LIVE.__assertion()); });
        &__HYDO_LIVE
    }};
    ($within:expr, $msg:literal) => { $crate::hydo_eventually!($within, $msg => $msg) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __decl_ty {
    (always) => { $crate::Assertion };
    (always_or_unreachable) => { $crate::Assertion };
    (sometimes) => { $crate::Assertion };
    (reachable) => { $crate::Assertion };
    (unreachable) => { $crate::Assertion };
    (eventually) => { $crate::Liveness };
    (buggify) => { $crate::Point };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __decl_init {
    (always, $id:literal, $msg:literal) => { $crate::Assertion::__decl($crate::Type::Always, $id, $msg, file!(), line!(), column!()) };
    (always_or_unreachable, $id:literal, $msg:literal) => { $crate::Assertion::__decl($crate::Type::AlwaysOrUnreachable, $id, $msg, file!(), line!(), column!()) };
    (sometimes, $id:literal, $msg:literal) => { $crate::Assertion::__decl($crate::Type::Sometimes, $id, $msg, file!(), line!(), column!()) };
    (reachable, $id:literal, $msg:literal) => { $crate::Assertion::__decl($crate::Type::Reachable, $id, $msg, file!(), line!(), column!()) };
    (unreachable, $id:literal, $msg:literal) => { $crate::Assertion::__decl($crate::Type::Unreachable, $id, $msg, file!(), line!(), column!()) };
    (eventually, $id:literal, $msg:literal, $within:expr) => { $crate::Liveness::__new($id, $msg, $within, file!(), line!(), column!(), false) };
    (buggify, $id:literal, $msg:literal) => {
        $crate::Point::__new(concat!("buggify.", $id), concat!("fault point fired: ", $msg), $id, file!(), line!(), column!(), false)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __decl_reg {
    (always, $n:ident) => { $crate::__register_static(&$n) };
    (always_or_unreachable, $n:ident) => { $crate::__register_static(&$n) };
    (sometimes, $n:ident) => { $crate::__register_static(&$n) };
    (reachable, $n:ident) => { $crate::__register_static(&$n) };
    (unreachable, $n:ident) => { $crate::__register_static(&$n) };
    (eventually, $n:ident) => { $crate::__register_static($n.__assertion()) };
    (buggify, $n:ident) => { $crate::__register_static($n.__assertion()) };
}

/// Declares named assertions as `static`s and puts them into the catalog before `main`, in
/// the order written (the Go SDK's package-level `var x = hydoassert.Always(...)`):
///
/// ```ignore
/// hydo_assert::declare! {
///     pub static ACKED_DURABLE: always("kv.acked_durable", "every acknowledged write reads back");
///     static RECOVERED: sometimes("kv.recovered", "the server replayed a non-empty log");
///     static BACK: eventually("kv.back", "the server answers within 5 s", Duration::from_secs(5));
///     static LOST_REPLY: buggify("kv.put.lost_reply", "the write lands but the reply is lost");
/// }
/// ```
///
/// Kinds: `always`, `always_or_unreachable`, `sometimes`, `reachable`, `unreachable` (an
/// [`Assertion`](crate::Assertion)), `eventually` (a [`Liveness`](crate::Liveness), with a
/// constant deadline) and `buggify` (a [`Point`](crate::Point)). Ids and messages are string
/// literals.
#[macro_export]
macro_rules! declare {
    ($( $(#[$m:meta])* $vis:vis static $name:ident : $kind:ident ( $id:literal, $msg:literal $(, $within:expr)? ) ; )*) => {
        $( $(#[$m])* $vis static $name: $crate::__decl_ty!($kind) = $crate::__decl_init!($kind, $id, $msg $(, $within)?); )*
        const _: () = {
            $crate::__ctor!({ $( $crate::__decl_reg!($kind, $name); )* });
        };
    };
}
