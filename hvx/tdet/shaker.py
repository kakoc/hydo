# shaker.py <pidfile-glob-pattern-dir> : every 50 ms move every hvx_explore process whose
# cmdline mentions DIR to one random CPU (forces migrations between run re-entries).
import os, random, sys, time
d = sys.argv[1]; end = time.time() + float(sys.argv[2]); n = 0
while time.time() < end:
    for p in os.listdir("/proc"):
        if not p.isdigit(): continue
        try:
            c = open("/proc/%s/cmdline" % p, "rb").read().split(b"\0")
        except OSError: continue
        if c and c[0].endswith(b"hvx_explore") and any(d.encode() in x for x in c):
            try: os.sched_setaffinity(int(p), {random.randrange(os.cpu_count())}); n += 1
            except OSError: pass
    time.sleep(0.05)
print("moves", n)
