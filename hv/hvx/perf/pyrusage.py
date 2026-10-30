#!/usr/bin/env python3
"""Run a Python script unchanged and print its own CPU use at exit (not its children's).

  pyrusage.py <script.py> [args ...]

Prints "PYRUSAGE user=<s> sys=<s> maxrss_kb=<n> wall=<s>" on stderr when the script ends, so
the explorer's own Python cost can be told apart from the engines it starts (whose time
/usr/bin/time adds in). Measurement only (PERF-RUN-COST.md).
"""
import atexit, os, resource, runpy, sys, time

t0 = time.time()


def report():
    r = resource.getrusage(resource.RUSAGE_SELF)
    sys.stderr.write("PYRUSAGE user=%.2f sys=%.2f maxrss_kb=%d wall=%.1f\n" % (r.ru_utime, r.ru_stime, r.ru_maxrss,
                                                                             time.time() - t0))
    sys.stderr.flush()


atexit.register(report)
script = sys.argv[1]
sys.argv = sys.argv[1:]
sys.path.insert(0, os.path.dirname(os.path.abspath(script)))
runpy.run_path(script, run_name="__main__")
