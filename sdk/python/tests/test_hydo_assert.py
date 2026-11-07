"""Unit tests (standard library unittest). Run: python3 -m unittest discover -s sdk/python/tests"""

import asyncio
import json
import os
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src"))

import hydo_assert as ha  # noqa: E402
from hydo_assert import _buggify, _json, _sink  # noqa: E402


class Console(object):
    """Point the sink at a temporary file for the duration of a test."""

    def __init__(self):
        fd, self.path = tempfile.mkstemp(prefix="hydo-console.")
        os.close(fd)
        os.environ["HYDO_ASSERT_OUT"] = self.path
        _sink._reset()

    def lines(self):
        with open(self.path, "rb") as f:
            return f.read().decode("utf-8").splitlines()

    def records(self, tag="HYDO-ASSERT "):
        return [json.loads(l[len(tag):]) for l in self.lines() if l.startswith(tag)]

    def close(self):
        os.environ["HYDO_ASSERT_OUT"] = "off"
        _sink._reset()
        os.unlink(self.path)


class TestJSON(unittest.TestCase):
    def test_floats_like_go(self):
        self.assertEqual(_json.encode(-0.0), "-0")
        cases = {
            0.0: "0", 1.0: "1", 3.0: "3", -2.5: "-2.5", 0.1: "0.1",
            1e20: "100000000000000000000", 1e21: "1e+21", 1e100: "1e+100",
            1e-6: "0.000001", 9.99e-7: "9.99e-7", 1.5e-7: "1.5e-7", 2.5e-300: "2.5e-300",
            5e-324: "5e-324", 123456789.125: "123456789.125", 1.0 / 3.0: "0.3333333333333333",
            1.7976931348623157e308: "1.7976931348623157e+308", 123456789012345680.0: "123456789012345680",
            12345.678e-3: "12.345678", 0.000123: "0.000123", 1e-5: "0.00001",
        }
        for f, want in cases.items():
            self.assertEqual(_json.encode(f), want, repr(f))
        for bad in (float("nan"), float("inf"), float("-inf")):
            self.assertRaises(_json.Unencodable, _json.encode, {"a": [bad]})

    def test_strings_like_go(self):
        self.assertEqual(_json.encode("<a&b>"), '"\\u003ca\\u0026b\\u003e"')
        self.assertEqual(_json.encode('"\\\b\f\n\r\t\x00\x1f\x7f'), '"\\"\\\\\\b\\f\\n\\r\\t\\u0000\\u001f\x7f"')
        self.assertEqual(_json.encode("  é😀"), '"\\u2028\\u2029é😀"')
        self.assertEqual(_json.encode("a\udcffb"), '"a\\ufffdb"')

    def test_values(self):
        self.assertEqual(_json.encode({"b": 1, "a": [True, None, (1, 2)], "é": {}, "B": b"\x00\xff"}),
                         '{"B":"AP8=","a":[true,null,[1,2]],"b":1,"é":{}}')
        self.assertEqual(_json.encode({1: "x", "0": "y"}), '{"0":"y","1":"x"}')
        self.assertEqual(_json.encode({"s": {"b", "a", "c"}}), '{"s":["a","b","c"]}')
        class Obj(object):
            def __str__(self):
                return "<obj>"

        self.assertEqual(_json.encode({"o": Obj()}), '{"o":"\\u003cobj\\u003e"}')


