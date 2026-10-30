#!/usr/bin/env python3
"""Print, one per line, every schedule string the explorer (hydo/explore/explore.py) produces over
a large, deterministic range: the corpus that tests/corpus.rs round-trips through the Rust parser.

usage: python3 corpus.py [--quick]     (no arguments: the full corpus; --quick: about a tenth)

What it covers, the way explore.py's main() composes schedules (and the hydo/explore/*_ident.py
scripts drive it): every generator (random, swarm, swarm-pos, process) with durable_only and the
random streams of --timing, --new-faults, --more-faults (fixed cf= range), --preempt (hvx: pp=/xt=,
hvm: pct=), --turns/--bursts (2 vCPUs), --buggify, --concurrency, --freeze-call, --protocol-faults,
--grpc-faults and the test composer's W=, switched per seed from a hash of the seed; without a
manifest (the stack's names) and with every example manifest under hydo/onboard/examples*/; for the
hvx and the hvm backend (hvm_backend.hvm_supported with and without a data disk and tc); every
sweep; the shrink candidates of the first schedules of each group (with a tick range, so seeded
jitter becomes an explicit tjx= list); the branch variants explore.py builds from a BRANCH run; the
cf= probe; freeze-sweep trace points (#0); and the resolved (`W=:...`) and saved (`W=@<key>`) forms
of the composer's compositions. Each line is "<backend>\t<schedule>" (hvx or hvm: the engine the string was made for);
lines are deduplicated per backend, in first-seen order.
"""
import glob
import hashlib
import importlib.util
import os
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
EXP = os.path.join(TOP, "hydo", "explore")
ONB = os.path.join(TOP, "hydo", "onboard")
sys.path.insert(0, EXP)
sys.path.insert(0, ONB)
import grpc_faults          # noqa: E402  (shared by every explore module loaded below)
import hydo_composer        # noqa: E402
import manifest_space       # noqa: E402

QUICK = "--quick" in sys.argv
SEEDS = 60 if QUICK else 600            # seeds per (manifest, backend, generator)
CAND_EVERY = 25                         # shrink candidates of every n-th seed
seen = set()
out = sys.stdout


CUR = ["hvx"]      # the backend of the group being generated: each line is "<backend>\t<schedule>"


def emit(x):
    k = (CUR[0], x)
    if k not in seen:
        seen.add(k)
        out.write("%s\t%s\n" % k)


_n = [0]


