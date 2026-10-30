#!/usr/bin/env python3
"""Summarise the engine's PHBOOT / PHASES lines (hvx_explore with HVX_EXPLORE_PHASES=1).

  phases.py runs   <stderr or worker-*.err files>   one row per run, the main phases
  phases.py mean   <files>                          mean of every phase, split by cut / no cut
  phases.py flow   <files>                          per worker: busy time, idle gaps between runs
                                                    (the engine waiting for its next RUN line),
                                                    and steady-state throughput
"""
import re, sys, collections

MAIN = ["fresh", "r_ram", "r_disk", "workload", "wfp", "check", "ffp", "durable", "rec_fresh",
        "rec_restore", "recovery", "rfp", "teardown", "rec_teardown"]


def parse(paths):
    runs, boots = [], []
    for p in paths:
        for line in open(p, errors="replace"):
            if line.startswith("PHASES "):
                parts = line.split()
                d = {"tag": parts[1], "file": p}
                for kv in parts[2:]:
                    if "=" in kv:
                        k, v = kv.split("=", 1)
                        try:
                            d[k] = float(v)
                        except ValueError:
                            d[k] = v
                runs.append(d)
            elif line.startswith("PHBOOT ") and "setup=" in line:
                d = {"file": p}
                for kv in line.split()[1:]:
                    if "=" in kv:
                        k, v = kv.split("=", 1)
                        try:
                            d[k] = float(v)
                        except ValueError:
                            d[k] = v
                boots.append(d)
    return runs, boots


def guest_s(r):
    return r.get("workload", 0) + r.get("check", 0) + r.get("recovery", 0)


def cmd_runs(runs):
    print("%-14s %4s " % ("tag", "cut") + " ".join("%8s" % k[:8] for k in MAIN) + " %8s %8s %6s" % ("total", "guest", "share"))
    for r in runs:
        t = r.get("total", 0)
        print("%-14s %4d " % (r["tag"][:14], int(r.get("cut", 0))) + " ".join("%8.2f" % r.get(k, 0) for k in MAIN) +
              " %8.2f %8.2f %5.0f%%" % (t, guest_s(r), 100 * guest_s(r) / t if t else 0))


def cmd_mean(runs, boots):
    for label, sel in (("all", lambda r: True), ("no cut", lambda r: not r.get("cut")), ("cut", lambda r: r.get("cut"))):
        rs = [r for r in runs if sel(r)]
        if not rs:
            continue
        n = len(rs)
        tot = sum(r.get("total", 0) for r in rs) / n
        print("== %s: %d runs, mean total %.2f s" % (label, n, tot))
        keys = [k for k in rs[0] if isinstance(rs[0][k], float) and k not in ("t0",)]
        for k in ("fresh", "r_snap", "r_ram", "r_disk", "r_con", "setup", "workload", "wfp", "savecon", "check", "ffp",
                  "durable", "rec_teardown", "rec_fresh", "rec_restore", "recovery", "rfp", "teardown", "run_ioctl",
                  "drain", "dirtyscan"):
            v = sum(r.get(k, 0) for r in rs) / n
            if v:
                print("   %-14s %8.3f s  %5.1f%%" % (k, v, 100 * v / tot if tot else 0))
        g = sum(guest_s(r) for r in rs) / n
        print("   %-14s %8.3f s  %5.1f%%  (workload + check + recovery: inside HVX_RUN)" % ("guest-run", g, 100 * g / tot))
        fp = sum(r.get("wfp", 0) + r.get("ffp", 0) + r.get("rfp", 0) for r in rs) / n
        print("   %-14s %8.3f s  %5.1f%%" % ("fingerprints", fp, 100 * fp / tot))
        for k in ("w_exits", "w_ticks", "w_vmx_s", "c_exits", "c_ticks", "rc_exits", "rc_ticks", "rc_vmx_s",
                  "w_ram_dirty", "w_disk_dirty", "c_ram_dirty", "c_disk_dirty", "rc_ram_dirty", "rss_kb"):
            vs = [r[k] for r in rs if k in r]
            if vs:
                print("   %-14s mean %12.1f  min %12.0f  max %12.0f" % (k, sum(vs) / len(vs), min(vs), max(vs)))
    for b in boots:
        print("PHBOOT %s: setup %.1f to_s0 %.1f snap_s0 %.1f fp_s0 %.1f to_s1 %.1f fp_s1 %.1f snap_s1 %.1f" % (
            b["file"].split("/")[-1], b.get("setup", 0), b.get("to_s0", 0), b.get("snap_s0", 0), b.get("fp_s0", 0),
            b.get("to_s1", 0), b.get("fp_s1", 0), b.get("snap_s1", 0)))


def cmd_flow(runs):
    byf = collections.defaultdict(list)
    for r in runs:
        byf[r["file"]].append(r)
    ends, starts = [], []
    for f, rs in sorted(byf.items()):
        rs.sort(key=lambda r: r["t0"])
        busy = sum(r["total"] for r in rs)
        gaps = [b["t0"] - (a["t0"] + a["total"]) for a, b in zip(rs, rs[1:])]
        span = rs[-1]["t0"] + rs[-1]["total"] - rs[0]["t0"]
        gs = sorted(gaps)
        print("%s: %d runs, busy %.0f s of %.0f s (%.1f%%); idle gaps sum %.1f s, median %.3f s, max %.1f s" % (
            f.split("/")[-1], len(rs), busy, span, 100 * busy / span if span else 0, sum(gaps),
            gs[len(gs) // 2] if gs else 0, max(gaps) if gaps else 0))
        starts.append(rs[0]["t0"])
        ends += [r["t0"] + r["total"] for r in rs]
    if ends:
        lo, hi = max(starts), max(ends)          # from the moment every worker has started running
        n = sum(1 for e in ends if lo <= e <= hi)
        print("steady state: %d runs finished in %.0f s after the last worker's first run: %.2f runs/min" % (
            n, hi - lo, 60 * n / (hi - lo) if hi > lo else 0))


def main():
    cmd, files = sys.argv[1], sys.argv[2:]
    runs, boots = parse(files)
    if cmd == "runs":
        cmd_runs(runs)
    elif cmd == "mean":
        cmd_mean(runs, boots)
    elif cmd == "flow":
        cmd_flow(runs)


if __name__ == "__main__":
    main()
