#!/usr/bin/env python3
"""Test-only: the Python reference (hydo/onboard/hydo_errors.py) for hydo-errors' equivalence
test (tests/equivalence.rs).

    py_ref.py --work DIR [--corpus DIR]

Prints one JSON line per case: {"op", "args", "out"} with "out" = {"ok": <value>} or
{"err": "ErrorsError" | "crash", "msg": <text>}; paths under the case's own directory read
"<dir>". The cases:

  1. synthetic consoles (the records of test_errlog.py and variants: appended boots, CRLF, bad
     JSON, numbers as text, a truncated boot) against known lists (none, the examples' lists, and
     lists with match:, service:, with:, during: in both forms), under fault schedules;
  2. known lists that load and that are refused (every message of load_known);
  3. fault windows of many schedules;
  4. `accept` into a new list, an existing one, one without a known: line, with --with/--during;
  5. `for_run_dir` on a run directory (the files hydo run leaves);
  6. with --corpus (a copy of the bare-metal machine's /var/tmp/m94, plus known/redis-errors.yaml
     and known/stack.yaml, the lists those runs used): every console of every run and
     exploration there, against no list and against each recorded list, and every exploration's
     aggregate and report section.
"""
import argparse, json, os, shutil, sys

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.normpath(os.path.join(HERE, "..", "..", "..", ".."))
ONB = os.path.join(TOP, "hydo", "onboard")
sys.path.insert(0, ONB)
import hydo_errors as he  # noqa: E402

N = [0]
RESES = [{"verdict": "HYDO-ORACLE: OK invariants=1 held"}, {"verdict": "HYDO-ORACLE: VIOLATION [x] y"},
         {"fp": "0123456789abcdef"}]


def emit(op, args, fn, dirs=()):
    try:
        out = {"ok": fn()}
    except he.ErrorsError as e:
        out = {"err": "ErrorsError", "msg": str(e)}
    except Exception as e:
        out = {"err": "crash", "msg": "%s: %s" % (type(e).__name__, e)}
    s = json.dumps({"op": op, "args": args, "out": out})
    for d in dirs:
        s = s.replace(json.dumps(d)[1:-1], "<dir>")
    print(s)
    N[0] += 1


def known_view(k):
    return {"path": k["path"], "sha256": k["sha256"], "unknown": k["unknown"],
            "known": [{x: v for x, v in e.items() if x != "_re"} for e in k["known"]]}


def read(srcs):
    return [(n, open(p, errors="replace").read()) for n, p in srcs]


def analyse(srcs, schedule, known, unknown=None):
    def f():
        k = he.load_known(known)
        if unknown:
            k["unknown"] = unknown
        an = he.analyse(read(srcs), schedule, k)
        out = {"an": an, "summary": he.summary_lines(an), "apply": [he.apply(dict(r), an, k) for r in RESES]}
        if an is not None:
            p = os.path.join(WORK, "run.json.%d" % N[0])
            he.write_run_json(p, an, k)
            out["run_json"] = open(p).read()
            os.remove(p)
        return out
    emit("analyse", {"srcs": srcs, "schedule": schedule, "known": known, "unknown": unknown}, f)


# ---------------------------------------------------------------- 1. synthetic consoles
CON = "\n".join([
    'hydo-ob: x',
    'HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"s","tpl":"#:C <date> <time> # Redis is starting","ex":"57:C ..."}',
    'HYDO-ERRLOG {"svc":"kafka","src":"log","n":3,"at":"10,11","tpl":"ERROR Connection to node # refused","ex":"ERROR Connection to node 1 refused"}',
    'HYDO-ERRLOG-SUM {"boot":"first","steps":20,"lines":4,"kinds":2,"shown":2}'])
