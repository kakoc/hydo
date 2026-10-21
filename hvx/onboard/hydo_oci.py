"""hydo_oci -- a hydo.yaml service whose files come from a Docker/OCI image (services[].image).

Used by hydo-build (x86_64) and hydo_build_arm64.py (--arch arm64). See MANIFEST.md, "Services
from a Docker/OCI image". Stdlib only. No network access: the image is a file on disk, either
  * an OCI image layout directory (oci-layout + index.json + blobs/), e.g.
        skopeo copy docker://docker.io/library/redis@sha256:<digest> oci:redis-oci:redis:7.2.4
  * a tar archive: `docker save` output (manifest.json, classic or Docker >= 25), skopeo's
    docker-archive: / oci-archive:, optionally gzip-compressed.

Design (load-bearing):
  * Each image service gets its OWN root in the guest, /oci/<service>, and runs chrooted there
    (the Docker model). Its files are not merged into the hydo root, so images with different
    C libraries or /etc contents can sit side by side and busybox + /sbin/init stay intact.
  * /proc /sys /dev /tmp /data of the guest are bind-mounted into that root before every start
    and every hydo-oci call. /data is the data disk (the fault target); /tmp is shared, so
    $HYDO_ACKED and friends are the same files inside and outside.
  * The image config gives the defaults: Entrypoint + Cmd (Docker semantics: a manifest `cmd:`
    replaces Cmd, an `entrypoint:` replaces Entrypoint and drops Cmd), Env (a manifest `env:`
    overrides by name), WorkingDir, User. A static busybox at /.hydo/busybox inside the image
    root does env -i / cd / user switch (chpst), so distroless images work too.
  * The layers are applied in order with whiteouts (.wh.<name>, .wh..wh..opq); file owners and
    modes (incl. setuid) come from the layer tars and are written into the ext4 root with the
    pinned debugfs (x86_64) or into the cpio headers (arm64). Timestamps are normalised like
    every other file of the image (fixed mtime, zeroed ctime/atime), so the output is a pure
    function of the image's content and the manifest. Devices, FIFOs and xattrs in layers are
    skipped (the guest's /dev is bind-mounted).
  * Every blob read is checked against its digest (OCI descriptors) and every uncompressed
    layer against the config's rootfs.diff_ids, so a manifest that pins image_ref by digest
    gets exactly that image.
"""
import gzip, hashlib, json, os, posixpath, re, shutil, stat, subprocess, sys, tarfile

OCI_KEYS = ("image", "image_ref", "entrypoint", "workdir", "user", "env")
ARCH = {"x86_64": "amd64", "arm64": "arm64"}
DEFAULT_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
BINDS = ("proc", "sys", "dev", "tmp", "data")
PASS_ENV = ("HYDO_FROM", "HYDO_TO", "HYDO_STEPS", "HYDO_ACKED", "HYDO_OUTCOMES", "HYDO_FAILED",
            "HYDO_LOG", "HYDO_SVC", "HYDO_VALUE")
HOSTS = "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n"
INDEX_TYPES = ("application/vnd.oci.image.index.v1+json",
               "application/vnd.docker.distribution.manifest.list.v2+json")


class OciError(Exception):
    pass


def shq(s):
    return "'" + str(s).replace("'", "'\\''") + "'"


# ------------------------------------------------------------------ reading an image file
class _Source:
    """Blobs of an OCI layout directory or of a tar archive, by path inside it."""

    def __init__(self, path):
        self.path = path
        self.tar = None
        if os.path.isdir(path):
            return
        try:
            self.tar = tarfile.open(path, "r:*")
        except tarfile.TarError as e:
            raise OciError("%s: not an OCI layout directory or a readable tar archive (%s)" % (path, e))
        self.members = {}
        for mi in self.tar.getmembers():
            self.members[posixpath.normpath(mi.name.lstrip("/"))] = mi

    def _member(self, name):
        name = posixpath.normpath(name.lstrip("/"))
        for _ in range(16):
            mi = self.members.get(name)
            if mi is None:
                return None
            if mi.issym():
                name = posixpath.normpath(posixpath.join(posixpath.dirname(name), mi.linkname))
                continue
            return mi
        return None

    def exists(self, name):
        if self.tar is None:
            return os.path.isfile(os.path.join(self.path, name))
        return self._member(name) is not None

    def open(self, name):
        if self.tar is None:
            p = os.path.join(self.path, name)
            if not os.path.isfile(p):
                raise OciError("%s: %s missing" % (self.path, name))
            return open(p, "rb")
        mi = self._member(name)
        if mi is None:
            raise OciError("%s: %s missing from the archive" % (self.path, name))
        return self.tar.extractfile(mi)

    def read(self, name, digest=None):
        with self.open(name) as f:
            b = f.read()
        if digest:
            _check_digest(b, digest, name)
        return b


