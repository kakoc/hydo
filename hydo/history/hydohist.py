"""hydohist -- operation histories recorded by guest programs, and the host-side checkers.

A workload client records every operation it performs as HYDO-HIST lines on the guest console
(format: hvx/HISTORIES.md). After the run the HOST reads the saved consoles (the workload VM's
and, after a power cut, the recovery VM's), rebuilds the history and checks it:

    linearizable   a key-value register / compare-and-set object, checked per key
                   (P-compositionality) with the Wing-Gong-Lowe search Porcupine and Knossos use;
                   unknown-outcome operations may take effect at any time after they began, or
                   never; optional store-assigned versions (etcd mod_revision and the like)
    list-append    Elle-style: lists per key that clients append unique elements to and read
                   whole, in single operations or multi-key transactions; direct checks (lost
                   acknowledged appends, duplicates, incompatible orders, aborted/garbage reads,
                   reads going back in time per process, read-your-writes) and a dependency-graph
                   cycle search (G0, G1c, G-single, G2, with real-time / process edges for the
                   strict and strong-session levels)

Each check yields a verdict (ok / violation / undecided) and a minimal explanation: for
linearizability the smallest set of operations of one key that cannot be ordered, for
list-append the anomaly with the operations involved (a cycle for G0..G2). run_checks() turns
the results into HYDO-ASSERT records and a combined HYDO-ORACLE verdict, so explore.py, the
shrinker and the reports work unchanged.

Ordering: events are ordered by their POSITION in the console (or file), not by their time
stamps. Every record is one write(), so if the completion of A was printed before the
invocation of B, A really completed before B began. Two consoles (workload, then recovery
boot) are concatenated in that order; guest time restarts in the recovery boot, position does
not. Stdlib only (the build boxes have no extra Python packages); Python 3.6+.
"""
import json
import re
import sys

TAG = "HYDO-HIST "
CHECK_TAG = "HYDO-CHECK "
FINAL_TAG = "HYDO-CHECK-FINAL "
INF = float("inf")
NAME_RE = re.compile(r"^[A-Za-z0-9_.\-]+$")
TYPES = ("invoke", "ok", "fail", "info")
LEVELS = ("strict-serializable", "strong-session-serializable", "serializable",
          "strong-snapshot-isolation", "strong-session-snapshot-isolation", "snapshot-isolation")
DEFAULT_BUDGET = 2000000          # WGL search steps per key before a key counts as undecided


# ------------------------------------------------------------------ encoding
def pdec(tok):
    """Percent-decode one token (%xx = the byte xx; the result is read as UTF-8). The token "%"
    alone is the empty string."""
    if "%" not in tok:
        return tok
    if tok == "%":
        return ""
    raw = re.sub(rb"%([0-9A-Fa-f]{2})", lambda m: bytes([int(m.group(1), 16)]), tok.encode("utf-8"))
    return raw.decode("utf-8", "replace")


def penc(s, extra=""):
    """Percent-encode the bytes that would break a token: '%', whitespace, control characters,
    '@' (the version separator) and any character in `extra` (',[]' inside lists, ':' in txns).
    The empty string becomes "%"."""
    if str(s) == "":
        return "%"
    if str(s) == "nil":
        return "%6Eil"           # the token nil means "absent"
    out = []
    for ch in str(s):
        if ch == "%" or ch == "@" or ch.isspace() or ord(ch) < 0x20 or ch in extra:
            out.append("".join("%%%02X" % x for x in ch.encode("utf-8")))
        else:
            out.append(ch)
    return "".join(out)


# ------------------------------------------------------------------ parsing
class Event(object):
    __slots__ = ("idx", "hist", "proc", "type", "time", "f", "args", "src", "line")

    def __init__(self, idx, hist, proc, typ, time, f, args, src, line):
        self.idx, self.hist, self.proc, self.type, self.time = idx, hist, proc, typ, time
        self.f, self.args, self.src, self.line = f, args, src, line


def parse_sources(sources):
    """sources: [(label, text)] in run order (workload console first, recovery console second).
    Returns (events, specs, warnings): events in order, check specs (HYDO-CHECK lines, first
    occurrence of each id wins; "_final_rc" = the exit status its final: command reported in a
    HYDO-CHECK-FINAL line), and a list of malformed-line warnings."""
    events, specs, warn = [], {}, []
    order, finals = [], {}
    idx = 0
    for label, text in sources:
        for ln, line in enumerate(text.splitlines(), 1):
            k = line.find(FINAL_TAG)
            if k >= 0:
                try:
                    fr = json.loads(line[k + len(FINAL_TAG):].strip())
                    finals[fr["id"]] = int(fr.get("rc", 0))
                except (ValueError, KeyError, TypeError):
                    warn.append("%s:%d: bad HYDO-CHECK-FINAL line" % (label, ln))
                continue
            k = line.find(CHECK_TAG)
            if k >= 0:
                try:
                    sp = json.loads(line[k + len(CHECK_TAG):].strip())
                    if isinstance(sp, dict) and "id" in sp and sp["id"] not in specs:
                        specs[sp["id"]] = sp
                        order.append(sp["id"])
                except ValueError:
                    warn.append("%s:%d: bad HYDO-CHECK line" % (label, ln))
                continue
            k = line.find(TAG)
            if k < 0:
                continue
            tok = line[k + len(TAG):].split()
            if len(tok) < 5 or tok[2] not in TYPES or not NAME_RE.match(tok[0]):
                warn.append("%s:%d: malformed HYDO-HIST record" % (label, ln))
                continue
            try:
                t = None if tok[3] == "-" else int(tok[3])
            except ValueError:
                warn.append("%s:%d: bad time %r" % (label, ln, tok[3]))
                continue
            events.append(Event(idx, tok[0], tok[1], tok[2], t, tok[4], tok[5:], label, ln))
            idx += 1
    for i, rc in finals.items():          # the exit status of the check's final: (last one)
        if i in specs:
            specs[i]["_final_rc"] = rc
    return events, [specs[i] for i in order], warn