REC = "\n".join([
    'HYDO-ERRLOG {"svc":"kafka","src":"log","n":1,"at":"r","tpl":"ERROR Connection to node # refused","ex":"..."}',
    'HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"r","tpl":"#:M <date> <time> # !!! Warning: short read while loading the AOF file appendonly.aof.#.incr.aof!","ex":"..."}',
    'HYDO-ERRLOG-SUM {"boot":"recovery","steps":20,"lines":2,"kinds":2,"shown":2}'])
ODD = "\r\n".join([
    '[   1.2] noise HYDO-ERRLOG {"svc":"mysql","src":"file","n":"2","at":"3,end,s,12,x","tpl":"[ERROR] [MY-012592] [InnoDB] Operating system error number # in a file operation.","ex":"é café ☃"}   ',
    'HYDO-ERRLOG {"svc":"mysql","src":"file","n":1.9,"at":"r,0","tpl":"[ERROR] [MY-012592] [InnoDB] Operating system error number # in a file operation.","ex":"x"}',
    'HYDO-ERRLOG {not json}',
    'HYDO-ERRLOG {"svc":"front","n":true,"at":7,"tpl":"panic: runtime error: index out of range [#] with length #"}',
    'HYDO-ERRLOG-SUMMARY {"x":1}',
    'HYDO-ERRLOG-SUM {"boot":"first","steps":"12","lines":5,"kinds":7,"shown":3} \t',
    'HYDO-ERRLOG {"svc":"front","src":"log","n":4,"at":"end","tpl":"level=error msg=\\"dial tcp #.#.#.#:#->#.#.#.#:3306: connect: connection refused\\"","ex":"dial"}'])
APPENDED = CON + "\n" + CON.replace('"n":3', '"n":1').replace("Redis is starting", "x") + "\n" + REC
EMPTY = "HYDO-ERRLOG-SUM {\"boot\":\"first\",\"steps\":4,\"lines\":0,\"kinds\":0,\"shown\":0}\n"

LISTS = {
    "match.yaml": """format: 1
unknown: fail
known:
  - match: 'Connection to node # (refused|reset)'
    service: kafka
    reason: clients retry
    during: K=kafka
  - match: '(?<!xx)short read.*\\.aof!$'
    reason: aof
    with: [C, disk=ioerr]
  - match: '(?i)REDIS IS STARTING'
    reason: banner
    service: redis
  - match: 'MY-\\d{6}\\] \\[InnoDB\\]'
    reason: innodb
    with: K=mysql
  - match: 'panic: runtime error: index out of range \\[#\\] with length #'
    reason: 0b
  - match: 'dial tcp [#.]+:#->[#.]{7}:3306'
    reason: front
    during: [N=mysql, K]
""",
    "sig.yaml": """known:
  - sig: %s
    reason: "kafka, by sig"
    with: [K=kafka]
  - sig: %s
    service: redis
    reason: banner
""" % (he.signature("kafka", "ERROR Connection to node # refused"), he.signature("redis", "#:C <date> <time> # Redis is starting")),
    "empty.yaml": "",
    "comment.yaml": "# nothing yet\n",
    "list.json": json.dumps({"format": 1, "unknown": "report", "known": [
        {"sig": he.signature("kafka", "ERROR Connection to node # refused"), "reason": "json"}]}),
}
BAD = {
    "notmap.yaml": "- a\n- b\n",
    "top.yaml": "format: 1\nbogus: 2\nzz: 1\n",
    "format.yaml": "format: 2\n",
    "format-true.yaml": "format: true\nknown:\n",
    "policy.yaml": "unknown: maybe\n",
    "known-map.yaml": "known:\n  a: 1\n",
    "entry.yaml": "known:\n  - just text\n",
    "fields.yaml": "known:\n  - sig: 0123456789ab\n    reason: x\n    color: red\n",
    "both.yaml": "known:\n  - sig: 0123456789ab\n    match: x\n    reason: x\n",
    "neither.yaml": "known:\n  - reason: x\n",
    "sig.yaml": "known:\n  - sig: 0123456789AB\n    reason: x\n",
    "sig-int.yaml": "known:\n  - sig: 123456789012\n    reason: x\n",
    "reason.yaml": "known:\n  - sig: 0123456789ab\n",
    "reason-blank.yaml": "known:\n  - sig: 0123456789ab\n    reason: '   '\n",
    "reason-zero.yaml": "known:\n  - sig: 0123456789ab\n    reason: 0\n",
    "regex.yaml": "known:\n  - match: 'a(b'\n    reason: x\n",
    "regex2.yaml": "known:\n  - match: '*a'\n    reason: x\n",
    "regex3.yaml": "known:\n  - match: '[z-a]'\n    reason: x\n",
    "with.yaml": "known:\n  - sig: 0123456789ab\n    reason: x\n    with: []\n",
    "with2.yaml": "known:\n  - sig: 0123456789ab\n    reason: x\n    with: 'K mysql'\n",
    "during.yaml": "known:\n  - sig: 0123456789ab\n    reason: x\n    during: [K, '']\n",
    "during2.yaml": "known:\n  - sig: 0123456789ab\n    reason: x\n    during: 3\n",
    "yaml.yaml": "known:\n\t- sig: x\n",
    "dup.yaml": "known: 1\nknown: 2\n",
    "bad.json": "{not json",
    "list.json": "[1, 2]",
}
SCHEDULES = [
    "disk=none | run",
    "disk=cut dseed=1 | run K=kafka@10+2 C=14",
    "disk=none | run K=kafka@10+2",
    "disk=none | run K=kafka@10+1",
    "disk=none | run K=kafka@3+2",
    "disk=ioerr:3 dseed=1 | run K=mysql@2+30 N=mysql@0-25:loss,100%",
    "disk=cut cf=7 | run K=redis@3+2 P=kafka@5~10:200 N=mysql@2-6:delay,100ms C=12 T=4:-30 R=everysec",
    "disk=none | run P=kafka@5+3 X=kafka@4 F=kafka<mysql@3#2:100 O=kafka@1-30:req:loss H=pay@0-1:drop W=0000002a,p4",
    "disk=none | K=x@1+1",
    "| run K=@3+1 K=a@b+1 K=a@3+x P=a@3 N=a@3- N=a@3-x C=x T=x:1 C=+5 T= = x=",
]