def _check_digest(b, digest, what):
    algo, _, hexd = str(digest).partition(":")
    if algo != "sha256":
        raise OciError("%s: digest %s: only sha256 is supported" % (what, digest))
    got = hashlib.sha256(b).hexdigest()
    if got != hexd:
        raise OciError("%s: content sha256:%s does not match its digest %s" % (what, got, digest))


def _blob(digest):
    algo, _, hexd = digest.partition(":")
    return "blobs/%s/%s" % (algo, hexd)


def _ref_matches(ref, names):
    """ref "redis:7.2.4" matches a tag annotation "redis:7.2.4", "docker.io/library/redis:7.2.4"
    or a bare "7.2.4"."""
    norm = lambda r: re.sub(r"^(docker\.io/)?(library/)?", "", r)
    for n in names:
        if not n:
            continue
        if n == ref or norm(n) == norm(ref) or (":" not in n and ref.rsplit(":", 1)[-1] == n):
            return True
    return False


class Image:
    """The selected image: config, layers, digests. Layers are read later (flatten)."""

    def __init__(self, path, ref, arch):
        self.path, self.ref, self.arch = path, ref, arch
        self.src = _Source(path)
        self.manifest_digest = None
        if self.src.tar is not None and self.src.exists("manifest.json"):
            self._docker()
        elif self.src.exists("index.json"):
            self._oci()
        else:
            raise OciError("%s: neither a docker save archive (manifest.json) nor an OCI layout (index.json)" % path)
        cfg = self.config
        want = ARCH[arch]
        got = cfg.get("architecture")
        if cfg.get("os", "linux") != "linux" or got != want:
            raise OciError("%s: the image is %s/%s, but this build is for %s (%s); use an image of that "
                           "architecture (e.g. skopeo copy --override-arch %s ...)" % (
                               path, cfg.get("os"), got, arch, want, want))
        self.diff_ids = (cfg.get("rootfs") or {}).get("diff_ids") or []
        if len(self.diff_ids) != len(self.layers):
            raise OciError("%s: %d layers but %d rootfs.diff_ids in the config" % (path, len(self.layers), len(self.diff_ids)))

    def _docker(self):
        entries = json.loads(self.src.read("manifest.json"))
        if not isinstance(entries, list) or not entries:
            raise OciError("%s: manifest.json lists no images" % self.path)
        if self.ref and self.ref.startswith("sha256:"):
            pick = [e for e in entries if posixpath.basename(e["Config"]).replace(".json", "") in (self.ref[7:],)]
        elif self.ref:
            pick = [e for e in entries if _ref_matches(self.ref, e.get("RepoTags") or [])]
        else:
            pick = entries
        if len(pick) != 1:
            raise OciError("%s: %s; the archive has: %s (set image_ref: to a tag or the image id)" % (
                self.path, "no image matches image_ref %r" % self.ref if not pick else "more than one image",
                ", ".join(",".join(e.get("RepoTags") or ["<untagged>"]) for e in entries)))
        e = pick[0]
        cname = e["Config"]
        hexd = posixpath.basename(cname).replace(".json", "")
        cdig = "sha256:" + hexd if re.fullmatch(r"[0-9a-f]{64}", hexd) else None
        self.config_bytes = self.src.read(cname, cdig)
        self.config_digest = cdig or "sha256:" + hashlib.sha256(self.config_bytes).hexdigest()
        self.config = json.loads(self.config_bytes)
        self.layers = [(name, None) for name in e["Layers"]]   # verified against diff_ids
        self.ref_names = e.get("RepoTags") or []

    def _oci(self):
        idx = json.loads(self.src.read("index.json"))
        descs = idx.get("manifests") or []
        if self.ref and self.ref.startswith("sha256:"):
            pick = [d for d in descs if d.get("digest") == self.ref]
        elif self.ref:
            pick = [d for d in descs if _ref_matches(self.ref, [(d.get("annotations") or {}).get(k) for k in (
                "org.opencontainers.image.ref.name", "io.containerd.image.name")])]
        else:
            pick = descs
        if len(pick) != 1:
            names = [(d.get("annotations") or {}).get("org.opencontainers.image.ref.name") or d.get("digest") for d in descs]
            raise OciError("%s: %s; index.json has: %s (set image_ref: to a tag or a digest)" % (
                self.path, "no manifest matches image_ref %r" % self.ref if not pick else "more than one image",
                ", ".join(map(str, names))))
        d = pick[0]
        self.ref_names = [x for x in [(d.get("annotations") or {}).get("org.opencontainers.image.ref.name")] if x]
        for _ in range(4):
            body = self.src.read(_blob(d["digest"]), d["digest"])
            doc = json.loads(body)
            mt = d.get("mediaType") or doc.get("mediaType")
            if mt in INDEX_TYPES or "manifests" in doc:
                want = ARCH[self.arch]
                cand = [x for x in doc.get("manifests") or [] if (x.get("platform") or {}).get("os") == "linux"
                        and (x.get("platform") or {}).get("architecture") == want]
                if not cand:
                    plats = sorted(set("%s/%s" % ((x.get("platform") or {}).get("os"), (x.get("platform") or {}).get("architecture"))
                                       for x in doc.get("manifests") or []))
                    raise OciError("%s: the image index has no linux/%s manifest (has %s)" % (self.path, want, ", ".join(plats)))
                d = cand[0]
                continue
            self.manifest_digest = d["digest"]
            c = doc["config"]
            self.config_bytes = self.src.read(_blob(c["digest"]), c["digest"])
            self.config_digest = c["digest"]
            self.config = json.loads(self.config_bytes)
            self.layers = [(_blob(l["digest"]), l["digest"]) for l in doc.get("layers") or []]
            return
        raise OciError("%s: image index nested too deeply" % self.path)


