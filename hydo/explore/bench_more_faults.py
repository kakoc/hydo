#!/usr/bin/env python3
"""bench_more_faults.py -- run hydo/bench/run.py with explore.py --more-faults (Milestone 62).

run.py composes the generators itself and has no --more-faults option, and hydo/bench belongs to
the benchmark work, so this wrapper loads it unchanged, adds the timed kill X= and the k-th-FLUSH
power cut cf= to every schedule run.py generates (explore.add_more_faults, its own random stream,
so every other field is what run.py would have run), and labels the results "<strategy>+xcf".

  python3 bench_more_faults.py [--runpy ../bench/run.py] --cf-range 1-260 <run.py arguments>

--cf-range is required: the bench images share one guest and workload, so one range measured on
the control (explore.py --schedule "disk=cut dseed=0 cfn | run", FLUSHCUT flushes=<n> in
worker-0.err) serves every defect image.
"""
import importlib.util, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))


def main():
    argv, runpy, cfr = [], os.path.join(os.path.dirname(HERE), "bench", "run.py"), None
    it = iter(sys.argv[1:])
    for x in it:
        if x == "--runpy":
            runpy = next(it)
        elif x == "--cf-range":
            cfr = next(it)
        else:
            argv.append(x)
    if not cfr and "--table" not in argv:
        raise SystemExit("bench_more_faults.py: --cf-range lo-hi is required")
    spec = importlib.util.spec_from_file_location("benchrun", runpy)
    R = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(R)
    ex = R.ex
    if os.path.dirname(os.path.abspath(ex.__file__)) != HERE:
        raise SystemExit("bench_more_faults.py: run.py imported %s, not this tree's explore.py" % ex.__file__)
    ex.MORE_FAULTS = True
    if cfr:
        ex.CF_RANGE = tuple(int(v) for v in cfr.split("-"))
    orig_sched, orig_seeded = R.schedules, R.run_seeded

    def schedules(strategy, seeds, *rest, **kw):
        out = []
        for sd, text in orig_sched(strategy, seeds, *rest, **kw):
            s = ex.add_more_faults(ex.parse(text), sd, strategy)
            out.append((sd, ex.render(s)))
        return out

    def run_seeded(a, *rest):
        if not a.label.endswith("+xcf"):
            a.label += "+xcf"
        return orig_seeded(a, *rest)

    R.schedules, R.run_seeded = schedules, run_seeded
    sys.argv = [runpy] + argv
    R.main()


if __name__ == "__main__":
    main()
