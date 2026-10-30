#!/usr/bin/env python3
"""The reference for tests/equivalence.rs: the Python hydo-build (hydo/onboard/hydo-build), run
in-process for many command lines. Reads JSON lines {"argv": [...]} on stdin and answers one JSON
line each: {"rc": <exit status>, "out": <stdout>, "err": <stderr>, "crash": <true when it stopped
with a Python exception (a traceback), with "exc" naming it>}.
"""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import sys

ONBOARD = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "onboard")


def load():
    loader = importlib.machinery.SourceFileLoader("hydo_build", os.path.join(ONBOARD, "hydo-build"))
    spec = importlib.util.spec_from_loader("hydo_build", loader)
    m = importlib.util.module_from_spec(spec)
    loader.exec_module(m)
    return m


def main():
    hb = load()
    for line in sys.stdin:
        if not line.strip():
            continue
        req = json.loads(line)
        out, err = io.StringIO(), io.StringIO()
        res = {"crash": False}
        sys.argv = ["hydo-build"] + req["argv"]
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                rc = hb.main()
        except SystemExit as e:
            rc = e.code if isinstance(e.code, int) else 1
            if not isinstance(e.code, int) and e.code is not None:
                err.write(str(e.code) + "\n")
        except Exception as e:      # a traceback in the real command
            rc = 1
            res["crash"] = True
            res["exc"] = "%s: %s" % (type(e).__name__, e)
        res.update({"rc": rc, "out": out.getvalue(), "err": err.getvalue()})
        sys.stdout.write(json.dumps(res) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
