"""hydo_composer -- the test composer: a project's test written as small commands and
properties, composed by hydo into seeded sequences and concurrent mixes (MANIFEST.md "The test
composer").

The engine-facing PLAN is the manifest's `test:` section (YAML or JSON, language-neutral):

    test:
      parallel: 4  per_round: 2  round_ms: 200  command_timeout: 60  drain_timeout: 60
      check_every: 2  check_within: 30  max_commands: 0  budget_ms: 0
      commands:   [{name, kind, run, timeout, content}]   kind: first | parallel_driver | serial_driver |
                                                   singleton_driver | anytime | eventually | finally
      checks:     [{name, kind, run, msg, every, within, after, poll}]   kind: final | always | eventually
      regressions: [{name, schedule, expect}]

`run` is an argv list (exec'd, no shell) or a string (a shell command, for hand-written plans).
A `hydo/` folder next to hydo.yaml fills the same structure: hydo/workload/<kind>_<name>,
hydo/checks/<kind>_<name> and hydo/regressions/*.schedule (scan_folder). SDKs write the plan
directly (sdk/rust: hydo-test).

This module is used by hydo-build (expand + stage, before validate), by manifest_space.py (the
explorer reads the expanded plan back from /etc/hydo/compose.json in an image) and by
explore.py (draw / render / shrink the composer token W=). plan() is mirrored exactly by the awk
in hydo-composer.sh; test_composer.py checks the two agree.

Three forms of W= (MANIFEST.md "Stable compositions", M98):
  seed form      W=<seed hex>,p..,q..,r..[,n..][,t..][,x<entries>][,o<command ids>]   (exploration)
  resolved form  W=:p<p>,r<ms>[,t<ms>],c<id>+<id>...,<entry>.<round>.<k>.<seed>,...   (stable)
  saved form     W=@<key>   the resolved form staged in the image as /etc/hydo/compose.saved/<key>
The resolved form names its commands by id (k indexes the c table) and lists every entry with
its round and HYDO_SEED, so it does not depend on the plan's command list or on plan(); the
shrinker outputs it and `hydo save` writes it. A regression saved in the old index form with
its `# commands:` list is resolved against that list at build time.
"""
import hashlib
import json
import os
import re
import shutil

FORMAT = 1                      # the plan format this engine reads (top-level `format:`)
WORKLOAD_KINDS = ("first", "parallel_driver", "serial_driver", "singleton_driver", "anytime", "eventually", "finally")
PLAN_KINDS = ("parallel_driver", "serial_driver", "singleton_driver", "anytime")   # composed into rounds
CHECK_KINDS = ("final", "always", "eventually")
# history checks (hydo/HISTORIES.md): the check runs on the HOST over the recorded history; its
# `run` (optional in a plan, the file itself in the hydo/ folder) is the final read-back. They
# become top-level `checks:` entries of the manifest (hydo-build's history-check support).
HISTORY_KINDS = ("linearizable", "list_append")
HISTORY_KEYS = ("history", "model", "initial", "consistency")
NAME_RE = re.compile(r"^[A-Za-z0-9_.\-]+$")
DEFAULTS = {"parallel": 4, "per_round": 2, "round_ms": 200, "command_timeout": 60, "drain_timeout": 60,
            "check_every": 2, "check_within": 30, "max_commands": 0, "budget_ms": 0}
BOUNDS = {"parallel": (1, 16), "per_round": (1, 8), "round_ms": (10, 60000), "command_timeout": (1, 3600),
          "drain_timeout": (1, 3600), "check_every": (1, 3600), "check_within": (1, 3600), "max_commands": (0, 100000),
          "budget_ms": (0, 3600000)}
KNOWN_TEST = set(DEFAULTS) | {"commands", "checks", "regressions", "description"}
KNOWN_CMD = {"name", "kind", "run", "timeout", "description", "content"}   # content: an SDK's hash of the command's code
KNOWN_CHECK = {"name", "kind", "run", "msg", "every", "within", "after", "poll", "description"} | set(HISTORY_KEYS)
KNOWN_REG = {"name", "schedule", "expect", "commands", "description", "compose", "contents", "saved_schedule"}
DEFAULT_STEPS = 20              # workload.steps when the plan composes the workload and gives none
COMPOSE_SH = "/etc/hydo/compose.sh"
SH = "/bin/busybox sh " + COMPOSE_SH
W_TOKEN = "W"                   # the composer's schedule token: W=<seed>[,p..][,q..][,r..][,n..][,x..][,o..]
SAVED_DIR = "/etc/hydo/compose.saved"   # resolved compositions of saved regressions (W=@<key>)
GUEST_MAX = 500                 # longest guest part of a schedule ("run ..."): hvx_explore keeps 511 bytes
CONTENT_RE = re.compile(r"^[0-9a-f]{8,64}$")


# ------------------------------------------------------------------ the hydo/ folder
def _header(path):
    """`# hydo: key=value ...` and `# hydo-msg: text` lines in the first 4 KiB of a text file."""
    kv, msg = {}, None
    try:
        with open(path, "rb") as f:
            head = f.read(4096)
    except OSError:
        return kv, msg
    if b"\0" in head:
        return kv, msg          # a binary: no header
    for line in head.decode("utf-8", "replace").splitlines():
        m = re.match(r"^\s*(?:#|//)\s*hydo-msg:\s*(.*?)\s*$", line)
        if m:
            msg = m.group(1)
            continue
        m = re.match(r"^\s*(?:#|//)\s*hydo:\s*(.*)$", line)
        if m:
            for tok in m.group(1).split():
                if "=" in tok:
                    k, v = tok.split("=", 1)
                    kv[k] = int(v) if re.fullmatch(r"\d+", v) else v
    return kv, msg


def _split(fname, kinds, where, E):
    for k in sorted(kinds, key=len, reverse=True):
        if fname.startswith(k + "_"):
            name = fname[len(k) + 1:]
            if not NAME_RE.match(name):
                raise E("%s/%s: the name after %s_ may use letters, digits, '.', '_', '-'" % (where, fname, k))
            return k, name
    raise E("%s/%s: unknown prefix (a command is <kind>_<name> with kind one of %s; helper_* files and "
            "dot files are ignored)" % (where, fname, ", ".join(kinds)))


