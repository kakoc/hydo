#!/usr/bin/env python3
"""Drive one explorer engine in serve mode (hvx_explore or the Mac's hvmx) and time it from outside.

  drive.py <schedules file> -- <engine command ...>

The schedules file has one "<tag> <schedule>" per line. Each RUN line is sent only after the
previous RESULT arrived, so "wall" is the whole cost of one schedule as explore.py sees it (VM
reset or creation, restore, workload, check or recovery, fingerprints), and "secs" is what the
engine itself reports. Prints READY time, then one line per run. Measurement only.
"""
import subprocess, sys, time

def main():
    i = sys.argv.index("--")
    lst, cmd = sys.argv[1], sys.argv[i + 1:]
    t0 = time.time()
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
    line = p.stdout.readline()
    print("READY after %.2f s: %s" % (time.time() - t0, line.strip()), flush=True)
    for row in open(lst):
        row = row.strip()
        if not row:
            continue
        tag, sched = row.split(" ", 1)
        t = time.time()
        p.stdin.write("RUN %s %s\n" % (tag, sched))
        p.stdin.flush()
        out = p.stdout.readline().strip()
        w = time.time() - t
        secs = out.split("secs=")[1].split()[0] if "secs=" in out else "?"
        print("RUN %-12s wall=%.3f engine_secs=%s %s" % (tag, w, secs, out[:200]), flush=True)
    p.stdin.write("QUIT\n")
    p.stdin.flush()
    p.wait()

if __name__ == "__main__":
    main()
