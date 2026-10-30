# hog.py <seconds> <target_avail_gb>: keep MemAvailable near the target with anonymous
# memory (touched), churn the page cache by reading big images, and spin 2 CPUs.
import mmap, os, sys, time, threading, glob
dur, target = float(sys.argv[1]), float(sys.argv[2]) * 2**30
end = time.time() + dur
def avail():
    for l in open("/proc/meminfo"):
        if l.startswith("MemAvailable:"): return int(l.split()[1]) * 1024
chunks = []
def spin():
    x = 0
    while time.time() < end: x += 1
for _ in range(2): threading.Thread(target=spin, daemon=True).start()
def churn():
    files = sorted(glob.glob("/home/user/hydo/build/*.img"), key=os.path.getsize)[-12:]
    while time.time() < end:
        for f in files:
            if time.time() >= end: break
            with open(f, "rb") as fh:
                while fh.read(1 << 22) and time.time() < end: pass
threading.Thread(target=churn, daemon=True).start()
lo = 10**9
while time.time() < end:
    a = avail(); lo = min(lo, a)
    if a > target + 2**30:
        m = mmap.mmap(-1, 256 << 20)
        for i in range(0, 256 << 20, 4096): m[i] = 1
        chunks.append(m)
    elif a < target - 2**30 and chunks:
        chunks.pop().close()
    time.sleep(0.5)
print("held_gb=%.1f min_avail_gb=%.1f" % (len(chunks) / 4, lo / 2**30))