class Op(object):
    """One operation: an invocation and (maybe) its completion."""
    __slots__ = ("n", "proc", "f", "inv", "out", "status", "call", "ret", "ev_inv", "ev_done")

    def __init__(self, n, ev):
        self.n, self.proc, self.f, self.inv = n, ev.proc, ev.f, ev.args
        self.out, self.status, self.call, self.ret = None, "info", ev.idx, INF
        self.ev_inv, self.ev_done = ev, None

    def where(self):
        """'t=12.34-12.41s' (guest time), with '(recovery boot)' for the second console."""
        def ts(e):
            return "?" if e is None or e.time is None else "%.2f" % (e.time / 1e9)
        s = "t=%s-%ss" % (ts(self.ev_inv), ts(self.ev_done) if self.ev_done else "?")
        if self.ev_inv.src and self.ev_inv.src.endswith("recovery"):
            s += " (recovery boot)"
        return s


def pair_ops(events, hist):
    """Pair invocations with completions per process (Jepsen's rule: a process has at most one
    operation in flight). An invocation still open when the same process invokes again, or at
    the end of the history, has an unknown outcome (info)."""
    ops, open_, warn = [], {}, []
    for ev in events:
        if ev.hist != hist:
            continue
        if ev.type == "invoke":
            if ev.proc in open_:
                open_.pop(ev.proc)            # stays info: never completed
            op = Op(len(ops), ev)
            ops.append(op)
            open_[ev.proc] = op
            continue
        op = open_.pop(ev.proc, None)
        if op is None:
            warn.append("%s:%d: %s %s by process %s without an open invocation (ignored)" % (
                ev.src, ev.line, ev.type, ev.f, ev.proc))
            continue
        if ev.f != op.f:
            warn.append("%s:%d: completion %s does not match the invocation %s (ignored)" % (ev.src, ev.line, ev.f, op.f))
            open_[ev.proc] = op
            continue
        op.status, op.ev_done = ev.type, ev
        op.out = ev.args
        op.ret = ev.idx if ev.type in ("ok", "fail") else INF
    return ops, warn


# ------------------------------------------------------------------ register / CAS model
NIL = None
ANY = ("<any>",)          # initial state "unknown": the first observation defines it


def parse_val(tok):
    """value token -> (value or None for nil, version or None). 'v77@83' = value v77, version 83."""
    if tok == "nil":
        return (None, None)
    m = re.match(r"^(.*)@(\d+)$", tok)
    if m:
        return (None if m.group(1) == "nil" else pdec(m.group(1)), int(m.group(2)))
    return (pdec(tok), None)


class ROp(object):
    """A register operation on one key."""
    __slots__ = ("op", "kind", "val", "ver", "old", "info", "call", "ret")

    def __init__(self, op, kind, val=None, ver=None, old=None, call=None, ret=None):
        self.op, self.kind, self.val, self.ver, self.old = op, kind, val, ver, old
        self.info = op.status == "info"
        self.call = op.call if call is None else call
        self.ret = op.ret if ret is None else ret


class HistError(Exception):
    pass


def register_ops(ops, allow_cas):
    """Ops -> {key: [ROp]}. Failed operations had no effect and are dropped; reads with an
    unknown outcome tell nothing and are dropped; writes/CAS with an unknown outcome stay with
    an infinite return time (they may take effect at any point after they began, or never)."""
    per = {}
    for op in ops:
        if op.status == "fail":
            continue
        a = op.out if op.status == "ok" and op.out else op.inv
        if op.f == "read":
            if op.status != "ok":
                continue
            if not a or len(a) % 2:
                raise HistError("read by process %s: an ok read lists <key> <value> pairs (got %r)" % (op.proc, a))
            seen = set()
            for i in range(0, len(a), 2):
                k = pdec(a[i])
                if k in seen:
                    continue
                seen.add(k)
                v, ver = parse_val(a[i + 1])
                per.setdefault(k, []).append(ROp(op, "r", v, ver))
        elif op.f == "write":
            if len(a) < 2:
                raise HistError("write by process %s needs <key> <value> (got %r)" % (op.proc, a))
            v, ver = parse_val(a[1])
            if op.status != "ok":
                ver = None
            per.setdefault(pdec(a[0]), []).append(ROp(op, "w", v, ver))
        elif op.f == "cas":
            if not allow_cas:
                raise HistError("cas by process %s in a history checked with model: register (use model: cas)" % op.proc)
            if len(a) < 3:
                raise HistError("cas by process %s needs <key> <old> <new> (got %r)" % (op.proc, a))
            old, _ = parse_val(a[1])
            v, ver = parse_val(a[2])
            if op.status != "ok":
                ver = None
            per.setdefault(pdec(a[0]), []).append(ROp(op, "c", v, ver, old=old))
        else:
            raise HistError("operation %r is not part of the register model (read, write, cas)" % op.f)
    return per


