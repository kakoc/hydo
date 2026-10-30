"""Error logs as findings, the host side (hvx/onboard/MANIFEST.md, "Error logs as findings").

A manifest with `errors:` makes the guest scan each service's log at the end of every boot and
print one record per error KIND (a service plus a template: the line with its timestamps,
numbers, ids and hex replaced) on the console (hydo-init-errlog.sh):

    HYDO-ERRLOG {"svc":"redis","src":"log","n":2,"at":"s,12","tpl":"#:M <date> <time> # ...","ex":"..."}
    HYDO-ERRLOG-SUM {"boot":"first","steps":20,"lines":5,"kinds":3,"shown":3}

This module turns those records into findings. A kind's SIGNATURE is the first 12 hex digits of
sha256("<service>\\n<template>"): the same kind of line gives the same signature in every run.
The project's reviewed list of expected kinds, hydo/known-errors.yaml, says which signatures (or
template patterns) are known and why, optionally only in runs with a given fault (`with:`) or only
for lines written while that fault was on (`during:`). Every other kind is NEW: explore.py reports
it as a finding of its own (grouped by signature, shrunk to the smallest schedule that still
produces it, run twice), `hydo run` lists it, and with `unknown: fail` it also fails the run.

The list is read only here, on the host: accepting a kind changes no image and no fingerprint,
and the findings of a run are a pure function of its consoles, its schedule and the list.

    hydo errors list   [DIR]                       the new (and known) kinds of a run or an exploration
    hydo errors accept SIG [SIG...] --reason TEXT [--by NAME] [--with FAULT]... [--during FAULT]...
                       [--from DIR] [--file FILE]  add kinds to the known list (with who and why)
    hydo errors check  [--file FILE]               validate the known list

Plain Python 3.8+, no packages (the YAML subset is hydo-build's parser).
"""
import hashlib, importlib.machinery, importlib.util, json, os, re, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
REC_RE = re.compile(r"HYDO-ERRLOG (\{.*\})\s*$")
SUM_RE = re.compile(r"HYDO-ERRLOG-SUM (\{.*\})\s*$")
KNOWN_KEYS = {"sig", "match", "service", "reason", "by", "date", "with", "during", "tpl", "example", "description"}
TOP_KEYS = {"format", "unknown", "known", "description"}
POLICIES = ("report", "fail")
DEFAULT_FILES = (os.path.join("hydo", "known-errors.yaml"), "known-errors.yaml")


USAGE = """usage:
    hydo errors list   [DIR]                       the new and known error kinds of a run or an exploration
                                                   (default DIR: the last exploration)
    hydo errors accept SIG [SIG...] --reason TEXT [--by NAME] [--with FAULT]... [--during FAULT]...
                       [--from DIR] [--file FILE]  add kinds to the known list (default hydo/known-errors.yaml)
    hydo errors check  [--file FILE]               validate the known list
FAULT: K (any kill), K=mysql, P=redis, N=kafka, C (a power cut), T (a clock jump), disk, disk=ioerr,
R=everysec (a knob value). --with: only in runs that have the fault; --during: only for lines
written while it was on (and in the workload range right after it)."""


class ErrorsError(Exception):
    pass


def signature(svc, tpl):
    return hashlib.sha256(("%s\n%s" % (svc, tpl)).encode("utf-8", "replace")).hexdigest()[:12]


# ---------------------------------------------------------------- the console records
def parse_consoles(srcs):
    """srcs: [(name, console text)] of ONE run (the workload boot, then the recovery boot).
    Returns None when no boot printed a HYDO-ERRLOG-SUM (the image has no errors:), else
    {"boots": [sum records], "steps": N, "kinds": [kind]} with one kind per signature:
    {"sig", "svc", "src", "tpl", "ex", "n", "at": [labels in run order]}."""
    boots, kinds, order = [], {}, []
    for _, text in srcs:
        for line in text.splitlines():
            i = line.find("HYDO-ERRLOG")
            if i < 0:
                continue
            line = line[i:]
            m = SUM_RE.match(line)
            if m:
                try:
                    boots.append(json.loads(m.group(1)))
                except ValueError:
                    pass
                continue
            m = REC_RE.match(line)
            if not m:
                continue
            try:
                r = json.loads(m.group(1))
            except ValueError:
                continue
            s = signature(r.get("svc", ""), r.get("tpl", ""))
            k = kinds.get(s)
            if k is None:
                k = kinds[s] = {"sig": s, "svc": r.get("svc", ""), "src": r.get("src", ""), "tpl": r.get("tpl", ""),
                                "ex": r.get("ex", ""), "n": 0, "at": []}
                order.append(s)
            k["n"] += int(r.get("n", 0))
            for lab in str(r.get("at", "")).split(","):
                if lab and lab not in k["at"]:
                    k["at"].append(lab)
    if not boots:
        return None
    steps = max(int(b.get("steps", 0)) for b in boots)
    for k in kinds.values():
        k["at"].sort(key=lambda l: label_rank(l, steps))
    return {"boots": boots, "steps": steps, "kinds": [kinds[s] for s in order]}


