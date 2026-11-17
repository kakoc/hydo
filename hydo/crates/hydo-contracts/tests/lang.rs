//! The contract language: where a contract belongs, and what its expressions mean.

use hydo_contracts::lang::{declared_function, parse_expr, scan, Ast};
use hydo_contracts::plan::{BinOp, ClauseKind};

#[test]
fn a_contract_belongs_to_the_next_function_past_comments_and_attributes() {
    let src = "\
/// Apply a debit.
// hydo: requires amount > 0
// hydo: ensures result == l.balance
// hydo: input amount in 1..=10_000
#[inline(never)]
pub(crate) fn apply(l: &mut Ledger, amount: u64) -> u64 {
    0
}
";
    let (cs, errs) = scan(src);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(cs.len(), 1);
    let c = &cs[0];
    assert_eq!((c.function.as_str(), c.line), ("apply", 6));
    assert_eq!(c.clauses.len(), 2);
    assert_eq!(c.clauses[0].kind, ClauseKind::Requires);
    assert_eq!(c.clauses[0].line, 2);
    assert_eq!(c.clauses[1].text, "result == l.balance");
    assert_eq!(c.inputs[0].name, "amount");
    assert_eq!(c.inputs[0].range, Some((1, 10_000)));
}

#[test]
fn go_functions_methods_and_directives() {
    let src = "\
// hydo: requires reqID > 0
//
//go:noinline
func apply(l *Ledger, reqID uint64) uint64 {
}

// hydo: ensures result >= 0
func (l *Ledger) Balance() int64 {
";
    let (cs, errs) = scan(src);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(cs[0].function, "apply");
    assert_eq!(cs[0].line, 4);
    assert_eq!(cs[1].function, "Balance");
    assert_eq!(
        declared_function("    pub async unsafe fn f<T>(x: T)"),
        Some("f".into())
    );
    assert_eq!(declared_function("extern \"C\" fn g()"), Some("g".into()));
    assert_eq!(declared_function("let fnord = 1;"), None);
}

#[test]
fn a_contract_without_a_function_is_an_error() {
    let (cs, errs) = scan("// hydo: requires x > 0\nlet y = 2;\n");
    assert!(cs.is_empty());
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].line, 1);
    assert!(errs[0].message.contains("function declaration"));
}

#[test]
fn precedence_old_and_fields() {
    let e = parse_expr("a > old(l.balance) - 1 || !ok && b == 2").unwrap();
    // `||` is loosest, `&&` binds tighter, comparisons tighter still, `-` tighter than `>`
    let Ast::Bin(BinOp::Or, l, r) = e else {
        panic!("{e:?}")
    };
    let Ast::Bin(BinOp::Gt, _, rhs) = *l else {
        panic!()
    };
    assert_eq!(
        *rhs,
        Ast::Bin(
            BinOp::Sub,
            Box::new(Ast::Old(Box::new(Ast::Field(
                Box::new(Ast::Name("l".into())),
                "balance".into()
            )))),
            Box::new(Ast::Int(1))
        )
    );
    assert!(matches!(*r, Ast::Bin(BinOp::And, _, _)));
    assert_eq!(
        parse_expr("0x10 + 1_000").unwrap(),
        Ast::Bin(BinOp::Add, Box::new(Ast::Int(16)), Box::new(Ast::Int(1000)))
    );
}

#[test]
fn strings_elements_and_the_functions_that_read() {
    use hydo_contracts::lang::Builtin;
    let name = |n: &str| Box::new(Ast::Name(n.into()));
    // len(v) and an element chosen by a parameter
    assert_eq!(
        parse_expr("len(v) > k && v[k] == 1").unwrap(),
        Ast::Bin(
            BinOp::And,
            Box::new(Ast::Bin(
                BinOp::Gt,
                Box::new(Ast::Call(Builtin::Len, name("v"))),
                name("k")
            )),
            Box::new(Ast::Bin(
                BinOp::Eq,
                Box::new(Ast::Index(name("v"), name("k"))),
                Box::new(Ast::Int(1))
            ))
        )
    );
    // a string literal with its escapes
    assert_eq!(
        parse_expr(r#"s == "a\"b""#).unwrap(),
        Ast::Bin(BinOp::Eq, name("s"), Box::new(Ast::Str("a\"b".into())))
    );
    // a variant's value, and a field of it
    assert_eq!(
        parse_expr("some(p).x").unwrap(),
        Ast::Field(Box::new(Ast::Call(Builtin::Some, name("p"))), "x".into())
    );
    // len(old(v)) is old(len(v)); Go's nil is 0
    assert_eq!(
        parse_expr("len(old(v))").unwrap(),
        Ast::Old(Box::new(Ast::Call(Builtin::Len, name("v"))))
    );
    assert_eq!(
        parse_expr("err == nil").unwrap(),
        Ast::Bin(BinOp::Eq, name("err"), Box::new(Ast::Int(0)))
    );
    assert!(parse_expr("len(1 + 2)")
        .unwrap_err()
        .contains("not an expression"));
    assert!(parse_expr("s == \"open").unwrap_err().contains("closing"));
}

#[test]
fn what_the_first_version_refuses() {
    let calls = parse_expr("balance(from) == 1").unwrap_err();
    assert!(calls.contains("is a call"), "{calls}");
    let method = parse_expr("v.push(1) > 0").unwrap_err();
    assert!(method.contains("is a call"), "{method}");
    // a built-in written as a method: the message names the function form
    let len = parse_expr("v.len() > 0").unwrap_err();
    assert!(len.contains("write `len(v)`"), "{len}");
    assert!(parse_expr("a < b < c")
        .unwrap_err()
        .contains("do not chain"));
    assert!(parse_expr("old(old(x))")
        .unwrap_err()
        .contains("inside `old()`"));
    assert!(parse_expr("old(result) == 1").is_err());
    let (_, errs) = scan("// hydo: requires result > 0\nfn f() -> u64 {}\n");
    assert!(errs[0].message.contains("judged at entry"), "{errs:?}");
    let (_, errs) = scan("// hydo: promises x\nfn f() {}\n");
    assert!(errs[0].message.contains("requires <expr>"), "{errs:?}");
    let (_, errs) = scan("// hydo: input n in 5..5\nfn f(n: u8) {}\n");
    assert!(errs[0].message.contains("empty"), "{errs:?}");
}
