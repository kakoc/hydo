"""Unit tests of hydo_assert.history. Run: python3 -m unittest discover -s sdk/python/tests

The byte-for-byte comparison with the reference SDK is tests/test_cases.py (the shared cases); these repeat
Go's own unit test (hydohist_test.go) and cover the argument checks, the sink switches, threads
and fork."""

import os
import re
import subprocess
import sys
import tempfile
import threading
import unittest

SRC = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src")
sys.path.insert(0, SRC)

from hydo_assert import history as h  # noqa: E402

LINE = re.compile(rb"^HYDO-HIST (\S+) (\S+) (invoke|ok|fail|info) ([0-9]+) (.*)$")


class Out(object):
    """Point HYDO_HIST_OUT at a temporary file for the duration of a test."""

    def __init__(self, create=True):
        d = tempfile.mkdtemp(prefix="hydo-hist.")
        self.path = os.path.join(d, "hist")
        if create:
            open(self.path, "wb").close()
        os.environ["HYDO_HIST_OUT"] = self.path
        h._reset()

    def raw(self):
        h._reset()
        with open(self.path, "rb") as f:
            return f.read()

    def masked(self):
        out = []
        for l in self.raw().decode("utf-8", "surrogateescape").splitlines():
            f = l.split(" ")
            assert re.match(r"^[0-9]+$", f[4]), l
            f[4] = "*"
            out.append(" ".join(f))
        return out

    def close(self):
        os.environ["HYDO_HIST_OUT"] = "off"
        h._reset()


class TestRecords(unittest.TestCase):
    def test_go_records(self):
        o = Out()
        try:
            kv = h.History("kv")
            op = kv.invoke(3, "write", "k1", h.enc("a b"))
            op.ok("k1", h.ver("a b", 83))
            op = kv.invoke(3, "read", "k1")
            op.ok("k1", h.value_or_nil(None))
            op = kv.invoke(4, "txn", h.append_mop("x", "1"), h.read_mop("y"))
            op.ok(h.append_mop("x", "1"), h.read_result_mop("y", "1", "2"))
            kv.invoke(5, "write", "k2", "").info()
            self.assertEqual(o.masked(), [
                "HYDO-HIST kv 3 invoke * write k1 a%20b",
                "HYDO-HIST kv 3 ok * write k1 a%20b@83",
                "HYDO-HIST kv 3 invoke * read k1",
                "HYDO-HIST kv 3 ok * read k1 nil",
                "HYDO-HIST kv 4 invoke * txn a:x:1 r:y",
                "HYDO-HIST kv 4 ok * txn a:x:1 r:y:[1,2]",
                "HYDO-HIST kv 5 invoke * write k2 %",
                "HYDO-HIST kv 5 info * write k2 %",
            ])
        finally:
            o.close()

    def test_ok_repeats_and_tokens(self):
        o = Out(create=False)  # the file is created
        try:
            t = h.History("t")
            t.invoke("w-3", "write", "k", "v").ok()
            t.invoke(1, "write", b"", b"\xff").fail()
            t.record(2, "info", "read")
            self.assertEqual(o.masked(), [
                "HYDO-HIST t w-3 invoke * write k v",
                "HYDO-HIST t w-3 ok * write k v",
                "HYDO-HIST t 1 invoke * write % \udcff",
                "HYDO-HIST t 1 fail * write % \udcff",
                "HYDO-HIST t 2 info * read",
            ])
            self.assertIn(b" \xff\n", o.raw())
        finally:
            o.close()


class TestEnc(unittest.TestCase):
    def test_go_enc(self):
        self.assertEqual(h.enc("a,b[c]:d@e%f\tg"), "a%2Cb%5Bc%5D%3Ad%40e%25f%09g")

    def test_classes(self):
        self.assertEqual(h.enc(""), "%")
        self.assertEqual(h.enc(b""), "%")
        self.assertEqual(h.enc("nil"), "%6Eil")
        self.assertEqual(h.enc(b"nil"), "%6Eil")
        self.assertEqual(h.enc("nil "), "nil%20")
        self.assertEqual(h.enc(" \n\r\x00\x1f\x7f"), "%20%0A%0D%00%1F%7F")
        self.assertEqual(h.enc("é 中文 😀"), "é%20中文%20😀")
        self.assertEqual(h.enc(42), "42")
        self.assertEqual(h.enc(-7), "-7")
        # invalid UTF-8: bytes stay bytes (as surrogateescape characters in the str)
        self.assertEqual(h.enc(b"a\xffb c"), "a\udcffb%20c")
        self.assertEqual(h.enc("a\udcffb").encode("utf-8", "surrogateescape"), b"a\xffb")
        # other lone surrogates: U+FFFD
        self.assertEqual(h.enc("a\ud800b"), "a\ufffdb")
        self.assertEqual(h.enc("\udc00"), "\ufffd")
        allascii = "".join(chr(c) for c in range(0x80))
        e = h.enc(allascii)
        self.assertTrue(e.startswith("%00%01"))
        self.assertTrue(e.endswith("xyz{|}~%7F"))
        self.assertEqual(h.NIL, "nil")

    def test_helpers(self):
        self.assertEqual(h.ver("v", -(1 << 63)), "v@-9223372036854775808")
        self.assertEqual(h.ver("v", (1 << 63) - 1), "v@9223372036854775807")
        self.assertRaises(ValueError, h.ver, "v", 1 << 63)
        self.assertRaises(TypeError, h.ver, "v", 1.0)
        self.assertRaises(TypeError, h.ver, "v", "3")
        self.assertRaises(TypeError, h.ver, "v", True)
        self.assertEqual(h.value_or_nil(None), "nil")
        self.assertEqual(h.value_or_nil(""), "%")
        self.assertEqual(h.value_or_nil("nil"), "%6Eil")
        self.assertEqual(h.list_value(), "[]")
        self.assertEqual(h.list_value("", "a,b", "nil"), "[%,a%2Cb,%6Eil]")
        self.assertEqual(h.read_result_mop("k"), "r:k:[]")
        self.assertRaises(TypeError, h.History("kv").invoke, 1.5, "read")
        self.assertRaises(TypeError, h.History("kv").invoke, 1, "read", 2.5)


