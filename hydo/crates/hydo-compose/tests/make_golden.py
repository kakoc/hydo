#!/usr/bin/env python3
"""Record what hydo/onboard/hydo_compose.py (the reference) does for every case of the hydo-compose
equivalence test, so that `cargo test -p hydo-compose` proves the Rust port byte-identical
without running Python (tests/equivalence.rs replays tests/cases.json against tests/golden.json).

    python3 hydo/crates/hydo-compose/tests/make_golden.py --pyyaml PYTHON_WITH_PYYAML [--stdlib PYTHON]

Each case copies a Compose project into a fresh directory D (D/proj), writes D/lock.json, and
runs `hydo_compose.main(argv)` in a subprocess with a clean environment (PATH, HOME=D/home,
LANG and the case's variables) and cwd D. The golden is the exit status, stdout and stderr (D
replaced by <DIR>) and the sha256 of every file the run created or changed. The reference is
the Python WITH PyYAML (the Rust reads YAML as PyYAML's safe_load does); --stdlib also runs a
Python without PyYAML (hydo-build's YAML subset) and records whether it gave the same result.

Lock files: the unit tests' LOCK (hydo/onboard/tests/test_hydo_compose.py) is written to
tests/fixtures/unit-lock.json; each real-world project gets tests/fixtures/realworld/<name>/
lock.json with a synthetic entry per image (no network). The voting app uses its own lock.
"""
import hashlib, json, os, re, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
ONB = os.path.join(TOP, "hydo", "onboard")
UNIT = os.path.join(ONB, "tests", "compose")
RW = os.path.join(HERE, "fixtures", "realworld")
VOTING = os.path.join(ONB, "examples", "compose-voting")

RUN = ("import sys; sys.path.insert(0, %r); import hydo_compose; sys.exit(hydo_compose.main(sys.argv[1:]))" % ONB)


def rel(p):
    return os.path.relpath(p, TOP)


# ---------------------------------------------------------------- locks
def unit_lock():
    sys.path.insert(0, os.path.join(ONB, "tests"))
    sys.path.insert(0, ONB)
    import test_hydo_compose as t
    return {"images": t.LOCK}


FAM = [  # (repository name prefix, ports, volumes, workdir, healthcheck)
    ("postgres", [5432], ["/var/lib/postgresql/data"], "/", None),
    ("mysql", [3306, 33060], ["/var/lib/mysql"], "/", None),
    ("mariadb", [3306], ["/var/lib/mysql"], "/", None),
    ("redis", [6379], ["/data"], "/data", None),
    ("valkey", [6379], ["/data"], "/data", None),
    ("mongo", [27017], ["/data/configdb", "/data/db"], "/", None),
    ("grafana", [3000], ["/var/lib/grafana"], "/usr/share/grafana",
     {"Test": ["CMD-SHELL", "wget -q -O- localhost:3000/api/health"], "Interval": 10000000000, "Retries": 5}),
    ("elasticsearch", [9200, 9300], ["/usr/share/elasticsearch/data"], "/usr/share/elasticsearch", None),
    ("traefik", [80], [], "/", None),
    ("nginx", [80], [], "/", None),
    ("memcached", [11211], [], "/", None),
    ("clickhouse", [8123, 9000], ["/var/lib/clickhouse"], "/", None),
    ("kafka", [9092], ["/bitnami/kafka"], "/", None),
    ("temporal", [7233], [], "/etc/temporal", None),
]


def synth_entry(ref):
    sys.path.insert(0, ONB)
    import hydo_compose
    reg, repo, tag, dig = hydo_compose.parse_ref(ref)
    base = repo.rsplit("/", 1)[-1]
    fam = next((f for f in FAM if base.startswith(f[0])), None)
    ports, vols, wd, hc = (fam[1], fam[2], fam[3], fam[4]) if fam else ([8080], [], "/app", None)
    conf = {"Entrypoint": ["docker-entrypoint.sh"] if fam else None, "Cmd": [base], "Env": ["PATH=/usr/bin:/bin"],
            "WorkingDir": wd, "User": "" if fam else "1000", "ExposedPorts": {"%d/tcp" % p: {} for p in ports} or None,
            "Healthcheck": hc, "Volumes": {v: {} for v in vols} or None}
    h = lambda s: "sha256:" + hashlib.sha256(s.encode()).hexdigest()
    p = {"amd64": {"digest": h("a" + ref), "variant": "", "variants": [""], "config": conf}}
    if len(base) % 2 == 0:       # some images also have arm64
        p["arm64"] = {"digest": h("b" + ref), "variant": "v8", "variants": ["v8"], "config": conf}
    return {"ref": ref, "name": hydo_compose.short_name(reg, repo), "tag": tag, "registry": reg, "repo": repo,
            "index_digest": h(ref), "platforms": p}


