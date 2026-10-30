#!/usr/bin/env python3
"""Identity check for stable composer ids (M98).

    python3 composer_ids_ident.py <reference hvx dir> <new hvx dir>

Each tree is loaded in its own process (its own explore.py, manifest_space.py, hydo_composer.py
and hydo-build), so the two trees' composer code never mix. Checks:
 (1) no manifest, and every example manifest without a test plan: identical schedule strings,
     shrink candidates and finding classes for every generator, with the other random streams
     on or off;
 (2) every example manifest: hydo-build --print-shell, --print-init and --check identical,
     except, for a plan with test commands, the one line that hands W= to the guest
     (HYDO_KNOB_W_set: compose.sh setw) and the check's summary;
 (3) on examples/redis-composed: identical schedule strings (W= is drawn as before); the shrink
     candidates are the same compositions, now naming omitted commands by id (o<id>+<id>)
     instead of by index (o<n>).
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SEEDS = range(1, 301)


def dump(hvx, manifest):
    """In a child process: the schedule space of one tree for one manifest (or none)."""
    import importlib.util
    sys.path.insert(0, os.path.join(hvx, "explore"))
    spec = importlib.util.spec_from_file_location("explore_t", os.path.join(hvx, "explore", "explore.py"))
    M = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(M)
    import manifest_space
    for k, v in (("SVCS", ["mysql", "kafka", "redis"]), ("NORDERS", 30), ("SPACE", None)):
        M.__dict__[k] = v
    if manifest != "-":
        manifest_space.load(manifest).apply(M.__dict__)
    combos = [(t, nf, c, b) for t in (False, True) for nf in (False, True) for c in (False, True) for b in (False, True)]
    out = {}
    for strat in sorted(M.GENERATORS):
        for opts in combos[::3]:
            M.TIMING, M.NEW_FAULTS, M.CONCURRENCY, M.BUGGIFY = opts
            g = M.GENERATORS[strat]
            ss = []
            for sd in SEEDS:
                s = g(sd)
                if strat not in ("swarm-pos", "process"):
                    s = M.add_new_faults(s, sd, strat == "swarm")
                s = M.add_buggify(M.add_concurrency(M.add_timing(s, sd), sd), sd)
                if hasattr(M, "add_compose"):
                    s = M.add_compose(s, sd)
                ss.append(M.render(s))
            cands = [[M.render(c) for c in M.candidates(M.parse(x))] for x in ss[:50]]
            out["%s %s" % (strat, opts)] = {"schedules": ss, "candidates": cands,
                                             "classes": [M.finding_class(M.parse(x)) for x in ss]}
    if M.SPACE is not None and M.SPACE.compose:
        out["_commands"] = [c["id"] for c in M.SPACE.compose["commands"]]
    return out


def build_out(hvx, manifest, flag):
    p = subprocess.run([sys.executable, os.path.join(hvx, "onboard", "hydo-build"), manifest, flag],
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return ("rc=%d\n%s" % (p.returncode, p.stdout)).replace(hvx, "<hvx>")


def child(hvx, manifest):
    json.dump(dump(hvx, manifest), sys.stdout)


def run_child(hvx, manifest):
    p = subprocess.run([sys.executable, os.path.abspath(__file__), "--child", hvx, manifest],
                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    return json.loads(p.stdout) if p.returncode == 0 else None     # (a manifest the explorer cannot load)


def o_by_index(sched, ids):
    """A candidate with o<id>+<id> written the old way (o<index ranges>), for comparison."""
    import re
    m = re.search(r"(W=[0-9a-f]{8}[^ ]*)", sched)
    if not m:
        return sched
    fs = m.group(1).split(",")
    for i, f in enumerate(fs):
        if f.startswith("o") and re.search(r"[A-Za-z]", f[1:]):
            ix = sorted(ids.index(c) for c in f[1:].split("+"))
            rr, j = [], 0
            while j < len(ix):
                k = j
                while k + 1 < len(ix) and ix[k + 1] == ix[k] + 1:
                    k += 1
                rr.append("%d" % ix[j] if j == k else "%d-%d" % (ix[j], ix[k]))
                j = k + 1
            fs[i] = "o" + ".".join(rr)
    return sched.replace(m.group(1), ",".join(fs))


def main():
    if sys.argv[1] == "--child":
        return child(sys.argv[2], sys.argv[3])
    ref, new = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    ex = os.path.join(new, "onboard", "examples")
    manifests = []
    for root, dirs, files in os.walk(ex):
        for f in files:
            if f.endswith((".yaml", ".json")) and not f.startswith("hydo-compose.lock") and \
                    "/." not in "/" + os.path.relpath(root, ex):
                p = os.path.join(root, f)
                head = open(p).read(4000)
                if "services:" in head or '"services"' in head:
                    manifests.append(os.path.relpath(p, ex))
    manifests.sort()
    # (2) the build's view of every example manifest
    nb = nc = nw = 0
    for rel in manifests:
        for flag in ("--print-shell", "--print-init", "--check"):
            a, b = build_out(ref, os.path.join(ref, "onboard", "examples", rel), flag), build_out(new, os.path.join(ex, rel), flag)
            # new: hydo-build warns about a saved regression in the seed form (only plans with a test plan)
            w = [x for x in b.splitlines() if x.startswith("hydo-build: warning: regression ")]
            if w:
                assert "test" in open(os.path.join(ex, rel)).read() or os.path.isdir(os.path.join(os.path.dirname(os.path.join(ex, rel)), "regressions")), rel
                nw += len(w)
                b = "\n".join(x for x in b.splitlines() if x not in w) + ("\n" if b.endswith("\n") else "")
            if a == b:
                nb += 1
                continue
            da = [x for x in a.splitlines() if x not in b.splitlines()]
            db = [x for x in b.splitlines() if x not in a.splitlines()]
            ok = (flag == "--print-shell" and len(da) == len(db) == 1 and da[0].startswith("HYDO_KNOB_W_set=")
                  and "compose.sh setw" in db[0])
            assert ok, (rel, flag, da[:3], db[:3])
            nc += 1
    print("(2) %d hydo-build outputs (--print-shell/--print-init/--check of %d example manifests) identical; "
          "%d differ only in HYDO_KNOB_W_set (plans with test commands); %d new warning lines about saved "
          "regressions in the seed form" % (nb, len(manifests), nc, nw))
    # (1) schedules without a test plan
    n = nm = 0
    plain = ["-"] + [os.path.join(ex, r) for r in manifests
                     if "test:" not in open(os.path.join(ex, r)).read() and '"test"' not in open(os.path.join(ex, r)).read()
                     and not os.path.isdir(os.path.join(ex, os.path.dirname(r), "workload"))]
    for m in plain:
        a, b = run_child(ref, m.replace(new, ref)), run_child(new, m)
        assert a == b, m
        if a is not None:
            nm += 1
            n += sum(len(v["schedules"]) for v in a.values() if isinstance(v, dict))
    print("(1) identical: %d schedule strings, their shrink candidates and classes, without a manifest and on %d "
          "manifests without a test plan" % (n, nm - 1))
    # (3) the composed example
    m = os.path.join(ex, "redis-composed", "hydo", "hydo.yaml")
    a, b = run_child(ref, m.replace(new, ref)), run_child(new, m)
    ids = b.pop("_commands")
    a.pop("_commands", None)
    ns = nca = nch = 0
    for k in a:
        assert a[k]["schedules"] == b[k]["schedules"], k
        assert a[k]["classes"] == b[k]["classes"], k
        ns += len(a[k]["schedules"])
        for ca, cb in zip(a[k]["candidates"], b[k]["candidates"]):
            assert ca == [o_by_index(x, ids) for x in cb], (k, ca[:3], cb[:3])
            nca += len(ca)
            nch += sum(1 for x, y in zip(ca, cb) if x != y)
    print("(3) examples/redis-composed: %d schedule strings identical (W= drawn as before); %d shrink candidates the "
          "same compositions, %d of them now name an omitted command by id" % (ns, nca, nch))


main()
