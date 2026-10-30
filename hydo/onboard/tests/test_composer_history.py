#!/usr/bin/env python3
"""History checks as test.checks entries (hydo_composer.history_check; hvx/HISTORIES.md).

    python3 hvx/onboard/tests/test_composer_history.py
"""
import os, subprocess, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ONB = os.path.dirname(HERE)
sys.path.insert(0, ONB)
import hydo_composer as hc  # noqa: E402


class E(Exception):
    pass


def base(checks):
    return {"name": "t", "workload": {"steps": 3},
            "test": {"commands": [{"name": "w", "kind": "parallel_driver", "run": ["/bin/w"]}], "checks": checks}}


class HistoryChecks(unittest.TestCase):
    def test_plan_entries_become_manifest_checks(self):
        m = base([{"name": "kv.lin", "kind": "linearizable", "history": "kv", "model": "cas", "run": ["/bin/readback"]},
                  {"name": "log.la", "kind": "list-append", "history": "log", "consistency": "serializable"},
                  {"name": "ok", "kind": "final", "run": ["/bin/ok"]}])
        meta = hc.expand(m, E)
        self.assertEqual([c["name"] for c in meta["checks"]], ["ok"])      # only the guest-side check
        self.assertEqual(m["checks"], [
            {"id": "kv.lin", "type": "linearizable", "history": "kv", "msg": "kv.lin", "model": "cas",
             "final": "exec '/bin/readback'"},
            {"id": "log.la", "type": "list-append", "history": "log", "msg": "log.la", "consistency": "serializable"}])
        self.assertNotIn("kv.lin", [v["id"] for v in m["invariants"]])

    def test_errors(self):
        for bad in ({"name": "x", "kind": "linearizable"},                                   # no history
                    {"name": "x", "kind": "linearizable", "history": "h", "within": 3},      # liveness field
                    {"name": "x", "kind": "final", "run": ["/bin/x"], "history": "h"}):      # history on a final check
            with self.assertRaises(E):
                hc.expand(base([bad]), E)

    def test_folder(self):
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "checks"))
        p = os.path.join(d, "checks", "list_append_app.log")
        with open(p, "w") as f:
            f.write("#!/bin/sh\n# hydo: history=log consistency=snapshot-isolation\n# hydo-msg: lists\n")
        os.chmod(p, 0o755)
        fo = hc.scan_folder(d, E)
        self.assertEqual(fo["checks"], [{"name": "app.log", "kind": "list_append", "run": ["/hydo/checks/list_append_app.log"],
                                         "history": "log", "consistency": "snapshot-isolation", "msg": "lists"}])
        m = base([])
        hc.expand(m, E, fo)
        self.assertEqual(m["checks"][0]["type"], "list-append")
        self.assertEqual(m["checks"][0]["final"], "exec '/hydo/checks/list_append_app.log'")

    def test_example_builds(self):
        y = os.path.join(ONB, "examples", "redis-composed-hist", "hydo", "hydo.yaml")
        r = subprocess.run([sys.executable, os.path.join(ONB, "hydo-build"), y, "--check"], stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, universal_newlines=True)
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertIn("+ 2 history checks", r.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=1)
