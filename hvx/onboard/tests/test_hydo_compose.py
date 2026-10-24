#!/usr/bin/env python3
"""Unit tests for hvx/onboard/hydo-compose (hydo_compose.py) and the services[].mounts launcher
extension in hydo_oci.py. No network: image metadata comes from a lock file written by the test.

    python3 -m unittest discover -s hvx/onboard/tests -v
"""
import io, json, os, shutil, subprocess, sys, tarfile, tempfile, hashlib, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ONB = os.path.dirname(HERE)
sys.path.insert(0, ONB)
import hydo_compose  # noqa: E402
import hydo_oci      # noqa: E402

FIX = os.path.join(HERE, "compose")


def cfg(ports=(), vols=(), user="", ent=None, cmd=None, hc=None, wd="/"):
    return {"Entrypoint": ent, "Cmd": cmd, "Env": ["PATH=/usr/bin:/bin"], "WorkingDir": wd, "User": user,
            "ExposedPorts": {"%d/tcp" % p: {} for p in ports} or None, "Healthcheck": hc,
            "Volumes": {v: {} for v in vols} or None}


def entry(ref, conf, arm=True, variant=""):
    reg, repo, tag, dig = hydo_compose.parse_ref(ref)
    p = {"amd64": {"digest": "sha256:" + hashlib.sha256(("a" + ref).encode()).hexdigest(), "variant": variant,
                   "variants": [variant] if variant else [""], "config": conf}}
    if arm:
        p["arm64"] = {"digest": "sha256:" + hashlib.sha256(("b" + ref).encode()).hexdigest(), "variant": "v8",
                      "variants": ["v8"], "config": conf}
    return {"ref": ref, "name": hydo_compose.short_name(reg, repo), "tag": tag, "registry": reg, "repo": repo,
            "index_digest": "sha256:" + hashlib.sha256(ref.encode()).hexdigest(), "platforms": p}


LOCK = {
    "redis:7.2.4": entry("redis:7.2.4", cfg(ports=[6379], vols=["/data"], wd="/data",
                                            ent=["docker-entrypoint.sh"], cmd=["redis-server"])),
    "postgres:16.4": entry("postgres:16.4", cfg(ports=[5432], vols=["/var/lib/postgresql/data"],
                                                ent=["docker-entrypoint.sh"], cmd=["postgres"])),
    "example/web:1.0": entry("example/web:1.0", cfg(ports=[80]), arm=False),
    "example/api:2.3": entry("example/api:2.3", cfg(ports=[9000], user="app", ent=["/app/api"], cmd=["serve"])),
    "example/app:1": entry("example/app:1", cfg()),
    "example/app:1.2": entry("example/app:1.2", cfg()),
    "example/seed:1": entry("example/seed:1", cfg()),
    "example/worker:1": entry("example/worker:1", cfg(hc={"Test": ["CMD-SHELL", "worker --ping"],
                                                           "Interval": 5000000000, "Retries": 2}), variant="v3"),
}


class Base(unittest.TestCase):
    def run_import(self, fixture, *extra, expect_rc=0, env=None):
        d = tempfile.mkdtemp(prefix="hydo-compose-test-")
        self.addCleanup(shutil.rmtree, d)
        src = os.path.join(d, "proj")
        shutil.copytree(os.path.join(FIX, fixture), src)
        lock = os.path.join(d, "lock.json")
        json.dump({"images": LOCK}, open(lock, "w"))
        out = os.path.join(src, "hydo.yaml")
        err = io.StringIO()
        old = dict(os.environ)
        try:
            if env:
                os.environ.update(env)
            sys_err, sys.stderr = sys.stderr, err
            rc = hydo_compose.main([os.path.join(src, "docker-compose.yml"), "--out", out, "--lock", lock] + list(extra))
        finally:
            sys.stderr = sys_err
            os.environ.clear(); os.environ.update(old)
        self.assertEqual(rc, expect_rc, err.getvalue())
        self.err = err.getvalue()
        if rc != 0:
            return None
        self.text = open(out).read()
        self.fetch = open(os.path.join(src, "fetch-images.sh")).read()
        hb = hydo_compose._hydo_build()
        m = hb.parse_yaml(self.text)            # the hydo-build subset parser reads it
        hb.validate(m)                          # and hydo-build accepts it
        self.order = hb.service_order(m["services"])
        self.svc = {s["name"]: s for s in m["services"]}
        return m