class TestAssertions(unittest.TestCase):
    def setUp(self):
        self.c = Console()

    def tearDown(self):
        self.c.close()

    def test_dedup_and_types(self):
        a = ha.always("u.always", "a")
        s = ha.sometimes("u.sometimes", "s")
        r = ha.reachable("u.reach", "r")
        u = ha.unreachable("u.unreach", "u")
        for i in range(10):
            a.check(i != 3, {"i": i})
            s.check(i == 7, {"i": i})
            r.hit()
            u.hit()
        recs = self.c.records()
        got = [(x["id"], x["ok"], x.get("details")) for x in recs]
        self.assertEqual(got, [
            ("u.always", True, {"i": 0}), ("u.reach", True, None), ("u.unreach", False, None),
            ("u.always", False, {"i": 3}), ("u.sometimes", True, {"i": 7}),
        ])

    def test_cmp_does_not_change_callers_details(self):
        a = ha.always("u.cmp", "c")
        d = {"k": 1}
        self.assertTrue(a.cmp(1, "<=", 2, d))
        self.assertFalse(a.cmp(1, ">", 2, d))
        self.assertFalse(a.cmp(1, "??", 1))
        self.assertFalse(a.cmp(1, "<", "x"))  # not comparable: false, not an exception
        self.assertEqual(d, {"k": 1})
        recs = self.c.records()
        self.assertEqual(recs[0]["details"], {"k": 1, "left": 1, "op": "<=", "right": 2})
        self.assertEqual(recs[1]["details"], {"k": 1, "left": 1, "op": ">", "right": 2})

    def test_nan_drops_the_record_but_counts_as_seen(self):
        a = ha.always("u.nan", "n")
        a.check(True, {"f": float("nan")})
        a.check(True, {"f": 1})
        a.check(False, {"f": 2})
        self.assertEqual([(x["ok"], x["details"]) for x in self.c.records()], [(False, {"f": 2})])

    def test_catalog_and_setup(self):
        ha.always("u.cat1", "first")
        ha.sometimes("u.cat2", "second")
        ha.catalog()
        ha.setup_complete()
        decl = self.c.records("HYDO-ASSERT-DECL ")
        ids = [x["id"] for x in decl]
        self.assertLess(ids.index("u.cat1"), ids.index("u.cat2"))
        self.assertEqual(self.c.lines()[-1], "HYDO-SETUP-COMPLETE")
        self.assertEqual(list(decl[0].keys()), ["t", "id", "msg"])

    def test_threads(self):
        a = ha.always("u.threads", "t")
        s = ha.sometimes("u.threads_s", "t")
        filler = ha.always("u.threads_big", "x" * 3000)

        def work(n):
            for i in range(2000):
                a.check(i % 997 != 5, {"n": n})
                s.check(i == 1500, {"n": n})
                filler.check(True, {"pad": "y" * 5000})

        ts = [threading.Thread(target=work, args=(n,)) for n in range(16)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        recs = self.c.records()  # every line parses: no interleaving
        keys = sorted((x["id"], x["ok"]) for x in recs)
        self.assertEqual(keys, [("u.threads", False), ("u.threads", True),
                                ("u.threads_big", True), ("u.threads_s", True)])

    def test_asyncio_and_eventually(self):
        ok = ha.eventually("u.ev_ok", "in time", 1.0)
        late = ha.eventually("u.ev_late", "never", 0.05)
        aok = ha.eventually("u.ev_async", "async in time", 1.0)
        n = [0]

        def cond():
            n[0] += 1
            return n[0] >= 3

        self.assertTrue(ok.await_cond(cond, 0.001))
        self.assertFalse(late.await_cond(lambda: False, 0.01, {"k": 1}))
        w = ha.eventually("u.ev_done_late", "late done", 0.001).begin()
        time.sleep(0.005)
        self.assertFalse(w.done())
        self.assertFalse(w.done())  # second end of an episode: not recorded

        async def main():
            m = [0]

            async def acond():
                await asyncio.sleep(0)
                m[0] += 1
                return m[0] >= 2

            r = await asyncio.gather(aok.await_async(acond, 0.001),
                                     aok.await_async(lambda: True, 0.001))
            return r

        self.assertEqual(asyncio.run(main()) if hasattr(asyncio, "run") else None, [True, True])
        recs = self.c.records()
        self.assertEqual([(x["id"], x["ok"]) for x in recs],
                         [("u.ev_ok", True), ("u.ev_late", False), ("u.ev_done_late", False),
                          ("u.ev_async", True)])
        self.assertEqual(recs[1]["details"]["within_ms"], 50)
        self.assertGreaterEqual(recs[1]["details"]["elapsed_ms"], 50)
        self.assertEqual(recs[1]["details"]["k"], 1)

    def test_off(self):
        os.environ["HYDO_ASSERT_OUT"] = "off"
        _sink._reset()
        a = ha.always("u.off", "o")
        self.assertFalse(a.check(False))
        ha.catalog()
        ha.setup_complete()
        self.assertEqual(self.c.lines(), [])

    @unittest.skipUnless(hasattr(os, "fork"), "no fork")
    def test_fork(self):
        a = ha.always("u.fork", "f")
        a.check(True)
        pid = os.fork()
        if pid == 0:  # child: inherits the dedup state, the lock is fresh
            a.check(True)
            a.check(False, {"who": "child"})
            os._exit(0)
        os.waitpid(pid, 0)
        a.check(False, {"who": "parent"})
        recs = [(x["ok"], x.get("details")) for x in self.c.records() if x["id"] == "u.fork"]
        self.assertEqual(recs, [(True, None), (False, {"who": "child"}), (False, {"who": "parent"})])


class TestBuggify(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.ps = [ha.buggify("t." + c, c) for c in "abcdefgh"]

    def reset(self, cfg):
        os.environ["HYDO_ASSERT_OUT"] = "off"
        _sink._reset()
        os.environ["HYDO_BUGGIFY"] = cfg
        os.environ["HYDO_BUGGIFY_FILE"] = "/nonexistent"
        _buggify._reset()

    def trace(self, n):
        return [p.fire() for _ in range(n) for p in self.ps]

    def tearDown(self):
        os.environ.pop("HYDO_BUGGIFY", None)
        os.environ.pop("HYDO_BUGGIFY_FILE", None)
        _buggify._reset()

    def test_off_and_deterministic(self):
        self.reset("")
        self.assertFalse(any(self.trace(50)))
        self.reset("1234abcd")
        a = self.trace(200)
        self.reset("1234abcd")
        self.assertEqual(a, self.trace(200))

    def test_rates(self):
        en = fired = hits = 0
        for s in range(1, 401):
            self.reset(format(s, "x"))
            with ha._core._mu:
                _buggify._load()
            for p in self.ps:
                if p._enabled():
                    en += 1
                    for _ in range(20):
                        hits += 1
                        fired += p.fire()
        pe = en / float(400 * len(self.ps))
        pf = fired / float(hits)
        self.assertTrue(0.2 < pe < 0.3 and 0.2 < pf < 0.3, (pe, pf))

    def test_record(self):
        c = Console()
        try:
            os.environ["HYDO_BUGGIFY"] = "ffff,1000,1000"
            _buggify._reset()
            p = ha.buggify("t.rec", "a")
            d = {"x": 1}
            self.assertTrue(p.fire(d))
            self.assertTrue(p.fire(d))
            self.assertEqual(d, {"x": 1})
            lines = c.lines()
            self.assertTrue(lines[0].startswith('HYDO-BUGGIFY {"seed":"ffff","enable_pm":1000,"fire_pm":1000,"enabled":["t.a",'))
            self.assertEqual(c.records(), [{"t": "sometimes", "id": "buggify.t.rec", "ok": True,
                                            "msg": "fault point fired: a", "details": {"hit": 1, "x": 1}}])
        finally:
            c.close()


if __name__ == "__main__":
    unittest.main()