def scan_folder(mdir, E):
    """The hydo/ folder next to hydo.yaml -> {"commands": [...], "checks": [...], "regressions":
    [...], "_dirs": {sub: path}}, or None when none of workload/, checks/, regressions/ exists."""
    dirs = {d: os.path.join(mdir, d) for d in ("workload", "checks", "regressions") if os.path.isdir(os.path.join(mdir, d))}
    if not dirs:
        return None
    out = {"commands": [], "checks": [], "regressions": [], "_dirs": dirs}
    for sub, kinds, key in (("workload", WORKLOAD_KINDS, "commands"), ("checks", CHECK_KINDS + HISTORY_KINDS, "checks")):
        if sub not in dirs:
            continue
        for fname in sorted(os.listdir(dirs[sub])):
            p = os.path.join(dirs[sub], fname)
            if fname.startswith(".") or fname.startswith("helper_"):
                continue
            if os.path.isdir(p):
                raise E("%s/%s: a directory; only helper_* directories may sit beside commands" % (sub, fname))
            k, name = _split(fname, kinds, sub, E)
            if not os.access(p, os.X_OK):
                raise E("%s/%s: not executable (chmod +x; a script needs a #! line)" % (sub, fname))
            kv, msg = _header(p)
            e = {"name": name, "kind": k, "run": ["/hydo/%s/%s" % (sub, fname)]}
            if key == "commands":
                with open(p, "rb") as f:
                    e["content"] = hashlib.sha256(f.read()).hexdigest()[:16]
            allowed = ("timeout",) if key == "commands" else (HISTORY_KEYS if k in HISTORY_KINDS else
                                                              ("every", "within", "after", "poll"))
            for a in kv:
                if a not in allowed:
                    raise E("%s/%s: header key %r (allowed here: %s)" % (sub, fname, a, ", ".join(allowed)))
                e[a] = kv[a]
            if msg and key == "checks":
                e["msg"] = msg
            out[key].append(e)
    if "regressions" in dirs:
        for fname in sorted(os.listdir(dirs["regressions"])):
            if not fname.endswith(".schedule"):
                continue
            out["regressions"].append(read_regression(os.path.join(dirs["regressions"], fname), E))
    return out


def read_regression(path, E):
    """A saved schedule: comment lines (`# expect: ok|violation|fp=<hex>`, free text) and one
    schedule line `<host tokens> | run <guest tokens>`."""
    sched, expect, cmds, compose, contents = None, "ok", None, None, {}
    for line in open(path):
        s = line.strip()
        if not s:
            continue
        if s.startswith("#"):
            m = re.match(r"#\s*expect:\s*(\S+)", s)
            if m:
                expect = m.group(1)
            m = re.match(r"#\s*commands:\s*(.*)$", s)
            if m:
                cmds = m.group(1).split()
            m = re.match(r"#\s*compose:\s*(\S+)\s*$", s)       # the resolved composition behind W=@<key>
            if m:
                compose = m.group(1)
            m = re.match(r"#\s*command-sha:\s*(\S+)\s+(\S+)\s*$", s)   # a used command's content when saved
            if m:
                contents[m.group(1)] = m.group(2)
            continue
        if sched is not None:
            raise E("%s: more than one schedule line" % path)
        sched = s
    name = os.path.basename(path)[:-len(".schedule")]
    r = {"name": name, "schedule": sched, "expect": expect}
    if cmds is not None:        # the old index form: the command ids W= referred to when saved
        r["commands"] = cmds
    if compose is not None:
        r["compose"] = compose
    if contents:
        r["contents"] = contents
    _check_regression(r, path, E)
    return r


def _check_regression(r, where, E):
    if not r.get("schedule") or "|" not in r["schedule"] or not r["schedule"].split("|", 1)[1].split()[:1] == ["run"]:
        raise E("%s: the schedule must look like '<host tokens> | run <guest tokens>' (got %r)" % (where, r.get("schedule")))
    ex = str(r.get("expect", "ok"))
    if ex not in ("ok", "violation") and not re.fullmatch(r"fp=[0-9a-f]{16}", ex):
        raise E("%s: expect is ok, violation or fp=<16 hex digits> (got %r)" % (where, ex))


# ------------------------------------------------------------------ validation + expansion
def _int(d, k, where, E):
    v = d[k]
    lo, hi = BOUNDS[k]
    if not isinstance(v, int) or isinstance(v, bool) or not lo <= v <= hi:
        raise E("%s.%s: a whole number in [%d, %d] (got %r)" % (where, k, lo, hi, v))
    return v


def _argv(run, where, E):
    if isinstance(run, str) and run.strip():
        return run
    if isinstance(run, list) and run and all(isinstance(a, (str, int)) for a in run):
        a = [str(x) for x in run]
        if any("\n" in x or "\0" in x for x in a):
            raise E("%s.run: an argument contains a newline or NUL" % where)
        return a
    raise E("%s.run: an argv list of strings (exec'd, no shell) or a shell command string" % where)


def shq(s):
    return "'" + str(s).replace("'", "'\\''") + "'"


def shell_of(run):
    """The guest's command line for a run: argv quoted word by word and exec'd, or the shell string."""
    return "exec " + " ".join(shq(a) for a in run) if isinstance(run, list) else run


def history_check(c, where, E):
    """A test.checks entry of kind linearizable / list_append (alias list-append) -> a manifest
    `checks:` entry: the host checks history <history> after the run; `run`, if given, is the
    final read-back (run at the end of the same boot or of the recovery boot)."""
    kind = "list-append" if c.get("kind") in ("list_append", "list-append") else "linearizable"
    if not c.get("history"):
        raise E("%s: a %s check needs history: (the name the workload records under)" % (where, c.get("kind")))
    for k in ("every", "within", "after", "poll"):
        if k in c:
            raise E("%s: %s: is not for a history check (it is evaluated once, on the host)" % (where, k))
    e = {"id": str(c["name"]), "type": kind, "history": str(c["history"]), "msg": str(c.get("msg") or c["name"])}
    for k in ("model", "initial", "consistency"):
        if k in c:
            e[k] = str(c[k])
    if c.get("run") is not None:
        e["final"] = shell_of(_argv(c["run"], where, E))
    return e