def label_rank(lab, steps):
    """s (before the workload) < 0..steps-1 (workload ranges) < end (= steps) < r (recovery boot)."""
    if lab == "s":
        return -1
    if lab == "end":
        return steps
    if lab == "r":
        return steps + 1
    try:
        return int(lab)
    except ValueError:
        return steps


def label_text(labels):
    return ",".join({"s": "setup", "r": "recovery"}.get(l, l) for l in labels)


# ---------------------------------------------------------------- faults of a schedule
INF = 1 << 30


def fault_windows(schedule, steps):
    """[(token, lo, hi)] for the schedule's fault and knob tokens: the step ranks during which a
    line counts as written 'while the fault was on' (hi inclusive: the workload range right after
    the fault is lifted is part of it, where a restarted or resumed service recovers)."""
    host, _, guest = schedule.partition("|")
    out = []
    for t in host.split():
        if t.startswith("disk=") and t != "disk=none":
            out.append((t, 0, steps))           # disk faults: the whole workload
        elif t.startswith("cf="):
            out.append((t, 0, INF))             # a power cut at the k-th FLUSH: somewhere in it
    toks = guest.split()
    if toks and toks[0] == "run":
        toks = toks[1:]
    cap = lambda v: min(v, steps)
    for t in toks:
        k, eq, v = t.partition("=")
        if not eq:
            continue
        m = re.match(r"^([^@]+)@(\d+)(.*)$", v)
        try:
            if k in ("K", "P") and m and m.group(3).startswith("+"):
                at = int(m.group(2)); out.append((t, at, cap(at + int(m.group(3)[1:]))))
            elif k in ("P", "X", "F") and m:
                at = int(m.group(2)); out.append((t, at, cap(at + 1)))
            elif k in ("N", "O", "H") and m and re.match(r"^-(\d+)", m.group(3)):
                out.append((t, int(m.group(2)), cap(int(re.match(r"^-(\d+)", m.group(3)).group(1)))))
            elif k == "C":
                out.append((t, int(v), INF))
            elif k == "T":
                out.append((t, int(v.split(":")[0]), INF))
            else:
                out.append((t, -1, INF))         # a knob (M=2, R=everysec), B=, W=, Q=: the whole run
        except ValueError:
            out.append((t, -1, INF))
    return out


def fault_matches(pat, tok):
    """FAULT patterns of the known list: "K" (any kill), "K=mysql" (a kill of mysql), "C" (a power
    cut, C= or cf=), "disk" (any disk fault), "disk=ioerr", "R=everysec" (a knob value)."""
    if tok == pat or any(tok.startswith(pat + c) for c in "@=:"):
        return True
    if pat == "C" and tok.startswith("cf="):
        return True
    return False


# ---------------------------------------------------------------- the known list
def _yaml():
    loader = importlib.machinery.SourceFileLoader("hydo_build_errors", os.path.join(HERE, "hydo-build"))
    spec = importlib.util.spec_from_loader("hydo_build_errors", loader)
    m = importlib.util.module_from_spec(spec)
    loader.exec_module(m)
    return m


def default_file(start="."):
    for f in DEFAULT_FILES:
        p = os.path.join(start, f)
        if os.path.isfile(p):
            return p
    return None


