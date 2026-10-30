#!/usr/bin/env python3
"""Tests of error logs as findings (MANIFEST.md "Error logs as findings"):

  1. the guest side (hydo-init-errlog.sh) under the real busybox: el_mark / el_report on
     fixture logs -- which lines are errors, their templates, labels, the summary record, the
     power-cut path, a log rotated by a restart, a file on the data disk;
  2. the templates are stable: lines that differ only in timestamps, numbers, ids, hex, uuids and
     ephemeral ports give one template; service ports and error codes are kept;
  3. the host side (hydo_errors.py): signatures, known-list matching with with:/during:,
     `hydo errors accept` round trip, unknown: fail;
  4. hydo-build: a manifest without errors: compiles exactly as before (manifest.sh and /sbin/init
     of every example manifest equal those of a reference hydo-build), and one with errors:
     validates and splices the extension.

    HYDO_BUSYBOX=<static busybox> python3 hvx/onboard/tests/test_errlog.py [<reference hydo-build>]

Without HYDO_BUSYBOX (or busybox on PATH) parts 1-2 are skipped; without a reference hydo-build
part 4 compares against `git show HEAD:hvx/onboard/hydo-build`.
"""
import glob, importlib.machinery, importlib.util, json, os, re, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ONB = os.path.dirname(HERE)
sys.path.insert(0, ONB)
import hydo_errors as he  # noqa: E402

BB = os.environ.get("HYDO_BUSYBOX") or shutil.which("busybox")
EXT = os.path.join(ONB, "hydo-init-errlog.sh")
FAILS = []


def check(cond, what):
    print(("ok    " if cond else "FAIL  ") + what)
    if not cond:
        FAILS.append(what)


def load_build(path, name):
    loader = importlib.machinery.SourceFileLoader(name, path)
    spec = importlib.util.spec_from_loader(name, loader)
    m = importlib.util.module_from_spec(spec)
    loader.exec_module(m)
    return m


HB = load_build(os.path.join(ONB, "hydo-build"), "hb_new")

MYSQL = [
    "2026-01-01T00:00:01.000001Z 0 [System] [MY-010116] [Server] /nix/store/x-mysql/bin/mysqld starting as process 77",
    "2026-01-01T00:00:02.000002Z 0 [Warning] [MY-010915] [Server] 'NO_ZERO_DATE' is deprecated",
    "2026-01-01T00:00:12.345678Z 0 [ERROR] [MY-012592] [InnoDB] Operating system error number 2 in a file operation.",
    "2026-01-01T00:07:44.000999Z 3 [ERROR] [MY-012592] [InnoDB] Operating system error number 28 in a file operation.",
]
KAFKA = [
    "INFO kafka.server.KafkaRaftServer Kafka Server started",
    "WARN org.apache.kafka.clients.NetworkClient [Producer clientId=producer-1] Connection to node 1 (localhost/127.0.0.1:9092) could not be established.",
    "ERROR [Controller id=1, targetBrokerId=1] Connection to node 1 (localhost/127.0.0.1:9093) could not be established. (org.apache.kafka.clients.NetworkClient)",
    "java.net.ConnectException: Connection refused",
    "\tat java.base/sun.nio.ch.Net.pollConnect(Native Method)",
    "Caused by: java.io.IOException: boom",
    "\t... 12 more",
    "ERROR [Controller id=1, targetBrokerId=1] Connection to node 1 (localhost/127.0.0.1:9093) could not be established. (org.apache.kafka.clients.NetworkClient)",
    "INFO kafka.log.LogManager Loading logs",
    "java.lang.IllegalStateException: request 3f2a9c1e-0b7d-4c1e-9a2b-1234567890ab failed at 0x7ffd1234 id deadbeef42",
]
FRONT = [
    'time=2026-01-01T00:00:05Z level=error msg="dial tcp 127.0.0.1:54321->127.0.0.1:3306: connect: connection refused"',
    'time=2026-01-01T00:00:09Z level=error msg="dial tcp 127.0.0.1:41000->127.0.0.1:3306: connect: connection refused"',
    "panic: runtime error: index out of range [3] with length 3",
    'HYDO-ASSERT {"t":"always","id":"x","ok":false}',
    "hydostack: server listening on 127.0.0.1:50051",
    'level=error msg="contains \\"quotes\\" and back\\\\slash and hydo-explore-workload-done"',
]
REDIS = [
    "57:C 01 Jan 2026 00:00:00.001 # oO0OoO0OoO0Oo Redis is starting oO0OoO0OoO0Oo",
    "57:M 01 Jan 2026 00:00:00.002 * Ready to accept connections tcp",
    "91:M 01 Jan 2026 00:03:10.100 # !!! Warning: short read while loading the AOF file appendonly.aof.1.incr.aof!",
]