class Simple(Base):
    def test_simple(self):
        m = self.run_import("simple")
        s = self.svc["cache"]
        self.assertEqual(s["cmd"], ["redis-server", "--appendonly", "yes", "--save", ""])   # Compose splits words
        self.assertEqual(s["env"], {"TZ": "UTC", "EMPTY": ""})
        self.assertEqual(s["port"], 6379)                     # the container port, not the published one
        self.assertEqual(s["addr"], "127.0.0.11")
        self.assertEqual(s["ready"], "nc -z -w 2 127.0.0.11 6379")
        self.assertNotIn("recovery", s)
        self.assertEqual(s["image"]["x86_64"], "images/redis-7.2.4-amd64.docker.tar")
        self.assertEqual(s["image"]["arm64"], "images/redis-7.2.4-arm64-oci")
        self.assertEqual(s["image_ref"], "redis:7.2.4")
        self.assertIn("127.0.0.11 cache\n", m["root"]["text"]["/etc/hosts"])
        dsts = [x["dst"] for x in s["mounts"]]
        self.assertEqual(dsts, ["/etc/hosts", "/run", "/dev/shm"])      # image VOLUME /data = the data disk
        self.assertIn(LOCK["redis:7.2.4"]["platforms"]["amd64"]["digest"], self.fetch)  # fetched by digest
        self.assertIn(LOCK["redis:7.2.4"]["index_digest"], self.text)    # tag and digest both recorded
        self.assertEqual(m["guest"]["ram_mib"], 1024)
        self.assertIn("TODO", m["workload"]["run"])
        self.assertIn("exit 2", m["invariants"][0]["cmd"])

    def test_deterministic_output(self):
        self.run_import("simple")
        a = self.text
        self.run_import("simple")
        self.assertEqual(a, self.text)

    def test_json_input(self):
        d = tempfile.mkdtemp(); self.addCleanup(shutil.rmtree, d)
        p = os.path.join(d, "compose.json")
        json.dump({"services": {"cache": {"image": "redis:7.2.4"}}}, open(p, "w"))
        lock = os.path.join(d, "lock.json"); json.dump({"images": LOCK}, open(lock, "w"))
        self.assertEqual(hydo_compose.main([p, "--lock", lock, "--out", os.path.join(d, "hydo.yaml")]), 0)

    def test_no_resolve(self):
        self.run_import("simple", "--no-resolve", "--lock", "/nonexistent/lock.json")
        self.assertIn("NOT pinned", self.fetch)
        self.assertIn("not pinned by digest", self.err)


class Health(Base):
    def test_depends_on_and_healthchecks(self):
        m = self.run_import("health")
        self.assertEqual(m["name"], "shop")
        self.assertEqual(self.order, ["db", "migrate", "api", "web"])          # dependencies first
        w, a, db = self.svc["web"], self.svc["api"], self.svc["db"]
        self.assertEqual(w["depends_on"], ["api"])
        self.assertEqual(w["ready"], "timeout 3 hydo-oci web /bin/sh -c 'wget -q -O - http://localhost/health | grep -q ok'")
        self.assertEqual(w["ready_timeout"], 60 + 10 * 5 + 60)                 # start_period + interval x retries
        # CMD form, run as the service's user like Docker's healthcheck
        self.assertEqual(a["ready"], "timeout 30 hydo-oci api /.hydo/busybox chpst -u 1000:1000 /app/healthcheck --port 9000")
        self.assertEqual(a["port"], 9000)                                       # from expose:
        self.assertEqual(db["ready"], "timeout 30 hydo-oci db /bin/sh -c 'pg_isready -U postgres'")
        self.assertEqual(db["port"], 5432)                                      # from the image's EXPOSE
        self.assertEqual(db["env"], {"POSTGRES_PASSWORD": "example"})
        # the one-shot job: idles, runs once in the setup of the service that waits for it
        mg = self.svc["migrate"]
        self.assertEqual(mg["entrypoint"], ["/.hydo/busybox", "sleep", "2147483647"])
        self.assertIs(mg["recovery"], False)                                    # restart: "no"
        self.assertEqual(mg["ready"], "true")
        self.assertIn("hydo-oci migrate /.hydo/busybox chpst -u app /app/api /app/migrate up", a["setup"])
        self.assertIn("/tmp/hydo/job.migrate.done", a["setup"])
        self.assertEqual([s["addr"] for s in m["services"]], ["127.0.0.11", "127.0.0.12", "127.0.0.13", "127.0.0.14"])
        self.assertNotIn("arm64", json.dumps(w["image"]))                       # amd64-only image