def merge_folder(m, folder, E):
    """Add the folder's commands/checks/regressions to m["test"] (a name defined in both is an error)."""
    t = m.get("test")
    if t is None:
        t = m["test"] = {}
    if not isinstance(t, dict):
        raise E("test: must be a mapping")
    for key in ("commands", "checks", "regressions"):
        have = {(e.get("kind"), e.get("name")) for e in (t.get(key) or []) if isinstance(e, dict)}
        for e in folder[key]:
            if (e.get("kind"), e["name"]) in have:
                raise E("test.%s: %s %r is defined both in hydo.yaml and in the hydo/ folder" % (key, e.get("kind", ""), e["name"]))
        if folder[key]:
            t[key] = list(t.get(key) or []) + folder[key]


def expand(m, E, folder=None, warn=None):
    """Validate m["test"] (plus the folder) and rewrite m so the generic init runs it: the
    workload becomes the composer, first commands join provisioning, eventually/finally
    commands and checks become invariants. Returns the plan meta (staged as
    /etc/hydo/compose.json), or None when m has no test commands/checks."""
    if folder:
        merge_folder(m, folder, E)
    if "format" in m:
        if m["format"] != FORMAT:
            raise E("format: this hydo reads plan format %d (got %r); update hydo or regenerate the plan" % (FORMAT, m["format"]))
    t = m.get("test")
    if t is None:
        return None
    if not isinstance(t, dict):
        raise E("test: must be a mapping")
    bad = sorted(set(t) - KNOWN_TEST)
    if bad:
        raise E("test: unknown field(s) %s (known: %s)" % (", ".join(bad), ", ".join(sorted(KNOWN_TEST))))
    par = dict(DEFAULTS)
    for k in DEFAULTS:
        if k in t:
            par[k] = _int(t, k, "test", E)
    cmds, seen = [], set()
    for i, c in enumerate(t.get("commands") or []):
        where = "test.commands[%d]" % i
        if not isinstance(c, dict):
            raise E("%s: must be a mapping" % where)
        bad = sorted(set(c) - KNOWN_CMD)
        if bad:
            raise E("%s: unknown field(s) %s" % (where, ", ".join(bad)))
        name, kind = str(c.get("name", "")), c.get("kind")
        if not NAME_RE.match(name):
            raise E("%s: name %r: letters, digits, '.', '_', '-'" % (where, name))
        if kind not in WORKLOAD_KINDS:
            raise E("%s: kind must be one of %s (got %r)" % (where, ", ".join(WORKLOAD_KINDS), kind))
        cid = "%s_%s" % (kind, name)
        if cid in seen:
            raise E("%s: duplicate command %s" % (where, cid))
        seen.add(cid)
        to = c.get("timeout", par["command_timeout"])
        if not isinstance(to, int) or isinstance(to, bool) or not 1 <= to <= 3600:
            raise E("%s: timeout is whole seconds in [1, 3600]" % where)
        run = _argv(c.get("run"), where, E)
        content = c.get("content")
        if content is not None and not (isinstance(content, str) and CONTENT_RE.match(content)):
            raise E("%s: content is 8 to 64 lowercase hex digits (a hash of the command's code)" % where)
        if content is None:     # what the command runs: its argv (or shell string) and its timeout
            content = hashlib.sha256(json.dumps([run, to]).encode()).hexdigest()[:16]
        cmds.append({"id": cid, "kind": kind, "name": name, "run": run, "timeout": to, "content": content})
    cmds.sort(key=lambda c: c["id"])
    checks, cseen, hist = [], set(), []
    for i, c in enumerate(t.get("checks") or []):
        where = "test.checks[%d]" % i
        if not isinstance(c, dict):
            raise E("%s: must be a mapping" % where)
        bad = sorted(set(c) - KNOWN_CHECK)
        if bad:
            raise E("%s: unknown field(s) %s" % (where, ", ".join(bad)))
        name, kind = str(c.get("name", "")), c.get("kind")
        if not NAME_RE.match(name):
            raise E("%s: name %r: letters, digits, '.', '_', '-'" % (where, name))
        if kind in HISTORY_KINDS + ("list-append",):
            if name in cseen:
                raise E("%s: duplicate check name %r (the name is the property id)" % (where, name))
            cseen.add(name)
            hist.append(history_check(c, where, E))
            continue
        if any(k in c for k in HISTORY_KEYS):
            raise E("%s: %s: are for kind linearizable or list_append" % (where, "/".join(HISTORY_KEYS)))
        if kind not in CHECK_KINDS:
            raise E("%s: kind must be one of %s (got %r)" % (where, ", ".join(CHECK_KINDS), kind))
        if name in cseen:
            raise E("%s: duplicate check name %r (the name is the property id)" % (where, name))
        cseen.add(name)
        e = {"name": name, "kind": kind, "run": _argv(c.get("run"), where, E), "msg": str(c.get("msg") or name)}
        for k, lo in (("every", 1), ("within", 1), ("poll", 1)):
            if k in c:
                if not isinstance(c[k], int) or isinstance(c[k], bool) or not lo <= c[k] <= 3600:
                    raise E("%s: %s is whole seconds in [1, 3600]" % (where, k))
                e[k] = c[k]
        if "after" in c:
            e["after"] = c["after"]
        if kind != "eventually" and any(k in e for k in ("within", "poll", "after")):
            raise E("%s: within/poll/after are for kind eventually" % where)
        if kind != "always" and "every" in e:
            raise E("%s: every is for kind always" % where)
        checks.append(e)
    checks.sort(key=lambda c: c["name"])
    if hist:        # history checks -> top-level checks: (hydo-build: validate_checks, hydo-init-history.sh)
        if m.get("checks") is not None and not isinstance(m["checks"], list):
            raise E("checks: must be a list")
        m["checks"] = list(m.get("checks") or []) + sorted(hist, key=lambda c: c["id"])
    regs = []
    for i, r in enumerate(t.get("regressions") or []):
        where = "test.regressions[%d]" % i
        if not isinstance(r, dict) or sorted(set(r) - KNOWN_REG):
            raise E("%s: a mapping with name, schedule, expect" % where)
        _check_regression(r, where, E)
        regs.append({"name": str(r.get("name") or "r%d" % i), "schedule": r["schedule"], "expect": str(r.get("expect", "ok"))})
        if r.get("commands") is not None:
            regs[-1]["commands"] = [str(c) for c in r["commands"]]
        for k in ("compose", "saved_schedule"):
            if r.get(k) is not None:
                regs[-1][k] = str(r[k])
        if r.get("contents") is not None:
            if not isinstance(r["contents"], dict):
                raise E("%s: contents is a mapping command id -> content hash" % where)
            regs[-1]["contents"] = {str(k): str(v) for k, v in sorted(r["contents"].items())}
    if not cmds and not checks:
        if regs:
            return {"params": par, "commands": [], "checks": [], "regressions": regs, "composed": False}
        return None
    planned = [c for c in cmds if c["kind"] in PLAN_KINDS]
    if cmds and not planned:
        raise E("test.commands: at least one parallel_driver, serial_driver, singleton_driver or anytime "
                "command is needed (the composer has nothing to run otherwise)")
    for k in (m.get("knobs") or []):
        if isinstance(k, dict) and k.get("token") == W_TOKEN:
            raise E("knobs: token W is the test composer's schedule token in a plan with test commands")
    wl = m.get("workload")
    if planned:
        if wl is None:
            wl = m["workload"] = {}
        if not isinstance(wl, dict):
            raise E("workload: must be a mapping")
        if "run" in wl or "process" in wl:
            raise E("workload.run/process: the test composer runs the workload (test.commands); remove them, "
                    "or drop the test commands")
        wl.setdefault("steps", DEFAULT_STEPS)
        wl["run"] = "%s step $HYDO_FROM $HYDO_TO" % SH
        wl["process"] = "compose.sh run1"      # the process of a command in flight (mid-request pauses)
    first = [c for c in cmds if c["kind"] == "first"]
    if first:
        prov = m.get("provision")
        m["provision"] = ("( %s\n) && %s first" % (prov, SH)) if prov else "%s first" % SH
    invs = []
    if planned:
        invs.append({"id": "compose.commands", "type": "always", "cmd": "%s verdict" % SH,
                     "msg": "no composed command reported a violation (exit status 100)"})
    for i, c in enumerate(cmds):
        if c["kind"] in ("eventually", "finally"):
            invs.append({"id": c["id"], "type": "always", "cmd": "%s end %d" % (SH, i),
                         "msg": ("after the faults stop, %s succeeds" if c["kind"] == "eventually" else
                                 "at the end, %s succeeds (skipped unless every started command completed)") % c["id"]})
    for i, c in enumerate(checks):
        if c["kind"] == "eventually":
            invs.append({"id": c["name"], "type": "eventually", "cmd": shell_of(c["run"]), "msg": c["msg"],
                         "within": c.get("within", par["check_within"]), "after": c.get("after", "heal"),
                         "poll": c.get("poll", 1)})
        else:
            v = {"id": c["name"], "type": "always", "cmd": "%s check %d" % (SH, i), "msg": c["msg"]}
            if c["kind"] == "always":
                v["every"] = c.get("every", par["check_every"])
                v["final"] = True
            invs.append(v)
    ids = {v["id"] for v in invs}
    for v in (m.get("invariants") or []):
        if isinstance(v, dict) and v.get("id") in ids:
            raise E("invariants: id %r is also a test command or check" % v.get("id"))
    m["invariants"] = invs + list(m.get("invariants") or [])
    meta = {"format": FORMAT, "params": par, "commands": cmds, "checks": checks, "regressions": regs,
            "composed": bool(planned), "steps": int((m.get("workload") or {}).get("steps") or 0), "stable_compose": 1}
    for r in regs:      # old index form -> resolved; ids and contents checked (MANIFEST "Stable compositions")
        st = regression_status(r, meta)
        if st["schedule"] != r["schedule"]:
            r.setdefault("saved_schedule", r["schedule"])
            r["schedule"] = st["schedule"]
            if st.get("compose"):
                r["compose"] = st["compose"]
        if warn:
            for w in ([st["error"]] if st["error"] else []) + st["warnings"]:
                warn("regression %s: %s" % (r["name"], w))
    return meta


