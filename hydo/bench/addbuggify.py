#!/usr/bin/env python3
"""addbuggify.py -- route a front door's store calls through the BUGGIFY wrappers.

    python3 hydo/bench/addbuggify.py <main.go> [--check]

The wrappers live in hydo/stack/buggify.go (Milestone 56); this rewrites the call sites of one
version of main.go -- the current one, or a benchmark defect's patched copy of an older one --
in place:

    s.db.ExecContext(          -> s.execBG(
    s.kp.ProduceSync(          -> s.produceBG(
    kafkaHasKey(ctx, id)       -> s.kafkaHasKeyBG(ctx, id)      (calls only, not the definition)
    s.redisSet("order:         -> s.redisSetBG("order:
    return wrapperspb.String("ok"), nil   (in place)  -> return s.ackBG()

Idempotent. Prints how many sites were rewritten per kind; --check only reports.
"""
import re, sys

SUBS = [
    ("exec", r"\bs\.db\.ExecContext\(", "s.execBG("),
    ("produce", r"\bs\.kp\.ProduceSync\(", "s.produceBG("),
    ("check", r"(?<![\w.])kafkaHasKey\(ctx, id\)", "s.kafkaHasKeyBG(ctx, id)"),
    ("redis", r"\bs\.redisSet\(\"order:", 's.redisSetBG("order:'),
]
ACK = 'return wrapperspb.String("ok"), nil'


def rewrite(src):
    counts = {}
    # the oldest front door (d0, pre-M39) has kafkaHasKey(id) without a context: give it one,
    # so the wrapper's call kafkaHasKey(ctx, id) compiles against every version
    if "func kafkaHasKey(id int64)" in src:
        src = src.replace("func kafkaHasKey(id int64)", "func kafkaHasKey(_ context.Context, id int64)")
        src = re.sub(r"(?<![\w.])kafkaHasKey\(id\)", "kafkaHasKey(ctx, id)", src)
    for name, pat, rep in SUBS:
        src, n = re.subn(pat, rep, src)
        counts[name] = n
    # the ACK: only inside func (s *orderServer) place(
    m = re.search(r"\nfunc \(s \*orderServer\) place\(.*?\n}\n", src, re.S)
    n = 0
    if m:
        body = m.group(0)
        n = body.count(ACK)
        src = src[:m.start()] + body.replace(ACK, "return s.ackBG()") + src[m.end():]
    counts["ack"] = n
    return src, counts


def main():
    path = sys.argv[1]
    src = open(path).read()
    out, counts = rewrite(src)
    print("addbuggify: %s %s" % (path, " ".join("%s=%d" % kv for kv in counts.items())))
    if "--check" not in sys.argv and out != src:
        open(path, "w").write(out)


if __name__ == "__main__":
    main()
