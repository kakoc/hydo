#!/usr/bin/env python3
"""Stable composer ids (M98): saved compositions name commands by id.

    python3 hvx/onboard/tests/test_composer_ids.py

On examples/redis-composed's plan: the resolved form names the same entries as the seed form;
`hydo save`'s file reads back to a regression that replays those entries after commands are
added and the list order changes; a removed command, a late round and an edited key are errors;
a changed command warns; an old index-form regression with its saved list resolves to what it
ran then; staging writes /etc/hydo/compose.saved/<key>; content hashes come from the files.
"""
import json
import os
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ONB = os.path.dirname(HERE)
sys.path.insert(0, ONB)
import hydo_composer as hc  # noqa: E402


class E(Exception):
    pass


def meta_of(folder_dir, extra=None):
    """The plan meta hydo-build computes for a hydo/ folder (+ extra workload files)."""
    m = {"name": "t", "workload": {"steps": 20}, "test": {"parallel": 4, "per_round": 2, "round_ms": 100}}
    return hc.expand(m, E, hc.scan_folder(folder_dir, E))


def main():
    src = os.path.join(ONB, "examples", "redis-composed", "hydo")
    with tempfile.TemporaryDirectory() as td:
        d = os.path.join(td, "hydo")
        shutil.copytree(src, d, symlinks=False)
        meta = meta_of(d)
        ids = [c["id"] for c in meta["commands"]]
        assert all(c["content"] and len(c["content"]) == 16 for c in meta["commands"])
        # the M86 minimal composition: two transfers in round 4
        w = "fc94c8ea,p2,q2,r200,n2,x4,o0.4.6"
        res = hc.stable_w(w, meta)
        assert res == ":p2,r200,cparallel_driver_transfer,7.4.0.721476632,8.4.0.40276333", res
        assert hc.describe(res, meta) == hc.describe(w, meta) == "r4: parallel_driver_transfer, parallel_driver_transfer"
        # hydo save -> a file -> read back
        x = {"from_seed": 6, "class": "violation", "min_verdict": "VIOLATION [total_conserved] balances sum to 495",
             "composition": hc.describe(res, meta), "minimal": "disk=none dseed=1 | run W=" + res,
             "stable": res, "stable_schedule": "disk=none dseed=1 | run W=" + res, "contents": hc.contents_of(res, meta)}
        rd = os.path.join(d, "regressions")
        with open(os.path.join(rd, "found.schedule"), "w") as f:
            f.write(hc.saved_regression_text(x))
        r = hc.read_regression(os.path.join(rd, "found.schedule"), E)
        assert r["contents"] == {"parallel_driver_transfer": meta["commands"][ids.index("parallel_driver_transfer")]["content"]}
        st = hc.regression_status(r, meta)
        assert st["error"] is None and not st["warnings"] and st["schedule"] == r["schedule"], st
        # add two commands (one sorts first: every index moves) and rebuild the meta
        for name, body in (("anytime_aaa_ping", "#!/bin/sh\ntrue\n"), ("parallel_driver_incr", "#!/bin/sh\ntrue\n")):
            p = os.path.join(d, "workload", name)
            open(p, "w").write(body)
            os.chmod(p, 0o755)
        meta2 = meta_of(d)
        ids2 = [c["id"] for c in meta2["commands"]]
        assert ids2.index("parallel_driver_transfer") != ids.index("parallel_driver_transfer")
        r2 = [x for x in meta2["regressions"] if x["name"] == "found"][0]
        st = hc.regression_status(r2, meta2)
        assert st["error"] is None and not st["warnings"], st
        same = [(e, r_, ids2[c], s) for e, r_, c, s in hc.entries_of(hc.w_of(r2["schedule"]), meta2)]
        assert same == [(e, r_, ids[c], s) for e, r_, c, s in hc.entries_of(w, meta)]
        # the seed form would now compose something else
        assert [(e, r_, ids2[c], s) for e, r_, c, s in hc.entries_of(w, meta2)] != same
        # the old index-form regression of the example (seed form + its saved list): resolved
        old = [x for x in meta2["regressions"] if x["name"] == "lost-update"][0]
        assert old["saved_schedule"] == "disk=none dseed=1 | run W=0000002a,p4,q2,r100"
        assert hc.w_of(old["schedule"]).startswith("@") and old["compose"]
        orig = [(e, r_, ids[c], s) for e, r_, c, s in hc.entries_of("0000002a,p4,q2,r100", meta)]
        now = [(e, r_, ids2[c], s) for e, r_, c, s in hc.entries_of(hc.w_of(old["schedule"]), meta2,
                                                                    saved={hc.w_of(old["schedule"])[1:]: old["compose"]})]
        assert now == orig and len(orig) == 34
        # a shrunk failure whose resolved form did not fail the same way: saved in the index form
        xs = dict(x, minimal="disk=none dseed=1 | run W=" + w, stable_note="did not fail the same way",
                  commands=ids)
        p = os.path.join(td, "fallback.schedule")
        open(p, "w").write(hc.saved_regression_text(xs))
        rf = hc.read_regression(p, E)
        assert rf["schedule"].endswith("W=" + w) and rf["commands"] == ids and rf["contents"], rf
        st = hc.regression_status(rf, meta)
        assert st["schedule"] == rf["schedule"] and st["error"] is None and not st["warnings"], st
        st = hc.regression_status(rf, meta2)          # the list changed: resolved, same entries
        assert hc.w_of(st["schedule"]) == res and st["error"] is None, st
        # staging: the saved form's file
        R = os.path.join(td, "root")
        hc.stage(meta2, R, ONB)
        key = hc.w_of(old["schedule"])[1:]
        assert open(os.path.join(R, "etc/hydo/compose.saved", key)).read().strip() == old["compose"]
        assert json.load(open(os.path.join(R, "etc/hydo/compose.json")))["stable_compose"] == 1
        # a changed command warns
        p = os.path.join(d, "workload", "parallel_driver_transfer")
        open(p, "a").write("# edited\n")
        st = hc.regression_status(r, meta_of(d))
        assert st["error"] is None and "changed since the regression was saved" in st["warnings"][0], st
        # a removed command fails, naming it
        os.remove(p)
        meta3 = meta_of(d)
        st = hc.regression_status(r, meta3)
        assert st["error"] and "parallel_driver_transfer" in st["error"] and "no longer has" in st["error"], st
        # a round the plan does not run, an edited key
        st = hc.regression_status(r, dict(meta, steps=4))
        assert "round 4" in st["error"], st
        bad = dict(r, schedule="disk=none | run W=@000000000000", compose=res)
        assert "disagree" in hc.regression_status(bad, meta)["error"]
        good = dict(r, schedule="disk=none | run W=@" + hc.key_of(res), compose=res)
        assert hc.regression_status(good, meta)["error"] is None
        # a seed form without a saved list: runs as it is, with a warning
        st = hc.regression_status({"name": "n", "schedule": "disk=none | run W=0000002a", "expect": "ok"}, meta)
        assert st["schedule"].endswith("W=0000002a") and "seed form" in st["warnings"][0]
        # o by id renders and parses, and omits the same command as by index
        dd = hc.parse_w("0000002a,p4,q2,r100,oparallel_driver_set", meta["params"])
        assert hc.render_w(dd) == "0000002a,p4,q2,r100,oparallel_driver_set"
        assert hc.kept(dd, 20, meta["commands"]) == hc.kept(hc.parse_w("0000002a,p4,q2,r100,o%d" % ids.index(
            "parallel_driver_set"), meta["params"]), 20, meta["commands"])
    print("test_composer_ids: resolved form, save/read round trip, replay after added and reordered commands, "
          "old index form, staging, changed/removed commands, keys, o by id: OK")


if __name__ == "__main__":
    main()
