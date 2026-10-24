"""hydo_composer -- the test composer: a project's test written as small commands and
properties, composed by hydo into seeded sequences and concurrent mixes (MANIFEST.md "The test
composer").

The engine-facing PLAN is the manifest's `test:` section (YAML or JSON, language-neutral):

    test:
      parallel: 4  per_round: 2  round_ms: 200  command_timeout: 60  drain_timeout: 60
      check_every: 2  check_within: 30  max_commands: 0  budget_ms: 0
      commands:   [{name, kind, run, timeout}]     kind: first | parallel_driver | serial_driver |
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
"""
import json
import os
import re
import shutil

FORMAT = 1                      # the plan format this engine reads (top-level `format:`)
WORKLOAD_KINDS = ("first", "parallel_driver", "serial_driver", "singleton_driver", "anytime", "eventually", "finally")
PLAN_KINDS = ("parallel_driver", "serial_driver", "singleton_driver", "anytime")   # composed into rounds
CHECK_KINDS = ("final", "always", "eventually")
# history checks (hvx/HISTORIES.md): the check runs on the HOST over the recorded history; its
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
KNOWN_CMD = {"name", "kind", "run", "timeout", "description"}
KNOWN_CHECK = {"name", "kind", "run", "msg", "every", "within", "after", "poll", "description"} | set(HISTORY_KEYS)
KNOWN_REG = {"name", "schedule", "expect", "commands", "description"}
DEFAULT_STEPS = 20              # workload.steps when the plan composes the workload and gives none
COMPOSE_SH = "/etc/hydo/compose.sh"
SH = "/bin/busybox sh " + COMPOSE_SH
W_TOKEN = "W"                   # the composer's schedule token: W=<seed>[,p..][,q..][,r..][,n..][,x..][,o..]


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
    sched, expect, cmds = None, "ok", None
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
            continue
        if sched is not None:
            raise E("%s: more than one schedule line" % path)
        sched = s
    name = os.path.basename(path)[:-len(".schedule")]
    r = {"name": name, "schedule": sched, "expect": expect}
    if cmds is not None:        # the command ids W= referred to when the schedule was saved
        r["commands"] = cmds
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


def expand(m, E, folder=None):
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
        cmds.append({"id": cid, "kind": kind, "name": name, "run": _argv(c.get("run"), where, E), "timeout": to})
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
    return {"format": FORMAT, "params": par, "commands": cmds, "checks": checks, "regressions": regs,
            "composed": bool(planned), "steps": int((m.get("workload") or {}).get("steps") or 0)}


def test_from_meta(meta):
    """The test: section that expands to meta again (the explorer reads meta back from an image)."""
    t = dict(meta["params"])
    t["commands"] = [{"name": c["name"], "kind": c["kind"], "run": c["run"], "timeout": c["timeout"]} for c in meta["commands"]]
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
    # W= reaches the guest through the init's knob path (set_knob): no change to hydo-init.sh
    L += ["HYDO_KNOB_W_default='-'",
          "HYDO_KNOB_W_set=%s" % shq('echo "$HYDO_VALUE" > /tmp/hydo/compose.W'),
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
    if meta["regressions"]:
        d = os.path.join(R, "hydo", "regressions")
        os.makedirs(d, exist_ok=True)
        for r in meta["regressions"]:
            p = os.path.join(d, r["name"] + ".schedule")
            if not os.path.exists(p):           # a plan's own regressions (SDK): written out
                with open(p, "w") as f:
                    if r.get("commands") is not None:
                        f.write("# commands: %s\n" % " ".join(r["commands"]))
                    f.write("# expect: %s\n%s\n" % (r["expect"], r["schedule"]))
    shutil.copy(os.path.join(here, "hydo-composer.sh"), os.path.join(R, COMPOSE_SH.lstrip("/")))
    os.chmod(os.path.join(R, COMPOSE_SH.lstrip("/")), 0o755)


# ------------------------------------------------------------------ version pin
def regression_warning(r, meta):
    """W= names commands by their index in the plan's command list; a regression saved with
    another list composes something else. The warning to print, or None."""
    if "W=" not in r["schedule"].split("|", 1)[-1] or r.get("commands") is None:
        return None
    now = [c["id"] for c in meta.get("commands") or []]
    if r["commands"] == now:
        return None
    return ("the plan's commands changed since %s was saved (then: %s; now: %s), so its W= composes different "
            "commands; re-explore and save it again" % (r["name"], " ".join(r["commands"]) or "-", " ".join(now) or "-"))


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
    """W value -> dict seed p q r n x o (defaults from the plan's params)."""
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
        s += ",o" + _rlist(d["o"])
    return s


def kept(d, steps, cmds):
    """The entries W runs: the plan minus x (entries) and o (commands), cut at n."""
    es = [e for e in plan(d["seed"], d["q"], steps, cmds) if e[0] not in d["x"] and e[2] not in d["o"]]
    return es[:d["n"]] if d["n"] else es


def describe(w, meta, steps=None):
    """One line per round, e.g. 'r0: parallel_driver_set x2 | r1: serial_driver_audit' (reports)."""
    steps = steps or meta["steps"]
    d = parse_w(w, meta["params"])
    cmds = meta["commands"]
    rounds = {}
    for i, r, c, _ in kept(d, steps, cmds):
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
    d = parse_w(w, meta["params"])
    cmds = meta["commands"]
    ks = kept(d, steps, cmds)
    out = []

    def var(**kw):
        t = dict(d, x=set(d["x"]), o=set(d["o"]))
        t.update(kw)
        out.append(render_w(t))
    used = sorted({e[2] for e in ks})
    if len(used) > 1:
        for c in used:
            var(o=d["o"] | {c})
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