def test_from_meta(meta):
    """The test: section that expands to meta again (the explorer reads meta back from an image)."""
    t = dict(meta["params"])
    t["commands"] = [dict({"name": c["name"], "kind": c["kind"], "run": c["run"], "timeout": c["timeout"]},
                          **({"content": c["content"]} if c.get("content") else {})) for c in meta["commands"]]
    t["checks"] = [{k: c[k] for k in ("name", "kind", "run", "msg", "every", "within", "after", "poll") if k in c}
                   for c in meta["checks"]]
    t["regressions"] = [dict(r) for r in meta["regressions"]]
    return t


def shell_lines(meta):
    """manifest.sh lines for a composed plan (appended last: other manifests compile unchanged)."""
    p = meta["params"]
    L = ["HYDO_COMPOSE=1"]
    for k in ("parallel", "per_round", "round_ms", "command_timeout", "drain_timeout", "max_commands", "budget_ms"):
        L.append("HYDO_COMPOSE_%s=%d" % (k.upper(), p[k]))
    # W= reaches the guest through the init's knob path (set_knob): no change to hydo-init.sh.
    # compose.sh setw keeps it and checks it (a command id this image lacks, a missing saved
    # composition: a console line and a failed knob, i.e. HYDO-ORACLE: ERROR)
    L += ["HYDO_KNOB_W_default='-'",
          "HYDO_KNOB_W_set=%s" % shq('%s setw "$HYDO_VALUE"' % SH),
          "HYDO_KNOB_W_name='test composer (W=)'"]
    return L