def refs_of(compose, env):
    """The image references of a Compose file after interpolation (as the importer sees them)."""
    code = ("import json, os, sys; sys.path.insert(0, %r); import hydo_compose as h\n"
            "i = h.Importer(sys.argv[1], '/nonexistent/hydo.yaml', {})\n"
            "i.load()\n"
            "print(json.dumps(sorted({str(s['image']) for s in i.doc['services'].values() if isinstance(s, dict) and 'image' in s})))" % ONB)
    r = subprocess.run([PYY, "-c", code, compose], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                       env=dict(env, PATH=os.environ["PATH"]))
    if r.returncode != 0:
        return []
    return json.loads(r.stdout)


# ---------------------------------------------------------------- cases
def cases():
    C = []

    def add(name, fixture, args, lock, env=None, compose="docker-compose.yml", setup=None, raw=False):
        C.append({"name": name, "fixture": fixture, "compose": compose, "args": args, "lock": lock,
                  "env": env or {}, "setup": setup, "raw": raw})

    std = [("", []), ("loopback", ["--network", "loopback"]), ("noaddr", ["--no-addr"]),
           ("bridge", ["--network", "bridge"])]
    for fx in sorted(os.listdir(UNIT)):
        for tag, extra in std:
            add("unit-%s-%s" % (fx, tag or "default"), rel(os.path.join(UNIT, fx)), extra, "unit")
        add("unit-%s-noresolve" % fx, rel(os.path.join(UNIT, fx)), ["--no-resolve"], "none")
    u = lambda f: rel(os.path.join(UNIT, f))
    add("unit-envfiles-shellenv", u("envfiles"), [], "unit", {"APP_TAG": "9.9", "HOSTNAME_ONLY": "from-shell"})
    add("unit-envfiles-useenv", u("envfiles"), ["--use-env"], "unit", {"APP_TAG": "1", "HOSTNAME_ONLY": "x"})
    add("unit-envfiles-envfile", u("envfiles"), ["--env-file", "proj/common.env"], "unit")
    add("unit-envfiles-nodotenv", u("envfiles"), ["--no-resolve"], "none", setup="rm .env")
    add("unit-unsupported-profile", u("unsupported"), ["--profile", "seed"], "unit")
    add("unit-unsupported-profile-loopback", u("unsupported"), ["--profile", "seed", "--network", "loopback"], "unit")
    add("unit-health-named", u("health"), ["--name", "My Shop_2", "--images-dir", "imgs"], "unit")
    add("unit-health-name9", u("health"), ["--name", "9lives"], "unit")
    add("unit-simple-netbad", u("simple"), ["--network", "host"], "unit")
    add("unit-simple-netempty", u("simple"), ["--network", ""], "unit")
    add("unit-simple-outmissing", u("simple"), ["--out"], "unit")
    add("unit-simple-unknownopt", u("simple"), ["--bogus"], "unit")
    add("unit-simple-twofiles", u("simple"), ["proj/other.yml"], "unit")
    add("unit-simple-help", u("simple"), ["-h"], "unit")
    for i, r in enumerate([[], ["-h"], ["--help"], ["proj/docker-compose.yml", "--no-resolve", "--lock", "x.json"]]):
        add("unit-raw-%d" % i, u("simple"), r, "none", raw=True)
    add("unit-simple-partlock", u("simple"), ["--no-resolve"], "partial")
    add("unit-simple-json", u("simple"), [], "unit", compose="compose.json",
        setup="json:{\"services\": {\"cache\": {\"image\": \"redis:7.2.4\"}}}")
    add("unit-build-only", u("simple"), ["--no-resolve"], "none",
        setup="write:docker-compose.yml:services:\n  app:\n    build: .\n")
    add("unit-not-compose", u("simple"), [], "unit", setup="write:docker-compose.yml:just: text\n")
    add("unit-missing-file", u("simple"), [], "unit", compose="nope.yml")
    add("unit-dup-names", u("simple"), [], "unit",
        setup="write:docker-compose.yml:services:\n  my-app:\n    image: redis:7.2.4\n  my_app:\n    image: redis:7.2.4\n")
    add("unit-unknown-dep", u("simple"), [], "unit",
        setup="write:docker-compose.yml:services:\n  a:\n    image: redis:7.2.4\n    depends_on: [b]\n")
    # the voting app with its own lock (the M84/M95 example)
    for tag, extra in std + [("noresolve", ["--no-resolve"])]:
        add("voting-%s" % (tag or "default"), rel(VOTING), extra, "voting" if tag != "noresolve" else "none")
    add("voting-named", rel(VOTING), ["--name", "voting", "--images-dir", "images"], "voting")
    # real-world projects with synthetic locks
    touch = {"immich": "touch:.env", "mastodon": "touch:.env.production"}
    for fx in sorted(os.listdir(RW)):
        comps = ["docker-compose.yml"] + (["docker-compose.images.yml"]
                                          if os.path.exists(os.path.join(RW, fx, "docker-compose.images.yml")) else [])
        variants = [(cf, fx + ("-images" if "images" in cf else ""), touch.get(fx)) for cf in comps]
        if os.path.exists(os.path.join(RW, fx, "example.env")):
            variants.append(("docker-compose.yml", fx + "-dotenv", "dotenv"))
        for cf, k, setup in variants:
            for tag, extra in std[:3]:
                add("rw-%s-%s" % (k, tag or "default"), rel(os.path.join(RW, fx)), extra, "fixture", compose=cf,
                    setup=setup)
            add("rw-%s-noresolve" % k, rel(os.path.join(RW, fx)), ["--no-resolve"], "none", compose=cf, setup=setup)
    return C