def manifest(level="error"):
    return {"name": "t", "services": [
        {"name": "mysql", "cmd": "x", "ready": "x", "errors": {"files": ["/tmp/mysqld.err"]}},
        {"name": "kafka", "cmd": "x", "ready": "x"},
        {"name": "front", "cmd": "x", "ready": "x"},
        {"name": "redis", "cmd": "x", "ready": "x", "errors": {"format": "redis"}}],
        "workload": {"steps": 20, "run": "x"}, "invariants": [{"id": "i", "cmd": "true"}],
        "errors": {"level": level}}


def guest_run(m, steps_script, recovery=False):
    """Run the extension under busybox sh with stub init functions; returns the console text."""
    HB.validate(m)
    order = [s["name"] for s in m["services"]]
    lines = HB.compile_errors(m, order)
    d = tempfile.mkdtemp(prefix="errlog-")
    H = os.path.join(d, "h")
    os.makedirs(H)
    sh = ["BB=%s" % BB, "H=%s" % H, "HYDO_SERVICES=%s" % HB.shq(" ".join(order)), "HYDO_STEPS=20",
          "PHASE=serve", "CMD=%s" % ("'oracle 1 2'" if recovery else "provision"), "cut=0"] + lines + [
          "log() { echo \"hydo-ob: $*\"; }",
          "svc_sig() { :; }",
          "_el_orig_mount_data() { :; }", "_el_orig_step_range() { :; }", "_el_orig_retry_workload() { :; }",
          "_el_orig_failed_ids() { :; }", "_el_orig_final_verdict() { echo 'HYDO-ORACLE: OK invariants=1 held'; }",
          "_el_orig_die_oracle() { echo \"$1\"; }",
          open(EXT).read(),
          "W() { printf '%s\\n' \"$2\" >> \"$1\"; }   # append one line to a log",
          steps_script.replace("$D", d)]
    p = os.path.join(d, "t.sh")
    open(p, "w").write("\n".join(sh) + "\n")
    r = subprocess.run([BB, "sh", p], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return r.stdout, d


def recs(out):
    rs = [json.loads(l.split(" ", 1)[1]) for l in out.splitlines() if l.startswith("HYDO-ERRLOG ")]
    sm = [json.loads(l.split(" ", 1)[1]) for l in out.splitlines() if l.startswith("HYDO-ERRLOG-SUM ")]
    return rs, sm


def put(name, lines):
    return "".join("W $H/%s %s\n" % (name, HB.shq(l)) for l in lines)


def test_guest():
    if not BB:
        print("skip  guest tests: no busybox (set HYDO_BUSYBOX)")
        return None
    d0 = tempfile.mkdtemp(prefix="errlog-f-")
    mysqlerr = os.path.join(d0, "mysqld.err")
    m = manifest()
    m["services"][0]["errors"]["files"] = [mysqlerr]
    wf = lambda ls: "".join("W %s %s\n" % (mysqlerr, HB.shq(l)) for l in ls)
    script = (put("mysql.log", ["mysqld starting"]) + put("redis.log", REDIS[:2]) + put("kafka.log", KAFKA[:2]) +
              "mount_data x\n" + wf(MYSQL[:2]) +
              "step_range 0 10\n" + put("front.log", FRONT[:1]) + wf(MYSQL[2:3]) + put("kafka.log", KAFKA[2:7]) +
              # a restart of kafka before step 10: start_svc moves the log into .log.old
              "cat $H/kafka.log >> $H/kafka.log.old; : > $H/kafka.log\n" +
              "step_range 10 20\n" + put("kafka.log", KAFKA[7:]) + put("front.log", FRONT[1:]) + wf(MYSQL[3:]) +
              "retry_workload\n" + put("redis.log", REDIS[2:]) +
              "final_verdict\nfinal_verdict\n")
    out, d = guest_run(m, script)
    print(out)
    rs, sm = recs(out)
    by = {(r["svc"], r["tpl"]): r for r in rs}
    tpls = [r["tpl"] for r in rs]
    check(len(sm) == 1 and sm[0]["boot"] == "first" and sm[0]["steps"] == 20, "one summary record, first boot, steps=20")
    check(out.rstrip().splitlines()[-1].startswith("HYDO-ORACLE: OK"), "the verdict line stays the last line")
    check(out.count("HYDO-ERRLOG-SUM") == 1, "el_report runs once per boot")
    my = [r for r in rs if r["svc"] == "mysql"]
    check(len(my) == 1 and my[0]["n"] == 2 and my[0]["at"] == "0,10", "MySQL: two [ERROR] lines, one kind, in ranges 0 and 10: %s" % my)
    check(my and "[MY-012592]" in my[0]["tpl"] and "<time>" in my[0]["tpl"] and "number #" in my[0]["tpl"],
          "MySQL template keeps the error code, drops time and errno-like numbers: %s" % (my[0]["tpl"] if my else None))
    check(not any("Warning" in r["tpl"] for r in rs if r["svc"] != "redis"), "warning-level lines are not errors at level error")
    ka = [r for r in rs if r["svc"] == "kafka"]
    check(len(ka) == 2, "Kafka: the ERROR line (twice, across the restart) and the standalone exception: %s" % ka)
    ke = [r for r in ka if r["tpl"].startswith("ERROR")]
    check(ke and ke[0]["n"] == 2 and ke[0]["at"] == "0,10" and "#.#.#.#:9093" in ke[0]["tpl"],
          "Kafka ERROR kind: n=2 over the rotated log, service port kept: %s" % ke)
    check(not any(r["tpl"].startswith("java.net.ConnectException") for r in ka), "an exception inside an error's trace is not counted again")
    ex = [r for r in ka if "IllegalStateException" in r["tpl"]]
    check(ex and "<uuid>" in ex[0]["tpl"] and "<hex>" in ex[0]["tpl"], "uuid and hex replaced: %s" % (ex[0]["tpl"] if ex else None))
    fr = [r for r in rs if r["svc"] == "front"]
    lv = [r for r in fr if "level=error" in r["tpl"] and "dial" in r["tpl"]]
    check(len(lv) == 1 and lv[0]["n"] == 2 and "#.#.#.#:#->#.#.#.#:3306" in lv[0]["tpl"],
          "logfmt level=error: ephemeral ports replaced, the service port kept, one kind: %s" % lv)
    check(any(r["tpl"].startswith("panic: runtime error") for r in fr), "Go panic line")
    check(not any("HYDO" in r["tpl"] and "ASSERT" in r["tpl"] for r in fr), "our own HYDO- lines are never errors")
    q = [r for r in fr if "quotes" in r["tpl"]]
    check(q and '"' not in q[0]["ex"] and "\\" not in q[0]["ex"] and "hydo-" not in q[0]["ex"],
          "no double quote, backslash or hydo- marker in the record text: %s" % (q[0]["ex"] if q else None))
    rd = [r for r in rs if r["svc"] == "redis"]
    check(len(rd) == 2 and rd[0]["at"] == "s" and rd[1]["at"] == "end", "Redis preset: the # lines, at setup and at end: %s" % rd)
    check(all("<date> <time>" in r["tpl"] for r in rd), "Redis dates replaced")
    shutil.rmtree(d); shutil.rmtree(d0)

    # the power cut path: failed_ids with cut=1 reports, the recovery boot labels everything r
    out, d = guest_run(manifest(), "step_range 0 12\n" + put("kafka.log", KAFKA[2:3]) + "cut=1\nfailed_ids\nfailed_ids\n")
    rs, sm = recs(out)
    check(len(sm) == 1 and rs and rs[0]["at"] == "0", "power cut: the first boot reports at the cut: %s" % rs)
    shutil.rmtree(d)
    out, d = guest_run(manifest(), put("kafka.log", KAFKA[2:3]) + "mount_data x\nfinal_verdict\n", recovery=True)
    rs, sm = recs(out)
    check(sm and sm[0]["boot"] == "recovery" and rs and rs[0]["at"] == "r", "recovery boot: label r: %s %s" % (sm, rs))
    shutil.rmtree(d)
    # a file on the data disk: lines from before this boot's mount are not reported again
    d0 = tempfile.mkdtemp(prefix="errlog-f-")
    f = os.path.join(d0, "err.log")
    open(f, "w").write(MYSQL[2] + "\n")
    m = manifest()
    m["services"][0]["errors"]["files"] = [f]
    out, d = guest_run(m, "mount_data x\n" + "W %s %s\n" % (f, HB.shq(MYSQL[3])) + "final_verdict\n", recovery=True)
    rs, sm = recs(out)
    my = [r for r in rs if r["svc"] == "mysql"]
    check(len(my) == 1 and my[0]["n"] == 1, "a data-disk file: only the lines written after this boot's mount: %s" % my)
    shutil.rmtree(d); shutil.rmtree(d0)
    # die_oracle reports before its verdict line
    out, d = guest_run(manifest(), put("kafka.log", KAFKA[2:3]) + "die_oracle 'HYDO-ORACLE: VIOLATION service cannot recover (kafka)'\n")
    L = out.strip().splitlines()
    check(L[-1].startswith("HYDO-ORACLE: VIOLATION") and any(l.startswith("HYDO-ERRLOG-SUM") for l in L[:-1]),
          "die_oracle: the records come before its verdict")
    shutil.rmtree(d)
    # max: at most HYDO_ERR_MAX kinds printed, the summary says how many there were
    m = manifest(); m["errors"]["max"] = 2
    out, d = guest_run(m, put("front.log", ["ERROR kind %s" % w for w in ("a", "b", "c", "d")]) + "final_verdict\n")
    rs, sm = recs(out)
    check(len(rs) == 2 and sm[0]["kinds"] == 4 and sm[0]["shown"] == 2, "errors.max bounds the records: %s" % sm)
    shutil.rmtree(d)
    # level: warning adds warning lines
    out, d = guest_run(manifest("warning"), put("kafka.log", KAFKA[:2]) + "final_verdict\n")
    rs, sm = recs(out)
    check(len(rs) == 1 and rs[0]["tpl"].startswith("WARN"), "level: warning: WARN lines too: %s" % rs)
    shutil.rmtree(d)
    return True


def test_host():
    con = "\n".join([
        'hydo-ob: x',
        'HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"s","tpl":"#:C <date> <time> # Redis is starting","ex":"57:C ..."}',
        'HYDO-ERRLOG {"svc":"kafka","src":"log","n":3,"at":"10,11","tpl":"ERROR Connection to node # refused","ex":"ERROR Connection to node 1 refused"}',
        'HYDO-ERRLOG-SUM {"boot":"first","steps":20,"lines":4,"kinds":2,"shown":2}'])
    rec = "\n".join([
        'HYDO-ERRLOG {"svc":"kafka","src":"log","n":1,"at":"r","tpl":"ERROR Connection to node # refused","ex":"..."}',
        'HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"r","tpl":"#:M <date> <time> # !!! Warning: short read while loading the AOF file appendonly.aof.#.incr.aof!","ex":"..."}',
        'HYDO-ERRLOG-SUM {"boot":"recovery","steps":20,"lines":2,"kinds":2,"shown":2}'])
    p = he.parse_consoles([("a", con), ("b", rec)])
    check(len(p["kinds"]) == 3, "three kinds over both boots")
    k = [x for x in p["kinds"] if x["svc"] == "kafka"][0]
    check(k["n"] == 4 and k["at"] == ["10", "11", "r"], "merged across boots: %s" % k)
    check(k["sig"] == he.signature("kafka", "ERROR Connection to node # refused") and re.fullmatch(r"[0-9a-f]{12}", k["sig"]),
          "signature = sha256(svc, template)[:12]")
    check(he.parse_consoles([("a", "no records")]) is None, "no summary record: errors: is off, nothing changes")
    d = tempfile.mkdtemp(prefix="known-")
    f = os.path.join(d, "known-errors.yaml")
    ban = [x for x in p["kinds"] if x["svc"] == "redis" and "starting" in x["tpl"]][0]
    aof = [x for x in p["kinds"] if "short read" in x["tpl"]][0]
    he.accept(f, [ban], "the startup banner: Redis logs it at its warning level", "test", [], [], date="2026-10-30")
    he.accept(f, [aof], "a power cut truncates the AOF; aof-load-truncated loads what was synced", "test", ["C"], [], date="2026-10-30")
    he.accept(f, [k], "Kafka's clients retry while kafka is down", "test", [], ["K=kafka"], date="2026-10-30")
    he.accept(f, [k], "again", "test", [], [])      # already there: not added twice
    kn = he.load_known(f)
    check(len(kn["known"]) == 3 and kn["unknown"] == "report", "accept: three entries, loads back: %s" % open(f).read())
    sched_cut = "disk=cut dseed=1 | run K=kafka@10+2 C=14"
    an = he.analyse([("a", con), ("b", rec)], sched_cut, kn)
    check([x["svc"] for x in an["new"]] == ["kafka"], "kafka's kind is new: its lines in the recovery boot are outside K=kafka@10+2")
    an2 = he.analyse([("a", con)], "disk=none | run K=kafka@10+2", kn)
    check(an2["new"] == [] and len(an2["known"]) == 2, "inside the kill window (10..12) kafka's kind is known")
    an3 = he.analyse([("a", con)], "disk=none | run K=kafka@10+1", kn)
    check(an3["new"] == [] , "the range right after the window counts as during (label 11 with K=kafka@10+1)")
    an4 = he.analyse([("a", con)], "disk=none | run K=kafka@3+2", kn)
    check([x["svc"] for x in an4["new"]] == ["kafka"], "outside the kill window: new")
    an5 = he.analyse([("b", rec)], "disk=none | run", kn)
    check(any("short read" in x["tpl"] for x in an5["new"]), "with: [C] -- the AOF kind without a power cut is new")
    res = he.apply({"verdict": "HYDO-ORACLE: OK x"}, an4, kn)
    check(res["verdict"] == "HYDO-ORACLE: OK x" and res["errors_new"] == [k["sig"]], "unknown: report keeps the verdict")
    kn["unknown"] = "fail"
    res = he.apply({"verdict": "HYDO-ORACLE: OK x"}, an4, kn)
    check(res["verdict"].startswith("VIOLATION error-log [kafka]") and res["pre_errlog_verdict"].startswith("HYDO-ORACLE: OK"),
          "unknown: fail turns OK into a VIOLATION: %s" % res["verdict"])
    w = he.fault_windows("disk=cut cf=7 | run K=redis@3+2 P=kafka@5~10:200 N=mysql@2-6:delay,100ms C=12 T=4:-30 R=everysec", 20)
    check(w == [("disk=cut", 0, 20), ("cf=7", 0, he.INF), ("K=redis@3+2", 3, 5), ("P=kafka@5~10:200", 5, 6),
                ("N=mysql@2-6:delay,100ms", 2, 6), ("C=12", 12, he.INF), ("T=4:-30", 4, he.INF), ("R=everysec", -1, he.INF)],
          "fault windows: %s" % w)
    check(he.fault_matches("C", "cf=7") and he.fault_matches("K", "K=redis@3+2") and he.fault_matches("K=redis", "K=redis@3+2")
          and not he.fault_matches("K=red", "K=redis@3+2") and he.fault_matches("disk", "disk=cut"), "fault patterns")
    try:
        open(f, "a").write("  - match: '('\n    reason: x\n")
        he.load_known(f)
        check(False, "a bad regular expression is refused")
    except he.ErrorsError:
        check(True, "a bad regular expression is refused")
    shutil.rmtree(d)


def test_build(ref):
    """Manifests without errors: compile and splice exactly as the reference hydo-build does."""
    top_dir = os.path.dirname(os.path.dirname(ONB))
    if ref is None and not os.path.exists(os.path.join(top_dir, ".git")):
        print("skip  build identity: not a repository checkout and no reference hydo-build given")
        return
    if ref is None:     # hvx/ where this branch left main (hydo-build reads its neighbours: init, composer, history)
        top = tempfile.mkdtemp(prefix="hbref-")
        base = os.environ.get("HYDO_REF") or subprocess.run(["git", "merge-base", "HEAD", "main"], cwd=top_dir,
                                                            stdout=subprocess.PIPE, text=True).stdout.strip()
        arc = subprocess.run(["git", "archive", base, "hvx/onboard", "hvx/history"],
                             cwd=os.path.dirname(os.path.dirname(ONB)), stdout=subprocess.PIPE).stdout
        subprocess.run(["tar", "-x", "-C", top], input=arc, check=True)
        ref = os.path.join(top, "hvx", "onboard", "hydo-build")
    n = 0
    for p in sorted(glob.glob(os.path.join(ONB, "examples", "**", "*.yaml"), recursive=True)):
        if p.endswith("known-errors.yaml") or "redis-errors" in p or "hydo-errors" in p or os.path.basename(p).startswith("hydo-grpcgen"):
            continue
        a = subprocess.run([sys.executable, ref, p, "--print-shell"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, cwd=ONB)
        b = subprocess.run([sys.executable, os.path.join(ONB, "hydo-build"), p, "--print-shell"], stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, text=True, cwd=ONB)
        if a.returncode != 0:
            continue        # needs files on the build box (OCI images, a proto dir): skipped in both
        ai = subprocess.run([sys.executable, ref, p, "--print-init"], stdout=subprocess.PIPE, text=True, cwd=ONB).stdout
        bi = subprocess.run([sys.executable, os.path.join(ONB, "hydo-build"), p, "--print-init"], stdout=subprocess.PIPE, text=True, cwd=ONB).stdout
        if a.stdout != b.stdout or ai != bi:
            check(False, "identical compile without errors: %s" % p)
        n += 1
    check(n >= 20, "%d example manifests without errors: compile to the same manifest.sh, prelude and /sbin/init" % n)
    p = os.path.join(ONB, "examples", "redis-errors", "hydo", "hydo.yaml")
    r = subprocess.run([sys.executable, os.path.join(ONB, "hydo-build"), p, "--print-init"], stdout=subprocess.PIPE, text=True)
    check(r.returncode == 0 and "_el_orig_final_verdict()" in r.stdout and "el_report()" in r.stdout,
          "a manifest with errors: splices hydo-init-errlog.sh")
    for bad, why in (({"errors": {"bogus": 1}}, "unknown field"), ({"errors": {"max": 0}}, "max"),
                     ({"errors": None, "svc_errors": {"format": "redis"}}, "services[0].errors needs errors:"),
                     ({"errors": True, "svc_errors": {"match": "("}}, "bad regex"),
                     ({"errors": True, "svc_errors": {"files": ["relative.log"]}}, "relative file")):
        m = {"name": "t", "services": [{"name": "a", "cmd": "x", "ready": "x"}], "workload": {"steps": 2, "run": "x"},
             "invariants": [{"id": "i", "cmd": "true"}]}
        if bad.get("errors") is not None:
            m["errors"] = bad["errors"]
        if "svc_errors" in bad:
            m["services"][0]["errors"] = bad["svc_errors"]
        try:
            HB.validate(m)
            check(False, "refused: %s" % why)
        except HB.ManifestError:
            check(True, "refused: %s" % why)


if __name__ == "__main__":
    test_guest()
    test_host()
    test_build(sys.argv[1] if len(sys.argv) > 1 else None)
    print("%d failed" % len(FAILS) if FAILS else "all passed")
    sys.exit(1 if FAILS else 0)