def reg_step(state, o):
    """The register's sequential specification. state = (value, version) or ANY. Returns the
    new state, or None if o cannot take effect in this state."""
    if state is ANY:
        if o.kind == "r":
            return (o.val, o.ver)
        if o.kind == "w":
            return (o.val, o.ver)
        return (o.val, o.ver) if not o.info else ANY    # cas on an unknown value: assume it matched
    sval, sver = state
    if o.kind == "r":
        if o.val != sval:
            return None
        if o.ver is not None:
            if sver is None:
                return (sval, o.ver)          # the read tells the version of the value in force
            if sver != o.ver:
                return None
        return state
    if o.kind == "w":
        if o.ver is not None and sver is not None and o.ver <= sver:
            return None                       # store-assigned versions increase
        return (o.val, o.ver)
    # cas: ok = it swapped; unknown outcome = whatever the state dictates
    if sval != o.old:
        return state if o.info else None
    if o.ver is not None and sver is not None and o.ver <= sver:
        return None
    return (o.val, o.ver)


def wgl(rops, init, budget=DEFAULT_BUDGET):
    """Wing-Gong-Lowe linearizability search with Lowe's memoization (as in Porcupine).
    Returns True (linearizable), False (not), or None (budget exhausted)."""
    n = len(rops)
    if n == 0:
        return True
    ev = []
    for i, o in enumerate(rops):
        ev.append((o.call, 0, i))
        ev.append((o.ret if o.ret != INF else INF, 1, i))
    ev.sort(key=lambda e: (e[0], e[1], rops[e[2]].call))
    m = len(ev)
    HEAD, TAIL = m, m + 1
    nxt = [0] * (m + 2)
    prv = [0] * (m + 2)
    is_call = [False] * m
    opi = [0] * m
    match = [0] * m
    callnode = [0] * n
    retnode = [0] * n
    for j, (_, r, i) in enumerate(ev):
        is_call[j] = r == 0
        opi[j] = i
        if r == 0:
            callnode[i] = j
        else:
            retnode[i] = j
    for i in range(n):
        match[callnode[i]] = retnode[i]
    order = [HEAD] + list(range(m)) + [TAIL]
    for a, b in zip(order, order[1:]):
        nxt[a] = b
        prv[b] = a

    def lift(e):
        r = match[e]
        nxt[prv[e]] = nxt[e]; prv[nxt[e]] = prv[e]
        nxt[prv[r]] = nxt[r]; prv[nxt[r]] = prv[r]

    def unlift(e):
        r = match[e]
        nxt[prv[r]] = r; prv[nxt[r]] = r
        nxt[prv[e]] = e; prv[nxt[e]] = e

    cache = set()
    calls = []
    state, lin = init, 0
    entry = nxt[HEAD]
    steps = 0
    while nxt[HEAD] != TAIL:
        steps += 1
        if steps > budget:
            return None
        if entry == TAIL:                     # cannot happen: every call precedes its return
            return False
        if is_call[entry]:
            i = opi[entry]
            ns = reg_step(state, rops[i])
            if ns is not None:
                nl = lin | (1 << i)
                key = (nl, ns)
                if key not in cache:
                    cache.add(key)
                    calls.append((entry, state))
                    state, lin = ns, nl
                    lift(entry)
                    entry = nxt[HEAD]
                    continue
            entry = nxt[entry]
        else:
            if not calls:
                return False
            entry, state = calls.pop()
            lin &= ~(1 << opi[entry])
            unlift(entry)
            entry = nxt[entry]
    return True


def _truncate(rops, t):
    """The history as it was known at position t: operations invoked before t; those still open
    at t have an unknown outcome (reads dropped, writes/CAS may take effect later or never)."""
    out = []
    for o in rops:
        if o.call >= t:
            continue
        if o.ret != INF and o.ret < t:
            out.append(o)
        elif o.kind != "r":
            c = ROp(o.op, o.kind, o.val, None, o.old, o.call, INF)
            c.info = True
            out.append(c)
    return out


def earliest_violation(rops, init, budget):
    """Binary search for the first position at which the history known so far is already not
    linearizable (monotone: a later truncation contains every constraint of an earlier one).
    Returns that truncation, or the full history if a search runs out of budget."""
    pts = sorted(set(o.ret for o in rops if o.ret != INF))
    lo, hi = 0, len(pts) - 1
    best = None
    while lo <= hi:
        mid = (lo + hi) // 2
        tr = _truncate(rops, pts[mid] + 1)
        r = wgl(tr, init, budget)
        if r is None:
            return rops
        if r is False:
            best, hi = tr, mid - 1
        else:
            lo = mid + 1
    return best if best is not None else rops


def _observers(ops, vals):
    return set(id(o) for o in ops if (o.kind == "r" and o.val in vals) or
               (o.kind == "c" and not o.info and o.old in vals))


