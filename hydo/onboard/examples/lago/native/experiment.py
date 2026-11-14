"""One native attempt at the double invoice (finding 8).

  python3 -I experiment.py <start HH:MM:SS UTC> <hold seconds of the first> <held customer>... -- <control customer>...

At the start time, each held customer's subscription is cancelled with the proxy armed: the
database's answer to the termination's UPDATE is held, so its transaction stays open. The
cancellations are 10 s apart, and each hold is 10 s shorter, so all are released at the same
moment. 60 s after the last, each control customer's subscription is cancelled without a hold.
COMPOSE_PROJECT names the Docker Compose project (hydo-native-lago-dbl, as double-invoice.sh).
"""
import datetime as dt
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT = ["docker", "compose", "-p", os.environ.get("COMPOSE_PROJECT", "hydo-native-lago-dbl"),
           "-f", HERE + "/compose.yaml", "-f", HERE + "/compose.hold.yaml"]


def log(msg):
    print(dt.datetime.now(dt.timezone.utc).strftime("%H:%M:%S"), msg, flush=True)


def terminate(sub):
    out = subprocess.run([sys.executable, "-I", HERE + "/lago.py", "terminate", sub],
                         capture_output=True, text=True)
    log(out.stdout.strip() + out.stderr.strip())


def arm(secs):
    subprocess.run(PROJECT + ["exec", "-T", "dbproxy", "ruby", "-e",
                              f'File.write("/tmp/armed", "{secs}")'], check=True)


if len(sys.argv) < 4 or "--" not in sys.argv:
    sys.exit(__doc__)
start, hold = sys.argv[1], int(sys.argv[2])
rest = sys.argv[3:]
sep = rest.index("--")
held, control = rest[:sep], rest[sep + 1:]
now = dt.datetime.now(dt.timezone.utc)
h, m, s = map(int, start.split(":"))
at = now.replace(hour=h, minute=m, second=s, microsecond=0)
if at < now:
    at += dt.timedelta(days=1)
log(f"waiting until {at:%H:%M:%S}")
time.sleep((at - now).total_seconds())
threads = []
for i, c in enumerate(held):
    arm(hold - 10 * i)
    time.sleep(1)
    log(f"cancel sub-{c} with a hold of {hold - 10 * i} s")
    t = threading.Thread(target=terminate, args=("sub-" + c,))
    t.start()
    threads.append(t)
    time.sleep(9)
time.sleep(60)
for c in control:
    log(f"cancel sub-{c} without a hold")
    terminate("sub-" + c)
for t in threads:
    t.join()
log("done")