def stage(meta, R, here, folder=None):
    """Copy the plan into the root: the hydo/ folder (whole, helpers included) under /hydo/,
    the command and check lists the guest composer reads, the plan for the explorer, and
    hydo-composer.sh."""
    if folder:
        for sub, src in sorted(folder["_dirs"].items()):
            dst = os.path.join(R, "hydo", sub)
            if os.path.isdir(dst):
                shutil.rmtree(dst)
            shutil.copytree(src, dst, symlinks=True)
    os.makedirs(os.path.join(R, "etc/hydo"), exist_ok=True)
    with open(os.path.join(R, "etc/hydo/compose.list"), "w") as f:
        for i, c in enumerate(meta["commands"]):    # <idx> <kind> <timeout> <id> <shell command>
            f.write("%d %s %d %s %s\n" % (i, c["kind"], c["timeout"], c["id"], shell_of(c["run"]).replace("\n", " ")))
    with open(os.path.join(R, "etc/hydo/compose.checks"), "w") as f:
        for i, c in enumerate(meta["checks"]):
            f.write("%d %s %d %s %s\n" % (i, c["kind"], meta["params"]["command_timeout"], c["name"],
                                          shell_of(c["run"]).replace("\n", " ")))
    with open(os.path.join(R, "etc/hydo/compose.json"), "w") as f:
        json.dump(meta, f, indent=1, sort_keys=True)
        f.write("\n")
    saved = {}
    for r in meta["regressions"]:              # W=@<key>: the resolved composition, by key
        w = w_of(r["schedule"])
        if w and w.startswith("@") and r.get("compose") and key_of(r["compose"]) == w[1:]:
            saved[w[1:]] = r["compose"]
    if saved:
        d = os.path.join(R, SAVED_DIR.lstrip("/"))
        os.makedirs(d, exist_ok=True)
        for k in sorted(saved):
            with open(os.path.join(d, k), "w") as f:
                f.write(saved[k] + "\n")
    if meta["regressions"]:
        d = os.path.join(R, "hydo", "regressions")
        os.makedirs(d, exist_ok=True)
        for r in meta["regressions"]:
            p = os.path.join(d, r["name"] + ".schedule")
            if not os.path.exists(p):           # a plan's own regressions (SDK): written out
                with open(p, "w") as f:
                    if r.get("commands") is not None:
                        f.write("# commands: %s\n" % " ".join(r["commands"]))
                    if r.get("compose"):
                        f.write("# compose: %s\n" % r["compose"])
                    for k, v in sorted((r.get("contents") or {}).items()):
                        f.write("# command-sha: %s %s\n" % (k, v))
                    f.write("# expect: %s\n%s\n" % (r["expect"], r.get("saved_schedule") or r["schedule"]))
    shutil.copy(os.path.join(here, "hydo-composer.sh"), os.path.join(R, COMPOSE_SH.lstrip("/")))
    os.chmod(os.path.join(R, COMPOSE_SH.lstrip("/")), 0o755)


# ------------------------------------------------------------------ stable compositions (M98)
def w_of(schedule):
    """The W= value of a schedule's guest part, or None."""
    for tok in schedule.split("|", 1)[-1].split():
        if tok.startswith(W_TOKEN + "="):
            return tok[len(W_TOKEN) + 1:]
    return None


def with_w(schedule, w):
    """The schedule with its W= value replaced in place (appended when it had none)."""
    host, guest = schedule.split("|", 1)
    toks, done = guest.split(), False
    for i, x in enumerate(toks):
        if x.startswith(W_TOKEN + "="):
            toks[i], done = "%s=%s" % (W_TOKEN, w), True
    if not done:
        toks.append("%s=%s" % (W_TOKEN, w))
    return "%s| %s" % (host, " ".join(toks))


def fits(schedule):
    """The guest part fits the engines' schedule buffer (hvx_explore: 511 bytes)."""
    return len(schedule.split("|", 1)[-1].strip()) <= GUEST_MAX


def key_of(resolved):
    """The key of a resolved composition (W=@<key>): a hash of its text, so the same composition
    has the same key in every image and a changed command list does not change it."""
    return hashlib.sha256(resolved.encode()).hexdigest()[:12]


def kind_of(cid):
    for k in sorted(WORKLOAD_KINDS, key=len, reverse=True):
        if cid.startswith(k + "_"):
            return k
    return None


def parse_resolved(v):
    """':p2,r200[,t300],c<id>+<id>,<entry>.<round>.<k>.<seed>,...' -> {p, r, t, entries:
    [(entry, round, id, seed)]}. ValueError on a malformed value."""
    if not v.startswith(":"):
        raise ValueError("a resolved composition starts with ':' (got %r)" % v[:20])
    d = {"p": None, "r": None, "t": 0, "entries": []}
    ids = None
    for f in v[1:].split(","):
        if re.fullmatch(r"[prt]\d+", f):
            d[f[0]] = int(f[1:])
        elif f.startswith("c") and ids is None:
            ids = f[1:].split("+")
            if not all(NAME_RE.match(x) and kind_of(x) for x in ids):
                raise ValueError("W=%s: c lists command ids <kind>_<name>" % v[:40])
        elif re.fullmatch(r"\d+\.\d+\.\d+\.\d+", f):
            e, r, k, s = map(int, f.split("."))
            if ids is None or k >= len(ids):
                raise ValueError("W=%s: entry %s names command %d of a table of %d" % (v[:40], f, k, len(ids or [])))
            d["entries"].append((e, r, ids[k], s))
        else:
            raise ValueError("W=%s: unknown field %r" % (v[:40], f))
    if not d["p"] or not d["r"] or not d["entries"]:
        raise ValueError("W=%s: a resolved composition has p, r and at least one entry" % v[:40])
    es = d["entries"]
    if len({e for e, _, _, _ in es}) != len(es) or any(es[i][1] > es[i + 1][1] for i in range(len(es) - 1)):
        raise ValueError("W=%s: entry numbers are unique and rounds never go back" % v[:40])
    return d