class _HashReader:
    def __init__(self, f):
        self.f, self.h = f, hashlib.sha256()

    def read(self, n=-1):
        b = self.f.read(n)
        self.h.update(b)
        return b

    def drain(self):
        while self.read(1 << 20):
            pass


class _Prefixed:
    """bytes already read from f, then the rest of f"""

    def __init__(self, pre, f):
        self.pre, self.f = pre, f

    def read(self, n=-1):
        if self.pre:
            if n is None or n < 0:
                b, self.pre = self.pre + self.f.read(), b""
                return b
            b, self.pre = self.pre[:n], self.pre[n:]
            return b
        return self.f.read(n)


def _layer_to_file(img, name, digest, diff_id, out):
    """The layer, uncompressed, in file out; checks the blob digest and the diff_id."""
    with img.src.open(name) as raw:
        hr = _HashReader(raw)
        head = hr.read(4)
        cat = _Prefixed(head, hr)
        if head[:2] == b"\x1f\x8b":
            stream = gzip.GzipFile(fileobj=cat, mode="rb")
        elif head == b"\x28\xb5\x2f\xfd":
            raise OciError("%s: layer %s is zstd-compressed; this Python cannot decompress zstd. "
                           "Re-export it gzip-compressed (skopeo copy --dest-compress-format gzip ...)" % (img.path, name))
        else:
            stream = cat
        uh = hashlib.sha256()
        with open(out, "wb") as fo:
            while True:
                b = stream.read(1 << 20)
                if not b:
                    break
                uh.update(b)
                fo.write(b)
        hr.drain()
    if digest and "sha256:" + hr.h.hexdigest() != digest:
        raise OciError("%s: layer %s: content does not match its digest %s" % (img.path, name, digest))
    if diff_id and "sha256:" + uh.hexdigest() != diff_id:
        raise OciError("%s: layer %s: uncompressed content does not match rootfs.diff_id %s" % (img.path, name, diff_id))