def synthetic():
    d = os.path.join(WORK, "con")
    os.makedirs(d, exist_ok=True)
    files = {}
    for name, text in (("con.log", CON), ("rec.log", REC), ("odd.log", ODD), ("appended.log", APPENDED),
                       ("empty.log", EMPTY), ("none.log", "no records here\n")):
        p = os.path.join(d, name)
        open(p, "w", newline="").write(text)
        files[name] = p
    sets = [[("a", files["con.log"]), ("b", files["rec.log"])], [("a", files["con.log"])], [("b", files["rec.log"])],
            [("a", files["odd.log"])], [("a", files["appended.log"])], [("a", files["empty.log"])],
            [("a", files["none.log"])], [("a", files["odd.log"]), ("b", files["rec.log"])]]
    lists = [None, os.path.join(ONB, "examples", "redis-errors", "hydo", "known-errors.yaml"),
             os.path.join(ONB, "examples", "stack", "known-errors.yaml")]
    for name, text in LISTS.items():
        p = os.path.join(WORK, "lists", name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "w").write(text)
        lists.append(p)
    for srcs in sets:
        for sched in SCHEDULES:
            for k in lists:
                analyse(srcs, sched, k)
        analyse(srcs, SCHEDULES[1], lists[3], "fail")
    for k in lists:
        emit("load_known", {"path": k}, lambda: known_view(he.load_known(k)))
    for name, text in BAD.items():
        p = os.path.join(WORK, "bad", name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "w").write(text)
        emit("load_known", {"path": p}, lambda: known_view(he.load_known(p)))
    for sched in SCHEDULES:
        for steps in (0, 4, 20):
            emit("fault_windows", {"schedule": sched, "steps": steps}, lambda: he.fault_windows(sched, steps))
    for lab in ("s", "end", "r", "0", " 7", "+3", "1_0", "x", "", "-2"):
        emit("label_rank", {"lab": lab, "steps": 20}, lambda: he.label_rank(lab, 20))
    # an exploration over the synthetic runs
    runs = [(i + 1, SCHEDULES[i % len(SCHEDULES)], s) for i, s in enumerate(sets)]
    for k in lists:
        explore_case(runs, k, {})
    return files