class Volumes(Base):
    def test_volumes(self):
        m = self.run_import("volumes")
        db, app = self.svc["db"]["mounts"], self.svc["app"]["mounts"]
        self.assertIn({"src": "/data/vol/pgdata", "dst": "/var/lib/postgresql/data", "copy_up": True}, db)
        files = {f["dst"]: f["src"] for f in m["root"]["files"]}
        init = [x for x in db if x["dst"] == "/docker-entrypoint-initdb.d/init.sql"][0]
        self.assertEqual(files[init["src"]], "init.sql")                       # copied into the root
        self.assertFalse(any(x["dst"] == "/etc/localtime" for x in db))         # host file skipped
        self.assertIn("/etc/localtime", self.err)
        self.assertIn({"src": "/data/vol/bind-missing-dir", "dst": "/var/backups"}, db)   # Docker creates it empty
        self.assertIn({"tmpfs": True, "dst": "/var/cache/pg", "size": "32m"}, db)
        sec = [x for x in db if x["dst"] == "/run/secrets/db_password"][0]
        self.assertEqual(files[sec["src"]], "secret.txt")
        self.assertIn({"src": "/data/vol/shared", "dst": "/srv/shared", "copy_up": True}, app)
        self.assertIn("external", self.err)
        conf = [x for x in app if x["dst"] == "/etc/app"][0]
        self.assertEqual(files[conf["src"]], "conf")
        self.assertIn({"src": "/data/vol/anon-app-var-lib-app-cache", "dst": "/var/lib/app/cache", "copy_up": True}, app)
        self.assertEqual(sorted(m["data"]["dirs"]), sorted(["vol/pgdata", "vol/bind-missing-dir", "vol/shared",
                                                            "vol/anon-app-var-lib-app-cache"]))
        self.assertGreaterEqual(m["data"]["size_mib"], 1024)


class EnvFiles(Base):
    def test_env(self):
        m = self.run_import("envfiles", env={"APP_TAG": "9.9", "HOSTNAME_ONLY": "from-shell"})
        a = self.svc["app"]
        self.assertEqual(a["image_ref"], "example/app:1.2")    # the shell's APP_TAG is NOT used ...
        self.assertIn("APP_TAG", self.err)                      # ... and the importer says so
        self.assertEqual(a["env"]["DB_URL"], "postgres://shop:p@ss word@db/app")
        self.assertEqual(a["env"]["GREETING"], "hello world")
        self.assertEqual(a["env"]["OVERRIDE"], "from-environment")    # environment: wins over env_file
        self.assertEqual(a["env"]["PLAIN"], "a b")
        self.assertEqual(a["env"]["PRICE"], "$5")                     # $$ is a literal $
        self.assertNotIn("HOSTNAME_ONLY", a["env"])
        self.assertEqual(a["cmd"], ["sh", "-c", "echo $HOME; exec app"])
        self.assertEqual(self.svc["db"]["image_ref"], "postgres:16.4")

    def test_use_env(self):
        self.run_import("envfiles", "--use-env", env={"APP_TAG": "1", "HOSTNAME_ONLY": "x"})
        self.assertEqual(self.svc["app"]["image_ref"], "example/app:1")
        self.assertEqual(self.svc["app"]["env"]["HOSTNAME_ONLY"], "x")

    def test_required_variable(self):
        d = tempfile.mkdtemp(); self.addCleanup(shutil.rmtree, d)
        shutil.copytree(os.path.join(FIX, "envfiles"), os.path.join(d, "p"))
        os.unlink(os.path.join(d, "p", ".env"))
        err = io.StringIO(); e0, sys.stderr = sys.stderr, err
        try:
            rc = hydo_compose.main([os.path.join(d, "p", "docker-compose.yml"), "--no-resolve"])
        finally:
            sys.stderr = e0
        self.assertEqual(rc, 1)
        self.assertIn("required variable DB_PASS", err.getvalue())