# ------------------------------------------------------------------ flattening the layers
class _Tree:
    """An image root being assembled on the build host. meta: rel path -> (S_IFMT, mode, uid, gid).
    Staged files are owned by the build user with permissive modes (u+rw, dirs u+rwx); the image's
    real owners and modes live in meta and are applied to the filesystem image afterwards."""

    def __init__(self, root):
        self.root = root
        self.meta = {}
        self.skipped = 0
        os.makedirs(root)
        self.meta[""] = (stat.S_IFDIR, 0o755, 0, 0)

    def _abs(self, rel):
        return os.path.join(self.root, rel) if rel else self.root

    @staticmethod
    def parts(name):
        p = [x for x in name.split("/") if x not in ("", ".")]
        if ".." in p:
            raise OciError("unsafe path in a layer: %r" % name)
        for x in p:
            if '"' in x or "\n" in x or "\\" in x:
                raise OciError("unsupported character in a layer path: %r" % name)
        return p

    def resolve_dir(self, comps, create=True):
        """comps resolved inside the root (symlinks followed, clamped at the root), as a list of
        real components; missing directories are created (0755 root, like Docker)."""
        out, todo, hops = [], list(comps), 0
        while todo:
            c = todo.pop(0)
            if c in ("", "."):
                continue
            if c == "..":
                out = out[:-1]
                continue
            p = self._abs("/".join(out + [c]))
            if os.path.islink(p):
                hops += 1
                if hops > 40:
                    raise OciError("symlink loop under %s" % "/".join(out + [c]))
                t = os.readlink(p)
                if t.startswith("/"):
                    out = []
                todo = [x for x in t.split("/")] + todo
                continue
            if not os.path.lexists(p):
                if not create:
                    return None
                os.mkdir(p, 0o755)
                self.meta["/".join(out + [c])] = (stat.S_IFDIR, 0o755, 0, 0)
            elif not os.path.isdir(p):
                raise OciError("a layer puts a file below %s, which is not a directory" % "/".join(out + [c]))
            out.append(c)
        return out

    def remove(self, rel):
        p = self._abs(rel)
        if os.path.isdir(p) and not os.path.islink(p):
            shutil.rmtree(p)
        elif os.path.lexists(p):
            os.unlink(p)
        for k in [k for k in self.meta if k == rel or k.startswith(rel + "/")]:
            del self.meta[k]

    def whiteout(self, name):
        p = self.parts(name)
        base = p[-1]
        d = self.resolve_dir(p[:-1], create=False)
        if d is None:
            return
        if base == ".wh..wh..opq":
            dp = self._abs("/".join(d))
            for n in sorted(os.listdir(dp)):
                self.remove("/".join(d + [n]))
        elif base.startswith(".wh."):
            self.remove("/".join(d + [base[4:]]))

    def add(self, tar, mi):
        p = self.parts(mi.name)
        if not p:           # "./" -- the image's root directory itself
            self.meta[""] = (stat.S_IFDIR, mi.mode & 0o7777, mi.uid, mi.gid)
            return
        d = self.resolve_dir(p[:-1])
        rel = "/".join(d + [p[-1]])
        dst = self._abs(rel)
        if mi.isdir():
            if not (os.path.isdir(dst) and not os.path.islink(dst)):
                if os.path.lexists(dst):
                    self.remove(rel)
                os.mkdir(dst, 0o755)
            self.meta[rel] = (stat.S_IFDIR, mi.mode & 0o7777, mi.uid, mi.gid)
            return
        if not (mi.isreg() or mi.issym() or mi.islnk()):
            self.skipped += 1          # character/block devices, FIFOs
            return
        if os.path.lexists(dst):
            self.remove(rel)
        if mi.issym():
            os.symlink(mi.linkname, dst)
            self.meta[rel] = (stat.S_IFLNK, 0o777, mi.uid, mi.gid)
        elif mi.islnk():
            tp = self.parts(mi.linkname)
            td = self.resolve_dir(tp[:-1], create=False)
            trel = "/".join((td or []) + [tp[-1]]) if tp else ""
            if td is None or not os.path.lexists(self._abs(trel)) or (
                    os.path.isdir(self._abs(trel)) and not os.path.islink(self._abs(trel))):
                raise OciError("hard link %s -> %s: target missing" % (mi.name, mi.linkname))
            os.link(self._abs(trel), dst, follow_symlinks=False)
            self.meta[rel] = self.meta.get(trel, (stat.S_IFREG, mi.mode & 0o7777, mi.uid, mi.gid))
        else:
            with tar.extractfile(mi) as fi, open(dst, "wb") as fo:
                shutil.copyfileobj(fi, fo, 1 << 20)
            os.chmod(dst, 0o644 | (0o111 if mi.mode & 0o111 else 0))
            self.meta[rel] = (stat.S_IFREG, mi.mode & 0o7777, mi.uid, mi.gid)

    def apply_layer(self, path):
        with tarfile.open(path, "r:") as tar:
            mems = tar.getmembers()
            for mi in mems:                     # whiteouts hide LOWER layers: apply them first
                b = posixpath.basename(mi.name.rstrip("/"))
                if b.startswith(".wh."):
                    self.whiteout(mi.name.rstrip("/"))
            for mi in mems:
                b = posixpath.basename(mi.name.rstrip("/"))
                if not b.startswith(".wh."):
                    self.add(tar, mi)

    def put_file(self, rel, content, mode=0o644):
        d = self.resolve_dir(rel.split("/")[:-1])
        r = "/".join(d + [rel.split("/")[-1]])
        if os.path.lexists(self._abs(r)):
            self.remove(r)
        with open(self._abs(r), "wb") as f:
            f.write(content if isinstance(content, bytes) else content.encode())
        os.chmod(self._abs(r), 0o644 | (0o111 if mode & 0o111 else 0))
        self.meta[r] = (stat.S_IFREG, mode, 0, 0)

    def ensure_dir(self, rel, mode=0o755):
        p = self._abs(rel)
        if os.path.islink(p) or (os.path.lexists(p) and not os.path.isdir(p)):
            raise OciError("the image has /%s, but not as a directory; hydo bind-mounts the guest's /%s there" % (rel, rel))
        if not os.path.isdir(p):
            os.mkdir(p, 0o755)
            self.meta[rel] = (stat.S_IFDIR, mode, 0, 0)

    def home_of(self, user):
        """The home directory of user (name or uid) from the image's /etc/passwd ("/" if unknown)."""
        if not user:
            return "/root"
        name = user.split(":")[0]
        try:
            with open(self._abs("etc/passwd"), "rb") as f:
                for line in f.read().decode("utf-8", "replace").splitlines():
                    x = line.split(":")
                    if len(x) >= 6 and (x[0] == name or x[2] == name):
                        return x[5] or "/"
        except OSError:
            pass
        return "/"