def explore_case(runs, known, shrunk, label=None):
    def f():
        k = he.load_known(known)
        an_runs = [(sd, sched, he.analyse(read(srcs), sched, k)) for sd, sched, srcs in runs]
        agg = he.collect(an_runs)
        md = he.report_md(agg, k, shrunk, lambda s: "`hydo run \"%s\"`" % s)
        return {"agg": list(agg.values()), "md": md}
    emit("explore", {"runs": runs, "known": known, "shrunk": shrunk, "label": label}, f)


# ---------------------------------------------------------------- 4. accept, 5. for_run_dir
def accept_cases(files):
    kinds = [dict(k.to_json() if hasattr(k, "to_json") else k) for k in
             he.parse_consoles(read([("a", files["odd.log"]), ("b", files["rec.log"])]))["kinds"]]
    # a template with a double quote (the guest never writes one) makes a list hydo-build's YAML
    # subset cannot read: refused on both sides; the others are what real records carry
    odd = [k for k in kinds if '"' in k["tpl"]] + [
        {"sig": "f0c792251150", "svc": "redis", "tpl": 'quote " back \\ tab\t é', "ex": ""}]
    kinds = [k for k in kinds if '"' not in k["tpl"]] + [
        {"sig": "0123456789ab", "svc": "x", "tpl": "café ☃ <date> #", "ex": "e" * 300}]
    befores = [None, LISTS["sig.yaml"], "format: 1\nunknown: report\n", "# just a comment", "known:\r\n  - sig: f0c792251150\r\n    reason: x\r\n",
               "known:\n  - sig: zz\n"]
    i = 0
    for before in befores:
        for ks, reason, by, with_, during in (
                (kinds, "expected", "t", [], []),
                (kinds[:1], "at boot", "someone", ["C", "disk=ioerr"], []),
                (kinds[1:3], "while down", "t", [], ["K=kafka"]),
                (kinds[:2] + kinds[:1], "twice", "t", ["K"], ["N=mysql"]),
                (kinds, "  ", "t", [], []),
                (kinds, None, "t", [], []),
                (kinds[:1], "x", "t", ["K mysql"], []),
                (odd, "odd", "t", [], [])):
            d = os.path.join(WORK, "accept", str(i))
            i += 1
            os.makedirs(d)
            p = os.path.join(d, "sub", "known-errors.yaml")
            if before is not None:
                os.makedirs(os.path.dirname(p))
                open(p, "w", newline="").write(before)

            def f():
                add = he.accept(p, ks, reason, by, with_, during, date="2026-10-30")
                return {"add": add, "text": open(p).read()}
            emit("accept", {"before": before, "kinds": ks, "reason": reason, "by": by, "with": with_,
                            "during": during, "dir": d}, f, dirs=(d,))


def for_run_dir_cases(files):
    i = 0
    for cons in ({"one.log": CON, "one.recovery.log": REC, "engine.log": CON, "one.checks.log": CON,
                  "worker-0.log": CON, ".hidden.log": CON},
                 {"one.log": ODD}, {"x.log": "nothing"}, {"b.log": REC, "a.recovery.log": CON, "a.log": EMPTY}):
        for known in (None, os.path.join(WORK, "lists", "match.yaml"), os.path.join(WORK, "bad", "regex.yaml"),
                      os.path.join(WORK, "lists", "sig.yaml")):
            for res in RESES:
                d = os.path.join(WORK, "runs", str(i))
                i += 1
                os.makedirs(d)
                for n, t in cons.items():
                    open(os.path.join(d, n), "w").write(t)
                printed = []

                def f():
                    r = he.for_run_dir(d, SCHEDULES[1], dict(res), known, out=printed.append)
                    p = os.path.join(d, "one.errors.json")
                    return {"res": r, "printed": printed, "one_errors": open(p).read() if os.path.exists(p) else None}
                emit("for_run_dir", {"files": cons, "schedule": SCHEDULES[1], "res": res, "known": known, "dir": d},
                     f, dirs=(d,))


