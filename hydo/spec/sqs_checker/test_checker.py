"""Tests: allowed nondeterminism must pass; planted-bug histories must be rejected.

Run: python3 -m unittest hvx/spec/sqs_checker/test_checker.py  (from the repo root)
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))
from checker import Spec, check  # noqa: E402

V = 30_000  # visibility timeout, ms


def send(i, t0, t1, outcome="ok", group=None, dedup=None):
    return {"op": "send", "id": i, "t0": t0, "t1": t1, "outcome": outcome,
            "group": group, "dedup": dedup}


def recv(t0, t1, *msgs):
    return {"op": "receive", "t0": t0, "t1": t1, "outcome": "ok",
            "msgs": [{"id": m, "receipt": r, "vis": V} for m, r in msgs]}


def delete(r, t0, t1, outcome="ok"):
    return {"op": "delete", "receipt": r, "t0": t0, "t1": t1, "outcome": outcome}


class Allowed(unittest.TestCase):
    """Behaviours a correct SQS-like queue may show."""

    def test_happy_path(self):
        h = [send("a", 0, 5), recv(10, 12, ("a", "r1")), delete("r1", 13, 14)]
        self.assertTrue(check(h, Spec.strict())["valid"])

    def test_redelivery_after_visibility_timeout(self):
        h = [send("a", 0, 5), recv(10, 12, ("a", "r1")), recv(10 + V + 1, 10 + V + 3, ("a", "r2"))]
        self.assertTrue(check(h, Spec.strict())["valid"])

    def test_standard_queue_may_duplicate_and_reorder(self):
        h = [send("a", 0, 1), send("b", 2, 3),
             recv(10, 11, ("b", "r1")), recv(12, 13, ("a", "r2")), recv(14, 15, ("b", "r3"))]
        self.assertTrue(check(h, Spec.sqs_standard())["valid"])

    def test_unknown_send_may_appear_or_not(self):
        appears = [send("a", 0, 100, "info"), recv(200, 201, ("a", "r1"))]
        absent = [send("a", 0, 100, "info")]
        self.assertTrue(check(appears, Spec.strict())["valid"])
        self.assertTrue(check(absent, Spec.strict())["valid"])

    def test_failed_send_never_counts_as_lost(self):
        self.assertTrue(check([send("a", 0, 1, "fail")], Spec.strict())["valid"])

    def test_fifo_order_and_group_lock_respected(self):
        h = [send("a", 0, 1, group="g"), send("b", 2, 3, group="g"),
             recv(10, 11, ("a", "r1")), delete("r1", 12, 13), recv(14, 15, ("b", "r2"))]
        self.assertTrue(check(h, Spec.strict(fifo=True))["valid"])

    def test_fifo_other_group_not_blocked(self):
        h = [send("a", 0, 1, group="g"), send("x", 2, 3, group="h"),
             recv(10, 11, ("a", "r1")), recv(12, 13, ("x", "r2"))]
        self.assertTrue(check(h, Spec.strict(fifo=True))["valid"])

    def test_dedup_retry_delivered_once(self):
        # client retried a send with the same dedup id; the queue kept one copy (id "a")
        h = [send("a", 0, 10, "info", group="g", dedup="d1"), send("a2", 20, 21, group="g", dedup="d1"),
             recv(30, 31, ("a", "r1"))]
        r = check(h, Spec.sqs_fifo())
        self.assertTrue(r["valid"], r["errors"])

    def test_dedup_outside_window_is_two_messages(self):
        late = 6 * 60 * 1000
        h = [send("a", 0, 1, group="g", dedup="d1"), send("a2", late, late + 1, group="g", dedup="d1"),
             recv(10, 11, ("a", "r1")), delete("r1", 11, 12), recv(late + 10, late + 11, ("a2", "r2"))]
        r = check(h, Spec.sqs_fifo())
        self.assertTrue(r["valid"], r["errors"])


class PlantedBugs(unittest.TestCase):
    """Each history is something no correct implementation could produce."""

    def bad(self, h, spec, word):
        r = check(h, spec)
        self.assertFalse(r["valid"])
        self.assertTrue(any(word in e for e in r["errors"]), r["errors"])

    def test_lost_acked_message(self):
        self.bad([send("a", 0, 5)], Spec.sqs_standard(), "lost")

    def test_phantom_message(self):
        self.bad([recv(10, 11, ("zz", "r1"))], Spec.sqs_standard(require_drain=False), "unexpected")

    def test_double_delivery_inside_visibility(self):
        h = [send("a", 0, 1), recv(10, 11, ("a", "r1")), recv(20, 21, ("a", "r2"))]
        self.bad(h, Spec.strict(), "visibility")

    def test_resurrected_after_delete(self):
        h = [send("a", 0, 1), recv(10, 11, ("a", "r1")), delete("r1", 12, 13),
             recv(13 + V + 10, 13 + V + 11, ("a", "r2"))]
        self.bad(h, Spec.strict(), "resurrected")

    def test_retention_violated(self):
        h = [send("a", 0, 1), recv(100_000, 100_001, ("a", "r1"))]
        self.bad(h, Spec.sqs_standard(retention_ms=60_000), "retention")

    def test_fifo_reordered(self):
        h = [send("a", 0, 1, group="g"), send("b", 2, 3, group="g"),
             recv(10, 11, ("b", "r1")), delete("r1", 11, 12), recv(13, 14, ("a", "r2"))]
        self.bad(h, Spec.strict(fifo=True), "order")

    def test_fifo_group_lock_broken(self):
        h = [send("a", 0, 1, group="g"), send("b", 2, 3, group="g"),
             recv(10, 11, ("a", "r1")), recv(20, 21, ("b", "r2"))]
        self.bad(h, Spec.strict(fifo=True), "group lock")

    def test_fifo_dedup_broken(self):
        h = [send("a", 0, 1, group="g", dedup="d1"), send("a2", 2, 3, group="g", dedup="d1"),
             recv(10, 11, ("a", "r1")), delete("r1", 11, 12), recv(13, 14, ("a2", "r2"))]
        self.bad(h, Spec.sqs_fifo(), "dedup")


if __name__ == "__main__":
    unittest.main()
