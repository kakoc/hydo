#!/usr/bin/env python3
"""Unit tests of the history checkers (hvx/history/hydohist.py) on synthetic histories.

    python3 hvx/history/test_hydohist.py        (or: python3 -m unittest -v test_hydohist)
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hydohist as H   # noqa: E402

T = [0]


def hist(lines, name="kv"):
    """'p type f args...' per line -> console text; time = line number in ms."""
    out = []
    for i, l in enumerate(lines):
        p, ty, rest = l.split(None, 2)
        out.append("HYDO-HIST %s %s %s %d %s" % (name, p, ty, (i + 1) * 1000000, rest))
    return "\n".join(out) + "\n"


def lin(lines, **spec):
    sp = {"id": "t", "type": "linearizable", "history": "kv"}
    sp.update(spec)
    ev, _, _ = H.parse_sources([("w", hist(lines))])
    return H.run_check(ev, sp)


def la(lines, **spec):
    sp = {"id": "t", "type": "list-append", "history": "kv"}
    sp.update(spec)
    ev, _, _ = H.parse_sources([("w", hist(lines))])
    return H.run_check(ev, sp)


class Register(unittest.TestCase):
    def test_sequential_ok(self):
        r = lin(["0 invoke write x 1", "0 ok write x 1", "1 invoke read x", "1 ok read x 1"])
        self.assertEqual(r["status"], "ok", r)
        self.assertEqual(r["details"]["ops"], 2)

    def test_concurrent_read_before_write(self):
        r = lin(["0 invoke write x 1", "1 invoke read x", "0 ok write x 1", "1 ok read x nil"])
        self.assertEqual(r["status"], "ok", r)

    def test_stale_read_after_ack_is_violation(self):
        r = lin(["0 invoke write x 1", "0 ok write x 1", "1 invoke read x", "1 ok read x nil"])
        self.assertEqual(r["status"], "violation")
        self.assertIn("acknowledged write is lost", r["summary"])
        self.assertEqual(r["details"]["witness_ops"], 2)

    def test_two_writes_read_old(self):
        r = lin(["0 invoke write x 1", "0 ok write x 1", "0 invoke write x 2", "0 ok write x 2",
                 "1 invoke read x", "1 ok read x 1"])
        self.assertEqual(r["status"], "violation")

    def test_unknown_write_may_take_effect_later(self):
        r = lin(["0 invoke write x 1", "1 invoke read x", "1 ok read x nil",
                 "1 invoke read x", "1 ok read x 1"])
        self.assertEqual(r["status"], "ok", r)

    def test_unknown_write_may_never_happen(self):
        r = lin(["0 invoke write x 1", "0 info write x 1", "1 invoke read x", "1 ok read x nil"])
        self.assertEqual(r["status"], "ok", r)

    def test_unknown_write_cannot_be_undone(self):
        r = lin(["0 invoke write x 1", "1 invoke read x", "1 ok read x 1",
                 "1 invoke read x", "1 ok read x nil"])
        self.assertEqual(r["status"], "violation")

    def test_failed_write_did_not_happen(self):
        r = lin(["0 invoke write x 1", "0 fail write x 1", "1 invoke read x", "1 ok read x 1"])
        self.assertEqual(r["status"], "violation")

    def test_cas(self):
        ok = lin(["0 invoke write x 0", "0 ok write x 0", "0 invoke cas x 0 1", "0 ok cas x 0 1",
                  "1 invoke read x", "1 ok read x 1"], model="cas")
        self.assertEqual(ok["status"], "ok", ok)
        bad = lin(["0 invoke write x 0", "0 ok write x 0", "0 invoke cas x 0 1", "0 ok cas x 0 1",
                   "1 invoke cas x 0 2", "1 ok cas x 0 2"], model="cas")
        self.assertEqual(bad["status"], "violation")

    def test_cas_needs_cas_model(self):
        r = lin(["0 invoke cas x 0 1", "0 ok cas x 0 1"])
        self.assertEqual(r["status"], "error")

    def test_unknown_cas_either_way(self):
        r = lin(["0 invoke write x 0", "0 ok write x 0", "1 invoke cas x 5 1",
                 "2 invoke read x", "2 ok read x 0"], model="cas")
        self.assertEqual(r["status"], "ok", r)

    def test_versions(self):
        ok = lin(["0 invoke write x a", "0 ok write x a@5", "1 invoke read x", "1 ok read x a@5"])
        self.assertEqual(ok["status"], "ok", ok)
        bad = lin(["0 invoke write x a", "0 ok write x a@5", "1 invoke read x", "1 ok read x a@4"])
        self.assertEqual(bad["status"], "violation")
        self.assertIn("version 5", bad["summary"])
        dec = lin(["0 invoke write x a", "0 ok write x a@5", "0 invoke write x b", "0 ok write x b@3"])
        self.assertEqual(dec["status"], "violation")

    def test_unknown_write_version_learned_from_read(self):
        r = lin(["0 invoke write x a", "1 invoke read x", "1 ok read x a@7",
                 "1 invoke read x", "1 ok read x a@8"])
        self.assertEqual(r["status"], "violation")

    def test_range_read_split_per_key(self):
        r = lin(["0 invoke write x 1", "0 ok write x 1", "0 invoke write y 2", "0 ok write y 2",
                 "1 invoke read *", "1 ok read x 1 y nil"])
        self.assertEqual(r["status"], "violation")
        self.assertEqual(r["details"]["key"], "y")
        self.assertEqual(r["details"]["keys"], 2)

    def test_minimal_witness(self):
        r = lin(["0 invoke write x 1", "0 ok write x 1", "1 invoke read x", "1 ok read x 1",
                 "0 invoke write x 2", "0 ok write x 2", "2 invoke read x", "2 ok read x 2",
                 "1 invoke read x", "1 ok read x 1"])
        self.assertEqual(r["status"], "violation")
        # write 1, write 2, then a read of 1: removing any one of the three explains it
        self.assertEqual(r["details"]["witness_ops"], 3, r["explanation"])
        self.assertIn("(an older value)", r["summary"])

    def test_initial_any(self):
        r = lin(["1 invoke read x", "1 ok read x 9", "1 invoke read x", "1 ok read x 9"], initial="any")
        self.assertEqual(r["status"], "ok", r)
        r = lin(["1 invoke read x", "1 ok read x 9"])
        self.assertEqual(r["status"], "violation")

    def test_reinvoke_closes_open_op(self):
        r = lin(["0 invoke write x 1", "0 invoke read x", "0 ok read x 1"])
        self.assertEqual(r["status"], "ok", r)

    def test_undecided(self):
        lines = []
        for p in range(12):
            lines.append("%d invoke write x %d" % (p, p))
        for p in range(12):
            lines.append("%d ok write x %d" % (p, p))
        lines += ["20 invoke read x", "20 ok read x 99"]
        r = lin(lines)
        self.assertEqual(r["status"], "violation")
        ev, _, _ = H.parse_sources([("w", hist(lines))])
        r = H.run_check(ev, {"id": "t", "type": "linearizable", "history": "kv"}, budget=5)
        self.assertEqual(r["status"], "undecided")

    def test_concurrency_search(self):
        # 8 concurrent writers, then reads that pin one order: linearizable
        lines = ["%d invoke write x %d" % (p, p) for p in range(8)] + ["%d ok write x %d" % (p, p) for p in range(8)]
        lines += ["9 invoke read x", "9 ok read x 5", "9 invoke read x", "9 ok read x 5"]
        self.assertEqual(lin(lines)["status"], "ok")

    def test_recovery_console_after_workload(self):
        w = hist(["0 invoke write x 1", "0 ok write x 1", "0 invoke write y 1"])
        rec = "HYDO-HIST kv 20 invoke 5000000 read x\nHYDO-HIST kv 20 ok 6000000 read x nil y nil\n"
        ev, _, _ = H.parse_sources([("w", w), ("w.recovery", rec)])
        r = H.run_check(ev, {"id": "t", "type": "linearizable", "history": "kv"})
        self.assertEqual(r["status"], "violation")
        self.assertEqual(r["details"]["key"], "x")
        self.assertIn("recovery boot", r["explanation"])


class ListAppend(unittest.TestCase):
    def test_ok(self):
        r = la(["0 invoke append x 1", "0 ok append x 1", "1 invoke append x 2", "1 ok append x 2",
                "2 invoke read x", "2 ok read x [1,2]"])
        self.assertEqual(r["status"], "ok", r)

    def test_lost_append(self):
        r = la(["0 invoke append x 1", "0 ok append x 1", "1 invoke append x 2", "1 ok append x 2",
                "2 invoke read x", "2 ok read x [2]"])
        self.assertEqual(r["status"], "violation")
        self.assertEqual(r["details"]["anomaly"], "lost-append")
        r = la(["0 invoke append x 1", "0 ok append x 1", "2 invoke read x", "2 ok read x []"],
               consistency="serializable")
        self.assertEqual(r["status"], "ok", r)

    def test_duplicate(self):
        r = la(["0 invoke append x 1", "0 ok append x 1", "2 invoke read x", "2 ok read x [1,1]"])
        self.assertIn("duplicate-elements", r["details"]["anomalies"])

    def test_incompatible(self):
        r = la(["0 invoke append x 1", "1 invoke append x 2", "0 ok append x 1", "1 ok append x 2",
                "2 invoke read x", "2 ok read x [1,2]", "3 invoke read x", "3 ok read x [2]"],
               consistency="serializable")
        self.assertEqual(r["details"]["anomaly"], "incompatible-order")

    def test_aborted_and_garbage(self):
        r = la(["0 invoke append x 3", "0 fail append x 3", "2 invoke read x", "2 ok read x [3,9]"])
        self.assertIn("aborted-read", r["details"]["anomalies"])
        self.assertIn("garbage-read", r["details"]["anomalies"])

    def test_g0(self):
        r = la(["0 invoke txn a:x:1 a:y:1", "1 invoke txn a:x:2 a:y:2", "0 ok txn a:x:1 a:y:1",
                "1 ok txn a:x:2 a:y:2", "2 invoke txn r:x r:y", "2 ok txn r:x:[1,2] r:y:[2,1]"],
               consistency="serializable")
        self.assertEqual(r["details"]["anomaly"], "G0", r["explanation"])

    def test_g1c(self):
        r = la(["0 invoke txn a:x:1 r:y", "1 invoke txn a:y:1 r:x", "0 ok txn a:x:1 r:y:[1]",
                "1 ok txn a:y:1 r:x:[1]"], consistency="serializable")
        self.assertEqual(r["details"]["anomaly"], "G1c", r["explanation"])

    def test_read_skew_g_single(self):
        lines = ["0 invoke txn r:x r:y", "1 invoke txn a:x:1 a:y:1", "1 ok txn a:x:1 a:y:1",
                 "0 ok txn r:x:[] r:y:[1]", "2 invoke txn r:x r:y", "2 ok txn r:x:[1] r:y:[1]"]
        for lv in ("serializable", "snapshot-isolation"):
            r = la(lines, consistency=lv)
            self.assertEqual(r["details"].get("anomaly"), "G-single", (lv, r))

    def test_write_skew_g2(self):
        lines = ["0 invoke txn r:x a:y:1", "1 invoke txn r:y a:x:1", "0 ok txn r:x:[] a:y:1",
                 "1 ok txn r:y:[] a:x:1", "2 invoke txn r:x r:y", "2 ok txn r:x:[1] r:y:[1]"]
        r = la(lines, consistency="serializable")
        self.assertEqual(r["details"]["anomaly"], "G2", r["explanation"])
        self.assertEqual(la(lines, consistency="snapshot-isolation")["status"], "ok")

    def test_monotonic_read(self):
        lines = ["5 invoke append x 1", "5 ok append x 1", "6 invoke append x 2", "6 ok append x 2",
                 "0 invoke read x", "7 invoke read x", "7 ok read x [1,2]", "0 ok read x [1,2]",
                 "0 invoke read x", "0 ok read x [1]"]
        r = la(lines, consistency="strong-session-serializable")
        self.assertIn("monotonic-read", r["details"]["anomalies"])
        self.assertEqual(la(lines, consistency="serializable")["status"], "ok")

    def test_read_your_writes(self):
        r = la(["0 invoke append x 1", "0 ok append x 1", "0 invoke read x", "0 ok read x []"],
               consistency="strong-session-snapshot-isolation")
        self.assertIn("read-your-writes", r["details"]["anomalies"])

    def test_unknown_append(self):
        self.assertEqual(la(["0 invoke append x 1", "2 invoke read x", "2 ok read x [1]"])["status"], "ok")
        self.assertEqual(la(["0 invoke append x 1", "0 info append x 1", "2 invoke read x", "2 ok read x []"])["status"], "ok")

    def test_internal(self):
        r = la(["0 invoke txn a:x:5 r:x", "0 ok txn a:x:5 r:x:[1]", "1 invoke append x 1", "1 ok append x 1"],
               consistency="serializable")
        self.assertIn("internal", r["details"]["anomalies"])

    def test_intermediate(self):
        r = la(["0 invoke txn a:x:1 a:x:2", "1 invoke read x", "0 ok txn a:x:1 a:x:2", "1 ok read x [1]"],
               consistency="serializable")
        self.assertIn("intermediate-read", r["details"]["anomalies"])

    def test_realtime_cycle_needs_strict(self):
        lines = ["0 invoke append x 1", "0 ok append x 1", "1 invoke append x 2", "1 ok append x 2",
                 "2 invoke read x", "2 ok read x [1]", "3 invoke read x", "3 ok read x [1,2]"]
        r = la(lines)
        self.assertEqual(r["status"], "violation")
        self.assertEqual(la(lines, consistency="serializable")["status"], "ok")

    def test_unique_elements_required(self):
        r = la(["0 invoke append x 1", "0 ok append x 1", "1 invoke append x 1", "1 ok append x 1"])
        self.assertEqual(r["status"], "error")


class Driver(unittest.TestCase):
    def test_run_checks_verdicts(self):
        decl = 'HYDO-CHECK {"id":"kv.lin","type":"linearizable","model":"register","history":"kv"}\n'
        good = decl + hist(["0 invoke write x 1", "0 ok write x 1", "1 invoke read x", "1 ok read x 1"])
        r = H.run_checks([("w", good)], "HYDO-ORACLE: OK invariants=1 held (a: b)")
        self.assertTrue(r["verdict"].startswith("HYDO-ORACLE: OK invariants=1"))
        self.assertIn("checks=1 held", r["verdict"])
        self.assertEqual(len(r["records"]), 2)
        bad = decl + hist(["0 invoke write x 1", "0 ok write x 1", "1 invoke read x", "1 ok read x nil"])
        r = H.run_checks([("w", bad)], "HYDO-ORACLE: OK invariants=1 held (a: b)")
        self.assertTrue(r["verdict"].startswith("HYDO-ORACLE: VIOLATION [kv.lin] key x"), r["verdict"])
        self.assertIn('"ok":false', r["records"][1])
        r = H.run_checks([("w", bad)], "HYDO-ORACLE: VIOLATION [g] guest first")
        self.assertEqual(r["verdict"], "HYDO-ORACLE: VIOLATION [g] guest first")
        self.assertIsNone(H.run_checks([("w", "nothing\n")], "HYDO-ORACLE: OK x")["verdict"])

    def test_encoding_roundtrip(self):
        s = "a b%c@d,e"
        self.assertEqual(H.pdec(H.penc(s, ",")), s)
        r = lin(["0 invoke write x %s" % H.penc("v 1"), "0 ok write x %s" % H.penc("v 1"),
                 "1 invoke read x", "1 ok read x v%201"])
        self.assertEqual(r["status"], "ok", r)

    def test_empty_token(self):
        self.assertEqual(H.pdec("%"), "")
        self.assertEqual(H.penc(""), "%")
        r = lin(["0 invoke write x %", "0 ok write x %", "1 invoke read x", "1 ok read x %"])
        self.assertEqual(r["status"], "ok", r)

    def test_go_and_shell_records(self):
        # the exact lines hvx/stack/hydohist (Go) and hydo_hist.sh write
        txt = ("HYDO-HIST kv 3 invoke 1000 write k1 a%20b\nHYDO-HIST kv 3 ok 2000 write k1 a%20b@83\n"
               "HYDO-HIST kv 4 invoke 3000 read k1\nHYDO-HIST kv 4 ok 4000 read k1 a%20b@83\n")
        ev, _, _ = H.parse_sources([("w", txt)])
        r = H.run_check(ev, {"id": "t", "type": "linearizable", "history": "kv"})
        self.assertEqual(r["status"], "ok", r)

    def test_prefix_text_and_malformed(self):
        txt = "[  12.3] junk HYDO-HIST kv 0 invoke 1 write x 1\nHYDO-HIST kv 0 bogus 2 write x 1\n"
        ev, _, warn = H.parse_sources([("w", txt)])
        self.assertEqual(len(ev), 1)
        self.assertEqual(len(warn), 1)


def gen_linearizable(seed, nproc=5, nops=300, keys=("x",), lists=False):
    """A random history that is linearizable by construction: each operation takes effect at a
    random instant between its invocation and its completion on a simulated store."""
    import random
    rnd = random.Random(seed)
    store = {}
    lines, open_ = [], {}
    n = 0
    elem = [0]
    while n < nops or open_:
        p = rnd.randrange(nproc)
        if p not in open_ and n < nops:
            k = rnd.choice(keys)
            if lists:
                f = rnd.choice(["append", "read"])
                if f == "append":
                    elem[0] += 1
                    open_[p] = ("append", k, str(elem[0]), False, None)
                    lines.append("%d invoke append %s %d" % (p, k, elem[0]))
                else:
                    open_[p] = ("read", k, None, False, None)
                    lines.append("%d invoke read %s" % (p, k))
            else:
                f = rnd.choice(["write", "read", "read"])
                v = str(rnd.randrange(4))
                open_[p] = (f, k, v, False, None)
                lines.append("%d invoke %s %s%s" % (p, f, k, " " + v if f == "write" else ""))
            n += 1
            continue
        if p not in open_:
            continue
        f, k, v, done, res = open_[p]
        if not done:                      # the effect, at some instant inside the interval
            if f == "write":
                store[k] = v
            elif f == "append":
                store.setdefault(k, []).append(v)
            else:
                res = store.get(k)
                res = list(res) if isinstance(res, list) else res
            open_[p] = (f, k, v, True, res)
            continue
        del open_[p]
        if f == "write":
            lines.append("%d ok write %s %s" % (p, k, v))
        elif f == "append":
            lines.append("%d ok append %s %s" % (p, k, v))
        elif lists:
            lines.append("%d ok read %s [%s]" % (p, k, ",".join(res or [])))
        else:
            lines.append("%d ok read %s %s" % (p, k, "nil" if res is None else res))
    return lines


class Randomized(unittest.TestCase):
    def test_random_linearizable_registers(self):
        for seed in range(20):
            r = lin(gen_linearizable(seed, keys=("x", "y")))
            self.assertEqual(r["status"], "ok", (seed, r["summary"]))

    def test_random_mutation_detected(self):
        found = 0
        for seed in range(20):
            lines = gen_linearizable(seed, nops=60)
            reads = [i for i, l in enumerate(lines) if " ok read " in l]
            i = reads[len(reads) // 2]
            lines[i] = lines[i].rsplit(" ", 1)[0] + " 7"       # a value never written
            found += lin(lines)["status"] == "violation"
        self.assertEqual(found, 20)

    def test_random_list_append(self):
        for seed in range(20):
            r = la(gen_linearizable(seed, keys=("x", "y", "z"), lists=True))
            self.assertEqual(r["status"], "ok", (seed, r["summary"]))


if __name__ == "__main__":
    unittest.main(verbosity=1)
