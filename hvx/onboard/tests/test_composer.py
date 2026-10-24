#!/usr/bin/env python3
"""The test composer's two plan implementations agree, and its plan code is sane.

    python3 hvx/onboard/tests/test_composer.py [--awk "/path/to/busybox awk"] [--n 3000]

hydo_composer.plan()/kept() (host: explorer, shrinker, reports) and the awk program in
hydo-compose.sh (guest) must produce the same entries for every W. Run it with the guest's
busybox awk (`--awk "<busybox> awk"`) on a machine that has it; the default is the local awk.
"""
import argparse, os, random, re, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import hydo_composer as hc  # noqa: E402


class E(Exception):
    pass


def awk_program():
    t = open(os.path.join(os.path.dirname(HERE), "hydo-compose.sh")).read()
    return re.search(r"PLAN_AWK='(.*?)'\n", t, re.S).group(1)


def cmd_sets():
    k = lambda kind, n: [{"kind": kind, "id": "%s_%d" % (kind, i)} for i in range(n)]
    sets = []
    for npar in (0, 1, 3):
        for nser in (0, 1, 2):
            for nsg in (0, 1, 2):
                for nany in (0, 1, 2):
                    if npar + nser + nsg + nany == 0:
                        continue
                    cs = k("parallel_driver", npar) + k("serial_driver", nser) + k("singleton_driver", nsg) + \
                         k("anytime", nany) + k("first", 1) + k("eventually", 1)
                    cs.sort(key=lambda c: c["id"])
                    sets.append(cs)
    return sets


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--awk", default="awk")
    ap.add_argument("--n", type=int, default=3000)
    a = ap.parse_args()
    prog = awk_program()
    rnd = random.Random(7)
    sets = cmd_sets()
    params = dict(hc.DEFAULTS)
    checked = 0
    with tempfile.TemporaryDirectory() as td:
        lst = os.path.join(td, "compose.list")
        for n in range(a.n):
            cmds = sets[n % len(sets)]
            with open(lst, "w") as f:
                for i, c in enumerate(cmds):
                    f.write("%d %s 60 %s exec /bin/true\n" % (i, c["kind"], c["id"]))
            steps = rnd.choice([1, 4, 20, 57])
            d = {"seed": rnd.getrandbits(32) if n % 5 else n, "p": 1 + rnd.randrange(4), "q": 1 + rnd.randrange(3),
                 "r": 100, "n": rnd.choice([0, 0, 3, 10]), "t": 0, "x": set(), "o": set()}
            full = hc.plan(d["seed"], d["q"], steps, cmds)
            if full and n % 3 == 0:
                d["x"] = set(rnd.sample(range(len(full)), min(len(full), rnd.randrange(1, 6))))
            if n % 4 == 0:
                d["o"] = {rnd.randrange(len(cmds))}
            w = hc.render_w(d)
            d2 = hc.parse_w(w, params)
            assert hc.render_w(d2) == w, (w, hc.render_w(d2))
            want = ["%d %d %d %s %d" % (i, r, c, cmds[c]["kind"], s) for i, r, c, s in hc.kept(d2, steps, cmds)]
            f = w.split(",")
            fx = [x[1:] for x in f if x.startswith("x")]
            fo = [x[1:] for x in f if x.startswith("o")]
            out = subprocess.run(a.awk.split() + ["-v", "seed=%d" % int(f[0], 16), "-v", "q=%d" % d["q"],
                                 "-v", "S=%d" % steps, "-v", "X=%s" % (fx[0] if fx else ""),
                                 "-v", "O=%s" % (fo[0] if fo else ""), "-v", "N=%d" % d["n"], prog, lst],
                                 stdout=subprocess.PIPE, text=True, check=True).stdout.split("\n")
            got = [x for x in out if x]
            assert got == want, "W=%s steps=%d cmds=%s\n awk: %s\n py:  %s" % (w, steps, [c["kind"] for c in cmds], got[:6], want[:6])
            checked += 1
    # every kind's rule: singleton plans have one singleton and nothing but anytime besides it
    for cmds in sets:
        for seed in range(200):
            es = hc.plan(seed, 2, 20, cmds)
            kinds = [cmds[c]["kind"] for _, _, c, _ in es]
            if "singleton_driver" in kinds:
                assert kinds.count("singleton_driver") == 1 and not set(kinds) & {"parallel_driver", "serial_driver"}
            assert all(cmds[c]["kind"] in hc.PLAN_KINDS for _, _, c, _ in es)
    # the shrinker's candidates are valid W values with no more entries than the original
    meta = {"params": params, "commands": sets[-1], "steps": 20}
    for seed in range(50):
        w = hc.render_w({"seed": seed * 7919, "p": 4, "q": 2, "r": 100, "n": 0, "t": 0, "x": set(), "o": set()})
        n0 = len(hc.kept(hc.parse_w(w, params), 20, meta["commands"]))
        for c in hc.candidates(w, meta):
            assert len(hc.kept(hc.parse_w(c, params), 20, meta["commands"])) <= n0
    print("test_composer: %d W values, awk == python (%s); plan rules and shrink candidates OK" % (checked, a.awk))


if __name__ == "__main__":
    main()