def minimize(rops, init, budget):
    """A small witness: first the earliest truncation that is already not linearizable, then
    delta debugging over it -- remove chunks of operations (earliest first; halves, quarters,
    ... single ones) together with the reads that observe the values the chunk's writes
    produced, while the rest is still not linearizable. Removing reads, and writes no remaining
    operation observes, keeps a WITNESS: any linearization of the full history restricted to the
    rest is a linearization of the rest (every remaining read still follows the write it saw), so
    'the rest cannot be ordered' implies 'the history cannot be ordered'."""
    cur = sorted(earliest_violation(rops, init, budget), key=lambda x: x.call)
    tries = 0
    size = max(1, len(cur) // 2)
    while tries < 3000:
        i, changed = 0, False
        while i < len(cur) and tries < 3000:
            chunk = cur[i:i + size]
            vals = set(o.val for o in chunk if o.kind in ("w", "c"))
            drop = set(id(o) for o in chunk) | _observers(cur, vals)
            cand = [o for o in cur if id(o) not in drop]
            tries += 1
            if cand and wgl(cand, init, budget) is False:
                cur, changed = cand, True
                continue
            i += size
        if size == 1 and not changed:
            break
        if not changed:
            size = max(1, size // 2)
    return cur


def _vs(val, ver):
    s = "nil" if val is None else str(val)
    return s + ("@%d" % ver if ver is not None else "")


def describe_rop(key, o):
    st = {"ok": "ok", "info": "unknown outcome", "fail": "failed"}[o.op.status]
    if o.kind == "r":
        what = "read %s -> %s" % (key, _vs(o.val, o.ver))
    elif o.kind == "w":
        what = "write %s=%s" % (key, _vs(o.val, o.ver))
    else:
        what = "cas %s %s->%s" % (key, _vs(o.old, None), _vs(o.val, o.ver))
    return "p%s %s (%s, %s)" % (o.op.proc, what, st, o.op.where())


def explain_register(key, mins):
    """One sentence for the common shapes, then the operations."""
    ops = "; ".join(describe_rop(key, o) for o in mins)
    ws = [o for o in mins if o.kind in ("w", "c")]
    rs = [o for o in mins if o.kind == "r"]
    if len(rs) == 1 and ws and all(w.ret != INF and w.ret < rs[0].call for w in ws):
        w, r = max(ws, key=lambda x: x.ret), rs[0]
        if r.val == w.val and r.ver is not None and w.ver is not None:
            head = ("key %s: the write was acknowledged with version %d, a later read returned version %d"
                    % (key, w.ver, r.ver))
        else:
            head = ("key %s: an acknowledged write is lost or a read is stale: a read that began after "
                    "the write of %s completed returned %s%s" % (key, _vs(w.val, w.ver), _vs(r.val, r.ver),
                                                           " (an older value)" if len(ws) > 1 else ""))
    else:
        head = "key %s: these %d operations cannot be ordered consistently with real time and a register" % (key, len(mins))
    return head + ": " + ops


def check_linearizable(ops, spec, budget=DEFAULT_BUDGET):
    """-> dict(status, summary, explanation, details)."""
    model = spec.get("model", "register")
    if model not in ("register", "cas"):
        raise HistError("model %r: register or cas" % model)
    init = ANY if spec.get("initial") == "any" else (None, None)
    per = register_ops(ops, allow_cas=(model == "cas"))
    nops = len([o for o in ops if o.status != "fail"])
    undecided, bad = [], []
    for key in sorted(per):
        r = wgl(per[key], init, budget)
        if r is None:
            undecided.append(key)
        elif r is False:
            bad.append(key)
    det = {"ops": nops, "keys": len(per)}
    if bad:
        # report the key whose minimal witness begins earliest (deterministic)
        best = None
        for key in bad:
            mins = minimize(per[key], init, budget)
            k = (mins[0].call, key)
            if best is None or k < best[0]:
                best = (k, key, mins)
        _, key, mins = best
        det.update({"key": key, "nonlinearizable_keys": len(bad), "witness_ops": len(mins)})
        return {"status": "violation", "details": det,
                "summary": explain_register(key, mins) + ("" if len(bad) == 1 else " (%d keys not linearizable)" % len(bad)),
                "explanation": "\n".join(["not linearizable: %d of %d keys" % (len(bad), len(per)),
                                          "keys: " + " ".join(bad[:50]),
                                          "witness (these operations alone cannot be ordered; operations are removed while that stays true):"] +
                                         ["  " + describe_rop(key, o) for o in mins])}
    if undecided:
        det["undecided_keys"] = len(undecided)
        return {"status": "undecided", "details": det,
                "summary": "search budget exhausted on %d keys (first %s)" % (len(undecided), undecided[0]),
                "explanation": "undecided keys: " + " ".join(undecided[:50])}
    return {"status": "ok", "details": det,
            "summary": "%d operations on %d keys linearizable" % (nops, len(per)), "explanation": ""}


# ------------------------------------------------------------------ list-append (Elle-style)
def parse_list(tok):
    if tok in ("nil", "[]"):
        return []
    if not (tok.startswith("[") and tok.endswith("]")):
        raise HistError("a list value is [e1,e2,...] (got %r)" % tok)
    body = tok[1:-1]
    return [pdec(x) for x in body.split(",")] if body else []


def parse_mops(op):
    """-> list of ('a', key, elem) / ('r', key, list or None). Uses the completion's micro-ops
    for an ok operation (they carry the read results), the invocation's otherwise."""
    a = op.out if op.status == "ok" and op.out else op.inv
    if op.f == "append":
        if len(a) < 2:
            raise HistError("append by process %s needs <key> <element>" % op.proc)
        return [("a", pdec(a[0]), pdec(a[1]))]
    if op.f == "read":
        if not a:
            raise HistError("read by process %s needs <key>" % op.proc)
        return [("r", pdec(a[0]), parse_list(a[1]) if op.status == "ok" and len(a) > 1 else None)]
    if op.f == "txn":
        mops = []
        for t in a:
            parts = t.split(":")
            if parts[0] == "a" and len(parts) == 3:
                mops.append(("a", pdec(parts[1]), pdec(parts[2])))
            elif parts[0] == "r" and len(parts) in (2, 3):
                mops.append(("r", pdec(parts[1]), parse_list(parts[2]) if op.status == "ok" and len(parts) == 3 else None))
            else:
                raise HistError("txn micro-op %r: a:<key>:<elem>, r:<key> or r:<key>:[...]" % t)
        return mops
    raise HistError("operation %r is not part of the list-append model (append, read, txn)" % op.f)


class Txn(object):
    __slots__ = ("op", "mops", "n")

    def __init__(self, n, op, mops):
        self.n, self.op, self.mops = n, op, mops

    def label(self):
        def mo(m):
            if m[0] == "a":
                return "append %s %s" % (m[1], m[2])
            return "read %s %s" % (m[1], "?" if m[2] is None else "[" + ",".join(m[2]) + "]")
        st = {"ok": "ok", "info": "unknown outcome", "fail": "failed"}[self.op.status]
        return "T%d p%s %s (%s, %s)" % (self.n, self.op.proc, "; ".join(mo(m) for m in self.mops), st, self.op.where())


def _level(spec):
    lv = spec.get("consistency", "strict-serializable")
    if lv not in LEVELS:
        raise HistError("consistency %r: one of %s" % (lv, ", ".join(LEVELS)))
    si = "snapshot-isolation" in lv
    rt = lv.startswith("strict") or lv.startswith("strong-snapshot")
    proc = rt or lv.startswith("strong-session")
    return lv, si, rt, proc


def _shortest_cycle(nodes, adj, allowed, must=None):
    """Shortest cycle within `nodes` using edge types in `allowed`. With must=<type>: the
    cycle contains exactly one edge of that type and the rest from `allowed` minus it
    (G-single), or at least one (must=('rw+',)). Returns [(u, v, type), ...] or None."""
    best = None
    from collections import deque
    pri = {"ww": 0, "wr": 1, "rw": 2, "proc": 3, "rt": 4}

    def pick(ts):
        return sorted(ts, key=lambda t: pri.get(t, 9))[0]

    def bfs(src, dst, types):
        prev = {src: None}
        dq = deque([src])
        while dq:
            u = dq.popleft()
            if u == dst and u != src:
                break
            for v in sorted(adj.get(u, {})):
                if v not in nodes or v in prev:
                    continue
                ts = adj[u][v] & types
                if not ts:
                    continue
                prev[v] = (u, pick(ts))
                if v == dst:
                    dq.clear()
                    break
                dq.append(v)
        if dst not in prev or dst == src:
            return None
        path, v = [], dst
        while prev[v] is not None:
            u, t = prev[v]
            path.append((u, v, t))
            v = u
        return path[::-1]

    if must is None:
        for u in sorted(nodes):
            for v in sorted(adj.get(u, {})):
                if v not in nodes:
                    continue
                ts = adj[u][v] & allowed
                if not ts:
                    continue
                if v == u:
                    continue
                back = bfs(v, u, allowed)
                if back is not None:
                    cyc = [(u, v, pick(ts))] + back
                    if best is None or len(cyc) < len(best):
                        best = cyc
        return best
    rest = allowed - {"rw"} if must == "rw1" else allowed | {"rw"}
    for u in sorted(nodes):
        for v in sorted(adj.get(u, {})):
            if v not in nodes or v == u or "rw" not in adj[u][v]:
                continue
            back = bfs(v, u, rest)
            if back is not None:
                cyc = [(u, v, "rw")] + back
                if best is None or len(cyc) < len(best):
                    best = cyc
    return best


def _sccs(nodes, adj, allowed):
    """Tarjan (iterative). Returns SCCs with more than one node, each a sorted list."""
    index, low, on, st, out = {}, {}, set(), [], []
    counter = [0]
    for s in sorted(nodes):
        if s in index:
            continue
        work = [(s, iter(sorted(v for v in adj.get(s, {}) if adj[s][v] & allowed)))]
        index[s] = low[s] = counter[0]; counter[0] += 1
        st.append(s); on.add(s)
        while work:
            u, it = work[-1]
            pushed = False
            for v in it:
                if v not in index:
                    index[v] = low[v] = counter[0]; counter[0] += 1
                    st.append(v); on.add(v)
                    work.append((v, iter(sorted(w for w in adj.get(v, {}) if adj[v][w] & allowed))))
                    pushed = True
                    break
                elif v in on:
                    low[u] = min(low[u], index[v])
            if pushed:
                continue
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[u])
            if low[u] == index[u]:
                comp = []
                while True:
                    w = st.pop(); on.discard(w); comp.append(w)
                    if w == u:
                        break
                if len(comp) > 1:
                    out.append(sorted(comp))
    return out


def check_list_append(ops, spec):
    lv, si, rt, proc = _level(spec)
    txns = []
    for op in ops:
        txns.append(Txn(len(txns), op, parse_mops(op)))
    anomalies = []            # (kind, sentence, [txn numbers])

    def add(kind, text, ts):
        anomalies.append((kind, text, [t.n for t in ts]))

    # who appended what; element values must be unique per key (the contract of the workload)
    appender = {}             # (key, elem) -> txn
    for t in txns:
        for m in t.mops:
            if m[0] == "a":
                if (m[1], m[2]) in appender and appender[(m[1], m[2])] is not t:
                    raise HistError("element %s appended to %s twice (T%d and T%d): elements must be unique per key"
                                    % (m[2], m[1], appender[(m[1], m[2])].n, t.n))
                appender[(m[1], m[2])] = t
    oks = [t for t in txns if t.op.status == "ok"]
    reads = {}                # key -> [(txn, list)]
    for t in oks:
        for m in t.mops:
            if m[0] == "r" and m[2] is not None:
                reads.setdefault(m[1], []).append((t, m[2]))
    # per-read checks
    for k in sorted(reads):
        for t, l in reads[k]:
            if len(set(l)) != len(l):
                dup = sorted(set(e for e in l if l.count(e) > 1))[0]
                add("duplicate-elements", "%s read %s=[%s]: element %s appears more than once" % (
                    "T%d" % t.n, k, ",".join(l), dup), [t])
            for e in l:
                w = appender.get((k, e))
                if w is None:
                    add("garbage-read", "T%d read element %s of %s that no operation appended" % (t.n, e, k), [t])
                elif w.op.status == "fail":
                    add("aborted-read", "T%d read element %s of %s, appended by a failed operation T%d (G1a)" % (
                        t.n, e, k, w.n), [t, w])
            if l:
                w = appender.get((k, l[-1]))
                if w is not None and w is not t:
                    own = [m[2] for m in w.mops if m[0] == "a" and m[1] == k]
                    if own and own[-1] != l[-1]:
                        add("intermediate-read", "T%d read %s=[...,%s], an intermediate state of T%d, which then appended %s (G1b)"
                            % (t.n, k, l[-1], w.n, own[own.index(l[-1]) + 1]), [t, w])
    # internal consistency: a transaction sees its own earlier appends
    for t in oks:
        mine, last = {}, {}
        for m in t.mops:
            if m[0] == "a":
                mine.setdefault(m[1], []).append(m[2])
                if m[1] in last:
                    last[m[1]] = last[m[1]] + [m[2]]
            elif m[2] is not None:
                if m[1] in last and last[m[1]] != m[2]:
                    add("internal", "T%d read %s=[%s] but its own earlier operations imply [%s]" % (
                        t.n, m[1], ",".join(m[2]), ",".join(last[m[1]])), [t])
                elif m[1] not in last and m[1] in mine and m[2][-len(mine[m[1]]):] != mine[m[1]]:
                    add("internal", "T%d read %s=[%s] without its own earlier appends %s" % (
                        t.n, m[1], ",".join(m[2]), ",".join(mine[m[1]])), [t])
                last[m[1]] = list(m[2])
    # version order per key: every read must be a prefix of the longest one
    vorder = {}
    for k in sorted(reads):
        ls = sorted(reads[k], key=lambda x: (-len(x[1]), x[0].n))
        longest_t, longest = ls[0]
        ok = True
        for t, l in ls[1:]:
            if longest[:len(l)] != l:
                add("incompatible-order", "%s: T%d read [%s] and T%d read [%s]: neither is a prefix of the other" % (
                    k, longest_t.n, ",".join(longest), t.n, ",".join(l)), [longest_t, t])
                ok = False
                break
        if ok:
            vorder[k] = longest
    # real time / process order: lost appends, stale reads, monotonic reads, read-your-writes
    if rt:
        app_by_key = {}
        for (kk, e), w in sorted(appender.items(), key=lambda x: x[1].n):
            if w.op.status == "ok":
                app_by_key.setdefault(kk, []).append((e, w))
        for k in sorted(reads):
            for t, l in reads[k]:
                sl = set(l)
                for e, w in app_by_key.get(k, []):
                    if e in sl or w is t:
                        continue
                    if w.op.ret < t.op.call:
                        add("lost-append", "%s: T%d appended %s and was acknowledged, but T%d, which began after that, read [%s] without it"
                            % (k, w.n, e, t.n, ",".join(l)), [w, t])
                        break
            prev = sorted(reads[k], key=lambda x: x[0].op.call)
            for t1, l1 in prev:
                for t2, l2 in prev:
                    if t1.op.ret < t2.op.call and len(l2) < len(l1) and l1[:len(l2)] == l2:
                        add("stale-read", "%s: T%d read [%s], then T%d, which began after T%d completed, read the older [%s]"
                            % (k, t1.n, ",".join(l1), t2.n, t1.n, ",".join(l2)), [t1, t2])
                        break
                else:
                    continue
                break
    if proc:
        byp = {}
        for t in sorted(txns, key=lambda x: x.op.call):
            byp.setdefault(t.op.proc, []).append(t)
        for p in sorted(byp):
            seen = {}          # key -> (txn, list) last read by this process
            wrote = {}         # key -> (txn, elem) own acknowledged appends
            for t in byp[p]:
                if t.op.status != "ok":
                    continue
                for m in t.mops:
                    if m[0] == "a":
                        wrote.setdefault(m[1], []).append((t, m[2]))
                    elif m[2] is not None:
                        if m[1] in seen:
                            t0, l0 = seen[m[1]]
                            if t0 is not t and len(m[2]) < len(l0) and l0[:len(m[2])] == m[2]:
                                add("monotonic-read", "process %s read %s=[%s] (T%d), then the older %s=[%s] (T%d): its reads went back in time"
                                    % (p, m[1], ",".join(l0), t0.n, m[1], ",".join(m[2]), t.n), [t0, t])
                        for tw, e in wrote.get(m[1], []):
                            if tw is not t and e not in m[2]:
                                add("read-your-writes", "process %s appended %s to %s (T%d, acknowledged), then read %s=[%s] without it (T%d)"
                                    % (p, e, m[1], tw.n, m[1], ",".join(m[2]), t.n), [tw, t])
                                break
                        seen[m[1]] = (t, m[2])
    # dependency graph: ww, wr, rw from the version orders; rt and process edges if the level has them
    committed = set(t.n for t in oks)
    for (k, e), w in appender.items():
        if w.op.status == "info" and e in set(vorder.get(k, [])):
            committed.add(w.n)                 # an unknown-outcome append that was observed committed
    adj, why = {}, {}

    def edge(u, v, typ, reason):
        if u == v or u not in committed or v not in committed:
            return
        adj.setdefault(u, {}).setdefault(v, set()).add(typ)
        why.setdefault((u, v, typ), reason)

    for k, vo in vorder.items():
        for a, b in zip(vo, vo[1:]):
            wa, wb = appender.get((k, a)), appender.get((k, b))
            if wa and wb:
                edge(wa.n, wb.n, "ww", "T%d appended %s to %s before T%d appended %s" % (wa.n, a, k, wb.n, b))
        for t, l in reads.get(k, []):
            if l and appender.get((k, l[-1])):
                w = appender[(k, l[-1])]
                edge(w.n, t.n, "wr", "T%d read %s=[...,%s], which T%d appended" % (t.n, k, l[-1], w.n))
            if len(l) < len(vo) and vo[:len(l)] == l and appender.get((k, vo[len(l)])):
                w = appender[(k, vo[len(l)])]
                edge(t.n, w.n, "rw", "T%d read %s=[%s], missing %s, which T%d appended next" % (
                    t.n, k, ",".join(l), vo[len(l)], w.n))
    extra = set()
    if rt:
        extra.add("rt")
        frontier, preds = set(), {}
        evs = []
        for t in txns:
            if t.n in committed:
                evs.append((t.op.call, 0, t.n))
                if t.op.ret != INF:
                    evs.append((t.op.ret, 1, t.n))
        for pos, kind, n in sorted(evs):
            if kind == 0:
                preds[n] = set(frontier)
            else:
                frontier -= preds.get(n, set())
                frontier.add(n)
        for n, ps in preds.items():
            for u in ps:
                edge(u, n, "rt", "T%d completed before T%d began" % (u, n))
    if proc:
        extra.add("proc")
        byp = {}
        for t in sorted(txns, key=lambda x: x.op.call):
            if t.n in committed:
                byp.setdefault(t.op.proc, []).append(t.n)
        for p, ns in byp.items():
            for a, b in zip(ns, ns[1:]):
                edge(a, b, "proc", "process %s ran T%d before T%d" % (p, a, b))
    allowed_all = {"ww", "wr", "rw"} | extra
    nodes_all = set(committed)
    found = []
    classes = [("G0", {"ww"}, None), ("G1c", {"ww", "wr"}, None), ("G-single", {"ww", "wr"}, "rw1")]
    if not si:
        classes.append(("G2", {"ww", "wr"}, "rw+"))
    for comp in _sccs(nodes_all, adj, allowed_all):
        cs = set(comp)
        for name, base, must in classes:
            cyc = _shortest_cycle(cs, adj, base, must)
            suffix = ""
            if cyc is None and extra:
                cyc = _shortest_cycle(cs, adj, base | extra, must)
                if cyc is not None:
                    used = set(t for _, _, t in cyc)
                    suffix = "-realtime" if "rt" in used else ("-process" if "proc" in used else "")
            if cyc is not None:
                found.append((name + suffix, cyc))
                break
    tx = {t.n: t for t in txns}
    for name, cyc in found:
        text = "cycle of %d transactions: %s" % (len(cyc), "; ".join(why[(u, v, t)] for u, v, t in cyc))
        anomalies.append((name, text, [u for u, _, _ in cyc]))
    det = {"ops": len([t for t in txns if t.op.status != "fail"]), "keys": len(set(k for k, _ in appender) | set(reads)),
           "consistency": lv}
    if not anomalies:
        return {"status": "ok", "details": det, "explanation": "",
                "summary": "%d operations on %d keys: no anomaly (%s)" % (det["ops"], det["keys"], lv)}
    kinds = []
    for a in anomalies:
        if a[0] not in kinds:
            kinds.append(a[0])
    first = anomalies[0]
    det.update({"anomaly": first[0], "anomalies": ",".join(kinds)})
    lines = ["anomalies (%s): %s" % (lv, ", ".join(kinds))]
    for kind, text, ns in anomalies[:20]:
        lines.append("%s: %s" % (kind, text))
        for n in ns:
            lines.append("    " + tx[n].label())
    return {"status": "violation", "details": det, "summary": "%s: %s" % (first[0], first[1]),
            "explanation": "\n".join(lines)}


# ------------------------------------------------------------------ driver
CHECK_TYPES = ("linearizable", "list-append")


def run_check(events, spec, budget=DEFAULT_BUDGET):
    t = spec.get("type")
    h = spec.get("history")
    try:
        if t not in CHECK_TYPES:
            raise HistError("check type %r: one of %s" % (t, ", ".join(CHECK_TYPES)))
        ops, warn = pair_ops(events, h)
        res = check_linearizable(ops, spec, budget) if t == "linearizable" else check_list_append(ops, spec)
        res["warnings"] = warn
        return res
    except HistError as e:
        return {"status": "error", "details": {}, "summary": str(e), "explanation": str(e), "warnings": []}


def _j(s):
    return json.dumps(s, sort_keys=True, separators=(",", ":"))


def run_checks(sources, guest_verdict=None, budget=DEFAULT_BUDGET, specs=None):
    """sources: [(label, console text)]. Returns dict(records=[HYDO-ASSERT lines],
    verdict=<combined HYDO-ORACLE line or None if there are no checks>, results={id: result},
    report=<text>). A guest verdict that is already a violation or an error stays the verdict
    (the guest's own invariants come first); otherwise the first failing check (in declaration
    order) makes it a VIOLATION, an undecided check an ERROR, and an OK verdict gets the
    checks' summary appended."""
    events, found, warn = parse_sources(sources)
    finals = dict((sp["id"], sp.get("_final_rc", 0)) for sp in found)
    specs = found if specs is None else specs
    if not specs:
        return {"records": [], "verdict": None, "results": {}, "report": "", "warnings": warn}
    recs, results, rep = [], {}, []
    first_bad = first_err = None
    oks = []
    for sp in specs:
        r = run_check(events, sp, budget)
        frc = finals.get(sp["id"], 0)
        if frc and r["status"] == "ok":
            # the read-back did not complete: the history may miss the reads that matter
            r = {"status": "error", "details": r["details"], "warnings": r.get("warnings", []),
                 "summary": "its final: read-back exited with status %d, so the history is incomplete (%s)" % (frc, r["summary"]),
                 "explanation": ""}
        results[sp["id"]] = r
        msg = sp.get("msg") or "%s history %s is %s" % (sp.get("type"), sp.get("history"), sp.get("model", sp.get("consistency", "")))
        recs.append("HYDO-ASSERT-DECL " + _j({"t": "always", "id": sp["id"], "msg": msg}))
        det = dict(r["details"])
        det["check"] = sp.get("type")
        det["out"] = r["summary"][:400]
        if r["status"] in ("ok", "violation"):
            recs.append("HYDO-ASSERT " + _j({"t": "always", "id": sp["id"], "ok": r["status"] == "ok",
                                              "msg": msg, "details": det}))
        rep.append("== %s (%s, history %s): %s" % (sp["id"], sp.get("type"), sp.get("history"), r["status"].upper()))
        rep.append(r["summary"])
        if r["explanation"]:
            rep.append(r["explanation"])
        for w in r.get("warnings", [])[:20]:
            rep.append("warning: " + w)
        rep.append("")
        if r["status"] == "violation" and first_bad is None:
            first_bad = (sp, r)
        elif r["status"] in ("undecided", "error") and first_err is None:
            first_err = (sp, r)
        elif r["status"] == "ok":
            oks.append((sp, r))
    gv = guest_verdict or ""
    if gv and "HYDO-ORACLE: OK" not in gv:
        verdict = gv
    elif first_bad:
        verdict = "HYDO-ORACLE: VIOLATION [%s] %s" % (first_bad[0]["id"], first_bad[1]["summary"][:600])
    elif first_err:
        verdict = "HYDO-ORACLE: ERROR check %s could not be evaluated: %s" % (first_err[0]["id"], first_err[1]["summary"][:300])
    else:
        tail = "checks=%d held (%s: %s)" % (len(oks), oks[0][0]["id"], oks[0][1]["summary"])
        if not gv or gv.strip() == "HYDO-ORACLE: OK invariants=0 held ()":
            verdict = "HYDO-ORACLE: OK " + tail       # a manifest whose only properties are checks
        else:
            verdict = gv + " | " + tail
    return {"records": recs, "verdict": verdict, "results": results, "report": "\n".join(rep), "warnings": warn}