# ---------------------------------------------------------------- running
def prepare(case, d):
    src = os.path.join(TOP, case["fixture"])
    shutil.copytree(src, os.path.join(d, "proj"), symlinks=True)
    os.makedirs(os.path.join(d, "home"))
    s = case.get("setup")
    if s == "rm .env":
        os.unlink(os.path.join(d, "proj", ".env"))
    elif s and s.startswith("json:"):
        open(os.path.join(d, "proj", case["compose"]), "w").write(s[5:])
    elif s == "dotenv":
        shutil.copy(os.path.join(d, "proj", "example.env"), os.path.join(d, "proj", ".env"))
    elif s and s.startswith("touch:"):
        for f in s[6:].split(","):
            open(os.path.join(d, "proj", f), "a").close()
    elif s and s.startswith("write:"):
        _, fn, body = s.split(":", 2)
        open(os.path.join(d, "proj", fn), "w").write(body)
    lk = case["lock"]
    if lk == "unit":
        shutil.copy(os.path.join(HERE, "fixtures", "unit-lock.json"), os.path.join(d, "lock.json"))
    elif lk == "partial":
        full = json.load(open(os.path.join(HERE, "fixtures", "unit-lock.json")))
        json.dump({"images": {k: v for k, v in full["images"].items() if k != "redis:7.2.4"}},
                  open(os.path.join(d, "lock.json"), "w"))
    elif lk == "voting":
        shutil.copy(os.path.join(VOTING, "hydo-compose.lock.json"), os.path.join(d, "lock.json"))
    elif lk == "fixture":
        shutil.copy(os.path.join(src, "lock.json"), os.path.join(d, "lock.json"))


def argv(case):
    if case.get("raw"):
        return case["args"]
    return (["proj/" + case["compose"]] + (["--out", "proj/hydo.yaml"] if "--out" not in case["args"] else [])
            + (["--lock", "lock.json"] if case["lock"] != "none" else ["--lock", "nolock.json"]) + case["args"])


def env_of(case, d):
    e = {"PATH": "/usr/bin:/bin", "HOME": os.path.join(d, "home"), "LANG": "C.UTF-8"}
    e.update(case["env"])
    return e


def snap(d):
    out = {}
    for top, dirs, files in os.walk(d):
        dirs.sort()
        for f in sorted(files):
            p = os.path.join(top, f)
            if os.path.islink(p):
                continue
            out[os.path.relpath(p, d)] = (hashlib.sha256(open(p, "rb").read()).hexdigest(),
                                          oct(os.stat(p).st_mode & 0o777))
    return out


def run(py, case):
    d = os.path.realpath(tempfile.mkdtemp(prefix="hydo-compose-golden-"))
    try:
        prepare(case, d)
        before = snap(d)
        r = subprocess.run([py, "-c", RUN] + argv(case), cwd=d, env=env_of(case, d), stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE)
        after = snap(d)
        norm = lambda b: b.decode("utf-8", "replace").replace(d, "<DIR>")
        files = {k: list(v) for k, v in after.items() if before.get(k) != v}
        if b"Traceback (most recent call last)" in r.stderr:
            # the Python stopped with an exception: keep only its last line (paths of the frames vary)
            return {"rc": r.returncode, "stdout": norm(r.stdout), "files": files,
                    "crash": norm(r.stderr).strip().splitlines()[-1]}, {}
        texts = {}
        for k in files:
            if k.endswith(("hydo.yaml", "fetch-images.sh", ".json")):
                texts[k] = open(os.path.join(d, k), encoding="utf-8").read()
        g = {"rc": r.returncode, "stdout": norm(r.stdout), "stderr": norm(r.stderr), "files": files}
        if len(g["stderr"]) > 4000:     # long warning lists: keep the start and the hash
            g["stderr_sha256"] = hashlib.sha256(g["stderr"].encode()).hexdigest()
            g["stderr"] = g["stderr"][:1500]
        return g, texts
    finally:
        shutil.rmtree(d)