def render_resolved(d):
    ids = []
    for _, _, cid, _ in d["entries"]:
        if cid not in ids:
            ids.append(cid)
    s = ":p%d,r%d" % (d["p"], d["r"]) + (",t%d" % d["t"] if d.get("t") else "") + ",c" + "+".join(ids)
    return s + "".join(",%d.%d.%d.%d" % (e, r, ids.index(cid), sd) for e, r, cid, sd in d["entries"])


def resolve_seed(w, cmds, steps, params):
    """A seed-form W over the command list cmds (dicts with id and kind, in index order) ->
    the resolved composition {p, r, t, entries by id}."""
    d = parse_w(w, params)
    return {"p": d["p"], "r": d["r"], "t": d["t"],
            "entries": [(e, r, cmds[c]["id"], s) for e, r, c, s in kept(d, steps, cmds)]}


def stable_w(w, meta, steps=None, saved=None):
    """W in the resolved form (':...'), from any form; saved: {key: resolved} for W=@<key>."""
    if w.startswith(":"):
        return w
    if w.startswith("@"):
        if not saved or w[1:] not in saved:
            raise ValueError("W=%s: the saved composition is not known here" % w)
        return saved[w[1:]]
    return render_resolved(resolve_seed(w, meta["commands"], steps or meta["steps"], meta["params"]))


def contents_of(w, meta, steps=None, saved=None):
    """{id: content hash} of the commands a composition uses (for `# command-sha:` lines)."""
    have = {c["id"]: c.get("content") for c in meta["commands"]}
    used = sorted({cid for _, _, cid, _ in parse_resolved(stable_w(w, meta, steps, saved))["entries"]})
    return {cid: have[cid] for cid in used if have.get(cid)}


def resolved_problems(d, meta, steps=None):
    """Errors of a resolved composition against this plan: a command it names that the plan does
    not have (or that is not composed), an entry in a round the plan does not run."""
    steps = steps or meta.get("steps") or DEFAULT_STEPS
    kinds = {c["id"]: c["kind"] for c in meta.get("commands") or []}
    errs = []
    missing = sorted({cid for _, _, cid, _ in d["entries"] if cid not in kinds})
    if missing:
        errs.append("the saved composition uses command%s %s, which the plan no longer has (commands now: %s); "
                    "restore %s, or delete the regression and save a new one" % (
                        "s" if len(missing) > 1 else "", ", ".join(missing), " ".join(sorted(kinds)) or "-",
                        "them" if len(missing) > 1 else "it"))
    bad = sorted({cid for _, _, cid, _ in d["entries"] if cid in kinds and kinds[cid] not in PLAN_KINDS})
    if bad:
        errs.append("command%s %s cannot be composed (not a driver or anytime command)" % ("s" if len(bad) > 1 else "", ", ".join(bad)))
    late = max([r for _, r, _, _ in d["entries"]] or [0])
    if late >= steps:
        errs.append("the saved composition runs an entry in round %d, the plan has %d rounds (workload.steps); "
                    "set steps to at least %d" % (late, steps, late + 1))
    return errs


def regression_status(r, meta, steps=None):
    """What replaying regression r on the plan meta does: {"schedule": what to run, "compose": the
    resolved value staged for W=@<key> (or None), "error": why it cannot run as saved (or
    None), "warnings": [...]}.
      - resolved (W=:...) or saved (W=@<key> + compose): its commands are looked up by id;
      - old index form (seed W= plus `commands`, the list when it was saved): when the list
        changed, it is resolved against the saved list, so it composes what it composed then;
      - seed W= without a saved list: runs as it is (it composes from the current list).
    A command whose content hash changed since the save gives a warning."""
    out = {"schedule": r["schedule"], "compose": None, "error": None, "warnings": []}
    w = w_of(r["schedule"])
    if w is None or w == "-":
        return out
    now = [c["id"] for c in meta.get("commands") or []]
    try:
        if w.startswith("@"):
            if not r.get("compose"):
                out["error"] = "W=%s but the regression has no `# compose:` line (the resolved composition)" % w
                return out
            if key_of(r["compose"]) != w[1:]:
                out["error"] = ("the `# compose:` line and W=%s disagree (one of them was edited): the line's key is @%s"
                                % (w, key_of(r["compose"])))
                return out
            out["compose"] = r["compose"]
            d = parse_resolved(r["compose"])
        elif w.startswith(":"):
            d = parse_resolved(w)
        elif r.get("commands") is not None and r["commands"] != now:
            old = [{"id": c, "kind": kind_of(c)} for c in r["commands"]]
            if not all(c["kind"] for c in old):
                out["error"] = "the saved command list has an id that is not <kind>_<name>: %s" % " ".join(r["commands"])
                return out
            d = resolve_seed(w, old, steps or meta.get("steps") or DEFAULT_STEPS, meta["params"])
            res = render_resolved(d)
            sched = with_w(r["schedule"], res)
            if not fits(sched):
                sched = with_w(r["schedule"], "@" + key_of(res))
                out["compose"] = res
            out["schedule"] = sched
            out["warnings"].append("saved in the index form with another command list (then: %s); replayed as the "
                                   "composition it ran then, by command id" % " ".join(r["commands"]))
        else:
            if r.get("commands") is None or not meta.get("commands"):
                if meta.get("composed"):
                    out["warnings"].append("W=%s is in the seed form without the command list it was saved with, so "
                                           "it composes from the current list; save it again for a stable composition" % w)
                return out
            # the same list: runs as saved; only the content check below
            d = resolve_seed(w, meta["commands"], steps or meta.get("steps") or DEFAULT_STEPS, meta["params"])
    except ValueError as e:
        out["error"] = str(e)
        return out
    errs = resolved_problems(d, meta, steps)
    if errs:
        out["error"] = "; ".join(errs)
    have = {c["id"]: c.get("content") for c in meta.get("commands") or []}
    for cid, sha in sorted((r.get("contents") or {}).items()):
        if have.get(cid) and have[cid] != sha:
            out["warnings"].append("command %s changed since the regression was saved (content %s, now %s)" % (cid, sha, have[cid]))
    return out


