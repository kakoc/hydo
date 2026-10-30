# Burst-turn identity check for explore.py --bursts (hvx/FASTTURN.md).
# usage: python3 bursts_ident.py <explore.py>
# With --turns --bursts every field other than qb equals the schedule with --turns alone
# (8,000 schedules: seeds 1-1000 x random/swarm x --timing x --new-faults), the strings
# round-trip through parse/render, about half of the schedules get qb, the drawn values are
# in their documented ranges, and the shrink candidates of qb are drop / rarer / shorter.
import importlib.util, sys


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    sys.path.insert(0, path.rsplit("/", 1)[0])
    sys.modules[name] = m
    spec.loader.exec_module(m)
    sys.path.pop(0)
    return m


B = load(sys.argv[1], "ex_new")


def compose(strat, sd, dur):
    s = B.GENERATORS[strat](sd)
    if dur:
        s = B.durable_only(s) if hasattr(B, "durable_only") else s
    s = B.add_new_faults(s, sd, swarm=(strat == "swarm"))
    return B.render(B.add_turns(B.add_timing(s, sd), sd))


B.VCPUS, B.TURNS, B.TIMING, B.NEW_FAULTS = 2, True, True, True
n = got = 0
for strat in ("random", "swarm"):
    for sd in range(1, 1001):
        B.BURSTS = False
        x = compose(strat, sd, False)
        B.BURSTS = True
        y = compose(strat, sd, False)
        p, q = B.parse(x), B.parse(y)
        assert B.render(q) == y, y
        assert {k: v for k, v in q.items() if k != "qb"} == p, (x, y)
        n += 1
        if "qb" in q:
            got += 1
            blen, bq, pm, bs = q["qb"]
            assert 2000 <= blen <= 200000 and 20 <= bq <= 1000 and pm in (50, 100, 250, 500, 1000) and bs, y
            c = [B.render(cd) for cd in B.candidates(q)]
            assert B.render({k: v for k, v in q.items() if k != "qb"}) in c, y
            assert B.render(dict(q, qb=(blen, bq, pm // 2, bs))) in c or pm < 2, y
            assert B.render(dict(q, qb=(blen // 2, bq, pm, bs))) in c or blen <= 2 * bq, y
for t in ("disk=none dseed=0 qb=20000:97:300 qbs=b0b5eed | run",
          "disk=none dseed=0 q=5000 qs=1f qb=4000:50:1000 qbs=1 | run K=kafka@5+2"):
    assert B.render(B.parse(t)) == t, (t, B.render(B.parse(t)))
print("bursts: %d of %d schedules got qb (%.1f%%); other fields unchanged; strings round-trip; "
      "shrink candidates drop / rarer / shorter" % (got, n, 100.0 * got / n))
