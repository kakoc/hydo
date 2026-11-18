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

#[test]
fn a_durable_line_names_the_check_of_the_recovery_boot() {
    let src = "\
// hydo: input text len 1..=300
// hydo: durable save_survives
pub fn save(text: String) -> Result<(), String> {
    Ok(())
}
// hydo: durable a
// hydo: durable b
fn twice() {}
// hydo: durable a b
fn spaced() {}
";
    let (cs, errs) = scan(src);
    assert_eq!(cs[0].durable, Some(("save_survives".to_string(), 2)));
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(errs[0].message.contains("one `durable` line"), "{errs:?}");
    assert!(errs[1].message.contains("durable <function>"), "{errs:?}");
}

// ---------------------------------------------------------------- Milestone 307

#[test]
fn matches_takes_a_pattern_of_variants_tuples_and_literals() {
    use hydo_contracts::lang::Pat;
    let a = parse_expr("!matches(result, Ok(Status::Complete((_, 0))))").unwrap();
    let Ast::Not(m) = a else { panic!("{a:?}") };
    let Ast::Matches(x, p) = *m else {
        panic!("{m:?}")
    };
    assert_eq!(*x, Ast::Name("result".into()));
    assert_eq!(
        p,
        Pat::Variant(
            "Ok".into(),
            Some(vec![Pat::Variant(
                "Complete".into(),
                Some(vec![Pat::Tuple(vec![Pat::Any, Pat::Int(0)])])
            )])
        )
    );
    assert_eq!(p.to_string(), "Ok(Complete((_, 0)))");
    // a bare name, a negative integer, a string and `old()`
    assert!(parse_expr("matches(op, Neg)").is_ok());
    assert!(parse_expr("matches(some(o), -1) || matches(old(s), \"x\")").is_ok());
    // a field of a variant reads like a field
    assert!(parse_expr("ok(result).Complete.0.1 == 0").is_ok());
    // what is refused
    assert!(parse_expr("matches(result)")
        .unwrap_err()
        .contains("matches(<value>, <pattern>)"));
    assert!(parse_expr("matches(1 + 2, _)")
        .unwrap_err()
        .contains("not an expression"));
    assert!(parse_expr("matches(r, Ok(+))")
        .unwrap_err()
        .contains("a pattern is"));
    // `sorted` is a function of the language
    assert!(parse_expr("sorted(v) && sorted(result)").is_ok());
}

#[test]
fn build_and_read_lines_name_a_constructor_and_a_method() {
    let src = "\
// hydo: build Bytes from Bytes::from(v: Vec<u8>)
// hydo: build Pair from Pair::new(a: &[u8], b: HashMap<u8, u8>)
// hydo: read Bytes as Vec<u8> by to_vec()
// hydo: with Self = i64
// hydo: with Self = u8, T = (usize, u64)
fn f() {}
// hydo: build Bytes
fn g() {}
// hydo: read Bytes as Vec<u8>
fn h() {}
";
    let (cs, errs) = scan(src);
    let c = &cs[0];
    assert_eq!(c.builds[0].ty, "Bytes");
    assert_eq!(c.builds[0].ctor, "Bytes::from");
    assert_eq!(c.builds[0].args, [("v".into(), "Vec<u8>".into())]);
    assert_eq!(c.builds[1].args[1], ("b".into(), "HashMap<u8, u8>".into()));
    assert_eq!(
        (c.reads[0].view.as_str(), c.reads[0].method.as_str()),
        ("Vec<u8>", "to_vec")
    );
    assert_eq!(c.with.len(), 3);
    assert_eq!(c.with[2], ("T".into(), "(usize, u64)".into()));
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(errs[0].message.contains("build <Type> from"), "{errs:?}");
    assert!(errs[1].message.contains("read <Type> as"), "{errs:?}");
}
