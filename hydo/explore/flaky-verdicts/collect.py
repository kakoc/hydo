"""collect.py <lite.json> <out dir> <task id>...: grade every run of each task and summarize.

<out dir>/<id>/ is task-remote.sh's output. Each run's test log is parsed with the SWE-bench
harness's own log parser for the repository (swebench 3.0.17), and each test of FAIL_TO_PASS and
PASS_TO_PASS gets the harness's verdict (passed = PASSED or XFAIL; a test missing from the log is
failed). A run is "resolved" when every FAIL_TO_PASS test and every PASS_TO_PASS test passes (the
harness's FULL resolution). Three kinds of runs: native (Docker, network off), fresh (hydo, a
fresh boot each) and snap (hydo, every run from one held snapshot).

Writes <out dir>/<id>/summary.json and prints one line per task.
"""
import base64
import gzip
import hashlib
import json
import os
import re
import statistics
import sys

from swebench.harness.constants import END_TEST_OUTPUT, START_TEST_OUTPUT
from swebench.harness.grading import test_passed
from swebench.harness.log_parsers import MAP_REPO_TO_PARSER
from swebench.harness.test_spec.test_spec import make_test_spec


def unpack(text):
    """The log between FV-BEGIN and FV-END (gzip, base64), or None."""
    m = re.search(r"FV-BEGIN[^\n]*\n(.*?)FV-END", text, re.S)
    if not m:
        return None
    try:
        return gzip.decompress(base64.b64decode(re.sub(r"\s", "", m.group(1)))).decode("utf-8", "replace")
    except Exception:
        return None


def snap_log(path):
    """The test log inside one `hydo inspect exec` answer."""
    d = json.load(open(path))
    out = d.get("exec", {}).get("stdout", {})
    if out.get("truncated"):
        return None
    return unpack(out.get("text", ""))


def times(path):
    out = {}
    if os.path.exists(path):
        for ln in open(path):
            p = ln.split()
            if not p or not p[0].isdigit():
                continue
            kv = dict(x.split("=", 1) for x in p[1:] if "=" in x)
            out[int(p[0])] = kv
    return out


def grade(spec, log):
    """(status map of every test the parser found, ok) for one log."""
    if log is None or START_TEST_OUTPUT not in log or END_TEST_OUTPUT not in log:
        return {}, False
    body = log.split(START_TEST_OUTPUT)[1].split(END_TEST_OUTPUT)[0]
    return MAP_REPO_TO_PARSER[spec.repo](body, spec), True


def main():
    lite, out = sys.argv[1:3]
    rows = {r["instance_id"]: r for r in json.load(open(lite))}
    for iid in sys.argv[3:]:
        d = os.path.join(out, iid)
        spec = make_test_spec(rows[iid], namespace="swebench")
        f2p, p2p = spec.FAIL_TO_PASS, spec.PASS_TO_PASS
        tests = f2p + p2p
        summ = {"instance_id": iid, "repo": spec.repo, "tests": len(tests), "modes": {}}
        for mode in ("native", "native-load", "fresh", "fresh-load", "snap"):
            md = os.path.join(d, mode)
            if not os.path.isdir(md):
                continue
            tm = times(os.path.join(md, "times"))
            runs = []
            for i in sorted(tm):
                if mode == "snap":
                    p = os.path.join(md, f"{i}.json")
                    log = snap_log(p) if os.path.exists(p) else None
                else:
                    p = os.path.join(md, f"{i}.log")
                    log = open(p, errors="replace").read() if os.path.exists(p) else None
                    if log == "":
                        log = None
                sm, ok = grade(spec, log)
                runs.append(
                    {
                        "i": i,
                        "ok": ok,
                        "wall": float(tm[i].get("wall", "nan")),
                        "fp": tm[i].get("fp"),
                        "pass": {t: test_passed(t, sm) for t in tests},
                        "all": sm,
                        "resolved": ok and all(test_passed(t, sm) for t in tests),
                        "log_sha": hashlib.sha256(log.encode()).hexdigest()[:16] if log else None,
                    }
                )
            if not runs:
                continue
            good = [r for r in runs if r["ok"]]
            flips = {}
            for t in tests:
                n = sum(r["pass"][t] for r in good)
                if 0 < n < len(good):
                    flips[t] = f"passed {n} of {len(good)}"
            # every test the parser saw, not only FAIL_TO_PASS and PASS_TO_PASS
            seen = sorted({t for r in good for t in r["all"]})
            other = {}
            for t in seen:
                st = [r["all"].get(t, "MISSING") for r in good]
                if len(set(st)) > 1 and t not in tests:
                    other[t] = {s: st.count(s) for s in set(st)}
            summ["modes"][mode] = {
                "runs": len(runs),
                "graded": len(good),
                "resolved": sum(r["resolved"] for r in good),
                "flips": flips,
                "other_flips": other,
                "verdict_by_test": {t: sum(r["pass"][t] for r in good) for t in tests},
                "wall_median": statistics.median(r["wall"] for r in runs),
                "wall_min": min(r["wall"] for r in runs),
                "wall_max": max(r["wall"] for r in runs),
                "fps": sorted({r["fp"] for r in runs if r["fp"]}),
                "seen_tests": len(seen),
                "distinct_logs": len({r["log_sha"] for r in runs if r["log_sha"]}),
                "resolved_runs": [r["resolved"] for r in runs],
                "log_shas": [r["log_sha"] for r in runs],
                "walls": [r["wall"] for r in runs],
            }
        # Tests whose verdict was the same in every native run and the same in every hydo run,
        # but not the same in both: a verdict hydo changed.
        changed = {}
        nat = summ["modes"].get("native")
        for mode in ("fresh", "fresh-load", "snap"):
            hm = summ["modes"].get(mode)
            if not nat or not hm or not nat["graded"] or not hm["graded"]:
                continue
            for t in tests:
                a, b = nat["verdict_by_test"][t], hm["verdict_by_test"][t]
                na, nb = nat["graded"], hm["graded"]
                if a in (0, na) and b in (0, nb) and (a == na) != (b == nb):
                    changed.setdefault(t, {})[mode] = "passes natively, fails in hydo" if a == na else "fails natively, passes in hydo"
        summ["changed"] = changed
        json.dump(summ, open(os.path.join(d, "summary.json"), "w"), indent=1)
        line = [iid]
        for mode, m in summ["modes"].items():
            line.append(
                f"{mode}: {m['graded']}/{m['runs']} graded, resolved {m['resolved']}, flips {len(m['flips'])}"
                f" (+{len(m['other_flips'])} other), {m['wall_median']:.1f}s, fps {len(m['fps'])}, logs {m['distinct_logs']}"
            )
        line.append(f"changed {len(changed)}")
        print(" | ".join(line))


if __name__ == "__main__":
    main()