# ---------------------------------------------------------------- 6. the recorded corpus
def schedules_of(d):
    """tag -> schedule for a run or exploration directory."""
    out = {}
    for name in ("runs.jsonl", "results.jsonl"):
        p = os.path.join(d, name)
        if os.path.exists(p):
            for line in open(p):
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if "tag" in r:
                    out[r["tag"]] = r["schedule"]
                elif "seed" in r:
                    out["seed-%d" % r["seed"]] = r["schedule"]
    p = os.path.join(d, "run.json")
    if os.path.exists(p):
        out[""] = json.load(open(p)).get("schedule", "")
    return out


def corpus(root):
    known = os.path.join(root, "known")
    lists = [None, os.path.join(known, "redis-errors.yaml"), os.path.join(known, "stack.yaml"),
             os.path.join(root, "redis-final", "fail.yaml")]
    nrun = nexp = 0
    for d, dirs, fs in sorted(os.walk(root)):
        dirs.sort()
        if "one.log" in fs:                 # a hydo run / repro directory
            sched = schedules_of(d).get("", "disk=none | run")
            cons = sorted(os.path.join(d, f) for f in fs if f.endswith(".log") and not f.startswith(".")
                          and not f.endswith((".checks.log", "engine.log")) and not f.startswith("worker"))
            cons.sort(key=lambda p: (p.endswith(".recovery.log"), p))
            srcs = [(os.path.basename(p), p) for p in cons]
            for k in lists:
                analyse(srcs, sched, k)
            nrun += 1
        if os.path.basename(d) == "consoles":
            parent = os.path.dirname(d)
            scheds = schedules_of(parent)
            tags = sorted({f[:-len(".recovery.log")] if f.endswith(".recovery.log") else f[:-4]
                           for f in fs if f.endswith(".log")})
            for t in tags:
                srcs = [(os.path.basename(p), p) for p in (os.path.join(d, t + ".log"), os.path.join(d, t + ".recovery.log"))
                        if os.path.exists(p)]
                for k in lists[:3]:
                    analyse(srcs, scheds.get(t, "disk=none | run"), k)
            seeds = sorted(int(t[5:]) for t in tags if t.startswith("seed-") and t[5:].isdigit())
            if seeds:
                runs = [(sd, scheds.get("seed-%d" % sd, ""), [(os.path.basename(p), p) for p in
                         (os.path.join(d, "seed-%d.log" % sd), os.path.join(d, "seed-%d.recovery.log" % sd))
                         if os.path.exists(p)]) for sd in seeds]
                shrunk = {}
                ej = os.path.join(parent, "errors.json")
                if os.path.exists(ej):
                    shrunk = {k["sig"]: k["shrunk"] for k in json.load(open(ej)).get("kinds") or [] if k.get("shrunk")}
                for k in lists[:3]:
                    explore_case(runs, k, shrunk, label=os.path.relpath(parent, root))
                nexp += 1
    return nrun, nexp


def main():
    global WORK
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--corpus")
    a = ap.parse_args()
    WORK = a.work
    files = synthetic()
    accept_cases(files)
    for_run_dir_cases(files)
    nrun = nexp = 0
    if a.corpus:
        nrun, nexp = corpus(a.corpus)
    print(json.dumps({"op": "summary", "args": {}, "out": {"ok": {"ops": N[0], "corpus_runs": nrun,
                                                                  "corpus_explorations": nexp}}}))


if __name__ == "__main__":
    main()
