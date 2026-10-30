#!/usr/bin/env python3
"""The OCI reference for tests/oci.rs.

    py_oci.py make <dir>                         write the synthetic images and manifests into <dir>
    py_oci.py stage <manifest.json> <arch> <root> <busybox>
                                                 hydo_oci.prepare + stage (the Python), into <root>;
                                                 prints one JSON object: services, meta, out, err

The images are made with Python's tarfile in both of its output formats (GNU and PAX), as a docker
save archive (plain and gzip-compressed) and as an OCI layout whose index points at a multi-arch
index; the layers exercise whiteouts, opaque directories, hard links, symlinked directories in
paths, long names, a setuid file, owners, devices (skipped) and a FIFO.
"""
import contextlib
import gzip
import hashlib
import io
import json
import os
import sys
import tarfile

HERE = os.path.dirname(os.path.abspath(__file__))
ONBOARD = os.path.join(HERE, "..", "..", "..", "onboard")


def tinfo(name, kind=tarfile.REGTYPE, mode=0o644, uid=0, gid=0, data=b"", link=""):
    ti = tarfile.TarInfo(name)
    ti.type, ti.mode, ti.uid, ti.gid, ti.mtime = kind, mode, uid, gid, 1234567
    ti.linkname = link
    ti.size = len(data) if kind == tarfile.REGTYPE else 0
    return ti, (io.BytesIO(data) if kind == tarfile.REGTYPE else None)


def layer(entries, fmt):
    b = io.BytesIO()
    with tarfile.open(fileobj=b, mode="w", format=fmt) as t:
        for e in entries:
            ti, f = tinfo(*e[:1], **e[1]) if isinstance(e, tuple) else tinfo(e)
            t.addfile(ti, f)
    return b.getvalue()


LONG = "opt/" + "deep/" * 25 + "long-file-name-that-needs-an-extension.txt"
L1 = [
    ("etc", dict(kind=tarfile.DIRTYPE, mode=0o755)),
    ("etc/passwd", dict(data=b"root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000::/home/app:/bin/sh\n")),
    ("usr", dict(kind=tarfile.DIRTYPE, mode=0o755)),
    ("usr/bin", dict(kind=tarfile.DIRTYPE, mode=0o755)),
    ("usr/bin/app", dict(mode=0o755, data=b"#!/bin/sh\necho app\n")),
    ("usr/bin/su", dict(mode=0o4755, data=b"setuid\n")),
    ("usr/bin/app2", dict(kind=tarfile.LNKTYPE, link="usr/bin/app")),
    ("bin", dict(kind=tarfile.SYMTYPE, link="usr/bin")),
    ("dev", dict(kind=tarfile.DIRTYPE, mode=0o755)),
    ("dev/null", dict(kind=tarfile.CHRTYPE, mode=0o666)),
    ("var/lib/data/x", dict(data=b"x\n", uid=1000, gid=1000)),
    ("var/run", dict(kind=tarfile.SYMTYPE, link="/run")),
    ("run", dict(kind=tarfile.DIRTYPE, mode=0o755)),
    ("opt/old/file", dict(data=b"old\n")),
    ("opt/keep", dict(data=b"keep\n")),
    ("home/app", dict(kind=tarfile.DIRTYPE, mode=0o700, uid=1000, gid=1000)),
    ("srv/fifo", dict(kind=tarfile.FIFOTYPE)),
    (LONG, dict(data=b"long\n")),
]
L2 = [
    ("opt/old/.wh.file", dict()),
    ("var/lib/data/.wh..wh..opq", dict()),
    ("var/lib/data/new", dict(data=b"new\n", uid=1000, gid=1000)),
    ("bin/tool", dict(mode=0o755, data=b"tool\n")),
    ("etc/app.conf", dict(mode=0o600, data=b"k=v\n", uid=1000, gid=1000)),
    ("./", dict(kind=tarfile.DIRTYPE, mode=0o755)),
    ("tmp", dict(kind=tarfile.DIRTYPE, mode=0o1777)),
]
CONFIG = {"architecture": None, "os": "linux",
          "config": {"Entrypoint": ["/usr/bin/app"], "Cmd": ["--serve", "it's"], "Env": ["PATH=/usr/bin:/bin", "A=1"],
                     "WorkingDir": "/srv", "User": "app"},
          "rootfs": {"type": "layers", "diff_ids": []}}


def sha(b):
    return "sha256:" + hashlib.sha256(b).hexdigest()


