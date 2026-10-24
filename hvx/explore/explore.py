#!/usr/bin/env python3
"""explore.py -- the automatic fault explorer for the four-service stack (Milestone 38).

Runs on the bare-metal test machine. Generates a fault schedule from every seed (a pure function of
the seed: splitmix64), runs the schedules on a pool of `hvx_explore serve` workers (each
boots + provisions the guest ONCE and then starts every schedule from a snapshot), keeps
every run whose invariant fails, re-runs each failure to confirm it reproduces bit-for-bit,
shrinks it to a minimal failing schedule, and writes a report.

  python3 explore.py --seeds 1-200 --workers 6 --out /var/tmp/explore/run1
  python3 explore.py --schedule "disk=cut dseed=0 | run C=20 M=2" --out ...   # one schedule

Schedule format (see hvx_explore.c / stack-explore.sh):
  disk=<none|ioerr:P|cut|lying:P> dseed=<hex> [timing] | run [K=svc@at+down] [N=svc@from-to:netem,args]
                                                   [C=at] [M=0|1|2] [R=always|everysec|no]
                                                   [P=svc@at+n | P=svc@at~off:dur] [T=at:delta]
                                                   [O=svc@from-to:req|resp:netem,args]
  new fault kinds (only with --new-faults, M46): P pauses a service (SIGSTOP/SIGCONT; svc may
  be "front"), for n orders or, timed, after off ms for dur ms of guest time; T steps the
  guest's realtime clock by delta seconds before order at; O is one-way netem (req = packets
  to the service's port, resp = packets from it). Drawn from a separate random stream, so
  every other field equals the schedule of the same seed without --new-faults.
  more faults (only with --more-faults, M62): X=svc@at~ms SIGKILLs svc (may be "front") ms of
  guest time after order at starts, so it can land mid-request; host token cf=<k> cuts the
  data disk's power at the k-th FLUSH of the workload (needs disk=cut, no C=; k from
  --cf-range, by default 1 .. the FLUSH count of a fault-free workload, measured with cfn).
  Drawn from a separate random stream, so every other field equals the schedule without it,
  except that cf= makes the disk "cut" and replaces a C= cut.
  timing (only with --timing): tj=<hex> tjpm=<n> tjmax=<n> [tjlo=<k> tjhi=<k>]  delays a
  seeded share of the PMU-clock ticks, so threads interleave differently; the shrinker turns
  it into an explicit list tjx=<tick:delay,...> and removes entries.
  freeze sweep (--sweep freeze, M76): traces the packet counts of each (pair, direction, match)
  stream per step with F=...#0 runs, then runs one F=<svc><dir><peer>@<step>#<n>[:<match>]:<dur>
  schedule per packet (options --fc-pairs/--fc-dirs/--fc-steps/--fc-match/--fc-durs,
  --sweep-every, --sweep-with, --sweep-dry); every failure is re-run twice.
  vCPU turns (only with --vcpus N > 1 and --turns, SMP Stage 3, TURNS.md): host tokens
  q=<branches> [qs=<hex>] set the length of the vCPUs' turns during the workload (fixed, or
  seeded lengths in [1, q]); half of the schedules get them, from a separate random stream.
Classes of findings:
  durable-defaults  every store configured durably, the disk honest -- the interesting ones
  unsafe-knob       needs a documented-unsafe setting (MySQL trx_commit != 1, Redis
                    appendfsync != always) AND a power cut or a lying disk (the settings only
                    trade durability across a power loss; default since M56, --knob-rule any
                    restores the old rule "any relaxed setting"): expected.
                    Caveat: MySQL trx_commit=0 can also lose ~1 s of commits on a mysqld kill;
                    such a run is now durable-defaults, and the bench's control check
                    (hvx/bench/run.py) still rejects it because the control image fails too.
  lying-disk        the disk acknowledged writes it never stored: expected
  io-errors         the disk returned I/O errors (also on reads of program files)
"""
import argparse, json, os, queue, re, subprocess, sys, threading, time
import hvm_backend

M64 = (1 << 64) - 1
NORDERS = 30
HERE = os.path.dirname(os.path.abspath(__file__))
HVX = os.path.dirname(HERE)
BUILD = os.environ.get("HYDO_BUILD") or os.path.expanduser("~/hydo/build")   # the release's `hydo` sets it
HVX_RUN = os.environ.get("HYDO_HVX_RUN") or os.path.expanduser("~/bin/hvx-run")  # shared module lock (flock -s)
SNAP_DIR = os.environ.get("HYDO_SNAP_DIR") or "/var/tmp/hvx-snap"
IMAGE = "explore-disk.img"
EXTRA_ENV = []          # extra env for the workers, e.g. coverage: ["HVX_EXPLORE_PT_ELF=..."] (guided.py)
TIMING = False          # --timing: add seeded tick-timing jitter to generated schedules
TJ_MAX = 500000         # max tick delay in guest branches (the PMU period is 2^20)
TJ_PM = [50, 100, 200]  # --tj-pm: delayed ticks per 1000 (a workload is often only ~60 ticks)
TJ_EXPLICIT_MAX = 32    # HVX_TJ_MAX_EXPLICIT
BRANCH_BUDGET = 16      # hvx_explore.c: the run budget of a branch (rb=16 in its variants)
NEW_FAULTS = False      # --new-faults: add pause / clock-jump / one-way-network faults (M46)
NF_SALT = 0x4E46A17C0FFEE046
PREEMPT = False         # --preempt (M49): shorter tick period pp= or inserted ticks xt= on half the schedules
PCT_POINTS = [10, 40]   # --preempt on --backend hvm (M57): forced preemption points per workload ...
PCT_EXITS = [3000, 10000]   # ... spread over this many exits from the workload's start (--pct-exits;
                            # a run's RESULT says how long its workload was: pctw=)
PP_VALUES = [1 << 16, 1 << 14]
XT_RATES = [4, 16]
BUGGIFY = False         # --buggify (M56): every schedule carries a seed for the front door's fault points (B=)
BG_SALT = 0xB0661F7ED5EED0B6
CONCURRENCY = False     # --concurrency: several clients place orders at the same time (Q= token)
CC_SALT = 0x51C0C0C0C0C0C051
MORE_FAULTS = False     # --more-faults (M62): the timed kill X= and the power cut at the k-th FLUSH cf=
MF_SALT = 0x3F7A11C0FFEE3F7A
MF_PX = {"random": 30, "swarm": 50, "swarm-pos": 50, "process": 40}  # % of schedules drawing X=
MF_PCF = {"random": 40, "swarm": 50, "swarm-pos": 50, "process": 60} # % drawing cf= (used only without K=)
X_OFF_MAX = 250         # timed kill: ms of guest time after the order starts (30 orders ~ 0.5 s)
CF_RANGE = None         # (lo, hi) FLUSH indices for cf=: --cf-range lo-hi, or measured (--cf-range auto)
                        # as 1 .. the FLUSH count of one fault-free workload (cfn) on the image
BACKEND = "hvx"         # --backend hvm: the Mac VMM (hvm_backend.py); HVM_KERNEL = its --hvm-kernel
HVM_KERNEL = None
CPU = "v2"              # guest CPU level: the manifest's guest.cpu, or --cpu; "v3" adds "--cpu v3"
                        # (guest AVX/AVX2) to hvx_explore, "v2" adds nothing
RAM_MIB = 1024          # guest RAM: the manifest's guest.ram_mib, or --ram-mib; the engines get
                        # "--ram-mib N" only when it is not 1024, so default commands are unchanged
RAM_MIB_BOUNDS = {"hvx": (256, 3072), "hvm": (256, 4096)}  # hvx_explore.c / hvmx (serve.c) check the same
VCPUS = 1               # --vcpus N (SMP Stage 3): the engine gets "--vcpus N" only when N > 1
TURNS = False           # --turns: half of the schedules get vCPU turn tokens q= [qs=] (needs VCPUS > 1)
TURNS_SALT = 0x7A4E5C0FFEE7A4E5
BURSTS = False          # --bursts: with --turns, half of the schedules also get seeded bursts of short turns qb= qbs=
BURSTS_SALT = 0xB0B5EED5C0FFEE77
Q_DEFAULT = 1 << 20     # hvx.c HVX_SCHED_QUANTUM: the module's turn length (guest conditional branches)
CLOCK_DELTAS = [-30 * 86400, -86400, -3600, -300, -30, 30, 300, 3600, 86400, 365 * 86400]
FREEZE_CALL = False     # --freeze-call (M70): freeze a service at the n-th packet of one of its conversations (F=)
FC_SALT = 0xF2EE2EC0FFEEF2EE
FC_P = 50               # % of schedules that draw F=
FC_NMAX = 64            # --fc-nmax: n is drawn log-uniformly from 1 .. FC_NMAX
FC_DURS = [100, 300, 1000, 3000, 10000, 30000, 60000, 90000, 150000]   # freeze length, ms of guest time
FC_PAIRS = []           # (service, peer) pairs that talk; manifest_space.py sets them (faults: [.., freeze])
FC_MATCH = []           # optional (payload string, direction) pairs (manifest freeze_match:): the freeze may
                        # count only packets that contain the string, so a known event has a small n; a
                        # direction "<" / ">" (replies: / requests:) is the one the string occurs in
FC_MATCH_NMAX = 32      # with a match, n is drawn 1 .. this (matched events are far fewer than all packets)
COMPOSE = True          # the test composer (hydo_composer.py): a plan with test commands gets W= on every
                        # schedule (--no-compose: the guest's default composition); other images never do
CW_SALT = 0xC0390538C0FFEE57


class SplitMix:
    def __init__(self, seed):
        self.s = seed & M64

    def next(self):
        self.s = (self.s + 0x9E3779B97F4A7C15) & M64
        z = self.s
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & M64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & M64
        return z ^ (z >> 31)

    def below(self, n):
        return self.next() % n

    def rng(self, lo, hi):          # inclusive
        return lo + self.below(hi - lo + 1)

    def pick(self, xs):
        return xs[self.below(len(xs))]


SVCS = ["mysql", "kafka", "redis"]
# The fault space (manifest_space.py rebinds these for an onboarded project; --manifest).
NET_SVCS = SVCS                         # services network faults (N=, O=) pick from
PAUSE_SVCS = SVCS + ["front"]           # services pause faults (P=) pick from
KNOBS = [("M", 2), ("M", 0), ("R", "everysec"), ("R", "no")]   # a relaxed-knob draw picks one
KNOB_TOKENS = ["M", "R"]
SPACE = None
KNOB_RULE = "cut"       # --knob-rule: "cut" (M56 default: a relaxed knob is unsafe only with a
                        # power cut or a lying disk) or "any" (the pre-M56 rule)


def generate(seed):
    """seed -> schedule dict. Pure function of the seed."""
    r = SplitMix(seed)
    s = {"disk": "none", "dseed": r.next()}
    if r.below(100) < 40:
        at = r.rng(0, NORDERS - 4)
        s["K"] = (r.pick(SVCS), at, r.rng(1, min(4, NORDERS - at)))
    if r.below(100) < 40:
        a = r.rng(0, NORDERS - 2)
        b = r.rng(a + 1, min(NORDERS, a + 15))
        kinds = []
        for _ in range(r.rng(1, 2)):
            k = r.pick(["delay", "loss", "duplicate", "reorder", "corrupt"])
            if k not in [x[0] for x in kinds]:
                kinds.append((k, {"delay": r.rng(10, 300), "loss": r.rng(5, 60), "duplicate": r.rng(5, 50),
                                  "reorder": r.rng(10, 50), "corrupt": r.rng(1, 10)}[k]))
        s["N"] = (r.pick(NET_SVCS), a, b, kinds)
    x = r.below(100)
    if x < 45:
        s["disk"] = "none"
    elif x < 60:
        s["disk"] = "ioerr:%d" % r.rng(1, 3)
    elif x < 85:
        s["disk"] = "cut"
    else:
        s["disk"] = "lying:%d" % r.rng(2, 12)
    if s["disk"] in ("cut",) or s["disk"].startswith("lying"):
        s["C"] = r.rng(1, NORDERS)
    if r.below(100) < 20:
        k = r.pick(KNOBS)
        s[k[0]] = k[1]
    return s


def add_timing(s, seed):
    """--timing: give half of the schedules a seeded tick jitter. A separate random stream,
    so every other field matches the same seed without --timing (any generator, or a sweep)."""
    if TIMING:
        t = SplitMix(seed ^ 0x7417D1CE7417D1CE)
        if t.below(100) < 50:
            s["tj"] = ("seed", t.next(), t.pick(TJ_PM), TJ_MAX, 0, 0)
    return s