class Unsupported(Base):
    def test_warnings(self):
        m = self.run_import("unsupported")
        for k in ("build:", "privileged", "cap_add", "devices", "networks", "replicas", "sysctls", "network_mode",
                  "init", "profile"):
            self.assertIn(k, self.err)
        self.assertNotIn("seed", self.svc)                                       # profile not active
        self.assertIn("my_worker_v2", self.svc)                                  # a valid hydo name ...
        self.assertIn("127.0.0.12 my-worker.v2", m["root"]["text"]["/etc/hosts"])   # ... the Compose name resolves
        self.assertEqual(m["guest"]["cpu"], "v3")                                # amd64 variant v3
        self.assertGreaterEqual(m["guest"]["ram_mib"], 1024 + 256)               # deploy memory limit 1g counted
        w = self.svc["my_worker_v2"]
        self.assertEqual(w["ready"], "timeout 30 hydo-oci my_worker_v2 /bin/sh -c 'worker --ping'")  # image HEALTHCHECK

    def test_profile(self):
        self.run_import("unsupported", "--profile", "seed")
        self.assertIn("seed", self.svc)

    def test_build_without_image(self):
        d = tempfile.mkdtemp(); self.addCleanup(shutil.rmtree, d)
        p = os.path.join(d, "docker-compose.yml")
        open(p, "w").write("services:\n  app:\n    build: .\n")
        err = io.StringIO(); e0, sys.stderr = sys.stderr, err
        try:
            rc = hydo_compose.main([p, "--no-resolve"])
        finally:
            sys.stderr = e0
        self.assertEqual(rc, 1)
        self.assertIn("docker save", err.getvalue())


# ------------------------------------------------------------------ hydo_oci mounts
def tiny_image(path):
    """a one-layer docker-save tar: /bin/x, /var/run -> /run (absolute symlink), /srv/data/seed."""
    lb = io.BytesIO()
    with tarfile.open(fileobj=lb, mode="w") as t:
        def add(name, data=None, typ=tarfile.REGTYPE, link="", mode=0o755, uid=0):
            ti = tarfile.TarInfo(name); ti.type = typ; ti.mode = mode; ti.uid = uid; ti.linkname = link
            if data is not None:
                ti.size = len(data); t.addfile(ti, io.BytesIO(data))
            else:
                t.addfile(ti)
        for d in ("bin", "var", "run", "srv", "srv/data", "etc"):
            add(d, typ=tarfile.DIRTYPE)
        add("var/run", typ=tarfile.SYMTYPE, link="/run", mode=0o777)
        add("srv/data/seed", b"seed\n", mode=0o644, uid=70)
        add("bin/x", b"#!/bin/sh\n")
    layer = lb.getvalue()
    diff = "sha256:" + hashlib.sha256(layer).hexdigest()
    conf = json.dumps({"architecture": "amd64", "os": "linux", "config": {"Cmd": ["/bin/x"]},
                       "rootfs": {"type": "layers", "diff_ids": [diff]}}).encode()
    cid = hashlib.sha256(conf).hexdigest()
    man = json.dumps([{"Config": cid + ".json", "RepoTags": ["t:1"], "Layers": ["l/layer.tar"]}]).encode()
    with tarfile.open(path, "w") as t:
        for n, b in ((cid + ".json", conf), ("l/layer.tar", layer), ("manifest.json", man)):
            ti = tarfile.TarInfo(n); ti.size = len(b); t.addfile(ti, io.BytesIO(b))


