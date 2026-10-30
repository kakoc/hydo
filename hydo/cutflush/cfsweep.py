#!/usr/bin/env python3
"""cfsweep.py <image> <outdir> <listfile>: run every "tag<TAB>schedule" line of listfile through one
hvx_explore serve process on the side module /dev/hvxcf; append RESULT/FLUSHCUT lines to
<outdir>/results.txt; touch <outdir>/DONE at the end."""
import subprocess, sys, os
img, out, lst = sys.argv[1:4]
os.makedirs(out, exist_ok=True)
home = os.path.expanduser("~")
hv = home + "/hydo-cutflush/hvx"
cmd = ["timeout", "36000", home + "/bin/hvx-run", "sudo", "env", "LD_PRELOAD=%s/race/devshim.so" % hv,
       "HVX_DEV=/dev/hvxcf", "HVX_SNAP_DIR=/var/tmp/hvx-snap", hv + "/hvx_explore", "serve",
       home + "/hydo/build/bzImage", home + "/hydo/build/initramfs-kafka.cpio.gz", home + "/hydo/build/" + img, out]
p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
res = open(out + "/results.txt", "a")
def readline():
    l = p.stdout.readline()
    if not l: raise SystemExit("engine died")
    return l
while True:
    l = readline()
    if l.startswith("READY"): res.write(l); res.flush(); break
for line in open(lst):
    if not line.strip(): continue
    tag, sched = line.rstrip("\n").split("\t", 1)
    p.stdin.write("RUN %s %s\n" % (tag, sched)); p.stdin.flush()
    while True:
        l = readline()
        if l.startswith("FLUSHCUT") or l.startswith("RESULT"):
            res.write(l); res.flush()
        if l.startswith("RESULT"): break
p.stdin.write("QUIT\n"); p.stdin.flush(); p.wait()
open(out + "/DONE", "w").close()
