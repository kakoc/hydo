"""gRPC-level faults for the explorer (explore.py --grpc-faults; MANIFEST.md, "gRPC faults").

On a manifest with a gRPC proxy (proxies: with protocol: grpc), --grpc-faults adds H= tokens with
the gRPC rules of hydo-world's streaming proxy (hydo/onboard/world/grpc.go): forward then drop the
answer, an injected status with or without the server having acted, a delay past the caller's
deadline, a stream or connection reset, a stream cut or held after n messages; each one aimed at a
method's calls in a window of workload steps, at one call index, or at a seeded share of the calls.

The tokens use the H= syntax of --protocol-faults (explore.py add_protocol_faults, render_h,
parse_h), so they render, parse, restrict and shrink with the same code; this module adds the
drawing, its own random stream (GF_SALT: every value is drawn whether or not it is used, so every
other field of a seed's schedule stays what it was without the flag) and shrink steps for the
gRPC-only keys. Without --grpc-faults nothing here runs and schedule strings are unchanged.
"""

ON = False
GF_SALT = 0x475250435F464C54          # "GRPC_FLT"
GF_P = [60, 20]                       # % of schedules with a first / a second gRPC fault
GF_KINDS = ["drop", "drop", "status", "acted", "delay", "reset", "conn", "cut", "hold"]
GF_STATUS = ["UNAVAILABLE", "DEADLINE_EXCEEDED", "RESOURCE_EXHAUSTED", "INTERNAL", "ABORTED"]
GF_DELAYS = [500, 3000, 30000]        # ms of guest time; 30 s is past most callers' deadlines
GF_NMSG = [0, 1, 1, 2, 5]             # cut= / hold= after this many messages
GF_LENS = [1, 1, 2, 3, 0]             # window length in steps (0 = to the end of the workload)
GF_SEL = ["all", "all", "call", "share"]
TARGETS = []                          # gRPC proxies and their routes (targets_of sets it)


def targets_of(m):
    """H= targets of the manifest's gRPC proxies: (target, [method paths of its routes]).
    Empty when the manifest's faults: list leaves out `protocol` (as manifest_space._pf_targets)."""
    if m is None or ("faults" in m and "protocol" not in (m.get("faults") or [])):
        return []
    out = []
    for e in m.get("proxies") or []:
        if e.get("protocol", "http") != "grpc":
            continue
        routes = e.get("routes") or []
        out.append((e["name"], [r.get("path") for r in routes if r.get("path") and "*" not in r["path"]]))
        out += [("%s.%s" % (e["name"], r["name"]), []) for r in routes]
    return out


def add(s, seed, SplitMix, norders):
    """Append up to two gRPC fault tokens to s["H"] (after any --protocol-faults ones)."""
    if not ON or not TARGETS:
        return s
    t = SplitMix(seed ^ GF_SALT)
    hs = []
    for k in range(2):
        on = t.below(100) < GF_P[k]
        tg, kind = t.pick(TARGETS), t.pick(GF_KINDS)
        a, ln = t.below(norders), t.pick(GF_LENS)
        st, dl, nm, dreq = t.pick(GF_STATUS), t.pick(GF_DELAYS), t.pick(GF_NMSG), t.below(100) < 30
        sel, coff, p, sd, rix = t.pick(GF_SEL), t.below(3), t.pick([50, 25]), t.below(1000), t.below(1 << 30)
        if not on:
            continue
        b = norders if ln == 0 else min(norders, a + ln)
        rules = {"drop": [("drop", None)],
                 "status": [("status", st)],
                 "acted": [("status", st), ("effect", "yes")],
                 "delay": [("delay", dl)],
                 "reset": [("reset", None)],
                 "conn": [("reset", "conn")],
                 "cut": [("cut", nm)] + ([("dir", "req")] if dreq else []),
                 "hold": [("hold", nm)] + ([("dir", "req")] if dreq else [])}[kind]
        name, paths = tg
        if paths:                      # a proxy-level target: aim at one of its methods
            rules.append(("rpc", paths[rix % len(paths)].rsplit("/", 1)[-1]))
        if sel == "call":              # one call: the call index the window's first step makes
            rules.append(("call", a + 1 + coff))
        elif sel == "share":
            rules += [("p", p), ("seed", sd)]
        hs.append((name, a, b, tuple(rules)))
    if hs:
        s["H"] = list(s.get("H") or []) + hs
    return s


GRPC_KEYS = ("rpc", "call", "seed", "cut", "hold", "dir")


def candidates(s, var):
    """Shrink steps for the gRPC-only keys of every H= token: without call= / rpc=, without the
    seeded share (p= and seed= together), a connection reset made a stream reset, a request-side
    cut/hold made a response-side one, cut=/hold= after fewer messages."""
    hs = s.get("H") or []
    for i, (tg, a, b, rules) in enumerate(hs):
        if not any(k in GRPC_KEYS or (k == "reset" and v == "conn") for k, v in rules):
            continue
        put = lambda r: var(H=hs[:i] + [(tg, a, b, tuple(r))] + hs[i + 1:])
        for k in ("call", "rpc"):
            if any(x == k for x, _ in rules):
                put([r for r in rules if r[0] != k])
        if any(x == "seed" for x, _ in rules):
            put([r for r in rules if r[0] not in ("p", "seed")])
        if ("reset", "conn") in rules:
            put([("reset", None) if r == ("reset", "conn") else r for r in rules])
        if ("dir", "req") in rules:
            put([r for r in rules if r != ("dir", "req")])
        for k in ("cut", "hold"):
            for x, v in rules:
                if x == k and isinstance(v, int) and v > 0:
                    put([(x2, v // 2 if x2 == k else v2) for x2, v2 in rules])
