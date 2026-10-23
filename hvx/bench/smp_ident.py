#!/usr/bin/env python3
"""smp_ident.py <old run.py> -- run.py's schedule strings without --vcpus/--turns are identical to
an older run.py's (every seeded strategy x new-faults x buggify x concurrency x the seeds of reps
1-3), and with --turns every field except the turn tokens equals the schedule without them."""
import importlib.util, itertools, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import run as new  # noqa: E402
spec = importlib.util.spec_from_file_location("run_old", sys.argv[1])
old = importlib.util.module_from_spec(spec)
spec.loader.exec_module(old)
n = 0
seeds = [r * 1000 + i for r in (1, 2, 3) for i in range(1, 61)]
for st in ("random", "swarm", "swarm-pos", "process"):
    for nf, bg, cc in itertools.product((False, True), repeat=3):
        a = old.schedules(st, seeds, nf, bg, cc)
        b = new.schedules(st, seeds, nf, bg, cc)
        assert a == b, (st, nf, bg, cc)
        n += len(a)
print("identical without --turns: %d schedule strings" % n)
new.ex.VCPUS = 2
for st in ("random", "swarm"):
    for cc in (False, True):
        a = new.schedules(st, seeds, False, False, cc, False)
        b = new.schedules(st, seeds, False, False, cc, True)
        on = 0
        for (_, t1), (_, t2) in zip(a, b):
            p2 = new.ex.parse(t2)
            on += "q" in p2
            p2.pop("q", None)
            assert new.ex.render(p2) == t1, (t1, t2)
        print("%s concurrency=%s: %d of %d schedules get turn tokens; the rest of every string unchanged"
              % (st, cc, on, len(b)))
new.ex.TURNS = False