def fresh():
    """A fresh copy of explore.py (its globals are rebound per manifest)."""
    _n[0] += 1
    spec = importlib.util.spec_from_file_location("ex_%d" % _n[0], os.path.join(EXP, "explore.py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def mix(x):
    x = (x + 0x9E3779B97F4A7C15) & (2 ** 64 - 1)
    x = ((x ^ (x >> 30)) * 0xBF58476D1CE4E5B9) & (2 ** 64 - 1)
    x = ((x ^ (x >> 27)) * 0x94D049BB133111EB) & (2 ** 64 - 1)
    return x ^ (x >> 31)


FLAGS = ["TIMING", "NEW_FAULTS", "DURABLE", "MORE_FAULTS", "PREEMPT", "TURNS", "BURSTS", "BUGGIFY",
         "CONCURRENCY", "FREEZE_CALL", "PROTOCOL_FAULTS", "GRPC", "COMPOSE"]


def flags_for(sd, salt):
    """The flag vector of a seed: all off, all on, then a hash of the seed (every combination
    is likely over a few hundred seeds)."""
    if sd == 1:
        return {f: False for f in FLAGS}
    if sd == 2:
        return {f: True for f in FLAGS}
    h = mix(sd * 0x1000193 ^ salt)
    return {f: bool((h >> i) & 1) for i, f in enumerate(FLAGS)}


def setflags(E, fl):
    E.TIMING, E.NEW_FAULTS, E.MORE_FAULTS, E.PREEMPT = fl["TIMING"], fl["NEW_FAULTS"], fl["MORE_FAULTS"], fl["PREEMPT"]
    E.TURNS, E.BURSTS, E.BUGGIFY, E.CONCURRENCY = fl["TURNS"], fl["BURSTS"], fl["BUGGIFY"], fl["CONCURRENCY"]
    E.FREEZE_CALL, E.PROTOCOL_FAULTS, E.COMPOSE = fl["FREEZE_CALL"], fl["PROTOCOL_FAULTS"], fl["COMPOSE"]
    E.VCPUS = 2 if fl["TURNS"] else 1
    grpc_faults.ON = fl["GRPC"]


def compose(E, strat, sd, fl):
    """explore.py main()'s composition of one seed's schedule (the dict, before render)."""
    s = E.GENERATORS[strat](sd)
    if fl["DURABLE"]:
        s = E.durable_only(s)
    if strat not in ("swarm-pos", "process"):
        s = E.add_new_faults(s, sd, strat == "swarm")
    s = E.add_grpc_faults(E.add_protocol_faults(E.add_freeze_call(E.add_more_faults(s, sd, strat), sd), sd), sd)
    return E.add_compose(E.add_turns(E.add_buggify(E.add_preempt(E.add_concurrency(E.add_timing(s, sd), sd), sd), sd), sd), sd)


def candidates(E, s, sd):
    """The shrink candidates of s, with and without a tick range (the run's tjk)."""
    tjk = (2 + sd % 7, 40 + (sd * 13) % 300)
    for tk in (None, tjk):
        for c in E.candidates(s, tk):
            t = E.render(c)
            emit(t)
            if "tj" in c and c["tj"][0] == "x":          # one step further: single tjx entries
                for c2 in E.candidates(E.parse(t), tk):
                    emit(E.render(c2))


def branch_variants(E, text, sd):
    """The plain schedules of a BRANCH run's base and variants (explore.py Worker.branch)."""
    host, guest = text.split("|", 1)
    pm = E.TJ_PM[len(E.TJ_PM) // 2]
    emit("%s rb=%d |%s" % (host.rstrip(), E.BRANCH_BUDGET, guest))
    for v in (1, 2, 7, 31):
        emit("%s tj=%x tjpm=%s tjmax=%s tjlo=%d tjhi=0 rb=%d |%s" % (
            host.rstrip(), v, pm, E.TJ_MAX, 3 + (sd + v) % 50, E.BRANCH_BUDGET, guest))


def composer_forms(E, text):
    """The resolved and saved forms of a schedule's W= (hydo_composer.stable_w / key_of), and the
    resolved form's own shrink candidates, as the explorer's stable-composition step writes them."""
    meta = E.SPACE.compose
    w = hydo_composer.w_of(text)
    if not w:
        return
    try:
        r = hydo_composer.stable_w(w, meta)
    except ValueError:
        return
    for x in [r] + hydo_composer.candidates(r, meta)[:8]:
        t = hydo_composer.with_w(text, x)
        if hydo_composer.fits(t):
            emit(t)
        else:
            emit(hydo_composer.with_w(text, "@" + hydo_composer.key_of(x)))
    emit(hydo_composer.with_w(text, "@" + hydo_composer.key_of(r)))
    # the old index form of o (M86), which the shrinker wrote before M98
    d = hydo_composer.parse_w(w, meta["params"])
    if d["o"]:
        ix = {c["id"]: i for i, c in enumerate(meta["commands"])}
        d = dict(d, o={ix[c] if c in ix else c for c in d["o"]})
        if all(isinstance(c, int) for c in d["o"]):
            emit(hydo_composer.with_w(text, hydo_composer.render_w(d)))


def freeze_points(E):
    """Freeze-sweep points: a #0 trace run and a few packet points per pair, direction and step."""
    for svc, peer in E.FC_PAIRS:
        for d in "<>":
            for match in [None] + [m for m, _ in E.FC_MATCH]:
                for st in range(0, E.NORDERS, 7):
                    for n in (0, 1, 5, 64):
                        emit(E.render({"disk": "none", "dseed": 0, "F": (svc, d, peer, st, n, match, 1000)}))


def group(E, label, backend, strats):
    """Every schedule of one (manifest, backend)."""
    E.BACKEND = backend
    CUR[0] = backend
    E.CF_RANGE = (1, 300)
    salt = int(hashlib.sha256(("%s/%s" % (label, backend)).encode()).hexdigest()[:15], 16)
    for strat in strats:
        for sd in range(1, SEEDS + 1):
            fl = flags_for(sd, salt ^ zlib.crc32(strat.encode()))
            setflags(E, fl)
            s = compose(E, strat, sd, fl)
            text = E.render(s)
            emit(text)
            if sd % CAND_EVERY == 0 or sd <= 3:
                candidates(E, E.parse(text), sd)
                if backend == "hvx":
                    branch_variants(E, E.render(E.GENERATORS[strat](sd)), sd)
            if E.SPACE is not None and E.SPACE.composed() and (sd % 5 == 0 or sd <= 3):
                composer_forms(E, text)
    # every sweep the manifest allows, with the main()'s per-point streams
    fl = {f: False for f in FLAGS}
    for sw in ("kill", "cut", "net", "killcut", "pause", "clock", "oneway", "frontkill", "tail", "depth1",
               "depth1-old", "protocol"):
        if E.SPACE is not None:
            try:
                E.SPACE.check_sweep(sw)
            except SystemExit:
                continue
        try:
            points = E.sweep(sw)
        except SystemExit:
            continue
        for timing in (False, True):
            fl["TIMING"] = timing
            fl["COMPOSE"] = True
            setflags(E, fl)
            for i, (_, sch) in enumerate(points, 1):
                emit(E.render(E.add_compose(E.add_turns(E.add_buggify(E.add_preempt(E.add_concurrency(
                    E.add_timing(dict(sch), i), i), i), i), i), i)))
    if E.FC_PAIRS:
        freeze_points(E)
    emit("disk=cut dseed=0 cfn | run")


def manifests():
    out = []
    for pat in ("examples/*/hydo*.yaml", "examples/*/hydo/hydo.yaml", "examples-mac/*/hydo*.yaml"):
        out += sorted(glob.glob(os.path.join(ONB, pat)))
    return out


def main():
    strats = ["random", "swarm", "swarm-pos", "process"]
    plan = [(None, "hvx"), (None, "hvm")] + [(m, b) for m in manifests() for b in ("hvx", "hvm")]
    if QUICK:
        plan = plan[::4]
    for manifest, backend in plan:
        E = fresh()
        E.SPACE = None
        if manifest:
            sp = manifest_space.load(manifest=manifest)
            sp.apply(vars(E))
            grpc_faults.TARGETS = grpc_faults.targets_of(sp.m)
        else:
            grpc_faults.TARGETS = []
        st = strats if E.NORDERS >= 15 else ["random", "swarm", "process"]
        if backend == "hvm":
            for disk, tc in ((True, True), (False, False)):
                E.hvm_backend.HAS_DISK, E.hvm_backend.HAS_TC = disk, tc
                group(E, "%s/%s%s" % (manifest, disk, tc), backend, st)
        else:
            group(E, str(manifest), backend, st)
    print("corpus: %d schedule strings" % len(seen), file=sys.stderr)


main()