def load_known(path):
    """The known list: {"path", "sha256", "unknown": report|fail, "known": [entry]} (each entry
    validated, its `match` compiled into "_re"). path None: an empty list."""
    if path is None:
        return {"path": None, "sha256": None, "unknown": "report", "known": []}
    text = open(path, encoding="utf-8").read()
    try:
        d = json.loads(text) if path.endswith(".json") else _yaml().parse_yaml(text, os.path.basename(path))
    except Exception as e:      # YamlError lives in hydo-build
        raise ErrorsError("%s: %s" % (path, e))
    if d is None:
        d = {}
    if not isinstance(d, dict):
        raise ErrorsError("%s: a mapping (format, unknown, known)" % path)
    bad = sorted(set(d) - TOP_KEYS)
    if bad:
        raise ErrorsError("%s: unknown field(s) %s (known: %s)" % (path, ", ".join(bad), ", ".join(sorted(TOP_KEYS))))
    if d.get("format", 1) != 1:
        raise ErrorsError("%s: format must be 1" % path)
    pol = d.get("unknown", "report")
    if pol not in POLICIES:
        raise ErrorsError("%s: unknown: report (a new kind is a finding of its own) or fail (it also fails the run)" % path)
    ents = d.get("known") or []
    if not isinstance(ents, list):
        raise ErrorsError("%s: known: a list of entries" % path)
    out = []
    for i, e in enumerate(ents):
        where = "%s: known[%d]" % (path, i)
        if not isinstance(e, dict):
            raise ErrorsError("%s: a mapping" % where)
        bad = sorted(set(e) - KNOWN_KEYS)
        if bad:
            raise ErrorsError("%s: unknown field(s) %s (known: %s)" % (where, ", ".join(bad), ", ".join(sorted(KNOWN_KEYS))))
        if ("sig" in e) == ("match" in e):
            raise ErrorsError("%s: exactly one of sig: (a signature) and match: (a regular expression on the template)" % where)
        if "sig" in e and not re.fullmatch(r"[0-9a-f]{12}", str(e["sig"])):
            raise ErrorsError("%s: sig: 12 hex digits, as `hydo errors list` prints them (got %r)" % (where, e["sig"]))
        if not str(e.get("reason") or "").strip():
            raise ErrorsError("%s: reason: why this kind is expected (required)" % where)
        e = dict(e)
        if "match" in e:
            try:
                e["_re"] = re.compile(str(e["match"]))
            except re.error as x:
                raise ErrorsError("%s: match: %r is not a regular expression (%s)" % (where, e["match"], x))
        for k in ("with", "during"):
            v = e.get(k)
            if v is None:
                continue
            if isinstance(v, str):
                v = e[k] = [v]
            if not isinstance(v, list) or not v or any(not isinstance(x, str) or not x or " " in x for x in v):
                raise ErrorsError("%s: %s: a list of faults such as K=mysql, C, disk=ioerr, R=everysec" % (where, k))
        out.append(e)
    h = hashlib.sha256(text.encode("utf-8")).hexdigest()
    return {"path": path, "sha256": h, "unknown": pol, "known": out}


def explain(kind, known, schedule, steps):
    """The index of the first known entry that covers this kind in this run, or None."""
    wins = None
    for i, e in enumerate(known["known"]):
        if e.get("service") and e["service"] != kind["svc"]:
            continue
        if "sig" in e and e["sig"] != kind["sig"]:
            continue
        if "_re" in e and not e["_re"].search(kind["tpl"]):
            continue
        if wins is None:
            wins = fault_windows(schedule, steps)
        if e.get("with") and not any(fault_matches(p, t) for p in e["with"] for t, _, _ in wins):
            continue
        if e.get("during"):
            ws = [(lo, hi) for t, lo, hi in wins if any(fault_matches(p, t) for p in e["during"])]
            if not all(any(lo <= label_rank(l, steps) <= hi for lo, hi in ws) for l in kind["at"]):
                continue
        return i
    return None


def analyse(srcs, schedule, known):
    """One run: None without errors:, else {"steps", "boots", "new": [kind], "known": [kind + "entry"]}."""
    p = parse_consoles(srcs)
    if p is None:
        return None
    new, old = [], []
    for k in p["kinds"]:
        i = explain(k, known, schedule, p["steps"])
        if i is None:
            new.append(k)
        else:
            old.append(dict(k, entry=i, reason=known["known"][i].get("reason")))
    return {"steps": p["steps"], "boots": p["boots"], "new": new, "known": old,
            "truncated": any(b.get("kinds", 0) > b.get("shown", 0) for b in p["boots"])}


def verdict_of(kind):
    """The VIOLATION line of a run failed by a new kind (unknown: fail)."""
    return "VIOLATION error-log [%s] %s (%s)" % (kind["svc"], kind["tpl"][:160], kind["sig"])