# ------------------------------------------------------------------ manifest side
def _words(argv):
    return " ".join(shq(a) for a in argv)


def _image_path(v, arch, mdir, where):
    if isinstance(v, dict):
        bad = sorted(set(v) - set(ARCH))
        if bad:
            raise OciError("%s: image: keys are architectures (%s), not %s" % (where, ", ".join(ARCH), ", ".join(bad)))
        if arch not in v:
            raise OciError("%s: image: has no entry for %s" % (where, arch))
        v = v[arch]
    if not isinstance(v, str) or not v:
        raise OciError("%s: image: a path to an OCI layout directory or a docker save tar (or a mapping "
                       "x86_64: / arm64: of such paths)" % where)
    p = os.path.expanduser(v)
    if not os.path.isabs(p):
        p = os.path.join(mdir, p)
    if not os.path.exists(p):
        raise OciError("%s: image %s does not exist. hydo-build does not pull images; produce the file with "
                       "`docker save -o <file>.tar <image>` or `skopeo copy docker://<image>@sha256:<digest> "
                       "oci:<dir>:<tag>` (MANIFEST.md)" % (where, p))
    return p


def prepare(m, manifest_path, arch, ManifestError):
    """For every service with image: read the image's config, compute the effective command,
    env, workdir and user, and point the service's cmd at the generated launcher. Removes the
    image keys from the service. Returns the list of image services ([] when none)."""
    mdir = os.path.dirname(os.path.abspath(manifest_path))
    out = []
    for i, s in enumerate(m.get("services") or []):
        where = "services[%d] (%s)" % (i, s.get("name"))
        if "image" not in s:
            extra = [k for k in OCI_KEYS if k in s]
            if extra:
                raise ManifestError("%s: %s only with image:" % (where, ", ".join(extra)))
            continue
        try:
            path = _image_path(s.pop("image"), arch, mdir, where)
            ref = s.pop("image_ref", None)
            img = Image(path, str(ref) if ref is not None else None, arch)
        except OciError as e:
            raise ManifestError(str(e))
        cfg = img.config.get("config") or {}
        if "entrypoint" in s:
            v = s.pop("entrypoint")
            entry = _words(v) if isinstance(v, list) else ("" if v is None else str(v))
            dflt_cmd = ""                                       # Docker: --entrypoint drops CMD
        else:
            entry = _words(cfg.get("Entrypoint") or [])
            dflt_cmd = _words(cfg.get("Cmd") or [])
        if "cmd" in s:
            v = s["cmd"]
            cmd = _words(v) if isinstance(v, list) else str(v)
        else:
            cmd = dflt_cmd
        final = (entry + " " + cmd).strip()
        if not final:
            raise ManifestError("%s: the image has no Entrypoint/Cmd; give cmd:" % where)
        env, keys = [], {}
        for kvs in cfg.get("Env") or []:
            k, _, v = kvs.partition("=")
            keys[k] = len(env); env.append([k, v])
        menv = s.pop("env", None) or {}
        if not isinstance(menv, dict):
            raise ManifestError("%s: env: must be a mapping" % where)
        for k, v in menv.items():
            if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", str(k)):
                raise ManifestError("%s: env: %r is not a valid variable name" % (where, k))
            v = "" if v is None else (str(v).lower() if isinstance(v, bool) else str(v))
            if k in keys:
                env[keys[k]][1] = v
            else:
                keys[k] = len(env); env.append([str(k), v])
        if "PATH" not in keys:
            env.insert(0, ["PATH", DEFAULT_PATH])
        workdir = str(s.pop("workdir", None) or cfg.get("WorkingDir") or "/")
        if not workdir.startswith("/"):
            raise ManifestError("%s: workdir %r must be absolute" % (where, workdir))
        user = s.pop("user", None)
        user = str(cfg.get("User") or "") if user is None else str(user)
        if user in ("root", "0", "0:0", "root:root"):
            user = ""
        name = s["name"]
        s["cmd"] = "/etc/hydo/oci/%s/run" % name
        out.append({"svc": name, "path": path, "img": img, "final": final, "env": env,
                    "workdir": workdir, "user": user})
        print("hydo-build: service %s from image %s (%s, config %s%s)" % (
            name, path, ",".join(img.ref_names) or "untagged", img.config_digest[:19],
            ", manifest %s" % img.manifest_digest[:19] if img.manifest_digest else ""), file=sys.stderr)
    return out


