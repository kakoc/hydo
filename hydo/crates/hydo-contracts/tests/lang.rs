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
    assert_eq!((c.inputs[0].lo, c.inputs[0].hi), (1, 10_000));
    assert!(c.inputs[0].inclusive);
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
            Box::new(Ast::Old(Box::new(Ast::Path(
                "l".into(),
                vec!["balance".into()]
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
fn what_the_first_version_refuses() {
    let calls = parse_expr("balance(from) == 1").unwrap_err();
    assert!(calls.contains("is a call"), "{calls}");
    let method = parse_expr("v.len() > 0").unwrap_err();
    assert!(method.contains("is a call"), "{method}");
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