def apply(res, an, known):
    """Fold one run's analysis into its result dict (explore.py, hydo run). Without errors:
    the result is unchanged."""
    if an is None:
        return res
    res["errors_new"] = [k["sig"] for k in an["new"]]
    res["errors_known"] = len(an["known"])
    if known["unknown"] == "fail" and an["new"] and "HYDO-ORACLE: OK" in res.get("verdict", ""):
        res["pre_errlog_verdict"] = res["verdict"]
        res["verdict"] = verdict_of(an["new"][0])
    return res


def run_files(prefix):
    """The consoles of one run: <prefix>.log, <prefix>.recovery.log."""
    out = []
    for suffix in ("", ".recovery"):
        p = "%s%s.log" % (prefix, suffix)
        if os.path.exists(p):
            out.append((os.path.basename(p), open(p, errors="replace").read()))
    return out


def summary_lines(an, limit=12):
    if an is None:
        return []
    L = ["error logs: %d new kind%s, %d known%s" % (len(an["new"]), "" if len(an["new"]) == 1 else "s", len(an["known"]),
                                                    " (the guest printed only the first kinds: errors.max)" if an["truncated"] else "")]
    for k in an["new"][:limit]:
        L.append("  NEW   %s  %-8s n=%-3d at %-14s %s" % (k["sig"], k["svc"], k["n"], label_text(k["at"]), k["ex"][:110]))
    if len(an["new"]) > limit:
        L.append("  ... %d more new kinds" % (len(an["new"]) - limit))
    for k in an["known"][:limit]:
        L.append("  known %s  %-8s n=%-3d at %-14s (%s)" % (k["sig"], k["svc"], k["n"], label_text(k["at"]), (k.get("reason") or "")[:80]))
    return L


# ---------------------------------------------------------------- an exploration's kinds
def collect(runs):
    """runs: [(seed, schedule, analysis)] in seed order -> {sig: aggregate}, new and known apart."""
    agg = {}
    for seed, sched, an in runs:
        if an is None:
            continue
        for status, ks in (("new", an["new"]), ("known", an["known"])):
            for k in ks:
                a = agg.get(k["sig"])
                if a is None:
                    a = agg[k["sig"]] = {"sig": k["sig"], "svc": k["svc"], "src": k["src"], "tpl": k["tpl"], "ex": k["ex"],
                                         "runs": 0, "new_runs": 0, "seeds": [], "first_seed": seed,
                                         "first_schedule": sched, "first_at": k["at"], "lines": 0}
                a["runs"] += 1
                a["lines"] += k["n"]
                if status == "new":
                    a["new_runs"] += 1
                    if "first_new_seed" not in a:
                        a["first_new_seed"], a["first_new_schedule"], a["first_new_at"] = seed, sched, k["at"]
                        a["first_new_ex"] = k["ex"]
                else:
                    a.setdefault("reason", k.get("reason"))
                if len(a["seeds"]) < 12:
                    a["seeds"].append(seed)
    return agg


def report_md(agg, known, shrunk, by_hand):
    """The report.md section (explore.py). by_hand(schedule) -> the command line of one run."""
    new = [a for a in agg.values() if a["new_runs"]]
    old = [a for a in agg.values() if not a["new_runs"]]
    L = ["## Error logs (errors: in the manifest)", "",
         "Known list: %s%s; unknown kinds: %s. %d new kind%s, %d only known." % (
             known["path"] or "none", " (sha256 %s)" % known["sha256"][:16] if known["sha256"] else "", known["unknown"],
             len(new), "" if len(new) == 1 else "s", len(old)), ""]
    if new:
        L += ["### New kinds", "", "| signature | service | runs | first seed | at (first run) | example |", "|---|---|---|---|---|---|"]
        for a in sorted(new, key=lambda a: (a["first_new_seed"], a["sig"])):
            L.append("| `%s` | %s | %d | %s | %s | `%s` |" % (a["sig"], a["svc"], a["new_runs"], a["first_new_seed"],
                                                           label_text(a["first_new_at"]), a["first_new_ex"][:100].replace("|", "/")))
        L.append("")
        for a in sorted(new, key=lambda a: (a["first_new_seed"], a["sig"])):
            L += ["#### `%s` (%s): `%s`" % (a["sig"], a["svc"], a["tpl"][:160]), "",
                  "- example: `%s`" % a["first_new_ex"][:200],
                  "- first schedule (seed %s): `%s`" % (a["first_new_seed"], a["first_new_schedule"])]
            s = shrunk.get(a["sig"])
            if s:
                L += ["- minimal schedule (%d shrink runs): `%s`" % (s["tries"], s["minimal"]),
                      "- run twice: fingerprints `%s` / `%s` (%s), the kind %s" % (
                          s["fp_a"], s["fp_b"], "identical" if s["fp_a"] == s["fp_b"] else "DIFFERENT",
                          "in both runs" if s["present_a"] and s["present_b"] else "NOT in both runs"),
                      "- by hand: %s" % by_hand(s["minimal"])]
            L += ["- expected? `hydo errors accept %s --reason \"...\"` (or `--during <fault>` to accept it only while a "
                  "fault is on)" % a["sig"], ""]
    if old:
        L += ["### Known kinds seen", "", "| signature | service | runs | reason |", "|---|---|---|---|"]
        for a in sorted(old, key=lambda a: a["sig"]):
            L.append("| `%s` | %s | %d | %s |" % (a["sig"], a["svc"], a["runs"], (a.get("reason") or "")[:100].replace("|", "/")))
        L.append("")
    return L


