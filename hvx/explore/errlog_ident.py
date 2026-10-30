# Identity check for error logs as findings (M94). usage:
#   python3 errlog_ident.py <reference explore.py> <new explore.py> [<manifest without errors:> ...]
# Error logs as findings adds no schedule token, so the explorer must be unchanged for every image
# whose manifest has no errors: (and for every run whose console has no HYDO-ERRLOG records):
#  (1) schedule strings, classes (both knob rules) and shrink candidates are byte-identical for all
#      four generators with every combination of the other random streams, without a manifest and
#      on each given manifest;
#  (2) a run result goes through host_errlog unchanged when its console carries no HYDO-ERRLOG-SUM
#      (with an empty known list and with one that has entries, report and fail policies);
#  (3) shrink() with the default test (no keep=) takes the same path as the reference shrink();
#  (4) write_report() without error kinds writes the same report.md as the reference.
import importlib.util, itertools, json, os, sys, tempfile


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec); sys.path.insert(0, os.path.dirname(path)); spec.loader.exec_module(m)
    sys.path.pop(0)
    return m


A = load(sys.argv[1], "ex_ref"); B = load(sys.argv[2], "ex_new")
MS_A = load(os.path.join(os.path.dirname(sys.argv[1]), "manifest_space.py"), "ms_ref")
MS_B = load(os.path.join(os.path.dirname(sys.argv[2]), "manifest_space.py"), "ms_new")
GLOBALS = ("SVCS", "NET_SVCS", "PAUSE_SVCS", "NORDERS", "KNOBS", "KNOB_TOKENS", "SPACE", "FC_PAIRS", "FC_MATCH", "PF_TARGETS")
SAVED = {M: {k: getattr(M, k) for k in GLOBALS if hasattr(M, k)} for M in (A, B)}


def reset(M):
    for k, v in SAVED[M].items():
        setattr(M, k, v)


def compose(M, strat, sd, dur):
    s = M.GENERATORS[strat](sd)
    if dur: s = M.durable_only(s)
    if strat not in ("swarm-pos", "process"): s = M.add_new_faults(s, sd, strat == "swarm")
    s = M.add_grpc_faults(M.add_protocol_faults(M.add_freeze_call(M.add_more_faults(s, sd, strat), sd), sd), sd)
    return M.render(M.add_compose(M.add_turns(M.add_buggify(M.add_preempt(M.add_concurrency(M.add_timing(s, sd), sd), sd), sd), sd), sd))


def run(label, strats=("random", "swarm", "swarm-pos", "process"), seeds=range(1, 121)):
    n = 0
    for strat in strats:
        for flags in itertools.product((False, True), repeat=7):
            nf, tm, cc, pe, bg, dur, mf = flags
            for M in (A, B):
                M.NEW_FAULTS, M.TIMING, M.CONCURRENCY, M.PREEMPT, M.BUGGIFY, M.MORE_FAULTS = nf, tm, cc, pe, bg, mf
                M.CF_RANGE = (1, 300)
            a = [compose(A, strat, sd, dur) for sd in seeds]
            b = [compose(B, strat, sd, dur) for sd in seeds]
            assert a == b, (label, strat, flags)
            for rule in ("cut", "any"):
                A.KNOB_RULE = B.KNOB_RULE = rule
                assert [A.finding_class(A.parse(x)) for x in a] == [B.finding_class(B.parse(x)) for x in b], (label, strat, flags, rule)
            A.KNOB_RULE = B.KNOB_RULE = "cut"
            assert [[A.render(c) for c in A.candidates(A.parse(x))] for x in a[:25]] == \
                   [[B.render(c) for c in B.candidates(B.parse(x))] for x in b[:25]], (label, strat, flags)
            n += len(a)
    return n


n1 = run("no manifest")
n2 = 0
for mf in sys.argv[3:]:
    reset(A); reset(B)
    MS_A.load(manifest=mf).apply(vars(A))
    MS_B.load(manifest=mf).apply(vars(B))
    strats = ("random", "swarm", "swarm-pos", "process") if B.NORDERS >= 15 else ("random", "swarm", "process")
    n2 += run(mf, strats)

