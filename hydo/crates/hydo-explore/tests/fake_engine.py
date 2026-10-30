#!/usr/bin/env python3
"""A scripted engine for tests/fake_engine.rs: it stands in for `hvx-run sudo env ... hvx_explore
serve ...` (it is put in $HYDO_HVX_RUN, so explore.py and hydo-explore start it the same way) and
speaks the serve protocol: READY, then a RESULT for every RUN, BRANCHPOINT/RESULT/BRANCHEND for
every BRANCH. Every answer is a pure function of the schedule text (never of the tag or of the
order of requests), so two explorers that ask for the same schedules must get the same answers.

The verdicts make the explorers' paths interesting: several failure signatures (some needing a
relaxed knob and a power cut, some a kill together with a network fault, some a timing change
together with a pause), error verdicts, failed always-properties (OK -> VIOLATION assertion),
eventually-properties with timings (the liveness table), error-log records (error kinds, new and
known), the cf= probe's FLUSH count, tick-jitter ranges, freeze traces and FROZE lines (the
freeze sweeps), and power cuts with a recovery console.
"""
import hashlib
import os
import re
import sys

M64 = (1 << 64) - 1
args = sys.argv[1:]
mode = "serve" if "serve" in args else "one"
consoles = args[-1] if mode == "serve" else args[-2]
os.makedirs(consoles, exist_ok=True)


def mix64(x):
    x = (x + 0x9E3779B97F4A7C15) & M64
    x = ((x ^ (x >> 30)) * 0xBF58476D1CE4E5B9) & M64
    x = ((x ^ (x >> 27)) * 0x94D049BB133111EB) & M64
    return x ^ (x >> 31)


def h(text, salt=""):
    return hashlib.sha256((salt + text).encode()).hexdigest()


def delays(host):
    """The tick delays a host part applies (tjx= or a seeded tj= over ticks 3..80)."""
    m = re.search(r"tjx=(\S+)", host)
    if m:
        return [tuple(int(v) for v in kd.split(":")) for kd in m.group(1).split(",")]
    m = re.search(r"\btj=([0-9a-f]+)", host)
    if not m:
        return None
    g = lambda k, d: int(re.search(k + r"=(\d+)", host).group(1)) if re.search(k + r"=(\d+)", host) else d
    seed, pm, mx, lo, hi = int(m.group(1), 16), g("tjpm", 10), g("tjmax", 500000), g("tjlo", 0), g("tjhi", 0)
    out = []
    for k in range(max(2, 4, lo), 81):
        if hi and k >= hi:
            break
        x = mix64((seed + k * 0x9E3779B97F4A7C15) & M64)
        if x % 1000 < pm:
            out.append((k, 1 + (x >> 16) % mx))
    return out


def tok(guest, k):
    for t in guest.split():
        if t.startswith(k + "="):
            return t[len(k) + 1:]
    return None


def fc_count(f, step):
    return (40 - step) * 3 + len(f) % 5