class TestSink(unittest.TestCase):
    def run_child(self, out, code="from hydo_assert import history as h\n"
                  "h.History('s').invoke(1, 'read', 'k').ok('k', 'nil')\n"):
        env = dict(os.environ, HYDO_HIST_OUT=out, PYTHONPATH=SRC)
        return subprocess.run([sys.executable, "-c", code], env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, check=True)

    def test_off_stderr_fallback(self):
        r = self.run_child("off")
        self.assertEqual(r.stdout + r.stderr, b"")
        r = self.run_child("stderr")
        self.assertEqual(r.stdout, b"")
        self.assertRegex(r.stderr, rb"^HYDO-HIST s 1 invoke [0-9]+ read k\nHYDO-HIST s 1 ok [0-9]+ read k nil\n$")
        r = self.run_child(os.path.join(tempfile.mkdtemp(), "no-such-dir", "x"))
        self.assertRegex(r.stderr, rb"^HYDO-HIST s 1 invoke ")

    def test_append_and_separate_from_assertions(self):
        d = tempfile.mkdtemp()
        hist, asrt = os.path.join(d, "hist"), os.path.join(d, "assert")
        with open(hist, "wb") as f:
            f.write(b"PRE\n")
        with open(asrt, "wb"):
            pass
        code = ("import sys, hydo_assert as ha\n"
                "assert 'hydo_assert.history' not in sys.modules\n"
                "from hydo_assert import history as h\n"
                "ha.always('x', 'x').check(True)\n"
                "h.History('a').record(1, 'ok', 'read')\n")
        env = dict(os.environ, HYDO_HIST_OUT=hist, HYDO_ASSERT_OUT=asrt, PYTHONPATH=SRC)
        subprocess.run([sys.executable, "-c", code], env=env, check=True)
        with open(hist, "rb") as f:
            self.assertRegex(f.read(), rb"^PRE\nHYDO-HIST a 1 ok [0-9]+ read\n$")
        with open(asrt, "rb") as f:
            self.assertEqual(f.read(), b'HYDO-ASSERT {"t":"always","id":"x","ok":true,"msg":"x"}\n')

    def test_one_write_per_record(self):
        o = Out()
        calls = []
        real = os.write
        try:
            def counting(fd, data):
                calls.append(len(data))
                return real(fd, data)
            os.write = counting
            t = h.History("t")
            for i in range(20):
                t.invoke(i, "write", "k", str(i)).ok()
        finally:
            os.write = real
            o.close()
        self.assertEqual(len(calls), 40)

    def test_threads(self):
        o = Out()
        try:
            t = h.History("thr")
            barrier = threading.Barrier(8)

            def worker(p):
                barrier.wait()
                for i in range(200):
                    t.invoke(p, "append", "k%d" % p, "e%d-%d" % (p, i) + "x" * (i % 50)).ok()

            ths = [threading.Thread(target=worker, args=(p,)) for p in range(8)]
            for th in ths:
                th.start()
            for th in ths:
                th.join()
            lines = o.raw().split(b"\n")
            self.assertEqual(lines[-1], b"")
            lines = lines[:-1]
            self.assertEqual(len(lines), 8 * 200 * 2)
            last = {}
            prev_t = -1
            for l in lines:
                m = LINE.match(l)
                self.assertIsNotNone(m, l)
                p, typ, tm, rest = int(m.group(2)), m.group(3), int(m.group(4)), m.group(5)
                self.assertGreaterEqual(tm, prev_t)  # read under the lock: never decreases
                prev_t = tm
                f, k, e = rest.split(b" ")
                self.assertEqual((f, k), (b"append", b"k%d" % p))
                if typ == b"invoke":
                    self.assertNotIn(p, last)
                    last[p] = e
                else:
                    self.assertEqual(typ, b"ok")
                    self.assertEqual(last.pop(p), e)
        finally:
            o.close()

    @unittest.skipUnless(hasattr(os, "fork"), "needs fork")
    def test_fork(self):
        o = Out()
        try:
            t = h.History("f")
            t.record(1, "ok", "parent")
            pid = os.fork()
            if pid == 0:
                try:
                    t.record(2, "ok", "child")
                finally:
                    os._exit(0)
            os.waitpid(pid, 0)
            t.record(1, "ok", "parent2")
            self.assertEqual(o.masked(), [
                "HYDO-HIST f 1 ok * parent",
                "HYDO-HIST f 2 ok * child",
                "HYDO-HIST f 1 ok * parent2",
            ])
        finally:
            o.close()


if __name__ == "__main__":
    unittest.main()
