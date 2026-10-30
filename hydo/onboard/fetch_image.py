#!/usr/bin/env python3
# Replaced by hydo-compose (its hydo-fetch-image program, hydo/crates/hydo-compose, docs/HISTORY.md
# M106); kept until every port is merged, then deleted.
"""fetch_image -- copy ONE image from a Docker registry by manifest digest into a `docker save`
style archive, without skopeo (a machine where skopeo is not installed and may not be).

    fetch_image.py <registry>/<repository> <sha256:manifest digest> <tag recorded in RepoTags> <out.tar>
    e.g. fetch_image.py docker.io/library/redis sha256:16b1... redis:alpine images/redis-alpine-amd64.docker.tar

The digest must be a single-platform image manifest (hydo-compose.lock.json records the linux/amd64
and linux/arm64 ones; fetch-images.sh's skopeo lines name them too). Every blob is checked against
its digest (the manifest itself, the config, each layer), so the archive holds exactly the image
that digest names. The archive is what hydo_oci.py reads as a `docker save` file: manifest.json
({Config, RepoTags, Layers}), the config as <hex>.json and the layers as downloaded (gzip; hydo_oci
decompresses them and checks the config's rootfs.diff_ids). hydo-build's image root depends only on
the image's content, so the result builds the same bytes as skopeo's docker-archive copy.
Anonymous pulls from Docker Hub (token from auth.docker.io); other registries with no auth or a
Bearer challenge. Standard library only.
"""
import hashlib, io, json, os, re, sys, tarfile, tempfile, urllib.error, urllib.parse, urllib.request

ACCEPT = ", ".join(["application/vnd.oci.image.manifest.v1+json",
                    "application/vnd.docker.distribution.manifest.v2+json"])


def split(ref):
    host, _, repo = ref.partition("/")
    if host in ("docker.io", "index.docker.io", "registry-1.docker.io"):
        host = "registry-1.docker.io"
        if "/" not in repo:
            repo = "library/" + repo
    return host, repo


class Reg:
    def __init__(self, host, repo):
        self.host, self.repo, self.token = host, repo, None

    def _auth(self, hdr):
        m = dict(re.findall(r'(\w+)="([^"]*)"', hdr))
        q = {"service": m.get("service", ""), "scope": m.get("scope") or "repository:%s:pull" % self.repo}
        with urllib.request.urlopen(m["realm"] + "?" + urllib.parse.urlencode(q), timeout=60) as r:
            j = json.load(r)
        self.token = j.get("token") or j.get("access_token")

    def get(self, path, accept=None):
        url = "https://%s/v2/%s/%s" % (self.host, self.repo, path)
        for _ in range(2):
            req = urllib.request.Request(url)
            if accept:
                req.add_header("Accept", accept)
            if self.token:
                req.add_header("Authorization", "Bearer " + self.token)
            try:
                return urllib.request.urlopen(req, timeout=600)
            except urllib.error.HTTPError as e:
                if e.code == 401 and not self.token and "Bearer" in e.headers.get("WWW-Authenticate", ""):
                    self._auth(e.headers["WWW-Authenticate"])
                    continue
                raise
        raise SystemExit("fetch_image: %s: unauthorized" % url)


def fetch(reg, digest, dst=None, accept=None):
    """The blob or manifest with this digest, checked; into file dst or returned as bytes."""
    h = hashlib.sha256()
    buf = io.BytesIO() if dst is None else open(dst, "wb")
    with reg.get(("manifests/" if accept else "blobs/") + digest, accept) as r:
        for b in iter(lambda: r.read(1 << 20), b""):
            h.update(b)
            buf.write(b)
    if "sha256:" + h.hexdigest() != digest:
        raise SystemExit("fetch_image: %s does not match its digest (got sha256:%s)" % (digest, h.hexdigest()))
    if dst is None:
        return buf.getvalue()
    buf.close()


def full_tag(tag):
    """The tag as skopeo's docker-archive records it in RepoTags: normalised, with the registry
    (docker.io/library/redis:alpine for redis:alpine). hydo_oci copies RepoTags into image.json."""
    first = tag.split("/")[0]
    if "/" not in tag or not ("." in first or ":" in first or first == "localhost"):
        tag = "docker.io/" + ("library/" + tag if "/" not in tag else tag)
    return tag


def main():
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    ref, digest, tag, out = sys.argv[1:]
    tag = full_tag(tag)
    reg = Reg(*split(ref))
    man = json.loads(fetch(reg, digest, accept=ACCEPT))
    if "manifests" in man:
        raise SystemExit("fetch_image: %s is an index; give the single-platform manifest digest" % digest)
    cfg_d = man["config"]["digest"]
    cfg = fetch(reg, cfg_d)
    tmp = tempfile.mkdtemp(prefix="fetch-image-", dir=os.path.dirname(os.path.abspath(out)) or ".")
    layers = []
    try:
        for i, l in enumerate(man["layers"]):
            p = os.path.join(tmp, "%s.tar" % l["digest"].split(":")[1])
            fetch(reg, l["digest"], p)
            layers.append(p)
            print("fetch_image: layer %d/%d %s" % (i + 1, len(man["layers"]), l["digest"][:19]), file=sys.stderr)
        mj = json.dumps([{"Config": cfg_d.split(":")[1] + ".json", "RepoTags": [tag],
                          "Layers": [os.path.basename(p) for p in layers]}]).encode()
        part = out + ".part"
        with tarfile.open(part, "w", format=tarfile.GNU_FORMAT) as t:
            for name, data in (("manifest.json", mj), (cfg_d.split(":")[1] + ".json", cfg)):
                ti = tarfile.TarInfo(name); ti.size = len(data); ti.mode = 0o644
                t.addfile(ti, io.BytesIO(data))
            for p in layers:
                t.add(p, arcname=os.path.basename(p))
        os.replace(part, out)
    finally:
        for p in layers:
            os.unlink(p)
        os.rmdir(tmp)
    print("fetch_image: %s@%s -> %s (%d layers, config %s)" % (ref, digest[:19], out, len(layers), cfg_d[:19]), file=sys.stderr)


if __name__ == "__main__":
    main()