def make(d):
    os.makedirs(d, exist_ok=True)
    layers = [layer(L1, tarfile.GNU_FORMAT), layer(L2, tarfile.PAX_FORMAT)]
    for arch in ("amd64", "arm64"):
        cfg = json.loads(json.dumps(CONFIG))
        cfg["architecture"] = arch
        cfg["rootfs"]["diff_ids"] = [sha(l) for l in layers]
        cb = json.dumps(cfg).encode()
        # docker save archive (and its gzip-compressed copy)
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode="w") as t:
            def add(name, b):
                ti = tarfile.TarInfo(name)
                ti.size, ti.mtime = len(b), 0
                t.addfile(ti, io.BytesIO(b))
            names = []
            for i, l in enumerate(layers):
                names.append("layer%d/layer.tar" % i)
                add(names[-1], l)
            cname = hashlib.sha256(cb).hexdigest() + ".json"
            add(cname, cb)
            add("manifest.json", json.dumps([{"Config": cname, "RepoTags": ["demo:1.0"], "Layers": names}]).encode())
        with open(os.path.join(d, "demo-%s.docker.tar" % arch), "wb") as f:
            f.write(out.getvalue())
        with open(os.path.join(d, "demo-%s.docker.tar.gz" % arch), "wb") as f:
            f.write(gzip.compress(out.getvalue(), mtime=0))
    # an OCI layout: index.json -> an image index (amd64, arm64) -> manifests with gzip layers
    lay = os.path.join(d, "demo-oci")
    os.makedirs(os.path.join(lay, "blobs", "sha256"), exist_ok=True)

    def blob(b):
        with open(os.path.join(lay, "blobs", "sha256", hashlib.sha256(b).hexdigest()), "wb") as f:
            f.write(b)
        return sha(b)
    mans = []
    for arch in ("amd64", "arm64"):
        gz = [gzip.compress(l, mtime=0) for l in layers]
        cfg = json.loads(json.dumps(CONFIG))
        cfg["architecture"] = arch
        cfg["rootfs"]["diff_ids"] = [sha(l) for l in layers]
        cb = json.dumps(cfg).encode()
        man = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
               "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": blob(cb), "size": len(cb)},
               "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": blob(g), "size": len(g)}
                          for g in gz]}
        mb = json.dumps(man).encode()
        mans.append({"mediaType": man["mediaType"], "digest": blob(mb), "size": len(mb),
                     "platform": {"os": "linux", "architecture": arch}})
    ib = json.dumps({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json", "manifests": mans}).encode()
    with open(os.path.join(lay, "index.json"), "w") as f:
        json.dump({"schemaVersion": 2, "manifests": [{"mediaType": "application/vnd.oci.image.index.v1+json",
                                                      "digest": blob(ib), "size": len(ib),
                                                      "annotations": {"org.opencontainers.image.ref.name": "demo:1.0"}}]}, f)
    with open(os.path.join(lay, "oci-layout"), "w") as f:
        f.write('{"imageLayoutVersion": "1.0.0"}')
    # a file of the hydo root that a mount binds (the hosts file case)
    os.makedirs(os.path.join(d, "root", "etc"), exist_ok=True)
    svc = lambda name, image, **kw: dict({"name": name, "image": image, "ready": "true"}, **kw)
    manifests = {
        "docker": {"services": [svc("a", {"x86_64": "demo-amd64.docker.tar", "arm64": "demo-arm64.docker.tar"},
                                    image_ref="demo:1.0", env={"B": True, "A": "2", "C": None},
                                    mounts=[{"src": "/data/vol/a", "dst": "/var/lib/data", "copy_up": True},
                                            {"tmpfs": True, "dst": "/run/x", "size": "64m", "copy_up": True},
                                            {"tmpfs": True, "dst": "/cache"},
                                            {"src": "/etc/hosts", "dst": "/etc/extra-hosts"},
                                            {"src": "/data/vol/b", "dst": "/var/run/b"}])]},
        "dockergz": {"services": [svc("b", {"x86_64": "demo-amd64.docker.tar.gz", "arm64": "demo-arm64.docker.tar.gz"},
                                      entrypoint=["/bin/tool", "x y"], user="0", workdir="/opt")]},
        "oci": {"services": [svc("c", "demo-oci", image_ref="demo:1.0", cmd="sleep 1", user="1000:1000"),
                             svc("d", "demo-oci", entrypoint=None, cmd=["echo", "hi"])]},
        "badref": {"services": [svc("e", "demo-oci", image_ref="nope:2")]},
        "badmount": {"services": [svc("f", "demo-oci", mounts=[{"src": "/x", "tmpfs": True, "dst": "/y"}])]},
    }
    for k, v in manifests.items():
        with open(os.path.join(d, k + ".json"), "w") as f:
            json.dump(dict({"name": k}, **v), f)


def stage(mpath, arch, R, busybox):
    sys.path.insert(0, ONBOARD)
    import hydo_oci

    class ManifestError(Exception):
        pass
    m = json.load(open(mpath))
    out, err = io.StringIO(), io.StringIO()
    res = {}
    os.makedirs(os.path.join(R, "bin"), exist_ok=True)
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            oci = hydo_oci.prepare(m, mpath, arch, ManifestError)
            meta = hydo_oci.stage(oci, R, busybox, R + ".tmp", netns=(arch == "arm64"))
        res["meta"] = {k: list(v) for k, v in meta.items()}
        res["services"] = m["services"]
    except (ManifestError, hydo_oci.OciError) as e:
        res["error"] = str(e)
    res["out"], res["err"] = out.getvalue(), err.getvalue()
    print(json.dumps(res))


if __name__ == "__main__":
    if sys.argv[1] == "make":
        make(sys.argv[2])
    else:
        stage(*sys.argv[2:6])