# ---------------------------------------------------------------- hydo errors (both CLIs)
def _take(args, flag, many=False):
    vals = []
    while flag in args:
        i = args.index(flag)
        if i + 1 >= len(args):
            raise ErrorsError("%s needs a value" % flag)
        vals.append(args[i + 1])
        del args[i:i + 2]
        if not many:
            break
    return vals if many else (vals[0] if vals else None)


def kinds_of_dir(d):
    """The kinds recorded in a run dir (hydo run: one.errors.json) or an explore dir (errors.json)."""
    for f in ("errors.json", "one.errors.json"):
        p = os.path.join(d, f)
        if os.path.isfile(p):
            j = json.load(open(p))
            return j, p
    raise ErrorsError("%s has no errors.json (hydo explore) or one.errors.json (hydo run); was the image built with "
                      "errors: in its manifest?" % d)


def _q(s):
    return json.dumps(str(s))


def accept(path, kinds, reason, by, with_, during, date=None):
    """Append one entry per kind to the known list (create it if needed). Returns the entries' text."""
    if not reason or not reason.strip():
        raise ErrorsError("--reason: why these kinds are expected (required)")
    new_file = not os.path.exists(path)
    text = "" if new_file else open(path, encoding="utf-8").read()
    have = set()
    if not new_file:
        have = {e.get("sig") for e in load_known(path)["known"]}
        if not re.search(r"(?m)^known:", text):
            text = text.rstrip("\n") + "\nknown:\n"
    else:
        text = ("# Known error kinds: lines a service writes to its log that are expected (hydo/onboard/MANIFEST.md,\n"
                "# \"Error logs as findings\"). Each entry needs a reason; `hydo errors accept` appends them.\n"
                "#   sig:    the kind's signature (or match: a regular expression on its template)\n"
                "#   with:   only in runs whose schedule has one of these faults (K=mysql, C, disk, R=everysec)\n"
                "#   during: only for lines written while one of these faults was on\n"
                "format: 1\nunknown: report       # report: a new kind is a finding; fail: it also fails the run\nknown:\n")
    add = []
    for k in kinds:
        if k["sig"] in have:
            continue
        e = ["  - sig: %s" % k["sig"], "    service: %s" % k["svc"], "    tpl: %s" % _q(k["tpl"])]
        if k.get("ex"):
            e.append("    example: %s" % _q(k["ex"][:200]))
        if with_:
            e.append("    with: [%s]" % ", ".join(with_))
        if during:
            e.append("    during: [%s]" % ", ".join(during))
        e += ["    reason: %s" % _q(reason), "    by: %s" % _q(by), "    date: %s" % _q(date or time.strftime("%Y-%m-%d"))]
        add.append("\n".join(e))
    if not text.endswith("\n"):
        text += "\n"
    text += "".join(a + "\n" for a in add)
    tmp = path + ".tmp"
    d = os.path.dirname(path)
    if d:
        os.makedirs(d, exist_ok=True)
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
    load_known(tmp)             # never write a list that does not load
    os.replace(tmp, path)
    return add