def with_images(text):
    """A copy of a Compose file in which every service that has build: and no image: gets
    `image: example/<service>:dev` (so the importer goes past the build: refusal)."""
    import re
    lines = text.split("\n")
    out, svc_start = [], None
    blocks = []                                  # (start, end) line ranges of the services
    in_services = False
    for i, ln in enumerate(lines):
        if re.match(r"^services:\s*$", ln):
            in_services = True
            continue
        if in_services and re.match(r"^\S", ln):
            in_services = False
        if in_services and re.match(r"^  [A-Za-z0-9_.-]+:\s*$", ln):
            blocks.append([i, len(lines)])
            if len(blocks) > 1:
                blocks[-2][1] = i
        elif not in_services and blocks and blocks[-1][1] == len(lines) and re.match(r"^\S", ln):
            blocks[-1][1] = i
    add = {}
    for a, b in blocks:
        body = lines[a + 1:b]
        if any(re.match(r"^    image:", x) for x in body):
            continue
        for j, x in enumerate(body):
            if re.match(r"^    build:", x):
                add[a + 1 + j] = "    image: example/%s:dev" % lines[a].strip().rstrip(":")
                break
    for i, ln in enumerate(lines):
        if i in add:
            out.append(add[i])
        out.append(ln)
    return "\n".join(out)


def main():
    global PYY
    a = sys.argv[1:]
    PYY = a[a.index("--pyyaml") + 1]
    std = a[a.index("--stdlib") + 1] if "--stdlib" in a else None
    keep = a[a.index("--keep") + 1] if "--keep" in a else None
    json.dump(unit_lock(), open(os.path.join(HERE, "fixtures", "unit-lock.json"), "w"), indent=1, sort_keys=True)
    for fx in sorted(os.listdir(RW)):
        src = open(os.path.join(RW, fx, "docker-compose.yml")).read()
        if re.search(r"(?m)^    build:", src):
            open(os.path.join(RW, fx, "docker-compose.images.yml"), "w").write(with_images(src))
    for fx in sorted(os.listdir(RW)):
        refs = sorted(set(refs_of(os.path.join(RW, fx, "docker-compose.yml"), {"HOME": "/nonexistent"})) |
                      set(refs_of(os.path.join(RW, fx, "docker-compose.images.yml"), {"HOME": "/nonexistent"})))
        if os.path.exists(os.path.join(RW, fx, "example.env")):
            t = tempfile.mkdtemp()
            shutil.copytree(os.path.join(RW, fx), os.path.join(t, "p"))
            shutil.copy(os.path.join(t, "p", "example.env"), os.path.join(t, "p", ".env"))
            refs = sorted(set(refs) | set(refs_of(os.path.join(t, "p", "docker-compose.yml"), {"HOME": "/nonexistent"})))
            shutil.rmtree(t)
        json.dump({"images": {r: synth_entry(r) for r in refs}}, open(os.path.join(RW, fx, "lock.json"), "w"),
                  indent=1, sort_keys=True)
    cs = cases()
    json.dump(cs, open(os.path.join(HERE, "cases.json"), "w"), indent=1)
    gold, agree, differ = {}, 0, []
    for c in cs:
        g, texts = run(PYY, c)
        gold[c["name"]] = g
        if keep:
            kd = os.path.join(keep, c["name"])
            os.makedirs(kd, exist_ok=True)
            json.dump(g, open(os.path.join(kd, "golden.json"), "w"), indent=1)
            for k, t in texts.items():
                open(os.path.join(kd, k.replace("/", "__")), "w").write(t)
        if std:
            s, _ = run(std, c)
            g["stdlib_same"] = (s == {k: v for k, v in g.items() if k != "stdlib_same"})
            if g["stdlib_same"]:
                agree += 1
            else:
                differ.append(c["name"])
    json.dump(gold, open(os.path.join(HERE, "golden.json"), "w"), indent=1, sort_keys=True)
    print("%d cases recorded" % len(cs))
    if std:
        print("the Python without PyYAML gave the same result in %d of %d cases; differs: %s"
              % (agree, len(cs), ", ".join(differ) or "none"))


if __name__ == "__main__":
    main()