def add_preempt(s, seed):
    """--preempt (M49): half of the schedules get finer preemption during the workload --
    a shorter PMU tick period (pp=, guest time per tick unchanged, so the timer and a thread
    switch come more often) or seeded extra ticks between the grid ticks (xt=). A separate
    random stream: every other field matches the same seed without --preempt."""
    if PREEMPT:
        t = SplitMix(seed ^ 0x9EE397C0FFEE9EE3)
        if t.below(100) < 50:
            if BACKEND == "hvm":                # the Mac VMM: forced preemption points at seeded exits (M57)
                s["pct"] = (t.next(), t.pick(PCT_POINTS), t.pick(PCT_EXITS))
            elif t.below(2):
                s["pp"] = t.pick(PP_VALUES)
            else:
                s["xt"] = (t.next(), t.pick(XT_RATES), 500)
    return s


def add_turns(s, seed):
    """--turns (SMP Stage 3): half of the schedules get a turn length for the vCPUs during the
    workload, mixed as in rr's chaos mode: most long (2^19 .. 2^21 branches, around the
    module's 2^20), about 10% at most 100, about 10% at most 10,000; half of them with seeded
    lengths in [1, q] (qs=) instead of fixed ones. The mix is per schedule: the module takes
    one length and one seed for all turns (TURNS.md). A separate random stream, so every
    other field matches the same seed without --turns."""
    if TURNS:
        t = SplitMix(seed ^ TURNS_SALT)
        on, x, seeded, qs = t.below(100) < 50, t.below(100), t.below(2), t.next() or 1
        q = t.rng(1, 100) if x < 10 else t.rng(101, 10000) if x < 20 else t.rng(Q_DEFAULT // 2, 2 * Q_DEFAULT)
        if on:
            s["q"] = (q, qs if seeded else 0)
    if BURSTS:                                       # FASTTURN.md: own stream, so the rest is unchanged
        t = SplitMix(seed ^ BURSTS_SALT)
        on, blen, bq, pm, bs = t.below(100) < 50, t.rng(2000, 200000), t.rng(20, 1000), \
            t.pick([50, 100, 250, 500, 1000]), t.next() or 1
        if on:
            s["qb"] = (blen, bq, pm, bs)
    return s


def render_turns(s):
    out = ""
    if "q" in s:
        q, qs = s["q"]
        out = (" q=%d" % q if q else "") + (" qs=%x" % qs if qs else "")
    if "qb" in s:
        out += " qb=%d:%d:%d qbs=%x" % s["qb"]
    return out


def render_preempt(s):
    out = " pp=%d" % s["pp"] if "pp" in s else ""
    if "xt" in s:
        out += " xt=%x xtr=%d xtpm=%d" % s["xt"]
    if "pct" in s:
        out += " pct=%x pctn=%d pctk=%d" % s["pct"]
    return out


def add_new_faults(s, seed, swarm=False):
    """--new-faults (M46): pause, clock jump and one-way network loss, each on with
    probability 35% (random) or 50% (swarm: per-run category switch). A separate random
    stream: every other field of s stays what it was for this seed."""
    if not NEW_FAULTS:
        return s
    t = SplitMix(seed ^ NF_SALT)
    p_on = 50 if swarm else 35
    if t.below(100) < p_on:                         # pause
        svc = t.pick(PAUSE_SVCS)
        at = t.rng(0, NORDERS - 2)
        if t.below(2):
            s["P"] = (svc, at, "+", t.rng(1, 3))
        else:
            # 30 orders take ~0.5 s of guest time: a 0-500 ms offset lands inside the workload
            s["P"] = (svc, at, "~", t.rng(0, 500), t.rng(500, 30000))
    if t.below(100) < p_on:                         # clock jump (realtime only)
        s["T"] = (t.rng(0, NORDERS - 1), t.pick(CLOCK_DELTAS))
    if t.below(100) < p_on:                         # one-way network fault
        svc = t.pick(NET_SVCS)
        a = t.rng(0, NORDERS - 2)
        b = t.rng(a + 1, min(NORDERS, a + 10))
        d = t.pick(["req", "resp"])
        kinds = [("loss", t.rng(30, 100))] if t.below(2) else [("delay", t.rng(100, 3000))]
        if "N" not in s:                            # one netem root qdisc at a time
            s["O"] = (svc, a, b, d, kinds)
    return s


def fc_n(t, nmax=None):
    """n of an F= token: log-uniform over 1 .. nmax (default FC_NMAX). The first events after a
    step begins are as likely as a whole later range."""
    import math
    nmax = nmax or FC_NMAX
    e = t.below(1000) / 1000.0 * math.log(nmax)
    return max(1, min(nmax, int(math.exp(e))))


def add_freeze_call(s, seed):
    """--freeze-call (M70): F=<svc><dir><peer>@<step>#<n>:<dur> on FC_P % of schedules -- freeze
    <svc> exactly at the n-th packet it sends to (">") or receives from ("<") <peer> after step
    <step> starts, for <dur> ms (hydo-init-freeze.sh). A separate random stream, and every value is
    drawn whether or not it is used, so every other field stays what it was for this seed."""
    if not FREEZE_CALL or not FC_PAIRS:
        return s
    t = SplitMix(seed ^ FC_SALT)
    # draw every value unconditionally and in a fixed order, so a manifest with a freeze_match
    # vocabulary and one without draw from the same stream for every field but the match.
    on = t.below(100) < FC_P
    pair, d, step, dur = t.pick(FC_PAIRS), t.pick([">", "<"]), t.rng(0, NORDERS - 1), t.pick(FC_DURS)
    want_match, mi, n_all, n_match = t.below(2), t.below(max(1, len(FC_MATCH))), fc_n(t), fc_n(t, FC_MATCH_NMAX)
    use_match = bool(FC_MATCH) and want_match        # half the time, when the manifest gives a vocabulary
    if on:
        if use_match:
            match, md = FC_MATCH[mi]
            s["F"] = (pair[0], md or d, pair[1], step, n_match, match, dur)   # the string's own direction
        else:
            s["F"] = (pair[0], d, pair[1], step, n_all, None, dur)
    return s


def render_fc(f):
    svc, d, peer, step, n, match, dur = f
    return "F=%s%s%s@%d#%d%s:%d" % (svc, d, peer, step, n, ":" + match if match else "", dur)  # noqa


FC_RE = re.compile(r"^([a-z0-9_]+)([<>])([a-z0-9_]+)@(\d+)(?:#(\d+))?(?::([^:]*))?:(\d+)$")


def parse_fc(v):
    m = FC_RE.match(v)
    if not m:
        raise ValueError("bad F= token %r" % v)
    return (m.group(1), m.group(2), m.group(3), int(m.group(4)), int(m.group(5) or 1), m.group(6) or None, int(m.group(7)))


def _hc():
    d = os.path.join(HVX, "onboard")
    if d not in sys.path:
        sys.path.insert(0, d)
    import hydo_composer
    return hydo_composer


def add_compose(s, seed):
    """The test composer's token W=<seed>,p<concurrency>,q<per round>,r<round ms>: which
    commands run, how many at once and how often (hydo_composer.draw), from a separate random
    stream, so every other field equals the schedule of the same seed without it. Only for a
    plan whose workload the composer runs (SPACE.composed()); every other schedule is unchanged."""
    if COMPOSE and SPACE is not None and SPACE.composed():
        s["W"] = _hc().draw(SplitMix(seed ^ CW_SALT).next, SPACE.compose)
    return s


def add_buggify(s, seed):
    """--buggify (M56): every schedule gets a BUGGIFY seed (guest token B=), from a separate
    random stream, so every other field matches the same seed without --buggify. The front
    door's fault points (hvx/stack/buggify.go) then misbehave at seeded places."""
    if BUGGIFY:
        s["B"] = "%x" % SplitMix(seed ^ BG_SALT).next()
    return s


def add_concurrency(s, seed):
    """--concurrency: Q=<n> runs the client's order placement with n concurrent requests;
    Q=<n>d also sends every order twice at the same time (a double submit). 60% of schedules
    get it, from a separate random stream, so every other field stays what it was."""
    if CONCURRENCY:
        t = SplitMix(seed ^ CC_SALT)
        if t.below(100) < 60:
            s["Q"] = (t.pick([2, 4, 8]), t.below(100) < 30)
    return s


def add_more_faults(s, seed, strategy="random"):
    """--more-faults (M62): a timed SIGKILL X=<svc>@<order>~<ms> (can land mid-request, M50) and a
    power cut at the k-th disk FLUSH cf=<k> (host token, M56/M58), on MF_PX / MF_PCF[strategy] % of
    schedules. A separate random stream, and every value is drawn whether or not it is used, so
    every other field stays what it was for this seed. X never targets the service K= kills.
    cf= needs the volatile write cache (disk=cut) and no C=: it turns disk=none into cut and
    replaces a cut before an order; it is not drawn with a lying or failing disk, nor with K=."""
    if not MORE_FAULTS:
        return s
    t = SplitMix(seed ^ MF_SALT)
    x_on, x_svc, x_at, x_off = t.below(100) < MF_PX.get(strategy, 30), t.next(), t.rng(0, NORDERS - 1), t.rng(0, X_OFF_MAX)
    c_on, c_k = t.below(100) < MF_PCF.get(strategy, 40), t.next()
    # After a cf= cut the data disk stays dead while the guest runs on, so a store restarted
    # after it (K=, or X= on a store, restarted at the end) waits on the dead disk until the
    # 900 s harness timeout: no verdict. So cf= is not drawn with K=, and with cf= an X= kills
    # only the order service (stateless; its restart does not touch the data disk).
    c_ok = bool(c_on and CF_RANGE and "K" not in s and s["disk"] in ("none", "cut") and
                (SPACE is None or "cut" in SPACE.faults))
    if x_on and SPACE is None:                      # X= is a stack-explore.sh token
        svcs = [v for v in PAUSE_SVCS if ("K" not in s or v != s["K"][0]) and (not c_ok or v == "front")]
        if svcs:
            s["X"] = (svcs[x_svc % len(svcs)], x_at, x_off)
    if c_ok:
        lo, hi = CF_RANGE
        s["disk"] = "cut"
        s.pop("C", None)
        s["cf"] = lo + c_k % (hi - lo + 1)
    return s


def measure_cf_range(pool, outdir, tag="cfn-probe"):
    """The cf= range for this image: 1 .. the FLUSHes of one fault-free workload. hvx_explore
    prints "FLUSHCUT <tag> target=0 flushes=<n>" on its stderr (worker-<i>.err) for a cfn run."""
    pool.run_all([(tag, "disk=cut dseed=0 cfn | run")])
    for i in range(len(pool.workers)):
        f = os.path.join(outdir, "worker-%d.err" % i)
        for line in open(f, errors="replace") if os.path.exists(f) else []:
            m = re.match(r"FLUSHCUT %s target=\d+ flushes=(\d+)" % re.escape(tag), line)
            if m and int(m.group(1)) > 1:
                return (1, int(m.group(1)))
    raise SystemExit("explore.py: --cf-range auto: no FLUSH count from the cfn probe (see worker-*.err)")


def durable_only(s):
    """--durable-only (M46): every store durable and the disk honest -- a lying disk becomes
    a plain power cut at the same point, I/O errors and relaxed knobs are dropped -- so every
    failure is a durable-defaults one."""
    if s["disk"].startswith("lying"):
        s["disk"] = "cut"
    elif s["disk"].startswith("ioerr"):
        s["disk"] = "none"
    for k in KNOB_TOKENS:
        s.pop(k, None)
    return s


def mix64(x):
    x = (x + 0x9E3779B97F4A7C15) & M64
    x = ((x ^ (x >> 30)) * 0xBF58476D1CE4E5B9) & M64
    x = ((x ^ (x >> 27)) * 0x94D049BB133111EB) & M64
    return x ^ (x >> 31)


def tj_delays(tj, k_first, k_last):
    """The (tick, delay) pairs a seeded jitter applies to ticks k_first..k_last -- the same
    formula as hvx_tj_delay() in hvx.c."""
    _, seed, pm, mx, lo, hi = tj
    out = []
    for k in range(max(2, k_first, lo), k_last + 1):
        if hi and k >= hi:
            break
        h = mix64((seed + k * 0x9E3779B97F4A7C15) & M64)
        if h % 1000 < pm:
            out.append((k, 1 + (h >> 16) % mx))
    return out


def render_tj(tj):
    if tj[0] == "x":
        return " tjx=" + ",".join("%d:%d" % kd for kd in tj[1])
    _, seed, pm, mx, lo, hi = tj
    t = " tj=%x tjpm=%d tjmax=%d" % (seed, pm, mx)
    if lo or hi:
        t += " tjlo=%d tjhi=%d" % (lo, hi)
    return t


# ---------------------------------------------------------------- swarm testing (Milestone 42)
# Each fault CATEGORY is switched on or off per run (a seeded choice), and the schedule is
# drawn only from the categories that are on. Faults of different kinds then stop masking
# each other, and a run with only one kind of fault is common instead of rare.
CATEGORIES = ["kill", "net", "cut", "disk", "knob"]
SWARM_SALT = 0x5A4A5A4A5A4A5A4A          # keeps swarm seed k different from random seed k


def swarm_categories(seed):
    r = SplitMix(seed ^ SWARM_SALT)
    on = [c for c in CATEGORIES if r.below(2)]
    if not on:
        on = [r.pick(CATEGORIES)]
    return on, r


def generate_swarm(seed):
    """seed -> schedule dict, swarm style. Pure function of the seed. Same schedule format and
    parameter ranges as generate(); only the per-run choice of fault kinds differs."""
    on, r = swarm_categories(seed)
    s = {"disk": "none", "dseed": r.next()}
    if "kill" in on:
        at = r.rng(0, NORDERS - 4)
        s["K"] = (r.pick(SVCS), at, r.rng(1, min(4, NORDERS - at)))
    if "net" in on:
        a = r.rng(0, NORDERS - 2)
        b = r.rng(a + 1, min(NORDERS, a + 15))
        kinds = []
        for _ in range(r.rng(1, 2)):
            k = r.pick(["delay", "loss", "duplicate", "reorder", "corrupt"])
            if k not in [x[0] for x in kinds]:
                kinds.append((k, {"delay": r.rng(10, 300), "loss": r.rng(5, 60), "duplicate": r.rng(5, 50),
                                  "reorder": r.rng(10, 50), "corrupt": r.rng(1, 10)}[k]))
        s["N"] = (r.pick(NET_SVCS), a, b, kinds)
    if "disk" in on:                    # a misbehaving disk (wins over a plain cut below)
        s["disk"] = r.pick(["ioerr:%d" % r.rng(1, 3), "lying:%d" % r.rng(2, 12)])
        if s["disk"].startswith("lying"):
            s["C"] = r.rng(1, NORDERS)
    if "cut" in on and s["disk"] == "none":
        s["disk"] = "cut"
        s["C"] = r.rng(1, NORDERS)
    if "knob" in on:
        k = r.pick(KNOBS)
        s[k[0]] = k[1]
    return s


# ---------------------------------------------------------------- fault position (M56)
# hvx/explore/RESEARCH-search.md: real bugs need few faults but at the right moment, and our
# generators place every window uniformly, so the end of the workload is rarely faulted (d7
# of hvx/bench needs Kafka down until the last order: ~1 in 2,800 random runs). Two generators
# with their own random streams; the old generators and their strings are unchanged.
POS_SALT = 0x9051710E9051710E
PROC_SALT = 0x7E5751A47E5751A4
POSITIONS = ["early", "middle", "late", "end"]
POS_CATEGORIES = ["kill", "pause", "net", "oneway", "clock", "cut", "disk", "knob"]


def pos_start(r, pos):
    """A start order for a fault in region pos; "end" starts in the last 4 orders."""
    lo, hi = {"early": (0, NORDERS // 3 - 1), "middle": (NORDERS // 3, 2 * NORDERS // 3 - 1),
              "late": (2 * NORDERS // 3, NORDERS - 5), "end": (NORDERS - 4, NORDERS - 1)}[pos]
    return r.rng(lo, hi)


def pos_window(r, pos, maxlen):
    """(start, end) of a window fault; an "end" window always runs to the last order."""
    a = pos_start(r, pos)
    b = NORDERS if pos == "end" else r.rng(a + 1, min(NORDERS, a + maxlen))
    return a, b


def generate_swarm_pos(seed):
    """--strategy swarm-pos: swarm (per-run category switch, M42) over every fault kind incl.
    the M46 ones, plus a per-run POSITION per category (early / middle / late / end-reaching)
    instead of uniform placement. Pure function of the seed."""
    r = SplitMix(seed ^ POS_SALT)
    on = [c for c in POS_CATEGORIES if r.below(2)] or [r.pick(POS_CATEGORIES)]
    pos = {c: r.pick(POSITIONS) for c in POS_CATEGORIES}
    s = {"disk": "none", "dseed": r.next()}
    if "kill" in on:
        at = pos_start(r, pos["kill"])
        s["K"] = (r.pick(SVCS), at, NORDERS - at if pos["kill"] == "end" else r.rng(1, min(4, NORDERS - at)))
    if "pause" in on:
        at = pos_start(r, pos["pause"])
        if pos["pause"] == "end" or r.below(2):
            s["P"] = (r.pick(PAUSE_SVCS), at, "+", NORDERS - at if pos["pause"] == "end" else r.rng(1, 3))
        else:
            s["P"] = (r.pick(PAUSE_SVCS), at, "~", r.rng(0, 500), r.rng(500, 30000))
    if "net" in on:
        a, b = pos_window(r, pos["net"], 15)
        kinds = []
        for _ in range(r.rng(1, 2)):
            k = r.pick(["delay", "loss", "duplicate", "reorder", "corrupt"])
            if k not in [x[0] for x in kinds]:
                kinds.append((k, {"delay": r.rng(10, 300), "loss": r.rng(5, 60), "duplicate": r.rng(5, 50),
                                  "reorder": r.rng(10, 50), "corrupt": r.rng(1, 10)}[k]))
        s["N"] = (r.pick(NET_SVCS), a, b, kinds)
    if "oneway" in on and "N" not in s:          # one netem root qdisc at a time
        a, b = pos_window(r, pos["oneway"], 10)
        kinds = [("loss", r.rng(30, 100))] if r.below(2) else [("delay", r.rng(100, 3000))]
        s["O"] = (r.pick(NET_SVCS), a, b, r.pick(["req", "resp"]), kinds)
    if "clock" in on:
        s["T"] = (pos_start(r, pos["clock"]), r.pick(CLOCK_DELTAS))
    if "disk" in on:
        s["disk"] = r.pick(["ioerr:%d" % r.rng(1, 3), "lying:%d" % r.rng(2, 12)])
        if s["disk"].startswith("lying"):
            s["C"] = pos_start(r, pos["disk"]) + 1
    if "cut" in on and s["disk"] == "none":
        s["disk"] = "cut"
        s["C"] = pos_start(r, pos["cut"]) + 1
    if "knob" in on:
        k = r.pick(KNOBS)
        s[k[0]] = k[1]
    return s


def generate_process(seed):
    """--strategy process: a Jepsen-style fault process -- 2-4 faults of different kinds, one
    after another over the whole workload in staggered, non-overlapping windows (the last one
    reaches the end half of the time); the guest heals everything before the oracle runs.
    The schedule format has one token per kind, so the process is one fault of each kind, not
    a kind repeating. Disk honest, settings durable, a power cut after the faults 1 time in 5."""
    r = SplitMix(seed ^ PROC_SALT)
    n = r.rng(2, 4)
    order = []
    while len(order) < n:
        k = r.pick(["K", "P", "net", "T"])
        if k not in order:
            order.append(k)
    s = {"disk": "none", "dseed": r.next()}
    w = NORDERS // n
    for i, k in enumerate(order):
        lo, hi = i * w, (NORDERS if i == n - 1 else (i + 1) * w)
        a = r.rng(lo, max(lo, hi - 2))
        b = NORDERS if i == n - 1 and r.below(2) else r.rng(a + 1, hi)
        if k == "K":
            s["K"] = (r.pick(SVCS), a, b - a)
        elif k == "P":
            s["P"] = (r.pick(PAUSE_SVCS), a, "+", b - a)
        elif k == "T":
            s["T"] = (a, r.pick(CLOCK_DELTAS))
        elif r.below(2):
            s["N"] = (r.pick(NET_SVCS), a, b, [("loss", r.rng(20, 100))] if r.below(2) else [("delay", r.rng(100, 1000))])
        else:
            s["O"] = (r.pick(NET_SVCS), a, b, r.pick(["req", "resp"]),
                      [("loss", r.rng(30, 100))] if r.below(2) else [("delay", r.rng(100, 3000))])
    if r.below(5) == 0:
        s["disk"] = "cut"
        s["C"] = NORDERS
    return s


GENERATORS = {"random": generate, "swarm": generate_swarm, "swarm-pos": generate_swarm_pos,
              "process": generate_process}


# ---------------------------------------------------------------- systematic sweeps (Milestone 42)
# Small fault spaces enumerated exhaustively, every store durable and the disk honest.
NETEM_SEVERITIES = [[("loss", 20)], [("loss", 40)], [("loss", 60)], [("delay", 100)], [("delay", 1000)]]


def sweep(name):
    """name -> list of (label, schedule dict). dseed is 0: no disk fault draws from it."""
    out = []
    if name == "kill":                  # every service killed before every order, 1 order down
        for svc in SVCS:
            for at in range(0, NORDERS):
                out.append(("%s@%d" % (svc, at), {"disk": "none", "dseed": 0, "K": (svc, at, 1)}))
    elif name == "cut":                 # a power cut before every order
        for c in range(1, NORDERS + 1):
            out.append(("C=%d" % c, {"disk": "cut", "dseed": 0, "C": c}))
    elif name == "net":                 # each service's traffic at a few severities, whole workload
        for svc in NET_SVCS:
            for kinds in NETEM_SEVERITIES:
                out.append(("%s:%s" % (svc, netem_args(kinds)), {"disk": "none", "dseed": 0, "N": (svc, 0, NORDERS, kinds)}))
    elif name == "killcut":             # kill, then a power cut a few orders later
        for svc in SVCS:
            for at in range(0, NORDERS - 1, 5):
                for d in (1, 2, 4, 8, 13):
                    if at + d <= NORDERS:
                        out.append(("%s@%d C=%d" % (svc, at, at + d),
                                    {"disk": "cut", "dseed": 0, "K": (svc, at, 1), "C": at + d}))
    elif name == "pause":               # M46: each service (and the front door) paused at a few points
        for svc in PAUSE_SVCS:
            for at in range(0, NORDERS, 5):
                out.append(("%s@%d+2" % (svc, at), {"disk": "none", "dseed": 0, "P": (svc, at, "+", 2)}))
                out.append(("%s@%d~200:5000" % (svc, at), {"disk": "none", "dseed": 0, "P": (svc, at, "~", 200, 5000)}))
    elif name == "clock":               # M46: realtime steps, both directions
        for at in (0, 10, 20):
            for d in (-86400, -3600, -60, 60, 3600, 86400):
                out.append(("T=%d:%d" % (at, d), {"disk": "none", "dseed": 0, "T": (at, d)}))
    elif name == "oneway":              # M46: one direction of one service's traffic, orders 5-15
        for svc in NET_SVCS:
            for d in ("req", "resp"):
                for kinds in ([("loss", 100)], [("loss", 50)], [("delay", 1000)]):
                    out.append(("%s:%s:%s" % (svc, d, netem_args(kinds)),
                                {"disk": "none", "dseed": 0, "O": (svc, 5, 15, d, kinds)}))
    elif name == "frontkill":           # the order service itself SIGKILLed by a timer while
        for q in (None, (4, False)):    # orders run, one request at a time and four at a time
            for at in range(0, NORDERS, 3):
                for off in (20, 80):
                    s = {"disk": "none", "dseed": 0, "X": ("front", at, off)}
                    if q:
                        s["Q"] = q
                    out.append(("front@%d~%d%s" % (at, off, " Q=4" if q else ""), s))
    elif name == "tail":                # every fault kind placed at the end of the workload: it
        last = range(NORDERS - 3, NORDERS)  # starts at one of the last 3 orders (and mostly runs to the end)
        for svc in SVCS:
            for at in last:
                for down in sorted({1, NORDERS - at}):
                    out.append(("K=%s@%d+%d" % (svc, at, down), {"disk": "none", "dseed": 0, "K": (svc, at, down)}))
        for svc in PAUSE_SVCS:
            for at in last:
                for n in range(1, NORDERS - at + 1):
                    out.append(("P=%s@%d+%d" % (svc, at, n), {"disk": "none", "dseed": 0, "P": (svc, at, "+", n)}))
                for off in (0, 200):
                    out.append(("P=%s@%d~%d:5000" % (svc, at, off),
                                {"disk": "none", "dseed": 0, "P": (svc, at, "~", off, 5000)}))
        for svc in NET_SVCS:
            for at in last:
                for d in ("req", "resp"):
                    for kinds in ([("loss", 100)], [("loss", 50)], [("delay", 1000)]):
                        out.append(("O=%s@%d-%d:%s:%s" % (svc, at, NORDERS, d, netem_args(kinds)),
                                    {"disk": "none", "dseed": 0, "O": (svc, at, NORDERS, d, kinds)}))
                for kinds in NETEM_SEVERITIES:
                    out.append(("N=%s@%d-%d:%s" % (svc, at, NORDERS, netem_args(kinds)),
                                {"disk": "none", "dseed": 0, "N": (svc, at, NORDERS, kinds)}))
        for c in range(NORDERS - 2, NORDERS + 1):
            out.append(("C=%d" % c, {"disk": "cut", "dseed": 0, "C": c}))
    elif name in ("depth1", "depth1-old"):
        # M56: the complete single-fault space -- every fault kind x service x start order x two
        # durations (one order, or to the end of the workload), durable settings, honest disk.
        # depth1-old: only the kinds the pre-M46 guest understands (K, N, C), for d0's image.
        new = name == "depth1"
        def durs(at):
            return sorted({1, NORDERS - at})
        for svc in SVCS:
            for at in range(NORDERS):
                for d in durs(at):
                    out.append(("K=%s@%d+%d" % (svc, at, d), {"disk": "none", "dseed": 0, "K": (svc, at, d)}))
        for svc in NET_SVCS:
            for at in range(NORDERS):
                for d in durs(at):
                    out.append(("N=%s@%d-%d:loss,60%%" % (svc, at, at + d),
                                {"disk": "none", "dseed": 0, "N": (svc, at, at + d, [("loss", 60)])}))
        for c in range(1, NORDERS + 1):
            out.append(("C=%d" % c, {"disk": "cut", "dseed": 0, "C": c}))
        if new:
            for svc in PAUSE_SVCS:
                for at in range(NORDERS):
                    for d in durs(at):
                        out.append(("P=%s@%d+%d" % (svc, at, d), {"disk": "none", "dseed": 0, "P": (svc, at, "+", d)}))
                    for dur in (500, 30000):
                        out.append(("P=%s@%d~0:%d" % (svc, at, dur), {"disk": "none", "dseed": 0, "P": (svc, at, "~", 0, dur)}))
            for svc in NET_SVCS:
                for at in range(NORDERS):
                    for d in durs(at):
                        for dr in ("req", "resp"):
                            out.append(("O=%s@%d-%d:%s:loss,100%%" % (svc, at, at + d, dr),
                                        {"disk": "none", "dseed": 0, "O": (svc, at, at + d, dr, [("loss", 100)])}))
            for at in range(NORDERS):
                for delta in (-86400, 365 * 86400):
                    out.append(("T=%d:%d" % (at, delta), {"disk": "none", "dseed": 0, "T": (at, delta)}))
    else:
        raise SystemExit("unknown sweep %r (kill, cut, net, killcut, pause, clock, oneway, frontkill, tail, depth1, depth1-old)" % name)
    return out


# ---------------------------------------------------------------- the freeze sweep (--sweep freeze)
# Every packet point of a run's traffic, one schedule each: F=<svc><dir><peer>@<step>#<n>[:<match>]:<dur>.
# The points come from the run itself: for each (pair, direction, match) stream and each step s, one
# `#0` trace run (F=...@s#0...) makes the guest print "freeze never fired: <N> of 0 matching events
# after step <s>" -- N = the stream's packets from step s to the end of fault injection (hydo-init-
# freeze.sh counts them in the helper's full log, so the console's 600-line trace limit does not
# matter). Step s then holds N(s) - N(s+1) packets (+ FC_SWEEP_MARGIN, since the two trace runs differ
# after step s begins; only where step s+1 is not swept itself); the last workload step holds N(s).
FC_SWEEP_MARGIN = 2
FC_TRACE_RE = re.compile(r"freeze never fired: (\d+) of 0 matching events after step (\d+)")


def fc_sweep_streams(a):
    """[(svc, peer, dir, match)] the sweep traces: pairs x (unmatched, each freeze_match string)."""
    pairs = list(FC_PAIRS)
    if a.fc_pairs:
        want = [tuple(re.split(r"[>:]", p.strip(), 1)) for p in a.fc_pairs.split(",") if p.strip()]
        bad = [p for p in want if p not in pairs]
        if bad:
            raise SystemExit("explore.py: --fc-pairs %s: not freeze pairs of this manifest (%s)"
                             % (", ".join(">".join(p) for p in bad), ", ".join(">".join(p) for p in pairs)))
        pairs = want
    dirs = [d for d in "<>" if d in a.fc_dirs.replace("resp", "<").replace("req", ">")]
    streams = []
    for item in a.fc_match.split(","):
        item = item.strip()
        if item == "all":
            streams += [(d, None) for d in dirs]
        elif item == "manifest":
            for m, md in FC_MATCH:
                streams += [(d, m) for d in ([md] if md else dirs) if d in dirs]
        elif item:
            if ":" in item or " " in item:
                raise SystemExit("explore.py: --fc-match %r: a match string may not contain ':' or a space" % item)
            md = dict((m, d) for m, d in FC_MATCH).get(item)
            streams += [(d, item) for d in ([md] if md else dirs) if d in dirs]
    seen, out = set(), []
    for svc, peer in pairs:
        for d, m in streams:
            if (svc, peer, d, m) not in seen:
                seen.add((svc, peer, d, m))
                out.append((svc, peer, d, m))
    return out


def fc_sweep_with(text):
    """--sweep-with: one other fixed fault every point (and every trace run) carries. Either a whole
    schedule ("disk=cut dseed=0 | run C=10") or guest tokens only ("K=kafka@9+1")."""
    if not text:
        return {"disk": "none", "dseed": 0}
    if "|" not in text:
        text = "disk=none dseed=0 | run " + text
    s = parse(text)
    if "F" in s:
        raise SystemExit("explore.py: --sweep-with may not carry F= (the sweep places it)")
    return s


def freeze_sweep(pool, a):
    """--sweep freeze: trace, then one point per packet. Returns [(label, schedule dict)]."""
    if not FC_PAIRS:
        raise SystemExit("explore.py: --sweep freeze needs an image whose manifest lists `freeze` in faults: "
                         "(manifest_space.py gives the pairs)")
    steps = sorted({v for part in a.fc_steps.split(",") for v in
                    (range(int(part.split("-")[0]), int(part.split("-")[1]) + 1) if "-" in part else [int(part)])}) \
        if a.fc_steps else list(range(NORDERS))
    if not steps or steps[0] < 0 or steps[-1] >= NORDERS:
        raise SystemExit("explore.py: --fc-steps must lie in 0..%d" % (NORDERS - 1))
    durs = [int(v) for v in a.fc_durs.split(",")]
    base = fc_sweep_with(a.sweep_with)
    streams = fc_sweep_streams(a)
    if not streams:
        raise SystemExit("explore.py: --sweep freeze: no streams (check --fc-pairs, --fc-dirs, --fc-match)")
    tsteps = sorted(set(steps) | {s + 1 for s in steps if s + 1 < NORDERS})
    known = {}                      # --fc-traces: counts of an earlier run's traces, by trace schedule
    for path in (a.fc_traces.split(",") if a.fc_traces else []):
        for line in open(path):
            rec = json.loads(line)
            if rec.get("count") is not None:
                known[rec["schedule"]] = rec
    items, keys, trec = [], {}, []
    for i, (svc, peer, d, m) in enumerate(streams):
        for st in tsteps:
            t = dict(base)
            t["F"] = (svc, d, peer, st, 0, m, 1)
            tag = "fctrace-%d-%d" % (i, st)
            keys[tag] = (svc, peer, d, m, st)
            text = render(t)
            if text in known:
                trec.append(dict(known[text], tag=tag, reused=True))
            else:
                items.append((tag, text))
    print("freeze sweep: %d streams x %d steps = %d trace runs (%d reused from --fc-traces)"
          % (len(streams), len(tsteps), len(items) + len(trec), len(trec)), file=sys.stderr)
    t0 = time.time()
    res = pool.run_all(items) if items else {}
    count = {keys[r["tag"]]: r["count"] for r in trec}
    for tag, (text, r) in sorted(res.items()):
        svc, peer, d, m, st = keys[tag]
        n = None
        try:
            for line in open(os.path.join(a.out, "consoles", tag + ".log"), errors="replace"):
                mm = FC_TRACE_RE.search(line)
                if mm and int(mm.group(2)) == st:
                    n = int(mm.group(1))
        except OSError:
            pass
        count[(svc, peer, d, m, st)] = n
        trec.append({"tag": tag, "schedule": text, "pair": "%s>%s" % (svc, peer), "dir": d, "match": m, "step": st,
                     "count": n, "verdict": r["verdict"], "fp": r["fp"]})
    with open(os.path.join(a.out, "fc-traces.jsonl"), "w") as f:
        for rec in trec:
            f.write(json.dumps(rec) + "\n")
    missing = [rec["tag"] for rec in trec if rec["count"] is None]
    if missing:
        raise SystemExit("explore.py: --sweep freeze: no packet count in the console of %s (a trace run that "
                         "never reached the end of fault injection -- e.g. a power cut in --sweep-with -- or an "
                         "image without the freeze fault)" % ", ".join(missing[:6]))
    out = []
    for svc, peer, d, m in streams:
        for st in steps:
            n_here = count[(svc, peer, d, m, st)]
            if st + 1 < NORDERS:           # the margin only where the next step is not swept itself
                n_here = max(0, n_here - count[(svc, peer, d, m, st + 1)]) + (0 if st + 1 in steps else a.fc_margin)
                n_here = min(n_here, count[(svc, peer, d, m, st)])
            for n in range(1, n_here + 1, a.sweep_every):
                for dur in durs:
                    s = dict(base)
                    s["F"] = (svc, d, peer, st, n, m, dur)
                    out.append((render_fc(s["F"])[2:], s))
    print("freeze sweep: traces took %.0f s; %d points (every %d-th packet, %d duration(s))"
          % (time.time() - t0, len(out), a.sweep_every, len(durs)), file=sys.stderr)
    return out


def netem_args(kinds):
    out = []
    has_delay = any(k == "delay" for k, _ in kinds)
    for k, v in kinds:
        if k == "delay":
            out += ["delay", "%dms" % v]
        elif k == "reorder":
            if not has_delay:
                out += ["delay", "20ms"]
            out += ["reorder", "%d%%" % v]
        else:
            out += [k, "%d%%" % v]
    return ",".join(out)


def render(s):
    if SPACE:
        s = SPACE.restrict(s)
    if "cf" in s and (s["disk"] != "cut" or "C" in s):   # cf= needs disk=cut and no C= (hvx_explore.c)
        s = dict(s)
        if s["disk"] != "cut":
            s.pop("cf")
        else:
            s.pop("C")
    if BACKEND == "hvm":
        s = hvm_backend.hvm_supported(s)
    g = ["run"]
    if "K" in s:
        g.append("K=%s@%d+%d" % s["K"])
    if "N" in s:
        svc, a, b, kinds = s["N"]
        g.append("N=%s@%d-%d:%s" % (svc, a, b, netem_args(kinds)))
    if "C" in s:
        g.append("C=%d" % s["C"])
    if "M" in s:
        g.append("M=%s" % s["M"])
    if "R" in s:
        g.append("R=%s" % s["R"])
    for k in KNOB_TOKENS:
        if k not in ("M", "R") and k in s:
            g.append("%s=%s" % (k, s[k]))
    if "P" in s:
        p = s["P"]
        g.append("P=%s@%d+%d" % (p[0], p[1], p[3]) if p[2] == "+" else "P=%s@%d~%d:%d" % (p[0], p[1], p[3], p[4]))
    if "T" in s:
        g.append("T=%d:%d" % s["T"])
    if "O" in s:
        svc, a, b, d, kinds = s["O"]
        g.append("O=%s@%d-%d:%s:%s" % (svc, a, b, d, netem_args(kinds)))
    if "Q" in s:
        g.append("Q=%d%s" % (s["Q"][0], "d" if s["Q"][1] else ""))
    if "X" in s:
        g.append("X=%s@%d~%d" % s["X"])
    if "B" in s:
        g.append("B=%s" % s["B"])
    if "F" in s:
        g.append(render_fc(s["F"]))
    if "W" in s:
        g.append("W=%s" % s["W"])
    return "disk=%s dseed=%x%s%s%s | %s" % (s["disk"], s["dseed"], render_tj(s["tj"]) if "tj" in s else "",
                                            render_preempt(s) + (" cf=%d" % s["cf"] if "cf" in s else "") + render_turns(s),
                                            " rb=%d" % s["rb"] if "rb" in s else "", " ".join(g))


def parse(text):
    host, guest = text.split("|", 1)
    s = {"disk": re.search(r"disk=(\S+)", host).group(1), "dseed": int(re.search(r"dseed=([0-9a-fA-F]+)", host).group(1), 16)}
    m = re.search(r"\brb=(\d+)", host)
    if m:
        s["rb"] = int(m.group(1))
    m = re.search(r"\bpp=(\d+)", host)
    if m:
        s["pp"] = int(m.group(1))
    m = re.search(r"\bcf=(\d+)", host)           # power cut at the k-th disk FLUSH (needs disk=cut)
    if m:
        s["cf"] = int(m.group(1))
    mq, mqs = re.search(r"(?:^|\s)q=(\d+)", host), re.search(r"(?:^|\s)qs=([0-9a-fA-F]+)", host)
    if mq or mqs:                                # vCPU turn lengths (SMP Stage 3)
        s["q"] = (int(mq.group(1)) if mq else 0, int(mqs.group(1), 16) if mqs else 0)
    m = re.search(r"(?:^|\s)qb=(\d+):(\d+):(\d+)(?: qbs=([0-9a-fA-F]+))?", host)
    if m:                                        # seeded bursts of short turns (FASTTURN.md)
        s["qb"] = (int(m.group(1)), int(m.group(2)), int(m.group(3)), int(m.group(4) or "1", 16))
    m = re.search(r"\bpct=([0-9a-fA-F]+) pctn=(\d+) pctk=(\d+)", host)
    if m:
        s["pct"] = (int(m.group(1), 16), int(m.group(2)), int(m.group(3)))
    m = re.search(r"\bxt=([0-9a-fA-F]+) xtr=(\d+) xtpm=(\d+)", host)
    if m:
        s["xt"] = (int(m.group(1), 16), int(m.group(2)), int(m.group(3)))
    m = re.search(r"tjx=(\S+)", host)
    if m:
        s["tj"] = ("x", [tuple(int(v) for v in kd.split(":")) for kd in m.group(1).split(",")])
    elif re.search(r"\btj=", host):
        g = lambda k, d: int(re.search(k + r"=(\d+)", host).group(1)) if re.search(k + r"=(\d+)", host) else d
        s["tj"] = ("seed", int(re.search(r"\btj=([0-9a-fA-F]+)", host).group(1), 16), g("tjpm", 10),
                   g("tjmax", TJ_MAX), g("tjlo", 0), g("tjhi", 0))
    for t in guest.split()[1:]:
        k, v = t.split("=", 1)
        if k == "K":
            svc, rest = v.split("@"); at, down = rest.split("+"); s["K"] = (svc, int(at), int(down))
        elif k == "N":
            svc, rest = v.split("@", 1); rng, args = rest.split(":", 1); a, b = rng.split("-")
            toks = args.split(","); kinds = []; i = 0
            while i < len(toks):
                kinds.append((toks[i], int(re.sub(r"\D", "", toks[i + 1])))); i += 2
            s["N"] = (svc, int(a), int(b), kinds)
        elif k == "C":
            s["C"] = int(v)
        elif k == "M":
            s["M"] = int(v) if v.isdigit() else v
        elif k == "R":
            s["R"] = v
        elif k == "P":
            svc, rest = v.split("@")
            if "~" in rest:
                at, r = rest.split("~"); off, dur = r.split(":"); s["P"] = (svc, int(at), "~", int(off), int(dur))
            else:
                at, n = rest.split("+"); s["P"] = (svc, int(at), "+", int(n))
        elif k == "T":
            at, d = v.split(":"); s["T"] = (int(at), int(d))
        elif k == "O":
            svc, rest = v.split("@", 1); rng, d, args = rest.split(":", 2); a, b = rng.split("-")
            toks = args.split(","); kinds = []; i = 0
            while i < len(toks):
                kinds.append((toks[i], int(re.sub(r"\D", "", toks[i + 1])))); i += 2
            s["O"] = (svc, int(a), int(b), d, kinds)
        elif k == "Q":
            s["Q"] = (int(v.rstrip("d")), v.endswith("d"))
        elif k == "X":
            svc, rest = v.split("@"); at, off = rest.split("~"); s["X"] = (svc, int(at), int(off))
        elif k == "B":
            s["B"] = v
        elif k == "F":
            s["F"] = parse_fc(v)
        elif k == "W":                           # the test composer (hydo_composer.py)
            s["W"] = v
        elif k in KNOB_TOKENS:
            s[k] = v
    return s


def finding_class(s):
    """The reason a failure is expected, most fundamental first: a lying or failing disk
    explains more than a relaxed durability setting does."""
    if s["disk"].startswith("lying"):
        return "lying-disk"
    if s["disk"].startswith("ioerr"):
        return "io-errors"
    relaxed = SPACE.unsafe_knob(s) if SPACE else (s.get("M", 1) != 1 or s.get("R", "always") != "always")
    if relaxed and (KNOB_RULE == "any" or s["disk"] == "cut"):     # a lying disk returned above
        return "unsafe-knob"
    return "durable-defaults"


# ---------------------------------------------------------------- assertions (hvx/ASSERTIONS.md)
ASSERT_RE = re.compile(r"HYDO-ASSERT (\{.*\})")


def assert_failures(outdir, tag):
    """Failed property evaluations in one run's console(s): an always / always_or_unreachable
    with ok=false, an eventually that missed its deadline (ok=false), or any unreachable that
    was emitted. Returns sorted assertion ids."""
    bad = set()
    for suffix in ("", ".recovery", ".checks"):
        path = os.path.join(outdir, "consoles", "%s%s.log" % (tag, suffix))
        if not os.path.exists(path):
            continue
        for line in open(path, errors="replace"):
            m = ASSERT_RE.search(line)
            if not m:
                continue
            try:
                rec = json.loads(m.group(1))
            except ValueError:
                continue
            t, ok = rec.get("t"), rec.get("ok")
            if (t in ("always", "always_or_unreachable", "eventually") and ok is False) or t == "unreachable":
                bad.add(rec.get("id", "?"))
    return sorted(bad)


def property_table(outdir, tags):
    """The exploration-wide property table from hvx/hydo_assert_report (built on the fly),
    over the consoles of the given runs. Returns its text, or None if no run emitted records."""
    # One console file per SCHEDULE: a power cut's recovery boot is part of the same run,
    # so its console is appended to the workload's (the report tool counts files as runs).
    files = []
    merged_dir = os.path.join(outdir, "consoles-merged")
    os.makedirs(merged_dir, exist_ok=True)
    for t in tags:
        text = ""
        for suffix in ("", ".recovery", ".checks"):
            p = os.path.join(outdir, "consoles", "%s%s.log" % (t, suffix))
            if os.path.exists(p):
                text += open(p, errors="replace").read()
        if "HYDO-ASSERT" in text:
            m = os.path.join(merged_dir, "%s.log" % t)
            open(m, "w").write(text)
            files.append(m)
    if not files:
        return None
    tool = os.path.join(outdir, "hydo_assert_report")
    subprocess.run(["cc", "-O2", "-o", tool, os.path.join(HVX, "hydo_assert_report.c")], check=True)
    r = subprocess.run([tool] + files, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return r.stdout


def liveness_summary(outdir):
    """Per bounded-liveness (eventually) property, over the merged per-schedule consoles that
    property_table wrote: schedules where it held in time / missed its deadline, and the
    slowest in-time and the first missed episode. Lines for report.md, or [] if none."""
    d = os.path.join(outdir, "consoles-merged")
    props = {}
    for f in sorted(os.listdir(d)) if os.path.isdir(d) else []:
        seen = {}
        for line in open(os.path.join(d, f), errors="replace"):
            m = ASSERT_RE.search(line)
            if not m:
                continue
            try:
                rec = json.loads(m.group(1))
            except ValueError:
                continue
            if rec.get("t") != "eventually":
                continue
            det = rec.get("details") or {}
            el = det.get("elapsed_s")
            el = float(el) if el not in (None, "?") else det.get("elapsed_ms", 0) / 1000.0
            seen.setdefault(rec.get("id", "?"), []).append((rec.get("ok") is True, el, det))
        for pid, eps in seen.items():
            p = props.setdefault(pid, {"held": 0, "missed": 0, "slowest": None, "first_miss": None})
            if all(ok for ok, _, _ in eps):
                p["held"] += 1
                worst = max(el for _, el, _ in eps)
                if p["slowest"] is None or worst > p["slowest"][0]:
                    p["slowest"] = (worst, f[:-4])
            else:
                p["missed"] += 1
                if p["first_miss"] is None:
                    p["first_miss"] = (f[:-4], next(det for ok, _, det in eps if not ok))
    if not props:
        return []
    L = ["## Liveness (eventually properties, hvx/ASSERTIONS.md)", "",
         "| property | held in time | missed deadline | slowest in time | first miss |", "|---|---|---|---|---|"]
    for pid in sorted(props):
        p = props[pid]
        slow = "%.2f s (%s)" % p["slowest"] if p["slowest"] else "-"
        miss = "%s: `%s`" % (p["first_miss"][0], p["first_miss"][1].get("out", "")) if p["first_miss"] else "-"
        L.append("| `%s` | %d | %d | %s | %s |" % (pid, p["held"], p["missed"], slow, miss))
    return L + [""]


def verdict_kind(v):
    if "HYDO-ORACLE: OK" in v:
        return "ok"
    if "VIOLATION" in v:
        return "violation"
    return "error"            # harness / guest setup problems, timeouts


def ram_args():
    """The engine option for a non-default guest RAM size (none at 1024 MiB)."""
    return [] if RAM_MIB == 1024 else ["--ram-mib", str(RAM_MIB)]


def engine_args():
    """Engine options: guest RAM, the vCPU count only when it is not 1, and the guest CPU level
    only for v3 (so the default command line is unchanged)."""
    return ram_args() + ([] if VCPUS == 1 else ["--vcpus", str(VCPUS)]) + \
        (["--cpu", "v3"] if CPU == "v3" and BACKEND == "hvx" else [])


def pick_cpu(cli, manifest):
    """--cpu, else the manifest's guest.cpu, else v2 (the default level, no engine option)."""
    v = cli if cli is not None else ((manifest.get("guest") or {}).get("cpu", "v2") if manifest else "v2")
    if v not in ("v2", "v3"):
        raise SystemExit("explore.py: guest CPU %r: expected v2 or v3" % (v,))
    if v == "v3" and BACKEND != "hvx":
        raise SystemExit("explore.py: guest CPU v3 (AVX) is an x86_64 option; --backend %s cannot run it" % BACKEND)
    return v


def pick_ram_mib(cli, manifest):
    """--ram-mib, else the manifest's guest.ram_mib, else 1024; checked against the backend's bounds."""
    src, v = ("--ram-mib", cli) if cli is not None else \
        ("guest.ram_mib", (manifest.get("guest") or {}).get("ram_mib", 1024)) if manifest else ("default", 1024)
    lo, hi = RAM_MIB_BOUNDS[BACKEND]
    if not isinstance(v, int) or isinstance(v, bool) or not lo <= v <= hi:
        raise SystemExit("explore.py: %s=%r: guest RAM must be a whole number of MiB in [%d, %d] on --backend %s"
                         % (src, v, lo, hi, BACKEND))
    return v


def signature(v):
    v = v.replace("HYDO-ORACLE: ", "")
    v = re.sub(r"\d+", "#", v)
    return v


# ---------------------------------------------------------------- host-side history checks (hvx/HISTORIES.md)
def host_checks(outdir, tag, res):
    """A run whose console declares history checks (HYDO-CHECK lines, from a manifest's
    checks:) is checked HERE, after the run: the consoles <tag>.log (+ .recovery.log) are read,
    the checkers in hvx/history/hydohist.py run, their HYDO-ASSERT records go to
    consoles/<tag>.checks.log (read with the consoles by assert_failures / property_table), the
    full explanation to consoles/<tag>.checks.txt, and res["verdict"] becomes the combined
    verdict (the guest's own verdict is kept in res["guest_verdict"]). A pure function of the
    consoles, so a reproduced run gets the same verdict. Runs without checks are untouched."""
    d = os.path.join(outdir, "consoles")
    srcs = []
    for suffix in ("", ".recovery"):
        p = os.path.join(d, "%s%s.log" % (tag, suffix))
        if os.path.exists(p):
            srcs.append((tag + suffix, open(p, errors="replace").read()))
    if not any("HYDO-CHECK " in t for _, t in srcs):
        return res
    sys.path.insert(0, os.path.join(HVX, "history"))
    import hydohist
    r = hydohist.run_checks(srcs, res.get("verdict"))
    with open(os.path.join(d, "%s.checks.log" % tag), "w") as f:
        f.write("".join(x + "\n" for x in r["records"]))
    with open(os.path.join(d, "%s.checks.txt" % tag), "w") as f:
        f.write(r["report"] + "\n" + r["verdict"] + "\n")
    res["guest_verdict"] = res.get("verdict")
    res["verdict"] = r["verdict"]
    return res


# ---------------------------------------------------------------- the worker pool
class Worker(threading.Thread):
    def __init__(self, idx, jobs, results, outdir):
        super().__init__(daemon=True)
        self.idx, self.jobs, self.results, self.outdir = idx, jobs, results, outdir
        self.boot_secs = None

    def run(self):
        cmd = [HVX_RUN, "sudo", "env", "HVX_SNAP_DIR=" + SNAP_DIR] + EXTRA_ENV + [
               os.path.join(HVX, "hvx_explore")] + engine_args() + ["serve", os.path.join(BUILD, "bzImage"),
               os.path.join(BUILD, "initramfs-kafka.cpio.gz"), os.path.join(BUILD, IMAGE),
               os.path.join(self.outdir, "consoles")]
        if BACKEND == "hvm":
            cmd = hvm_backend.engine_cmd(IMAGE, os.path.join(self.outdir, "consoles"), HVM_KERNEL, ram_args())
        err = open(os.path.join(self.outdir, "worker-%d.err" % self.idx), "w")
        p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, bufsize=1)
        line = p.stdout.readline()
        if not line.startswith("READY"):
            print("worker %d failed to start: %r" % (self.idx, line), file=sys.stderr)
            return
        self.boot_secs = float(line.split("boot_secs=")[1])
        while True:
            job = self.jobs.get()
            if job is None:
                break
            tag, text = job
            if text.startswith("BRANCH "):
                self.results.put((tag, text, self.branch(p, tag, text)))
                continue
            p.stdin.write("RUN %s %s\n" % (tag, text))
            p.stdin.flush()
            out = p.stdout.readline()
            m = re.match(r"RESULT (\S+) fp=(\S+) wfp=(\S+) secs=(\S+)((?: (?:tj|xt|pct|qn)\S+)*) verdict=(.*)", out.strip())
            if not m:
                self.results.put((tag, text, {"fp": "0", "secs": 0, "verdict": "ERROR worker: %r" % out, "worker": self.idx}))
                continue
            res = {"fp": m.group(2), "wfp": m.group(3), "secs": float(m.group(4)),
                   "verdict": m.group(6), "worker": self.idx}
            tk = re.search(r"tjk=(\d+)-(\d+) tjn=(\d+)", m.group(5))
            if tk:      # the workload's tick range and how many ticks were delayed
                res["tjk"] = [int(tk.group(1)), int(tk.group(2))]
                res["tjn"] = int(tk.group(3))
            pf = re.search(r"pctf=(\d+) pctw=(\d+)", m.group(5))
            if pf:      # forced preemption points (Mac, M57): how many fired, and the workload's length in exits
                res["pctf"], res["pctw"] = int(pf.group(1)), int(pf.group(2))
            xn = re.search(r"xtn=(\d+)", m.group(5))
            if xn:      # M49: extra ticks taken during the workload
                res["xtn"] = int(xn.group(1))
            qn = re.search(r"qn=(\d+)", m.group(5))
            if qn:      # SMP Stage 3: vCPU turns taken during the workload
                res["qn"] = int(qn.group(1))
            host_checks(self.outdir, tag, res)
            self.results.put((tag, text, res))
        p.stdin.write("QUIT\n")
        p.stdin.flush()
        p.wait()

    def branch(self, p, tag, text):
        """One BRANCH request: 'BRANCH <n> <pm> <max> <marker> <schedule>'. Returns
        {"tjlo": k or None, "runs": [(subtag, standalone schedule text, result), ...]}."""
        n, pm, mx, mark, sched = text[7:].split(" ", 4)
        p.stdin.write("BRANCH %s %s %s %s %s %s\n" % (tag, n, pm, mx, mark, sched))
        p.stdin.flush()
        host, guest = sched.split("|", 1)
        out = {"tjlo": None, "runs": []}
        while True:
            line = p.stdout.readline()
            if not line or line.startswith("BRANCHEND"):
                break
            m = re.match(r"BRANCHPOINT \S+ tjlo=(\d+)(?: held=(\d+))?", line)
            if m:
                out["tjlo"] = int(m.group(1))
                out["held"] = int(m.group(2)) if m.group(2) else None
                continue
            m = re.match(r"RESULT (\S+) fp=(\S+) wfp=(\S+) secs=(\S+)((?: (?:tj|pct|qn)\S+)*) verdict=(.*)", line.strip())
            if not m:
                continue
            sub = m.group(1)
            res = {"fp": m.group(2), "wfp": m.group(3), "secs": float(m.group(4)), "verdict": m.group(6),
                   "worker": self.idx}
            vm = re.search(r"-v(\d+)$", sub)
            if vm and out["tjlo"]:      # the variant as a plain schedule (see hvx_explore.c)
                st = "%s tj=%x tjpm=%s tjmax=%s tjlo=%d tjhi=0 rb=%d |%s" % (
                    host.rstrip(), int(vm.group(1)), pm, mx, out["tjlo"], BRANCH_BUDGET, guest)
            else:               # the base run: no jitter, same run budget
                st = "%s rb=%d |%s" % (host.rstrip(), BRANCH_BUDGET, guest)
            tk = re.search(r"tjk=(\d+)-(\d+) tjn=(\d+)", m.group(5))
            if tk:
                res["tjk"] = [int(tk.group(1)), int(tk.group(2))]
                res["tjn"] = int(tk.group(3))
            host_checks(self.outdir, sub, res)
            out["runs"].append((sub, st, res))
        return out


class Pool:
    def __init__(self, n, outdir):
        self.jobs, self.results = queue.Queue(), queue.Queue()
        os.makedirs(os.path.join(outdir, "consoles"), exist_ok=True)
        self.workers = [Worker(i, self.jobs, self.results, outdir) for i in range(n)]
        for w in self.workers:
            w.start()

    def run_all(self, items):
        """items: list of (tag, schedule text) -> dict tag -> (text, result)"""
        for it in items:
            self.jobs.put(it)
        out = {}
        while len(out) < len(items):
            tag, text, res = self.results.get()
            out[tag] = (text, res)
        return out

    def close(self):
        for _ in self.workers:
            self.jobs.put(None)
        for w in self.workers:
            w.join()


# ---------------------------------------------------------------- shrinking
def candidates(s, tjk=None):
    """Simpler variants of schedule s, most aggressive first. tjk: the tick range of the
    run of s (needed to turn a seeded timing jitter into an explicit list)."""
    out = []
    def var(**kw):
        t = dict(s)
        for k, v in kw.items():
            if v is None:
                t.pop(k, None)
            else:
                t[k] = v
        out.append(t)
    if "K" in s:
        var(K=None)
    if "N" in s:
        var(N=None)
    for k in ("P", "T", "O"):
        if k in s:
            var(**{k: None})
    for k in KNOB_TOKENS:
        if k in s:
            var(**{k: None})
    if s["disk"] != "none":
        var(disk="none")
    if "C" in s and s["disk"] == "none":
        var(C=None)
    if "N" in s:
        svc, a, b, kinds = s["N"]
        if len(kinds) > 1:
            for i in range(len(kinds)):
                var(N=(svc, a, b, kinds[:i] + kinds[i + 1:]))
        for i, (k, v) in enumerate(kinds):
            if v > 1:
                var(N=(svc, a, b, kinds[:i] + [(k, max(1, v // 2))] + kinds[i + 1:]))
        if b - a > 1:
            var(N=(svc, a, a + max(1, (b - a) // 2), kinds))
    if "K" in s and s["K"][2] > 1:
        var(K=(s["K"][0], s["K"][1], 1))
    if s["disk"].startswith(("lying", "ioerr")):
        p = int(s["disk"].split(":")[1])
        if p > 1:
            var(disk="%s:%d" % (s["disk"].split(":")[0], max(1, p // 2)))
    nf_candidates(s, var)
    if "X" in s:                                    # timed kill: drop it, then kill earlier
        var(X=None)
        if s["X"][2] > 0:
            var(X=s["X"][:2] + (s["X"][2] // 2,))
    if "Q" in s:                                    # concurrency: drop it, then make it milder
        var(Q=None)
        n, dup = s["Q"]
        if dup:
            var(Q=(n, False))
        if n > 2:
            var(Q=(n // 2, dup))
    if "cf" in s:                                   # cut at the k-th FLUSH (M62): drop it, then cut earlier
        var(cf=None)
        if s["cf"] > 1:
            var(cf=s["cf"] // 2)
    if "B" in s:                                    # BUGGIFY (M56): drop the fault points
        var(B=None)
    if "F" in s:                                    # freeze at a call (M70): drop it, shorter, an earlier event
        var(F=None)
        f = s["F"]
        if f[6] > 100:
            var(F=f[:6] + (max(100, f[6] // 2),))
        if f[4] > 1:
            var(F=f[:4] + (f[4] // 2,) + f[5:])
    if "pp" in s:                                   # finer preemption (M49): drop it, then milder
        var(pp=None)
    if "xt" in s:
        var(xt=None)
        if s["xt"][1] > 1:
            var(xt=(s["xt"][0], s["xt"][1] // 2, s["xt"][2]))
    if "pct" in s:                                  # forced preemption points (M57): drop, then fewer
        var(pct=None)
        if s["pct"][1] > 1:
            var(pct=(s["pct"][0], s["pct"][1] // 2, s["pct"][2]))
    if "q" in s:                                    # vCPU turns (SMP Stage 3): drop them, then fixed
        var(q=None)                                 # lengths, then longer turns toward the default
        q, qs = s["q"]
        if qs:
            var(q=(q, 0))
        if q and q < Q_DEFAULT:
            var(q=(min(Q_DEFAULT, q * 8), qs))
    if "qb" in s:                                   # bursts: drop, then rarer, then shorter
        var(qb=None)
        blen, bq, pm, bs = s["qb"]
        if pm > 1:
            var(qb=(blen, bq, max(1, pm // 2), bs))
        if blen > 2 * bq:
            var(qb=(blen // 2, bq, pm, bs))
    tj_candidates(s, tjk, var)
    if "W" in s and SPACE is not None and SPACE.compose:   # the composition: fewer commands, less overlap
        for w in _hc().candidates(s["W"], SPACE.compose):
            var(W=w)
    return out


def nf_candidates(s, var):
    """M46 faults: shorter pauses, smaller clock steps, a narrower / milder one-way fault."""
    if "P" in s:
        p = s["P"]
        if p[2] == "+" and p[3] > 1:
            var(P=p[:3] + (1,))
        if p[2] == "~":
            if p[4] > 500:
                var(P=p[:4] + (max(500, p[4] // 2),))
            if p[3] > 0:
                var(P=p[:3] + (0, p[4]))
    if "T" in s and abs(s["T"][1]) > 30:
        var(T=(s["T"][0], int(s["T"][1] / 2)))
    if "O" in s:
        svc, a, b, d, kinds = s["O"]
        if b - a > 1:
            var(O=(svc, a, a + max(1, (b - a) // 2), d, kinds))
        k, v = kinds[0]
        if v > 1:
            var(O=(svc, a, b, d, [(k, max(1, v // 2))]))


def tj_candidates(s, tjk, var):
    """Timing: drop it; a seeded jitter becomes its explicit (tick, delay) list (or, with
    too many delayed ticks, keeps only one half of its tick window); an explicit list loses
    one half, then single entries. Removing an early delay changes everything after it, so
    removals that keep the failure are the informative ones."""
    if "tj" not in s:
        return
    var(tj=None)
    tj = s["tj"]
    if tj[0] == "seed" and tjk:
        pairs = tj_delays(tj, tjk[0] + 1, tjk[1])
        if pairs and len(pairs) <= TJ_EXPLICIT_MAX:
            var(tj=("x", pairs))
        elif pairs:
            mid = pairs[len(pairs) // 2][0]
            var(tj=tj[:4] + (tj[4], mid))       # keep the first half of the delays
            var(tj=tj[:4] + (mid, tj[5]))       # keep the second half
    elif tj[0] == "x" and len(tj[1]) > 1:
        kd = tj[1]
        if len(kd) > 3:
            var(tj=("x", kd[:len(kd) // 2]))
            var(tj=("x", kd[len(kd) // 2:]))
        for i in range(len(kd)):
            var(tj=("x", kd[:i] + kd[i + 1:]))


def same_failure(v_ref, v):
    """A variant counts as 'still failing the same way' when it is a violation of the same
    kind in the same service (numbers ignored)."""
    if verdict_kind(v) != "violation":
        return False
    return signature(v).split(" order=")[0] == signature(v_ref).split(" order=")[0]


def shrink(pool, s, verdict, log, budget=40, tjk=None):
    cur, cur_v, tries, rnd = s, verdict, 0, 0
    if "tj" in s:
        budget *= 3             # explicit tick lists shrink one entry at a time
    if "W" in s:
        budget *= 2             # a composition shrinks by commands and entries too
    while tries < budget:
        cands = candidates(cur, tjk)
        if not cands:
            break
        rnd += 1
        items = [("shrink-%d-%d" % (rnd, i), render(c)) for i, c in enumerate(cands)]
        res = pool.run_all(items)
        tries += len(items)
        progressed = False
        for (tag, text), c in zip(items, cands):
            v = res[tag][1]["verdict"]
            log.write(json.dumps({"shrink": text, "verdict": v}) + "\n")
            log.flush()
            if same_failure(cur_v, v):
                cur, cur_v, progressed = c, v, True
                tjk = res[tag][1].get("tjk", tjk)
                break
        if not progressed:
            break
    return cur, cur_v, tries


# ---------------------------------------------------------------- main
def main():
    global IMAGE, TIMING, TJ_PM, NEW_FAULTS, SPACE, CONCURRENCY, BACKEND, HVM_KERNEL, PREEMPT, BUGGIFY, KNOB_RULE, RAM_MIB, MORE_FAULTS, CF_RANGE, VCPUS, TURNS, CPU, BURSTS
    global FREEZE_CALL, FC_NMAX, FC_MATCH, COMPOSE
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", default="1-200")
    ap.add_argument("--workers", type=int, default=3)   # the box has 4 cores: 3 is fastest (M38)
    ap.add_argument("--image", default="explore-disk.img", help="guest disk image under ~/hydo/build")
    ap.add_argument("--out", required=True)
    ap.add_argument("--schedule", help="run just this schedule (no generation)")
    ap.add_argument("--no-shrink", action="store_true")
    ap.add_argument("--timing", action="store_true", help="add seeded tick-timing jitter to half of the schedules")
    ap.add_argument("--tj-pm", default=",".join(map(str, TJ_PM)),
                    help="comma list of delayed-ticks-per-1000 densities to pick from")
    ap.add_argument("--strategy", choices=sorted(GENERATORS), default="random",
                    help="schedule generator for --seeds: random, swarm (M42), swarm-pos (swarm + a per-run "
                         "fault position, M56), process (a Jepsen-style staggered fault process, M56)")
    ap.add_argument("--sweep", help="enumerate a small fault space instead of seeds: kill, cut, net, killcut, pause, "
                                    "clock, oneway, frontkill, tail, depth1 (every single fault, M56), depth1-old, "
                                    "freeze (every packet point of a traced run, F=; see --fc-*)")
    ap.add_argument("--fc-pairs", help="--sweep freeze: only these freeze pairs, comma list of svc>peer (default: every "
                                       "pair the manifest allows, as --freeze-call draws them)")
    ap.add_argument("--fc-dirs", default="<>", help="--sweep freeze: directions, any of < (packets the frozen service "
                                                    "receives) and > (packets it sends); default both")
    ap.add_argument("--fc-steps", help="--sweep freeze: workload steps whose packets are swept, e.g. 8-9 or 3,7 "
                                       "(default every step)")
    ap.add_argument("--fc-match", default="all,manifest",
                    help="--sweep freeze: packet streams, comma list: all = every packet with payload, manifest = "
                         "each freeze_match: string in its own direction, or a payload string (default %(default)s)")
    ap.add_argument("--fc-durs", default="150000", help="--sweep freeze: freeze lengths, ms of guest time, comma list "
                                                        "(default %(default)s, the longest --freeze-call draws)")
    ap.add_argument("--fc-margin", type=int, default=FC_SWEEP_MARGIN,
                    help="--sweep freeze: extra packets of the last swept step before an unswept one, beyond "
                         "N(s) - N(s+1) (default %(default)s)")
    ap.add_argument("--fc-traces", help="--sweep freeze: reuse the trace counts of earlier runs (comma list of their "
                                        "fc-traces.jsonl); only traces with the same schedule are reused")
    ap.add_argument("--sweep-every", type=int, default=1, help="--sweep freeze: keep every k-th packet (n = 1, 1+k, ...)")
    ap.add_argument("--sweep-with", help="--sweep freeze: one fixed fault every point carries, guest tokens "
                                         "(\"K=kafka@9+1\") or a whole schedule (\"disk=cut dseed=0 | run C=10\")")
    ap.add_argument("--sweep-dry", action="store_true", help="--sweep freeze: run the traces, write the points, stop")
    ap.add_argument("--repro", type=int, help="re-runs of every failure (default 1; 2 for --sweep freeze)")
    ap.add_argument("--knob-rule", choices=["cut", "any"], default="cut",
                    help="finding class of a relaxed durability knob: cut (default, M56) = unsafe-knob only with a "
                         "power cut or lying disk; any = the pre-M56 rule")
    ap.add_argument("--branch", metavar="MARKER",
                    help="mid-run branching: for each seed's schedule, snapshot at the first "
                         "console line containing MARKER and run --variants timing variants from there")
    ap.add_argument("--variants", type=int, default=8)
    ap.add_argument("--durable-only", action="store_true",
                    help="M46: drop disk faults other than a plain power cut and relaxed durability knobs")
    ap.add_argument("--new-faults", action="store_true",
                    help="M46: add pause / realtime-clock-jump / one-way-network faults (separate random stream)")
    ap.add_argument("--concurrency", action="store_true",
                    help="several clients place orders at once (Q= token; separate random stream)")
    ap.add_argument("--more-faults", action="store_true",
                    help="M62: add a timed kill (X=svc@order~ms) and a power cut at the k-th disk FLUSH (cf=k) "
                         "to generated schedules (separate random stream; any --strategy)")
    ap.add_argument("--freeze-call", action="store_true",
                    help="M70: freeze a service at the n-th packet of one of its conversations (F=svc>peer@step#n:ms; "
                         "manifests whose faults: list freeze; separate random stream)")
    ap.add_argument("--fc-nmax", type=int, default=FC_NMAX,
                    help="--freeze-call: n is drawn log-uniformly from 1 .. this (default %(default)s)")
    ap.add_argument("--cf-range", default="auto",
                    help="--more-faults: FLUSH indices cf= draws from, lo-hi, or auto (default) = 1 .. the FLUSH "
                         "count of one fault-free workload on --image, measured with a cfn run before generating")
    ap.add_argument("--pct-exits", default=",".join(map(str, PCT_EXITS)),
                    help="--preempt on --backend hvm: comma list of exit ranges the forced preemption points are spread over")
    ap.add_argument("--preempt", action="store_true",  # on --backend hvm: pct= forced preemption points (M57)
                    help="M49: shorter tick period (pp=) or inserted ticks (xt=) on half the schedules")
    ap.add_argument("--buggify", action="store_true",
                    help="M56: give every schedule a BUGGIFY seed (B=) for the front door's code-level fault points")
    ap.add_argument("--backend", choices=["hvx", "hvm"], default="hvx",
                    help="hvm: the Mac VMM (hvx-mac/explore/hvmx); --image is then an arm64 manifest initramfs")
    ap.add_argument("--hvm-kernel", help="arm64 kernel for --backend hvm (default hvx-mac/explore/out/Image-explore)")
    ap.add_argument("--manifest", help="an onboarded project's hydo.yaml: draw faults from its services, ports, "
                                       "steps and knobs (manifest_space.py); default for explore-ob-* images: "
                                       "the manifest inside the image")
    ap.add_argument("--cpu", choices=["v2", "v3"], help="guest CPU level (default: the manifest's guest.cpu, "
                    "else v2); v3 = AVX/AVX2 (hvx_explore --cpu v3, x86_64 only)")
    ap.add_argument("--vcpus", type=int, default=1,
                    help="SMP Stage 3: give the guest this many vCPUs, run in exact turns inside hvx (needs the PMU "
                         "clock; 1..8, default 1 = the engine's command line unchanged)")
    ap.add_argument("--turns", action="store_true",
                    help="with --vcpus N > 1: half of the schedules get vCPU turn lengths q= [qs=] for the workload "
                         "(rr chaos-mode mix; separate random stream)")
    ap.add_argument("--no-compose", action="store_true",
                    help="a plan with test commands: do not draw W= (every schedule runs the default composition)")
    ap.add_argument("--bursts", action="store_true",
                    help="with --turns: half of the schedules also get seeded bursts of short turns qb=B:s:permille "
                         "qbs=<hex> (FASTTURN.md; separate random stream; needs a module with HVX_SET_TURNCTL)")
    ap.add_argument("--turn-margin", type=int,
                    help="landing margin of turn ends in branches (HVX_TURN_MARGIN for the engine; default 256; "
                         "faster when smaller, must exceed the PMU overflow skid, FASTTURN.md)")
    ap.add_argument("--ram-mib", type=int, help="guest RAM in MiB (default: the manifest's guest.ram_mib, else 1024; "
                                                "256..3072 on hvx, 256..4096 on hvm)")
    a = ap.parse_args()
    BACKEND, HVM_KERNEL = a.backend, a.hvm_kernel
    if BACKEND == "hvm":
        hvm_backend.configure(a.image)
    if a.manifest or a.image.startswith("explore-ob-") or os.path.basename(a.image).startswith("explore-ob-"):
        import manifest_space
        if BACKEND == "hvm" and not a.manifest:      # the manifest inside an arm64 initramfs
            sp = manifest_space.Space(hvm_backend.embedded_manifest(a.image), "hydo.yaml",
                                      compose=hvm_backend.embedded_compose(a.image))
        else:
            sp = manifest_space.load(a.manifest, os.path.join(BUILD, a.image))
        sp.apply(globals())
        print("fault space: %s" % json.dumps(SPACE.describe()), file=sys.stderr)
    RAM_MIB = pick_ram_mib(a.ram_mib, SPACE.m if SPACE else None)
    CPU = pick_cpu(a.cpu, SPACE.m if SPACE else None)
    if CPU != "v2":
        print("guest CPU: %s" % CPU, file=sys.stderr)
    if RAM_MIB != 1024:
        print("guest RAM: %d MiB" % RAM_MIB, file=sys.stderr)
    VCPUS, TURNS = a.vcpus, a.turns
    if not 1 <= VCPUS <= 8:
        raise SystemExit("explore.py: --vcpus must be 1..8 (hvx's HVX_MAX_VCPUS)")
    if VCPUS > 1 and BACKEND == "hvm":
        raise SystemExit("explore.py: --vcpus > 1 needs --backend hvx (the Mac VMM has one vCPU and no PMU clock)")
    if TURNS and VCPUS < 2:
        raise SystemExit("explore.py: --turns needs --vcpus N > 1 (turn tokens on a one-vCPU VM are an ERROR)")
    BURSTS = a.bursts
    if BURSTS and not TURNS:
        raise SystemExit("explore.py: --bursts needs --turns (and --vcpus N > 1)")
    if a.turn_margin is not None:
        if VCPUS < 2 or not 8 <= a.turn_margin <= (1 << 20):
            raise SystemExit("explore.py: --turn-margin needs --vcpus N > 1 and a value in [8, 2^20]")
        EXTRA_ENV.append("HVX_TURN_MARGIN=%d" % a.turn_margin)
    NEW_FAULTS = a.new_faults
    COMPOSE = not a.no_compose
    KNOB_RULE = a.knob_rule
    CONCURRENCY = a.concurrency
    MORE_FAULTS = a.more_faults
    FREEZE_CALL, FC_NMAX = a.freeze_call, a.fc_nmax
    if FREEZE_CALL and not FC_PAIRS:
        raise SystemExit("explore.py: --freeze-call needs an image whose manifest lists `freeze` in faults: and has "
                         "two services that talk (manifest_space.py)")
    if MORE_FAULTS and BACKEND == "hvm":
        raise SystemExit("explore.py: --more-faults needs --backend hvx (X= and cf= are stack/hvx_explore tokens)")
    if MORE_FAULTS and a.cf_range != "auto":
        CF_RANGE = tuple(int(v) for v in a.cf_range.split("-"))
    PREEMPT = a.preempt
    PCT_EXITS[:] = [int(v) for v in a.pct_exits.split(",")]
    BUGGIFY = a.buggify
    IMAGE = a.image
    TIMING = a.timing
    TJ_PM = [int(x) for x in a.tj_pm.split(",")]
    os.makedirs(a.out, exist_ok=True)
    t0 = time.time()
    pool = Pool(a.workers, a.out)
    if a.schedule:
        res = pool.run_all([("manual", a.schedule)])
        print(json.dumps(res["manual"][1]))
        pool.close()
        return
    if a.branch:
        branch_main(a, pool, t0)
        return
    if a.sweep:
        if SPACE:
            SPACE.check_sweep(a.sweep)
        points = freeze_sweep(pool, a) if a.sweep == "freeze" else sweep(a.sweep)
        seeds = list(range(1, len(points) + 1))            # sweep point index stands in for the seed
        items = [("seed-%d" % i, render(add_compose(add_turns(add_buggify(add_preempt(add_concurrency(add_timing(sch, i), i), i), i), i), i))) for i, (_, sch) in zip(seeds, points)]
        open(os.path.join(a.out, "sweep-points.jsonl"), "w").write(
            "".join(json.dumps({"seed": i, "point": lab}) + "\n" for i, (lab, _) in zip(seeds, points)))
        if a.sweep == "freeze" and a.sweep_dry:
            open(os.path.join(a.out, "points.txt"), "w").write("".join("seed-%d %s\n" % it for it in zip(seeds, [t for _, t in items])))
            pool.close()
            print(json.dumps({"points": len(points)}))
            return
    else:
        lo, hi = [int(x) for x in a.seeds.split("-")]
        seeds = list(range(lo, hi + 1))
        gen = GENERATORS[a.strategy]
        dur = durable_only if a.durable_only else (lambda s: s)
        nf = (lambda x, sd: x) if a.strategy in ("swarm-pos", "process") else \
            (lambda x, sd: add_new_faults(x, sd, a.strategy == "swarm"))      # M56 generators draw them already
        if MORE_FAULTS and CF_RANGE is None:
            CF_RANGE = measure_cf_range(pool, a.out)
            print("cf= range (measured with cfn): %d-%d" % CF_RANGE, file=sys.stderr)
        mf = lambda x, sd: add_freeze_call(add_more_faults(x, sd, a.strategy), sd)
        items = [("seed-%d" % sd, render(add_compose(add_turns(add_buggify(add_preempt(add_concurrency(add_timing(mf(nf(dur(gen(sd)), sd), sd), sd), sd), sd), sd), sd), sd)))
                 for sd in seeds]
    t_sweep = time.time()
    res = pool.run_all(items)
    t_sweep = time.time() - t_sweep
    runs = open(os.path.join(a.out, "runs.jsonl"), "w")
    fails = []
    for sd in seeds:
        tag = "seed-%d" % sd
        text, r = res[tag]
        s = parse(text)
        rec = {"seed": sd, "schedule": text, "class": finding_class(s), **r}
        rec["assert_failures"] = assert_failures(a.out, tag)
        if rec["assert_failures"] and verdict_kind(r["verdict"]) == "ok":
            # the HYDO-ORACLE line said OK but a property failed: the property wins
            rec["verdict"] = r["verdict"] = "VIOLATION assertion " + ",".join(rec["assert_failures"])
        runs.write(json.dumps(rec) + "\n")
        if verdict_kind(r["verdict"]) != "ok":
            fails.append(rec)
    runs.close()
    # reproduce every failure once (on whichever worker is free -- usually a different one);
    # --sweep freeze (or --repro 2) twice: the second run is "repro2-<seed>"
    nrep = a.repro if a.repro is not None else (2 if a.sweep == "freeze" else 1)
    rep = pool.run_all([("%s-%d" % ("repro" if k == 0 else "repro%d" % (k + 1), f["seed"]), f["schedule"])
                        for k in range(nrep) for f in fails])
    for f in fails:
        rr = rep["repro-%d" % f["seed"]][1]
        f["repro_fp"], f["repro_verdict"] = rr["fp"], rr["verdict"]
        f["reproduced"] = (rr["fp"] == f["fp"])
        for k in range(1, nrep):
            rk = rep["repro%d-%d" % (k + 1, f["seed"])][1]
            f["repro%d_fp" % (k + 1)] = rk["fp"]
            f["reproduced"] = f["reproduced"] and rk["fp"] == f["fp"]
    # shrink one representative per (class, signature)
    reps = {}
    for f in fails:
        key = (f["class"], signature(f["verdict"]).split(" order=")[0] if verdict_kind(f["verdict"]) == "violation" else signature(f["verdict"]))
        reps.setdefault(key, f)
    slog = open(os.path.join(a.out, "shrink.jsonl"), "w")
    shrunk = {}
    if not a.no_shrink:
        for key, f in reps.items():
            if verdict_kind(f["verdict"]) != "violation":
                continue
            m, mv, n = shrink(pool, parse(f["schedule"]), f["verdict"], slog, tjk=f.get("tjk"))
            mt = render(m)
            conf = pool.run_all([("min-a", mt), ("min-b", mt)])
            shrunk[key] = {"from_seed": f["seed"], "minimal": mt, "verdict": mv, "tries": n,
                           "min_class": finding_class(m),
                           "fp_a": conf["min-a"][1]["fp"], "fp_b": conf["min-b"][1]["fp"],
                           "min_verdict": conf["min-a"][1]["verdict"]}
            if "W" in m and SPACE is not None and SPACE.compose:     # the minimal composition, round by round
                shrunk[key]["composition"] = _hc().describe(m["W"], SPACE.compose)
                shrunk[key]["commands"] = [c["id"] for c in SPACE.compose["commands"]]
    slog.close()
    boot = [w.boot_secs for w in pool.workers if w.boot_secs]
    pool.close()
    total = time.time() - t0
    secs = [res["seed-%d" % sd][1]["secs"] for sd in seeds]
    summary = {"schedules": len(seeds), "strategy": ("sweep:" + a.sweep) if a.sweep else a.strategy,
               "image": IMAGE, "workers": a.workers, "sweep_wall_s": round(t_sweep, 1),
               "runs_per_hour": round(len(seeds) / t_sweep * 3600, 1), "mean_run_s": round(sum(secs) / len(secs), 1),
               "worker_boot_s": round(sum(boot) / max(1, len(boot)), 1), "total_wall_s": round(total, 1),
               "failures": len(fails), "reproduced": sum(1 for f in fails if f["reproduced"])}
    if nrep != 1:
        summary["repro_runs"] = nrep
    if RAM_MIB != 1024:
        summary["ram_mib"] = RAM_MIB
    if VCPUS != 1:
        summary["vcpus"] = VCPUS
    if CPU != "v2":
        summary["cpu"] = CPU
    json.dump({"summary": summary, "failures": fails,
               "shrunk": [{"class": k[0], "signature": k[1], **v} for k, v in shrunk.items()]},
              open(os.path.join(a.out, "report.json"), "w"), indent=1)
    table = property_table(a.out, ["seed-%d" % sd for sd in seeds])
    if table is not None:
        open(os.path.join(a.out, "properties.txt"), "w").write(table)
    write_report(a.out, summary, fails, shrunk, table)
    print(json.dumps(summary))


def branch_main(a, pool, t0):
    """--branch: each seed's schedule (without timing) runs once to the
    marker; n timing variants then start from a snapshot there. Every failing variant is re-run
    as its standalone schedule (rb=16, tjlo=..) to check it reproduces from S1 too."""
    lo, hi = [int(x) for x in a.seeds.split("-")]
    gen = GENERATORS[a.strategy]
    pm = TJ_PM[len(TJ_PM) // 2]
    items = []
    for sd in range(lo, hi + 1):
        s = gen(sd)
        items.append(("seed-%d" % sd, "BRANCH %d %d %d %s %s" % (a.variants, pm, TJ_MAX, a.branch, render(s))))
    t1 = time.time()
    res = pool.run_all(items)
    t_sweep = time.time() - t1
    runs = open(os.path.join(a.out, "runs.jsonl"), "w")
    fails, nrun, nbp, secs = [], 0, 0, 0.0
    for tag, _ in items:
        text, r = res[tag]
        nbp += r.get("tjlo") is not None
        for sub, st, rr in r.get("runs", []):
            nrun += 1
            secs += rr["secs"]
            rec = {"tag": sub, "schedule": st, "class": finding_class(parse(st)), "branch_tjlo": r.get("tjlo"),
                   "branch_held": r.get("held"), **rr}
            runs.write(json.dumps(rec) + "\n")
            if verdict_kind(rr["verdict"]) != "ok":
                fails.append(rec)
    runs.close()
    rep = pool.run_all([("repro-%d" % i, f["schedule"]) for i, f in enumerate(fails)])
    for i, f in enumerate(fails):
        rr = rep["repro-%d" % i][1]
        f["repro_fp"], f["repro_verdict"] = rr["fp"], rr["verdict"]
        f["reproduced"] = rr["fp"] == f["fp"]
    base_sig = {}
    for f in fails:
        if f["tag"].endswith("-base"):
            base_sig[f["tag"][:-5]] = signature(f["verdict"])
    for f in fails:     # a variant that fails while its base run passes (or fails differently) needs the timing
        f["timing_only"] = not f["tag"].endswith("-base") and \
            base_sig.get(f["tag"].rsplit("-v", 1)[0]) != signature(f["verdict"])
    summary = {"schedules": len(items), "branch_points": nbp, "runs": nrun, "variants_per_point": a.variants,
               "tjpm": pm, "sweep_wall_s": round(t_sweep, 1), "runs_per_hour": round(nrun / max(t_sweep, 1) * 3600, 1),
               "mean_run_s": round(secs / max(nrun, 1), 1), "failures": len(fails),
               "reproduced_standalone": sum(1 for f in fails if f["reproduced"]),
               "timing_only_failures": sum(1 for f in fails if f["timing_only"]),
               "total_wall_s": round(time.time() - t0, 1)}
    if RAM_MIB != 1024:
        summary["ram_mib"] = RAM_MIB
    if VCPUS != 1:
        summary["vcpus"] = VCPUS
    if CPU != "v2":
        summary["cpu"] = CPU
    json.dump({"summary": summary, "failures": fails}, open(os.path.join(a.out, "report.json"), "w"), indent=1)
    L = ["# Branch report (marker `%s`)" % a.branch, "", "| | |", "|---|---|"]
    L += ["| %s | %s |" % kv for kv in summary.items()]
    L += ["", "## Failures", "", "| run | class | timing only | verdict | reproduced standalone |", "|---|---|---|---|---|"]
    for f in fails:
        L.append("| %s | %s | %s | `%s` | %s |" % (f["tag"], f["class"], "yes" if f["timing_only"] else "no",
                                               f["verdict"][:90], "yes" if f["reproduced"] else "NO"))
    open(os.path.join(a.out, "report.md"), "w").write("\n".join(L) + "\n")
    pool.close()
    print(json.dumps(summary))


def write_report(out, summary, fails, shrunk, table=None):
    L = ["# Explorer report", "", "| | |", "|---|---|"]
    for k, v in summary.items():
        L.append("| %s | %s |" % (k, v))
    by = {}
    for f in fails:
        k = (f["class"], signature(f["verdict"]).split(" order=")[0] if verdict_kind(f["verdict"]) == "violation" else signature(f["verdict"]))
        by.setdefault(k, []).append(f)
    L += ["", "## Failures by class and signature", "", "| class | signature | runs | seeds | reproduced |", "|---|---|---|---|---|"]
    for (c, sig), fs in sorted(by.items()):
        L.append("| %s | `%s` | %d | %s | %d/%d |" % (c, sig, len(fs), " ".join(str(f["seed"]) for f in fs[:8]),
                                                     sum(1 for f in fs if f["reproduced"]), len(fs)))
    L += ["", "## Minimal reproducers", ""]
    for (c, sig), v in shrunk.items():
        mc = v.get("min_class", c)
        head = mc if mc == c else "%s (found as %s; the minimal schedule needs only %s)" % (mc, c, mc)
        L += ["### %s: `%s`" % (head, sig), "",
              "- from seed %d, %d shrink runs" % (v["from_seed"], v["tries"]),
              "- minimal schedule: `%s`" % v["minimal"],
              "- needs a tick-timing change: %s" % ("yes" if " tj" in v["minimal"] else "no"),
              ] + (["- composition (test composer, W=): %s" % v["composition"]] if v.get("composition") else []) + (["- needs vCPU turn lengths: %s" % ("yes" if re.search(r"(?:^|\s)(?:qs?|qb)=", v["minimal"].split("|")[0]) else "no")]
                   if VCPUS > 1 else []) + [
              "- verdict: `%s`" % v["min_verdict"],
              "- fingerprint, two runs: `%s` / `%s` (%s)" % (v["fp_a"], v["fp_b"], "identical" if v["fp_a"] == v["fp_b"] else "DIFFERENT"),
              "- by hand: `~/bin/hvx-run sudo ./hvx_explore %sone ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/%s <outdir> \"%s\"`" % ("".join(x + " " for x in engine_args()), IMAGE, v["minimal"])
              if BACKEND != "hvm" and not os.environ.get("HYDO_CLI") else
              "- by hand: `hydo run --image %s \"%s\"`" % (os.path.join(BUILD, IMAGE), v["minimal"]) if BACKEND != "hvm" else "- by hand: `%s`" % hvm_backend.repro_line(IMAGE, v["minimal"], HVM_KERNEL, ram_args()), ""]
    L += ["## Property table over every schedule (hvx/ASSERTIONS.md)", ""]
    if table is None:
        L += ["The guest emitted no HYDO-ASSERT records (an image built without the assertion SDK).", ""]
    else:
        L += ["```", table.rstrip(), "```", ""]
        L += liveness_summary(out)
    open(os.path.join(out, "report.md"), "w").write("\n".join(L) + "\n")


if __name__ == "__main__":
    main()
