# SMP Stage 3 identity check for --vcpus / --turns (TURNS.md).
# usage: python3 turns_ident.py <reference explore.py> <new explore.py> [<reference hvm_backend.py>]
# (1) without --turns the new explore.py gives byte-identical schedule strings for
#     (a) seeds 1-1000 x random/swarm x --timing x --new-faults (8,000 strings, composed as
#         explore.py main does),
#     (b) all four generators x every combination of the other streams (new faults, timing,
#         concurrency, preempt, buggify, durable-only, more faults) x seeds 1-200,
#     (c) every sweep x every combination of the streams a sweep is composed with,
#     plus identical finding classes (both knob rules), shrink candidates and engine args;
# (2) with --turns every field other than q equals the schedule without it, strings
#     round-trip, the shrink candidates of q are drop / fixed / longer, about half of the
#     schedules get q, and the length mix is as documented;
# (3) hvm_backend drops q (and is otherwise unchanged against the reference, if given).
import importlib.util, itertools, sys


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    sys.path.insert(0, path.rsplit("/", 1)[0])
    sys.modules[name] = m
    spec.loader.exec_module(m)
    sys.path.pop(0)
    return m


A = load(sys.argv[1], "ex_ref")
B = load(sys.argv[2], "ex_new")
STREAMS = ("NEW_FAULTS", "TIMING", "CONCURRENCY", "PREEMPT", "BUGGIFY", "MORE_FAULTS")


def setf(**kw):
    for M in (A, B):
        for k, v in kw.items():
            setattr(M, k, v)
        M.CF_RANGE = (1, 300)


def compose(M, strat, sd, dur):
    s = M.GENERATORS[strat](sd)
    if dur:
        s = M.durable_only(s)
    if strat not in ("swarm-pos", "process"):
        s = M.add_new_faults(s, sd, strat == "swarm")
    s = M.add_more_faults(s, sd, strat)
    s = M.add_buggify(M.add_preempt(M.add_concurrency(M.add_timing(s, sd), sd), sd), sd)
    if hasattr(M, "add_turns"):
        s = M.add_turns(s, sd)
    return M.render(s)


def compare(strings_a, strings_b, what):
    assert strings_a == strings_b, what
    for rule in ("cut", "any"):
        A.KNOB_RULE = B.KNOB_RULE = rule
        assert [A.finding_class(A.parse(x)) for x in strings_a] == [B.finding_class(B.parse(x)) for x in strings_b], (what, rule)
    A.KNOB_RULE = B.KNOB_RULE = "cut"
    assert [[A.render(c) for c in A.candidates(A.parse(x))] for x in strings_a[:40]] == \
           [[B.render(c) for c in B.candidates(B.parse(x))] for x in strings_b[:40]], what


B.TURNS, B.VCPUS = False, 1
n = 0
# (1a) the explicit 8,000
for strat in ("random", "swarm"):
    for tm, nf in itertools.product((False, True), repeat=2):
        setf(NEW_FAULTS=nf, TIMING=tm, CONCURRENCY=False, PREEMPT=False, BUGGIFY=False, MORE_FAULTS=False)
        a = [compose(A, strat, sd, False) for sd in range(1, 1001)]
        b = [compose(B, strat, sd, False) for sd in range(1, 1001)]
        compare(a, b, ("1a", strat, tm, nf))
        n1a = n = n + len(a)
# (1b) every generator x every stream combination
for strat in ("random", "swarm", "swarm-pos", "process"):
    for flags in itertools.product((False, True), repeat=len(STREAMS) + 1):
        setf(**dict(zip(STREAMS, flags[:-1])))
        a = [compose(A, strat, sd, flags[-1]) for sd in range(1, 201)]
        b = [compose(B, strat, sd, flags[-1]) for sd in range(1, 201)]
        compare(a, b, ("1b", strat, flags))
        n += len(a)
