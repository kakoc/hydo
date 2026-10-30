import re, collections, statistics
hit = collections.Counter(); n = collections.Counter(); rep = collections.Counter(); secs = collections.defaultdict(list); ticks = collections.defaultdict(list)
import os
lines = [l for l in open("/var/tmp/m49-preempt/camp.txt") if not l.startswith("W=2 ")]
if os.path.exists("/var/tmp/m49-preempt/camp2.txt"):
    lines += [l for l in open("/var/tmp/m49-preempt/camp2.txt") if l.startswith("W=2 ")]
for l in lines:
    m = re.search(r"W=(\d+) arm=(\S+) seed=\d+ secs=([\d.]+).*lost=(-?\d+)", l)
    if not m: continue
    k = (int(m.group(1)), m.group(2)); n[k] += 1
    secs[k].append(float(m.group(3)))
    t = re.search(r"ticks=(\d+)", l); ticks[k].append(int(t.group(1)) if t else 0)
    if m.group(4) != "0":
        hit[k] += 1
        if "replay=IDENTICAL" in l: rep[k] += 1
print("%-4s %-4s %6s %6s %8s %8s %8s" % ("W", "arm", "runs", "hits", "replayed", "med s", "ticks"))
for k in sorted(n, key=lambda k: (-k[0], k[1])):
    print("%-4d %-4s %6d %6d %8d %8.1f %8d" % (k[0], k[1], n[k], hit[k], rep[k], statistics.median(secs[k]), statistics.median(ticks[k])))