def answer(tag, sched):
    host, guest = sched.split("|", 1)
    lines = ["hydo-fake: run %s" % sched]
    rec_lines = None
    secs = "%.1f" % (1 + int(h(sched)[:4], 16) % 90 / 10.0)
    extras = ""
    d = delays(host)
    if d is not None:
        extras += " tjk=3-80 tjn=%d" % len(d)
    if "cfn" in host.split():
        sys.stderr.write("FLUSHCUT %s target=0 flushes=37\n" % tag)
        sys.stderr.flush()
    K, N, C, M, R = tok(guest, "K"), tok(guest, "N"), tok(guest, "C"), tok(guest, "M"), tok(guest, "R")
    P, T, O, W, F, Q, X = (tok(guest, k) for k in "PTOWFQX")
    H = [t[2:] for t in guest.split() if t.startswith("H=")]
    cf = re.search(r"\bcf=(\d+)", host)
    cut = C is not None or cf is not None
    relaxed = (M not in (None, "1")) or (R not in (None, "always")) or any(
        t[:2] in ("A=", "S=", "D=") and not t.endswith("=always") for t in guest.split())
    verdict = "HYDO-ORACLE: OK acked=30"
    if F is not None:
        m = re.match(r"([a-z0-9_]+)([<>])([a-z0-9_]+)@(\d+)#(\d+)(?::([^:]*))?:(\d+)", F)
        svc, dr, peer, st, n, match = m.group(1), m.group(2), m.group(3), int(m.group(4)), int(m.group(5)), m.group(6)
        if n == 0:
            total = 0
            for s in range(st, 40):
                total += fc_count(F.split("@")[0] + (match or ""), s) - fc_count(F.split("@")[0] + (match or ""), s + 1)
            cnt = fc_count(F.split("@")[0] + (match or ""), st)
            lines.append("hydo-fc: freeze never fired: %d of 0 matching events after step %d" % (cnt, st))
            for k in range(1, min(cnt, 50) + 1):
                lines.append("hydo-fc-trace: %s%d len=10 t=%d" % ("req" if dr == ">" else "resp", k, 1000 + st * 100 + k * 7))
                lines.append("hydo-fc-trace: %s%d len=12 t=%d" % ("resp" if dr == ">" else "req", k, 1003 + st * 100 + k * 7))
        else:
            word = "req" if dr == ">" else "resp"
            lines.append("hydo-fc: FROZE %s at %s #%d (%s%d, len 10)" % (svc, word, n, word, n + (3 if match else 0)))
            if st in (8, 9) and 20 <= n + (3 if match else 0) <= 27:
                verdict = "HYDO-ORACLE: VIOLATION frozen-%s order=%d" % (peer, st)
    elif "disk=lying" in host:
        verdict = "HYDO-ORACLE: VIOLATION lost acknowledged write order=%d" % int(C or 3)
    elif "disk=ioerr" in host and int(h(sched)[:2], 16) % 3 == 0:
        verdict = "ERROR harness: guest did not finish (I/O error)"
    elif cut and relaxed:
        verdict = "HYDO-ORACLE: VIOLATION lost acknowledged write order=%s" % (C or cf.group(1))
    elif K and N and K.split("@")[0] in N:
        verdict = "HYDO-ORACLE: VIOLATION duplicate in %s order=%s" % (K.split("@")[0], K.split("@")[1].split("+")[0])
    elif P and d and any(k % 5 == 1 for k, _ in d):
        verdict = "HYDO-ORACLE: VIOLATION race between orders order=%d" % len(d)
    elif X and Q:
        verdict = "HYDO-ORACLE: VIOLATION front lost an order order=%s" % X.split("@")[1].split("~")[0]
    elif W and re.search(r",p([2-9])", W) and int(h(W)[:2], 16) % 2 == 0:
        verdict = "HYDO-ORACLE: VIOLATION lost update key=k%d" % (int(h(W)[2:4], 16) % 5)
    elif H and any("drop" in x or "status=503" in x for x in H):
        verdict = "HYDO-ORACLE: VIOLATION charged twice order=%s" % H[0].split("@")[1].split("-")[0]
    elif O and "loss" in O and K:
        verdict = "HYDO-ORACLE: VIOLATION stale read order=%s" % O.split("@")[1].split("-")[0]
    # properties: a failed always-property for clock jumps backwards, an eventually with a timing
    ok_e = not (O and "delay" in O)
    el = 0.5 + int(h(sched, "e")[:3], 16) % 300 / 100.0
    lines.append('HYDO-ASSERT {"t":"eventually","id":"orders.drained","ok":%s,"details":{"elapsed_s":"%.2f","out":"pending=%d"}}'
                 % ("true" if ok_e else "false", el, int(h(sched, "o")[:2], 16)))
    if T and T.split(":")[1].startswith("-") and int(T.split(":")[1]) < -3600:
        lines.append('HYDO-ASSERT {"t":"always","id":"clock.monotonic","ok":false,"details":{}}')
    lines.append('HYDO-ASSERT {"t":"sometimes","id":"retry.taken","ok":%s}' % ("true" if K else "false"))
    # error logs: a kind in runs with a kill (new unless the list knows it), one before the workload
    el_recs = []
    if K:
        el_recs.append('HYDO-ERRLOG {"svc":"%s","src":"log","n":2,"at":"%s","tpl":"connection to %s lost <n>","ex":"connection to %s lost 3"}'
                       % (K.split("@")[0], K.split("@")[1].split("+")[0], K.split("@")[0], K.split("@")[0]))
    if P:
        el_recs.append('HYDO-ERRLOG {"svc":"app","src":"log","n":1,"at":"end","tpl":"slow request <n> ms","ex":"slow request 900 ms"}')
    el_recs.append('HYDO-ERRLOG {"svc":"app","src":"log","n":1,"at":"s","tpl":"config reloaded","ex":"config reloaded"}')
    lines += el_recs
    lines.append('HYDO-ERRLOG-SUM {"boot":"first","steps":30,"lines":%d,"kinds":%d,"shown":%d}' % (len(el_recs), len(el_recs), len(el_recs)))
    if cut:
        rec_lines = ["hydo-fake: recovery of %s" % tag, 'HYDO-ERRLOG-SUM {"boot":"recovery","steps":30,"lines":0,"kinds":0,"shown":0}',
                     verdict]
    else:
        lines.append(verdict)
    with open(os.path.join(consoles, tag + ".log"), "a") as f:
        f.write("\r\n".join(lines) + "\r\n")
    if rec_lines:
        with open(os.path.join(consoles, tag + ".recovery.log"), "a") as f:
            f.write("\n".join(rec_lines) + "\n")
    v = verdict
    return "RESULT %s fp=%s wfp=%s secs=%s%s verdict=%s" % (tag, h(sched, "fp")[:16], h(sched, "w")[:16], secs, extras, v)


def main():
    if mode == "one":
        print(answer("one", args[-1]))
        return
    print("READY boot_secs=2.5", flush=True)
    for line in sys.stdin:
        line = line.rstrip("\n")
        if line == "QUIT":
            return
        if line.startswith("RUN "):
            _, tag, sched = line.split(" ", 2)
            print(answer(tag, sched), flush=True)
        elif line.startswith("BRANCH "):
            _, tag, n, pm, mx, mark, sched = line.split(" ", 6)
            host, guest = sched.split("|", 1)
            if int(h(sched, "bp")[:2], 16) % 4 == 0:
                print("BRANCHPOINT %s none" % tag)
            else:
                lo = 5 + int(h(sched, "lo")[:2], 16) % 20
                print("BRANCHPOINT %s tjlo=%d held=%d" % (tag, lo, int(h(sched, "held")[:1], 16)))
                print(answer(tag + "-base", "%s rb=16 |%s" % (host.rstrip(), guest)).replace(
                    "RESULT %s-base" % tag, "RESULT %s-base" % tag))
                for v in range(1, int(n) + 1):
                    st = "%s tj=%x tjpm=%s tjmax=%s tjlo=%d tjhi=0 rb=16 |%s" % (host.rstrip(), v, pm, mx, lo, guest)
                    print(answer("%s-v%d" % (tag, v), st))
            print("BRANCHEND %s" % tag, flush=True)
        else:
            print("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request", flush=True)


main()
