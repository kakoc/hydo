"""The Python half of the conformance test (tests/conformance.sh): the same calls, in the same
order, as tests/go-conformance/main.go makes with the Go SDK."""

import math
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src"))

import hydo_assert as ha  # noqa: E402


def rich():
    return {
        "s": "<>&\"\\\n\r\t\b\f\x01\x1f\x7f é     😀 日本語",
        "neg": -5,
        "zero": 0,
        "umax": (1 << 64) - 1,
        "imin": -(1 << 63),
        "imax": (1 << 63) - 1,
        "f1": 0.1,
        "fneg0": -0.0,
        "f0": 0.0,
        "f21": 1e21,
        "f20": 1e20,
        "fsm": 1.5e-7,
        "f6": 1e-6,
        "f6b": 9.99e-7,
        "fmix": 123456789.125,
        "f3": 3.0,
        "fneg": -2.5,
        "ftiny": 2.5e-300,
        "fbig": 1e100,
        "fmaxf": sys.float_info.max,
        "fsub": 5e-324,
        "fthird": 1.0 / 3.0,
        "f1e15": 123456789012345680.0,
        "t": True,
        "f": False,
        "nil": None,
        "bytes": b"hi\x00\xff",
        "ebytes": b"",
        "list": [1, "a", False, None, {"z": 1, "a": 2}, [[]]],
        "empty": [],
        "emap": {},
        "nest": {"é": 1, "B": 2, "a": 3, "_": 4, "a b": {"x": "y"}, "😀": 5, "Z": 6, "ÿ": 7, "~": 8},
        "deep": {"l1": {"l2": {"l3": [{"k": "v", "j": [1.25, -0.5]}]}}},
        "<key>": "&",
    }


def main():
    r0 = rich()
    if os.environ.get("HYDO_CONF_INVALID_UTF8"):
        r0["inv"] = "a\udcffb\udcc3"
        r0["\udcffk"] = {"a\udcfe": 1, "a": 2}
    a = ha.always("c.always", 'always <holds> & "quotes"')
    b = ha.always_or_unreachable("c.aou", "always or unreachable")
    s = ha.sometimes("c.sometimes", "sometimes é   true")
    r = ha.reachable("c.reach", "reached")
    u = ha.unreachable("c.unreach", "never reached")
    ha.always("c.never", "never evaluated")
    cmp = ha.always("c.cmp", "a comparison")
    scmp = ha.sometimes("c.scmp", "a sometimes comparison")
    gt = ha.always("c.gt", "greater than")
    ge = ha.sometimes("c.ge", "greater than or equal")
    lt = ha.always("c.lt", "less than")
    le = ha.always("c.le", "less than or equal")
    nan = ha.always("c.nan", "details with a NaN")
    uni = ha.always("c.ünï/日本", "unicode id and \x01 control")
    loop = ha.always("c.loop", "evaluated many times")
    l1 = ha.eventually("c.live_ok", "in time", 10)
    l2 = ha.eventually("c.live_late", "never", 0)
    l3 = ha.eventually("c.live_await", "await, true at once", 1.5)
    ps = [ha.buggify(i, "point " + i.upper()) for i in
          ["p.a", "p.b", "p.c", "p.d", "p.e", "p.f", "p.g", "p.h", "p.é"]]

    ha.catalog()

    a.check(True)
    a.check(True, {"x": 1})
    a.check(False, r0)
    a.check(False, {"dup": True})
    b.check(True, {})
    b.check(False, {"why": None})
    s.check(False)
    s.check(True, {"n": 3})
    s.check(True, {"n": 4})
    r.hit()
    r.hit({"again": 1})
    u.hit({"why": "test"})
    u.hit()
    cmp.cmp(3, "<=", 5)
    cmp.cmp(7, "<", 5, {"extra": "e", "left": "overwritten"})
    cmp.cmp(1, "??", 1)
    scmp.cmp(1, ">", 2)
    scmp.cmp(2, "==", 2)
    gt.greater_than(2, 1)
    gt.greater_than(1, 2, {"k": "v"})
    ge.greater_than_or_equal_to(-1, 0)
    ge.greater_than_or_equal_to(0, 0)
    lt.less_than(-(1 << 63), (1 << 63) - 1)
    le.less_than_or_equal_to(5, 4)
    le.cmp(4, "!=", 4)
    nan.check(True, {"f": math.nan})
    nan.check(True, {"f": 1})
    nan.check(False, {"nested": {"f": math.inf}})
    nan.check(False, {"f": 2})
    uni.check(False, {"ключ": "значение", "\t": "\t"})
    for i in range(1000):
        loop.check(i != 500, {"i": i})
    w = l1.begin()
    w.done()
    w.done()
    l1.await_cond(lambda: True, 0.001)
    w2 = l2.begin()
    w2.expire({"k": "v"})
    w2.done()
    l3.await_cond(lambda: True, 0.001, {"probe": "ping"})
    ha.setup_complete()

    fires = []
    for i in range(60):
        for j, p in enumerate(ps):
            d = None
            if j == 0:
                d = {"round": i}
            if j == 1:
                d = {"hit": "replaced", "x": [i]}
            fires.append("1" if p.fire(d) else "0")
    sys.stdout.write("".join(fires) + "\n")
    ha.setup_complete()
    sys.stdout.flush()


if __name__ == "__main__":
    main()
