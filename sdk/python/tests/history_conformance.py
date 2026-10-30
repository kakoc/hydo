"""The Python half of the history recorder's conformance test (tests/history_conformance.sh): the
same calls, in the same order, as tests/history-go/main.go makes with the Go recorder. Where Go
passes U+FFFD this side passes a lone surrogate; where Go passes invalid UTF-8 this side passes
bytes or a surrogateescape string."""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src"))

from hydo_assert import history as h  # noqa: E402

_out = []


def println(*a):
    _out.append(" ".join(a) + "\n")


def main():
    kv = h.History("kv")
    la = h.History("la")
    other = h.History("Other.hist_2-x")

    ascii_ = "".join(chr(c) for c in range(0x80))
    vals = [
        "", "nil", "Nil", "nil ", " nil", "nilnil", "%", "%6Eil", "@", " ", "\t", "\n", "\r",
        "\x00", "\x01", "\x1f", "\x7f", ",", "[", "]", ":", "a,b[c]:d@e%f\tg", "a b",
        "plain-Token_1.2", "~!#$^&*()_+-={}|\\;'\"<>?/`", "é", "café au lait", "中文",
        "日本語,テスト", "😀", "a😀b c", "\u00a0", "\u2028\u2029", "\uFFFD", "\uFFFF",
        "a\ud800b",
        "\udc00\ud83d",
        42,
        -7,
        ascii_,
    ]
    for i, v in enumerate(vals):
        println(h.enc(v))
        k = "k%d" % i
        op = kv.invoke(i % 3, "write", k, h.enc(v))
        if i % 4 == 0:
            op.ok()
        elif i % 4 == 1:
            op.ok(k, h.ver(v, i * 1000003))
        elif i % 4 == 2:
            op.fail()
        else:
            op.info()

    vers = [0, 1, -1, 83, 0, 2 ** 53, (1 << 63) - 1, -(1 << 63), 9007199254740993, -9007199254740993]
    for i, n in enumerate(vers):
        println(h.ver("v", n))
        op = kv.invoke(10 + i, "write", "kv", "v")
        op.ok("kv", h.ver("v", n))
    println(h.ver("", 5), h.ver("nil", -5), h.ver("a@b", 7), h.ver(12, 12))

    von = [
        h.value_or_nil(""), h.value_or_nil(None), h.value_or_nil("nil"),
        h.value_or_nil("x y"), h.value_or_nil(None), h.value_or_nil("é"),
        h.value_or_nil(None), h.value_or_nil(None), h.value_or_nil("v"),
    ]
    println(" ".join(von))
    op = kv.invoke(20, "read", "k1", "k2", "k3")
    op.ok("k1", von[0], "k2", von[1], "k3", h.NIL)
    op = kv.invoke(20, "read", "k4")
    op.ok("k4", h.value_or_nil("x y"))

    lists = [
        h.list_value(), h.list_value(""), h.list_value("a"),
        h.list_value("a", "b,c", "[x]", "nil", "", "é", "1 2", "%", ":"),
        h.list_value(1, 2, "3"),
    ]
    println(" ".join(lists))
    la.invoke(1, "append", "x", 1).ok()
    la.invoke(1, "read", "x").ok("x", h.list_value("1"))
    la.invoke(2, "read", "empty").ok("empty", h.list_value())
    la.invoke(2, "read", "odd").ok("odd", lists[3])

    mops = [
        h.append_mop("x", 2), h.read_mop("y"), h.append_mop("k:1", "e,2"), h.read_mop(""),
        h.read_result_mop("y"), h.read_result_mop("", "a", "b c"), h.read_result_mop("nil", "nil"),
        h.append_mop("", ""), h.read_mop("@"),
    ]
    println(" ".join(mops))
    tx = la.invoke(3, "txn", mops[0], mops[1], mops[2], mops[3])
    tx.ok(mops[0], mops[4], mops[2], mops[5])
    tx = la.invoke(3, "txn", h.append_mop("x", "3"))
    tx.fail()
    tx = la.invoke(4, "txn", h.read_mop("x"), h.append_mop("y", "1"))
    tx.info()

    other.record(7, "invoke", "cas", "k", "a", "b")
    other.record(7, "ok", "cas", "k", "a", "b@9")
    other.record("8", "info", "read")
    other.record(0, "fail", "write", "", "x", "")

    op = kv.invoke(9, "write", "", "")
    op.ok("", "")
    op = kv.invoke(9, "write", "", "")
    op.ok()

    op = kv.invoke(5, "noop")
    op.ok()
    op = kv.invoke(5, "noop")
    op.fail()
    op = kv.invoke(5, "noop")
    op.info()
    kv.invoke(1 << 40, "write", "big", "1").ok()
    kv.invoke(-5, "write", "neg", 1).ok()
    kv.invoke((1 << 63) - 1, "write", "max", "1").info()
    kv.invoke(-(1 << 63), "write", "min", "1").fail()

    a = kv.invoke(30, "write", "i", "1")
    b = la.invoke(30, "append", "i", "1")
    c = other.invoke(31, "read", "i")
    b.ok()
    c.ok("i", 1)
    a.info()

    other.invoke(40, "write", "x\udc00", "é").ok()
    other.invoke(41, "write", "n", 42).ok("n", 42, -7)
    println(h.NIL)

    # Invalid UTF-8.
    bad = [b"\xff", "a\udcffb", bytearray(b"\x80"), b"\xc3", b"\xc3(", b"\xed\xa0\x80", "\udce2\udc82",
           "é\udcff中", "\udcff", "\ud800", memoryview(b"\xff\xfe nil"), b"nil"]
    for i, v in enumerate(bad):
        println(h.enc(v), h.ver(v, -i), h.value_or_nil(v))
        op = kv.invoke(50 + i, "write", "b", h.enc(v))
        op.ok("b", h.ver(v, i))
    println(h.list_value(b"\xff", "ok", b"\xc3"), h.append_mop(b"\xff", "\udc80"), h.read_result_mop("\udcfe", b"\xff"))
    kv.invoke(60, "write", b"\xffraw", "x").ok()
    kv.invoke(61, "write", "k", "é\udcff").ok("k", h.list_value(b"\xff", "ok", b"\xc3"))
    other.record(62, "ok", "read", b"\xe2\x82", b"")

    sys.stdout.buffer.write("".join(_out).encode("utf-8", "surrogateescape"))
    sys.stdout.buffer.flush()


main()
