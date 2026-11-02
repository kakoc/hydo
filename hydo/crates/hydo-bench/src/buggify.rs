//! Route an order service's store calls through the BUGGIFY wrappers (addbuggify.py; the
//! wrappers live in hydo/stack/buggify.go, M56). build-order-services.sh (build-bench-bg.sh until M146) runs it on one version of
//! main.go (a benchmark defect's patched copy of an older one) before building the image:
//!
//! ```text
//! s.db.ExecContext(          -> s.execBG(
//! s.kp.ProduceSync(          -> s.produceBG(
//! kafkaHasKey(ctx, id)       -> s.kafkaHasKeyBG(ctx, id)      (calls only, not the definition)
//! s.redisSet("order:         -> s.redisSetBG("order:
//! return wrapperspb.String("ok"), nil   (in place)  -> return s.ackBG()
//! ```
//!
//! Idempotent. The counts of rewritten sites per kind are printed in addbuggify.py's form.

use regex::Regex;

const ACK: &str = "return wrapperspb.String(\"ok\"), nil";

/// Python's `\w` for one character (Unicode letters, digits and the underscore).
fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `re.subn(r"(?<![\w.])<lit>", rep, src)`: the literal where it is not preceded by a word
/// character or a dot (the regex crate has no look-behind; the literal cannot overlap itself).
fn sub_not_after_word(src: &str, lit: &str, rep: &str) -> (String, usize) {
    let mut out = String::with_capacity(src.len());
    let (mut i, mut n) = (0usize, 0usize);
    while let Some(j) = src[i..].find(lit) {
        let at = i + j;
        let prev = src[..at].chars().next_back();
        out.push_str(&src[i..at]);
        if prev.is_some_and(|c| word(c) || c == '.') {
            out.push_str(lit);
        } else {
            out.push_str(rep);
            n += 1;
        }
        i = at + lit.len();
    }
    out.push_str(&src[i..]);
    (out, n)
}

fn subn(src: &str, pat: &str, rep: &str) -> (String, usize) {
    let re = Regex::new(pat).expect("a valid pattern");
    let n = re.find_iter(src).count();
    (re.replace_all(src, regex::NoExpand(rep)).into_owned(), n)
}

/// addbuggify.py `rewrite`: the new source and the counts (exec, produce, check, redis, ack).
pub fn rewrite(src: &str) -> (String, Vec<(&'static str, usize)>) {
    let mut src = src.to_string();
    // the oldest order service (d0, pre-M39) has kafkaHasKey(id) without a context: give it one,
    // so the wrapper's call kafkaHasKey(ctx, id) compiles against every version
    if src.contains("func kafkaHasKey(id int64)") {
        src = src.replace(
            "func kafkaHasKey(id int64)",
            "func kafkaHasKey(_ context.Context, id int64)",
        );
        src = sub_not_after_word(&src, "kafkaHasKey(id)", "kafkaHasKey(ctx, id)").0;
    }
    let mut counts = Vec::new();
    let (s, n) = subn(&src, r"\bs\.db\.ExecContext\(", "s.execBG(");
    counts.push(("exec", n));
    let (s, n) = subn(&s, r"\bs\.kp\.ProduceSync\(", "s.produceBG(");
    counts.push(("produce", n));
    let (s, n) = sub_not_after_word(&s, "kafkaHasKey(ctx, id)", "s.kafkaHasKeyBG(ctx, id)");
    counts.push(("check", n));
    let (mut s, n) = subn(&s, r#"\bs\.redisSet\("order:"#, "s.redisSetBG(\"order:");
    counts.push(("redis", n));
    // the ACK: only inside func (s *orderServer) place(
    let place = Regex::new(r"(?s)\nfunc \(s \*orderServer\) place\(.*?\n\}\n").expect("pattern");
    let mut n = 0;
    if let Some(m) = place.find(&s) {
        let body = m.as_str();
        n = body.matches(ACK).count();
        s = format!(
            "{}{}{}",
            &s[..m.start()],
            body.replace(ACK, "return s.ackBG()"),
            &s[m.end()..]
        );
    }
    counts.push(("ack", n));
    (s, counts)
}

/// The line addbuggify.py printed: `addbuggify: <path> exec=N produce=N check=N redis=N ack=N`.
pub fn report(path: &str, counts: &[(&str, usize)]) -> String {
    format!(
        "addbuggify: {path} {}",
        counts
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_calls_but_not_the_definition() {
        let src = "func kafkaHasKey(ctx context.Context, id int64) bool {\n}\nfunc (s *orderServer) place(x int) {\n\tok := kafkaHasKey(ctx, id)\n\tx.kafkaHasKey(ctx, id)\n\ts.db.ExecContext(ctx)\n\treturn wrapperspb.String(\"ok\"), nil\n}\nfunc other() {\n\treturn wrapperspb.String(\"ok\"), nil\n}\n";
        let (out, c) = rewrite(src);
        assert_eq!(
            report("m.go", &c),
            "addbuggify: m.go exec=1 produce=0 check=1 redis=0 ack=1"
        );
        assert!(out.contains("ok := s.kafkaHasKeyBG(ctx, id)"));
        assert!(out.contains("x.kafkaHasKey(ctx, id)"));
        assert!(out.contains("func other() {\n\treturn wrapperspb.String(\"ok\"), nil"));
        // idempotent
        let (again, c2) = rewrite(&out);
        assert_eq!(again, out);
        assert!(c2.iter().all(|(_, n)| *n == 0));
    }
}