# ------------------------------------------------------------------ staging into the root
def _scripts(o, home):
    svc, R = o["svc"], "/oci/%s" % o["svc"]
    env = [shq("%s=%s" % (k, v)) for k, v in o["env"]]
    if not any(k == "HOME" for k, _ in o["env"]):
        env.append(shq("HOME=%s" % home))
    env += ['%s="$%s"' % (k, k) for k in PASS_ENV]
    head = ("#!/bin/busybox sh\n"
            "# generated by hvx/onboard/hydo_oci.py -- service %s from %s\n"
            "BB=/bin/busybox R=%s\n"
            "for d in %s; do $BB mountpoint -q $R/$d || $BB mount --bind /$d $R/$d || exit 125; done\n"
            % (svc, o["img"].config_digest, R, " ".join(BINDS)))
    chroot = "exec $BB chroot $R /.hydo/busybox env -i %s /.hydo/busybox sh -c" % " ".join(env)
    user = ("/.hydo/busybox chpst -u %s " % shq(o["user"])) if o["user"] else ""
    run = head + "%s %s\n" % (chroot, shq("cd %s || exit 127; exec %s%s" % (shq(o["workdir"]), user, o["final"])))
    ex = (head.replace("for d in", '[ $# -gt 0 ] || { echo "usage: hydo-oci %s <command> [args]" >&2; exit 2; }\nfor d in' % svc, 1)
          + "%s %s %s \"$@\"\n" % (chroot, shq('cd "$0" || exit 127; exec "$@"'), shq(o["workdir"])))
    return run, ex


HYDO_OCI = """#!/bin/busybox sh
# hydo-oci <service> <command> [args...]: run a command inside an image service's root, with the
# image's env and working directory (as root). Generated by hvx/onboard/hydo_oci.py.
s=$1
[ -n "$s" ] && [ -x /etc/hydo/oci/$s/exec ] || { echo "hydo-oci: no image service '$s'" >&2; exit 127; }
shift
exec /etc/hydo/oci/$s/exec "$@"
"""