def regression_warning(r, meta):
    """(Older callers) one line of warnings and errors for a regression, or None."""
    st = regression_status(r, meta)
    msgs = ([st["error"]] if st["error"] else []) + st["warnings"]
    return "; ".join(msgs) if msgs else None


def saved_regression_text(x):
    """A `hydo save` file for one shrunk failure of a report (explore.py's report.json "shrunk")."""
    L = ["# found by `hydo explore` (seed %s): %s" % (x["from_seed"], x["class"]),
         "# verdict: %s" % x["min_verdict"][:200]]
    if x.get("composition"):
        L.append("# composition: %s" % x["composition"][:400])
    sched = x.get("stable_schedule") or x["minimal"]
    seedform = w_of(x["minimal"]) or ""
    if x.get("stable") and x.get("stable_note") and x.get("commands") is not None and \
            not seedform.startswith((":", "@")):
        # the resolved form did not fail the same way: keep the seed form that failed, with its
        # command list (the index form), so it replays exactly while the list is unchanged and is
        # resolved by id when it changes
        sched = x["minimal"]
        L.append("# commands: %s" % " ".join(x["commands"]))
        for k, v in sorted((x.get("contents") or {}).items()):
            L.append("# command-sha: %s %s" % (k, v))
        L.append("# note: %s" % x["stable_note"])
    elif x.get("stable"):
        w = w_of(sched)
        if w and w.startswith("@"):
            L.append("# compose: %s" % x["stable"])
        for k, v in sorted((x.get("contents") or {}).items()):
            L.append("# command-sha: %s %s" % (k, v))
        if x.get("stable_note"):
            L.append("# note: %s" % x["stable_note"])
    elif x.get("commands") is not None:     # an older report: the index form and its command list
        L.append("# commands: %s" % " ".join(x["commands"]))
    L.append("# expect: violation      (change to ok when the bug is fixed)")
    L.append(sched)
    return "\n".join(L) + "\n"


def engine_version(here):
    for p in (os.path.join(here, "..", "VERSION"), os.path.join(here, "..", "..", "VERSION")):
        if os.path.isfile(p):
            return open(p).read().strip()
    return "0.0.0"


def _vt(s):
    n = [int(x) for x in re.findall(r"\d+", s)[:3]]
    return tuple(n + [0] * (3 - len(n)))


def check_version(want, have, E, warn):
    """hydo_version: "0.1" / "0.1.2" (same major.minor; a different patch is a warning) or
    ">=0.1" (at least). A mismatch is an error unless HYDO_VERSION_CHECK=warn."""
    want = str(want).strip()
    if not re.fullmatch(r"(>=)?\d+(\.\d+){0,2}", want):
        raise E("hydo_version: \"0.1\", \"0.1.0\" or \">=0.1\" (got %r)" % want)
    h = _vt(have)
    if want.startswith(">="):
        ok, why = h >= _vt(want[2:]), "at least %s" % want[2:]
    else:
        w = _vt(want)
        ok, why = h[:2] == w[:2], "%s.x" % ".".join(map(str, w[:2]))
        if ok and len(re.findall(r"\d+", want)) == 3 and h != w:
            warn("hydo_version: the plan names %s, this hydo is %s (same major.minor: building)" % (want, have))
    if not ok:
        msg = "hydo_version: the plan needs hydo %s, this hydo is %s" % (why, have)
        if os.environ.get("HYDO_VERSION_CHECK") == "warn":
            warn(msg + " (HYDO_VERSION_CHECK=warn: building anyway)")
        else:
            raise E(msg + " (install that hydo, or set HYDO_VERSION_CHECK=warn)")


# ------------------------------------------------------------------ the plan (mirrored in hydo-composer.sh)
M31 = 2147483647


class Minstd:
    """Park-Miller "minimal standard" generator: exact in awk's doubles (s*48271 < 2^47)."""
    def __init__(self, seed):
        self.s = seed % (M31 - 1) + 1
        for _ in range(8):
            self.next()

    def next(self):
        self.s = (self.s * 48271) % M31
        return self.s

    def below(self, n):
        return self.next() % n


def plan(seed, q, steps, cmds):
    """All entries (i, round, cmd index, entry seed) of the composition, before W's filters.
    cmds: the plan's commands in index order (dicts with "kind")."""
    idx = lambda k: [i for i, c in enumerate(cmds) if c["kind"] == k]
    P, SE, SG, A = idx("parallel_driver"), idx("serial_driver"), idx("singleton_driver"), idx("anytime")
    rng = Minstd(seed)
    out = []

    def add(r, c):
        out.append((len(out), r, c, rng.next()))
    single = bool(SG) and (not (P or SE) or rng.below(4) == 0)
    g = SG[rng.below(len(SG))] if single else -1
    for r in range(steps):
        if single:
            if r == 0:
                add(r, g)
        elif SE and (not P or rng.below(4) == 0):
            add(r, SE[rng.below(len(SE))])
        elif P:
            k = 1 + rng.below(q)
            for _ in range(k):
                add(r, P[rng.below(len(P))])
        if A and ((not (P or SE or SG)) or rng.below(3) == 0):
            add(r, A[rng.below(len(A))])
    return out


def _ilist(s):
    out = set()
    for it in s.split(".") if s else []:
        if "-" in it:
            a, b = it.split("-")
            out.update(range(int(a), int(b) + 1))
        elif it:
            out.add(int(it))
    return out


def _rlist(xs):
    xs, out, i = sorted(xs), [], 0
    while i < len(xs):
        j = i
        while j + 1 < len(xs) and xs[j + 1] == xs[j] + 1:
            j += 1
        out.append("%d" % xs[i] if i == j else "%d-%d" % (xs[i], xs[j]))
        i = j + 1
    return ".".join(out)