def cli(argv, default_dir):
    """hydo errors list|accept|check (both release CLIs). Returns an exit code."""
    args = list(argv)
    if not args or args[0] in ("-h", "--help", "help"):
        print(USAGE)
        return 0 if args else 2
    verb = args.pop(0)
    try:
        f = _take(args, "--file") or default_file() or DEFAULT_FILES[0 if os.path.isdir("hydo") else 1]
        if verb == "check":
            k = load_known(f)
            print("hydo: %s: %d known kinds, unknown kinds: %s" % (f, len(k["known"]), k["unknown"]))
            return 0
        if verb == "list":
            d = args[0] if args else default_dir
            j, p = kinds_of_dir(d)
            kinds = j.get("kinds") or []
            print("hydo: %s (%d kinds; the known list is applied as recorded in that run)" % (p, len(kinds)))
            for k in kinds:
                print("%-5s %s  %-8s runs=%-3s at %-14s %s" % ("NEW" if k.get("new") else "known", k["sig"], k["svc"],
                                                            k.get("runs", 1), label_text(k.get("at") or k.get("first_at") or []),
                                                            (k.get("ex") or "")[:110]))
            return 0
        if verb == "accept":
            reason = _take(args, "--reason")
            by = _take(args, "--by") or os.environ.get("USER") or "unknown"
            with_ = _take(args, "--with", many=True)
            during = _take(args, "--during", many=True)
            d = _take(args, "--from") or default_dir
            sigs = [a for a in args if not a.startswith("-")]
            if not sigs:
                raise ErrorsError("usage: hydo errors accept SIG [SIG...] --reason TEXT [--by NAME] [--with FAULT]... "
                                  "[--during FAULT]... [--from DIR] [--file FILE]")
            j, p = kinds_of_dir(d)
            idx = {k["sig"]: k for k in j.get("kinds") or []}
            pick = []
            for s in sigs:
                hit = [k for sig, k in idx.items() if sig.startswith(s)]
                if len(hit) != 1:
                    raise ErrorsError("%s: %s in %s" % (s, "no such kind" if not hit else "ambiguous prefix", p))
                pick.append(hit[0])
            add = accept(f, pick, reason, by, with_, during)
            print("hydo: %d kind%s added to %s%s" % (len(add), "" if len(add) == 1 else "s", f,
                                                     "" if len(add) == len(pick) else " (the others were already there)"))
            return 0
        raise ErrorsError("unknown verb %r (list, accept, check)" % verb)
    except ErrorsError as e:
        print("hydo: %s" % e, file=sys.stderr)
        return 1


def for_run_dir(outdir, schedule, res, known_path=None, out=print):
    """`hydo run` / `hydo repro` (both CLIs): the error kinds of the run whose consoles are in
    outdir (<tag>.log, <tag>.recovery.log), compared with the known list (known_path, else
    hydo/known-errors.yaml or ./known-errors.yaml if present); writes one.errors.json, prints the
    summary and folds the result into res. Nothing happens for an image without errors:."""
    import glob
    cons = [p for p in sorted(glob.glob(os.path.join(outdir, "*.log")))
            if not p.endswith((".checks.log", "engine.log")) and not os.path.basename(p).startswith("worker")]
    cons.sort(key=lambda p: (p.endswith(".recovery.log"), p))
    srcs = [(os.path.basename(p), open(p, errors="replace").read()) for p in cons]
    try:
        known = load_known(known_path or default_file())
    except ErrorsError as e:
        out("hydo: known errors: %s (every kind counts as new)" % e)
        known = load_known(None)
    an = analyse(srcs, schedule, known)
    if an is None:
        return res
    write_run_json(os.path.join(outdir, "one.errors.json"), an, known)
    for line in summary_lines(an):
        out("hydo: " + line)
    if an["new"]:
        out("hydo: expected? hydo errors accept <signature> --reason \"...\" --from %s" % outdir)
    return apply(res, an, known)


def write_run_json(path, an, known):
    """one.errors.json of `hydo run` (what `hydo errors list|accept --from <run dir>` read)."""
    kinds = [dict(k, new=True, runs=1) for k in an["new"]] + [dict(k, new=False, runs=1) for k in an["known"]]
    json.dump({"known_file": known["path"], "known_sha256": known["sha256"], "unknown": known["unknown"],
               "steps": an["steps"], "boots": an["boots"], "kinds": kinds}, open(path, "w"), indent=1)
