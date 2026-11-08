"""logs.py <task out dir> <mode> <dest dir>: write each run's test log of one kind of run to
<dest dir>/<i>.log and print each log's sha256, to look at how runs differ (diff the files).
<mode> is native, native-load, fresh, fresh-load or snap (the log inside each exec answer)."""
import hashlib
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from collect import snap_log  # noqa: E402

src, mode, dest = sys.argv[1:4]
os.makedirs(dest, exist_ok=True)
md = os.path.join(src, mode)
for name in sorted(os.listdir(md), key=lambda n: (len(n), n)):
    i, ext = os.path.splitext(name)
    if not i.isdigit():
        continue
    p = os.path.join(md, name)
    log = snap_log(p) if ext == ".json" else open(p, errors="replace").read()
    if log is None:
        print(i, "no log")
        continue
    open(os.path.join(dest, f"{i}.log"), "w").write(log)
    print(i, hashlib.sha256(log.encode()).hexdigest()[:16])