# (2) host_errlog on consoles without records
he = B._he()
d = tempfile.mkdtemp(prefix="errlog-ident-")
os.makedirs(os.path.join(d, "consoles"))
open(os.path.join(d, "consoles", "seed-1.log"), "w").write("hydo-ob: x\nHYDO-ASSERT {}\nHYDO-ORACLE: OK fine\n")
open(os.path.join(d, "consoles", "seed-1.recovery.log"), "w").write("HYDO-ORACLE: VIOLATION x\n")
kf = os.path.join(d, "known.yaml")
open(kf, "w").write("format: 1\nunknown: fail\nknown:\n  - match: 'x'\n    reason: test\n")
for known in (he.load_known(None), he.load_known(kf)):
    for res in ({"fp": "1", "verdict": "HYDO-ORACLE: OK fine", "secs": 1.0}, {"fp": "2", "verdict": "VIOLATION y"}):
        B.ERRLOG = known
        r = B.host_errlog(d, "seed-1", "disk=none | run K=redis@3+1", dict(res))
        assert r == res, (r, res)
assert B.errlog_of(d, "seed-1") is None

# (3) shrink without keep= follows same_failure on the verdict, like the reference
class FakePool:
    def __init__(self, M):
        self.M = M
    def run_all(self, items):
        out = {}
        for tag, text in items:
            s = self.M.parse(text)
            # a toy oracle: it fails while the schedule kills redis
            v = "HYDO-ORACLE: VIOLATION lost key k:3" if s.get("K") and s["K"][0] == "redis" else "HYDO-ORACLE: OK"
            out[tag] = (text, {"verdict": v, "fp": "f"})
        return out


class Log:
    def write(self, s): pass
    def flush(self): pass


reset(A); reset(B)
for M in (A, B):
    M.NEW_FAULTS = M.TIMING = M.CONCURRENCY = M.PREEMPT = M.BUGGIFY = M.MORE_FAULTS = False
cnt = 0
for sd in range(1, 80):
    s = A.generate(sd)
    if not (s.get("K") and s["K"][0] == "redis"):
        continue
    ra = A.shrink(FakePool(A), A.parse(A.render(s)), "HYDO-ORACLE: VIOLATION lost key k:3", Log())
    rb = B.shrink(FakePool(B), B.parse(B.render(s)), "HYDO-ORACLE: VIOLATION lost key k:3", Log())
    assert A.render(ra[0]) == B.render(rb[0]) and ra[1:] == rb[1:], (sd, ra, rb)
    cnt += 1
assert cnt > 3, cnt

# (4) the report without error kinds
summary = {"schedules": 3, "strategy": "random", "image": "x.img", "failures": 1}
fails = [{"seed": 2, "class": "durable-defaults", "verdict": "HYDO-ORACLE: VIOLATION lost key k:3", "reproduced": True}]
shrunk = {("durable-defaults", "VIOLATION lost key k:#"): {"from_seed": 2, "minimal": "disk=none | run K=redis@3+1", "tries": 4,
                                                           "fp_a": "a", "fp_b": "a", "min_verdict": "VIOLATION lost key k:3"}}
ra, rb = tempfile.mkdtemp(), tempfile.mkdtemp()
for M in (A, B):
    M.IMAGE, M.BACKEND = "x.img", "hvx"
A.write_report(ra, summary, fails, shrunk, None)
B.write_report(rb, summary, fails, shrunk, None)
assert open(os.path.join(ra, "report.md")).read() == open(os.path.join(rb, "report.md")).read()
print("identical without errors: %d schedule strings without a manifest, %d on %d manifests (4 generators x 128 "
      "stream combinations; classes under both knob rules; shrink candidates); host_errlog leaves %s results "
      "unchanged without records; shrink() paths identical on %d schedules; report.md identical"
      % (n1, n2, len(sys.argv) - 3, 4, cnt))