class Mounts(unittest.TestCase):
    class ME(Exception):
        pass

    def test_validation(self):
        f = lambda v: hydo_oci._mounts(v, "s", self.ME)
        self.assertEqual(f(None), [])
        for bad in ([{"dst": "/x"}], [{"src": "/a", "tmpfs": True, "dst": "/x"}], [{"src": "/a", "dst": "/"}],
                    [{"src": "/a", "dst": "/data"}], [{"src": "/a", "dst": "rel"}], [{"src": "/a b", "dst": "/x"}],
                    [{"src": "/a", "dst": "/x", "size": "1m"}], [{"src": "/a", "dst": "/x", "bogus": 1}]):
            with self.assertRaises(self.ME, msg=bad):
                f(bad)

    def test_stage_and_launcher(self):
        d = tempfile.mkdtemp(); self.addCleanup(shutil.rmtree, d)
        tiny_image(os.path.join(d, "t.tar"))
        R = os.path.join(d, "root"); os.makedirs(os.path.join(R, "bin")); os.makedirs(os.path.join(R, "etc"))
        open(os.path.join(R, "etc/hosts"), "w").write("127.0.0.11 svc\n")
        bb = os.path.join(d, "busybox"); open(bb, "w").write("bb")
        m = {"services": [{"name": "svc", "image": "t.tar", "ready": "true", "mounts": [
            {"src": "/etc/hosts", "dst": "/etc/hosts"},
            {"tmpfs": True, "dst": "/var/run", "copy_up": True},
            {"tmpfs": True, "dst": "/dev/shm", "size": "64m"},
            {"src": "/data/vol/v", "dst": "/srv/data", "copy_up": True},
            {"src": "/etc/hosts", "dst": "/opt/new/hosts"}]}]}
        man = os.path.join(d, "hydo.yaml"); open(man, "w").write("x")
        oci = hydo_oci.prepare(m, man, "x86_64", self.ME)
        meta = hydo_oci.stage(oci, R, bb, os.path.join(d, "tmp"))
        mts = {x["dst"] for x in oci[0]["mounts"]}
        self.assertIn("/run", mts)                    # /var/run -> /run resolved INSIDE the image root
        self.assertNotIn("/var/run", mts)
        self.assertTrue(os.path.isfile(os.path.join(R, "oci/svc/opt/new/hosts")))   # missing target created
        self.assertIn("oci/svc/opt/new", meta)
        self.assertFalse(os.path.exists(os.path.join(R, "oci/svc/dev/shm")))        # under /dev: at run time
        run = open(os.path.join(R, "etc/hydo/oci/svc/run")).read()
        ex = open(os.path.join(R, "etc/hydo/oci/svc/exec")).read()
        for s in (run, ex):
            self.assertIn("$BB mount --bind /etc/hosts $R/etc/hosts", s)
            self.assertIn("$BB mount -t tmpfs -o size=64m none /tmp/hydo/oci-tmpfs/svc/2", s)
            self.assertIn("$BB chmod 1777 /tmp/hydo/oci-tmpfs/svc/2", s)
            self.assertIn("$BB cp -a $R/srv/data/. /data/vol/v/", s)
            self.assertIn("$BB flock 9", s)
        self.assertLess(run.index("mount --bind /etc/hosts"), run.index("exec $BB chroot"))
        info = json.load(open(os.path.join(R, "etc/hydo/oci/svc/image.json")))
        self.assertEqual(len(info["effective"]["mounts"]), 5)

    def test_without_mounts_unchanged(self):
        """an image service without mounts: gets byte-for-byte the launcher of the previous hydo_oci.py"""
        d = tempfile.mkdtemp(); self.addCleanup(shutil.rmtree, d)
        old = subprocess.run(["git", "-C", ONB, "show", "main:hvx/onboard/hydo_oci.py"], stdout=subprocess.PIPE)
        if old.returncode != 0:
            self.skipTest("no git main")
        open(os.path.join(d, "hydo_oci_old.py"), "wb").write(old.stdout)
        sys.path.insert(0, d)
        import hydo_oci_old
        tiny_image(os.path.join(d, "t.tar"))
        man = os.path.join(d, "hydo.yaml"); open(man, "w").write("x")
        bb = os.path.join(d, "busybox"); open(bb, "w").write("bb")
        outs = []
        for mod, r in ((hydo_oci, "new"), (hydo_oci_old, "old")):
            R = os.path.join(d, r); os.makedirs(os.path.join(R, "bin"))
            m = {"services": [{"name": "svc", "image": "t.tar", "ready": "true", "env": {"A": "1"}}]}
            o = mod.prepare(m, man, "x86_64", self.ME)
            meta = mod.stage(o, R, bb, os.path.join(d, "tmp-" + r))
            outs.append((meta, [open(os.path.join(R, "etc/hydo/oci/svc", n)).read() for n in ("run", "exec", "image.json")],
                         m))
        self.assertEqual(outs[0], outs[1])


if __name__ == "__main__":
    unittest.main()