# (1c) every sweep, composed as explore.py main composes a sweep
SWEEPS = ("kill", "cut", "net", "killcut", "pause", "clock", "oneway", "frontkill", "tail", "depth1", "depth1-old")
nsw = 0
for name in SWEEPS:
    for flags in itertools.product((False, True), repeat=5):
        setf(**dict(zip(STREAMS[1:5] + ("NEW_FAULTS",), flags)))
        pa, pb = A.sweep(name), B.sweep(name)
        a = [A.render(A.add_buggify(A.add_preempt(A.add_concurrency(A.add_timing(sch, i), i), i), i))
             for i, (_, sch) in enumerate(pa, 1)]
        b = [B.render(B.add_turns(B.add_buggify(B.add_preempt(B.add_concurrency(B.add_timing(sch, i), i), i), i), i))
             for i, (_, sch) in enumerate(pb, 1)]
        assert [l for l, _ in pa] == [l for l, _ in pb], name
        compare(a, b, ("1c", name, flags))
        nsw += len(a)
n += nsw
# engine args unchanged by default
for ram in (1024, 2048):
    A.RAM_MIB = B.RAM_MIB = ram
    assert A.ram_args() == B.engine_args(), ram
A.RAM_MIB = B.RAM_MIB = 1024
B.VCPUS = 2
assert B.engine_args() == ["--vcpus", "2"]
B.VCPUS = 1
print("(1) identical without --turns: %d schedule strings (%d of them the seeds 1-1000 x random/swarm x "
      "--timing x --new-faults set, %d from sweeps); classes under both knob rules; shrink candidates; engine args" % (n, n1a, nsw))

# (2) with --turns
setf(NEW_FAULTS=True, TIMING=True, CONCURRENCY=True, PREEMPT=False, BUGGIFY=False, MORE_FAULTS=True)
tot = withq = short = mid = long_ = seeded = 0
for strat in ("random", "swarm", "swarm-pos", "process"):
    for sd in range(1, 2001):
        B.TURNS = False
        x = compose(B, strat, sd, False)
        B.TURNS = True
        y = compose(B, strat, sd, False)
        p, q = B.parse(x), B.parse(y)
        assert B.render(q) == y, y
        assert {k: v for k, v in q.items() if k != "q"} == p, (x, y)
        tot += 1
        if "q" in q:
            withq += 1
            ql, qs = q["q"]
            short += ql <= 100
            mid += 100 < ql <= 10000
            long_ += ql >= B.Q_DEFAULT // 2
            seeded += qs != 0
            c = [B.render(cd) for cd in B.candidates(q)]
            assert B.render({k: v for k, v in q.items() if k != "q"}) in c, y
            if qs:
                assert B.render(dict(q, q=(ql, 0))) in c, y
            if ql < B.Q_DEFAULT:
                assert B.render(dict(q, q=(min(B.Q_DEFAULT, ql * 8), qs))) in c, y
B.TURNS = False
print("(2) --turns: %d of %d schedules got q (%.1f%%); lengths <=100: %.1f%%, 101..10,000: %.1f%%, "
      "2^19..2^21: %.1f%%; seeded (qs=): %.1f%%; other fields unchanged; strings round-trip"
      % (withq, tot, 100.0 * withq / tot, 100.0 * short / withq, 100.0 * mid / withq, 100.0 * long_ / withq,
         100.0 * seeded / withq))
# hand-written token forms
for t in ("disk=none dseed=0 q=50 | run", "disk=none dseed=0 q=50 qs=1f | run", "disk=none dseed=0 qs=1f | run",
          "disk=cut dseed=0 tj=1 tjpm=50 tjmax=500000 q=1048576 qs=abc | run C=3"):
    assert B.render(B.parse(t)) == t, (t, B.render(B.parse(t)))

# (3) hvm_backend drops q
hb = load(sys.argv[2].rsplit("/", 1)[0] + "/hvm_backend.py", "hb_new")
for has_tc in (False, True):
    hb.HAS_TC, hb.HAS_DISK = has_tc, True
    assert "q" not in hb.hvm_supported({"disk": "none", "dseed": 0, "q": (50, 3)})
if len(sys.argv) > 3:
    R = load(sys.argv[3], "hb_ref")
    for has_tc, has_disk in itertools.product((False, True), repeat=2):
        R.HAS_TC, R.HAS_DISK, hb.HAS_TC, hb.HAS_DISK = has_tc, has_disk, has_tc, has_disk
        setf(NEW_FAULTS=True, TIMING=True, CONCURRENCY=True, PREEMPT=True, BUGGIFY=True, MORE_FAULTS=False)
        for sd in range(1, 501):
            s = B.parse(compose(B, "swarm", sd, False))
            assert R.hvm_supported(s) == hb.hvm_supported(s), sd
print("(3) hvm_backend drops q; otherwise identical to the reference")