def stage(oci, R, busybox, tmpdir):
    """Flatten every image service's image into R/oci/<svc>, add the launchers. Returns meta:
    path relative to R -> (S_IFMT, mode, uid, gid) for everything under R/oci."""
    meta = {"oci": (stat.S_IFDIR, 0o755, 0, 0)}
    if not oci:
        return {}
    os.makedirs(os.path.join(R, "oci"), exist_ok=True)
    if os.path.isdir(tmpdir):
        shutil.rmtree(tmpdir)
    os.makedirs(tmpdir)
    for o in oci:
        img = o["img"]
        T = _Tree(os.path.join(R, "oci", o["svc"]))
        try:
            for k, ((name, dig), diff) in enumerate(zip(img.layers, img.diff_ids)):
                lt = os.path.join(tmpdir, "layer-%03d.tar" % k)
                _layer_to_file(img, name, dig, diff, lt)
                T.apply_layer(lt)
                os.unlink(lt)
            for d in BINDS:
                T.ensure_dir(d, 0o1777 if d == "tmp" else 0o755)
            T.ensure_dir(".hydo")
            T.meta[".hydo"] = (stat.S_IFDIR, 0o755, 0, 0)
            with open(busybox, "rb") as f:
                T.put_file(".hydo/busybox", f.read(), 0o755)
            T.put_file("etc/hosts", HOSTS)          # like Docker: the runtime owns /etc/hosts
            home = T.home_of(o["user"])
        except OciError as e:
            raise OciError("service %s, image %s: %s" % (o["svc"], o["path"], e))
        for k, v in T.meta.items():
            meta["oci/%s%s" % (o["svc"], "/" + k if k else "")] = v
        run, ex = _scripts(o, home)
        d = os.path.join(R, "etc/hydo/oci", o["svc"])
        os.makedirs(d, exist_ok=True)
        for n, c in (("run", run), ("exec", ex)):
            with open(os.path.join(d, n), "w") as f:
                f.write(c)
            os.chmod(os.path.join(d, n), 0o755)
        cfg = img.config.get("config") or {}
        rec = {"service": o["svc"], "source": os.path.basename(o["path"].rstrip("/")),
               "ref_names": img.ref_names, "manifest_digest": img.manifest_digest,
               "config_digest": img.config_digest, "architecture": img.config.get("architecture"),
               "layers": img.diff_ids, "image_config": {k: cfg.get(k) for k in
                                                        ("Entrypoint", "Cmd", "Env", "WorkingDir", "User")},
               "effective": {"command": o["final"], "env": o["env"], "workdir": o["workdir"],
                             "user": o["user"]},
               "skipped_special_files": T.skipped}
        with open(os.path.join(d, "image.json"), "w") as f:
            json.dump(rec, f, indent=1, sort_keys=True)
            f.write("\n")
        print("hydo-build: image %s -> /oci/%s (%d layers, %d entries%s)" % (
            img.config_digest[:19], o["svc"], len(img.layers), len(T.meta),
            ", %d device/FIFO entries skipped" % T.skipped if T.skipped else ""))
    with open(os.path.join(R, "bin/hydo-oci"), "w") as f:
        f.write(HYDO_OCI)
    os.chmod(os.path.join(R, "bin/hydo-oci"), 0o755)
    shutil.rmtree(tmpdir)
    return meta


def fix_ext4(meta, T, img, fake_ts):
    """Write the image files' real owners and modes into the ext4 root with debugfs (the build
    user cannot chown). Paths are guest paths; run under the same frozen clock as mkfs."""
    if not meta:
        return
    dbg = os.environ.get("HYDO_DEBUGFS") or os.path.join(os.path.dirname(T["mke2fs"]), "debugfs")
    if not os.path.isfile(dbg):
        raise SystemExit("hydo-build: debugfs not found at %s (set HYDO_DEBUGFS)" % dbg)
    lines = []
    for rel in sorted(meta):
        kind, mode, uid, gid = meta[rel]
        p = '"/%s"' % rel
        if kind != stat.S_IFLNK:
            lines.append("sif %s mode 0%o" % (p, kind | mode))
        lines.append("sif %s uid %d" % (p, uid))
        lines.append("sif %s gid %d" % (p, gid))
    cmdf = img + ".debugfs"
    with open(cmdf, "w") as f:
        f.write("\n".join(lines) + "\n")
    r = subprocess.run([T["faketime"], fake_ts, dbg, "-w", "-f", cmdf, img],
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    errs = [x for x in r.stderr.splitlines() if x.strip() and not x.startswith("debugfs ")]
    os.unlink(cmdf)
    if r.returncode != 0 or errs:
        raise SystemExit("hydo-build: debugfs owner/mode pass failed:\n  " + "\n  ".join(errs[:10]))
    print("hydo-build: image file owners and modes written (%d entries)" % len(meta))