def parse_w(w, params):
    """A seed-form W value -> dict seed p q r n x o (defaults from the plan's params). o holds
    command indices (old form, `o0.4-6`) or command ids (`oparallel_driver_set+serial_driver_x`)."""
    d = {"seed": 0, "p": params["parallel"], "q": params["per_round"], "r": params["round_ms"],
         "n": params["max_commands"], "t": params.get("budget_ms", 0), "x": set(), "o": set()}
    if not w or w == "-":
        return d
    f = w.split(",")
    d["seed"] = int(f[0], 16) & 0xFFFFFFFF
    for x in f[1:]:
        if not x:
            continue
        k, v = x[0], x[1:]
        if k in "pqrnt":
            d[k] = int(v)
        elif k == "o" and re.search(r"[A-Za-z]", v):
            d[k] = set(v.split("+"))
        elif k in "xo":
            d[k] = _ilist(v)
        else:
            raise ValueError("W=%s: unknown field %r" % (w, x))
    return d


def render_w(d):
    s = "%08x,p%d,q%d,r%d" % (d["seed"], d["p"], d["q"], d["r"])
    if d.get("n"):
        s += ",n%d" % d["n"]
    if d.get("t"):
        s += ",t%d" % d["t"]
    if d.get("x"):
        s += ",x" + _rlist(d["x"])
    if d.get("o"):
        if all(isinstance(c, int) for c in d["o"]):
            s += ",o" + _rlist(d["o"])
        else:
            s += ",o" + "+".join(sorted(str(c) for c in d["o"]))
    return s


def kept(d, steps, cmds):
    """The entries W runs: the plan minus x (entries) and o (commands), cut at n."""
    es = [e for e in plan(d["seed"], d["q"], steps, cmds)
          if e[0] not in d["x"] and e[2] not in d["o"] and cmds[e[2]]["id"] not in d["o"]]
    return es[:d["n"]] if d["n"] else es


def entries_of(w, meta, steps=None, saved=None):
    """The entries (entry, round, command index in meta's list, seed) any form of W runs here;
    ValueError when it names a command this plan does not have."""
    steps = steps or meta["steps"]
    if w.startswith(":") or w.startswith("@"):
        d = parse_resolved(stable_w(w, meta, steps, saved))
        errs = resolved_problems(d, meta, steps)
        if errs:
            raise ValueError("; ".join(errs))
        ix = {c["id"]: i for i, c in enumerate(meta["commands"])}
        return [(e, r, ix[cid], s) for e, r, cid, s in d["entries"]]
    return kept(parse_w(w, meta["params"]), steps, meta["commands"])


def describe(w, meta, steps=None):
    """One line per round, e.g. 'r0: parallel_driver_set x2 | r1: serial_driver_audit' (reports)."""
    steps = steps or meta["steps"]
    cmds = meta["commands"]
    rounds = {}
    for i, r, c, _ in entries_of(w, meta, steps):
        rounds.setdefault(r, []).append(cmds[c]["id"])
    return " | ".join("r%d: %s" % (r, ", ".join(rounds[r])) for r in sorted(rounds))


# ------------------------------------------------------------------ explorer side
def draw(rng_next, meta):
    """A W value for a schedule: seed, concurrency and round length from the explorer's own
    random stream (rng_next() -> 64-bit int)."""
    p = meta["params"]
    seed = rng_next() & 0xFFFFFFFF
    par = 1 + rng_next() % p["parallel"]
    q = 1 + rng_next() % p["per_round"]
    r = [max(10, p["round_ms"] // 2), p["round_ms"], min(60000, p["round_ms"] * 2)][rng_next() % 3]
    return render_w({"seed": seed, "p": par, "q": q, "r": r, "n": p["max_commands"], "t": p["budget_ms"], "x": set(), "o": set()})


def candidates(w, meta, steps=None):
    """Simpler compositions than W, most aggressive first: one command never run (o), fewer
    commands (n halves), no concurrency (p=1, then p-1), half of the remaining entries, then
    single entries (when few are left)."""
    steps = steps or meta["steps"]
    if w.startswith(":"):
        return _resolved_candidates(w)
    if w.startswith("@"):
        return []
    d = parse_w(w, meta["params"])
    cmds = meta["commands"]
    ks = kept(d, steps, cmds)
    out = []
    o_ids = {cmds[c]["id"] if isinstance(c, int) else c for c in d["o"] if not isinstance(c, int) or c < len(cmds)}

    def var(**kw):
        t = dict(d, x=set(d["x"]), o=set(o_ids))
        t.update(kw)
        out.append(render_w(t))
    used = sorted({e[2] for e in ks})
    if len(used) > 1:
        for c in used:
            var(o=o_ids | {cmds[c]["id"]})     # by id: a stored candidate names the command, not its position
    if len(ks) > 1:
        var(n=max(1, len(ks) // 2))
    if d["p"] > 1:
        var(p=1)
        if d["p"] > 2:
            var(p=d["p"] - 1)
    if len(ks) > 3:
        half = len(ks) // 2
        var(x=d["x"] | {e[0] for e in ks[:half]})      # (the second half goes with n above)
    if 1 < len(ks) <= 8:
        for e in ks:
            var(x=d["x"] | {e[0]})
    seen, res = set(), []
    for c in out:
        if c != w and c not in seen:
            seen.add(c)
            res.append(c)
    return res


def _resolved_candidates(w):
    """Simpler resolved compositions: one command's entries dropped, the second half dropped,
    no concurrency, the first half dropped, single entries (when 8 or fewer are left)."""
    d = parse_resolved(w)
    es, out = d["entries"], []

    def var(entries=None, **kw):
        t = dict(d, **kw)
        t["entries"] = es if entries is None else entries
        if t["entries"]:
            out.append(render_resolved(t))
    used = []
    for _, _, cid, _ in es:
        if cid not in used:
            used.append(cid)
    if len(used) > 1:
        for cid in used:
            var([e for e in es if e[2] != cid])
    if len(es) > 1:
        var(es[:max(1, len(es) // 2)])
    if d["p"] > 1:
        var(p=1)
        if d["p"] > 2:
            var(p=d["p"] - 1)
    if len(es) > 3:
        var(es[len(es) // 2:])
    if 1 < len(es) <= 8:
        for i in range(len(es)):
            var(es[:i] + es[i + 1:])
    seen, res = set(), []
    for c in out:
        if c != w and c not in seen:
            seen.add(c)
            res.append(c)
    return res
