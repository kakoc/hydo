#!/usr/bin/env python3
"""nix_cache_fetch -- copy store paths out of the public Nix binary cache without Nix.

    nix_cache_fetch.py [--closure] <out root> <store path or hash-name>...

For each path: fetches https://cache.nixos.org/<hash>.narinfo, downloads the archive it names,
checks the compressed file against the narinfo's FileHash and the unpacked archive against its
NarHash (both sha256, Nix base-32), and unpacks the whole tree (directories, files with their
executable bit, symlinks) at <out root>/nix/store/<hash-name>. With --closure it follows the
narinfo's References, so the result is the path's whole runtime closure. A path already present
under <out root> is not fetched again. Prints one line per path: its NarHash and its name.

The cache serves the same bytes for a store path forever (the path's hash fixes its inputs, and
NarHash fixes its contents), so a path name plus its NarHash is a pin. Used in Milestone 99 to
take the aarch64 iproute2 (`ip`) and iptables of nixpkgs onto the Mac, which has no Nix, for the
arm64 guests with service networks. Standard library only.
"""
import hashlib, lzma, os, sys, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from nar_extract import Nar, nix32      # noqa: E402

CACHE = os.environ.get("HYDO_NIX_CACHE", "https://cache.nixos.org")


def narinfo(h):
    with urllib.request.urlopen("%s/%s.narinfo" % (CACHE, h), timeout=60) as r:
        info = {}
        for line in r.read().decode().splitlines():
            k, _, v = line.partition(": ")
            info[k] = v
    return info


class Hashing:
    """A read-only file object that hashes what is read through it."""
    def __init__(self, f):
        self.f, self.h = f, hashlib.sha256()

    def read(self, n=-1):
        b = self.f.read(n)
        self.h.update(b)
        return b


SKIPPED = []        # entries not written: a name that differs from an earlier one only in case


def unpack(n, dst):
    """Unpack one NAR node at dst (Nar from nar_extract.py, positioned at a node's "(").
    On a case-insensitive file system (macOS's default) two names that differ only in case
    (iptables' libxt_dscp.so and libxt_DSCP.so) are one file: the later one is read (the hashes
    still cover it) but not written, and listed at the end. dst None = read only."""
    if dst is not None and os.path.lexists(dst):
        SKIPPED.append(dst)
        dst = None
    n.expect("(")
    n.expect("type")
    t = n.s()
    if t == "regular":
        x = n.s()
        ex = x == "executable"
        if ex:
            n.expect("")
            x = n.s()
        assert x == "contents"
        if dst is None:
            n.bytes_(lambda b: None)
        else:
            with open(dst, "wb") as f:
                n.bytes_(f.write)
            os.chmod(dst, 0o555 if ex else 0o444)
        n.expect(")")
    elif t == "symlink":
        n.expect("target")
        tgt = n.s()
        if dst is not None:
            os.symlink(tgt, dst)
        n.expect(")")
    elif t == "directory":
        if dst is not None:
            os.mkdir(dst)
        while True:
            x = n.s()
            if x == ")":
                break
            assert x == "entry"
            n.expect("("); n.expect("name"); name = n.s(); n.expect("node")
            if "/" in name or name in ("", ".", ".."):
                raise SystemExit("nix_cache_fetch: bad entry name %r" % name)
            unpack(n, None if dst is None else os.path.join(dst, name))
            n.expect(")")
        if dst is not None:
            os.chmod(dst, 0o555)
    else:
        raise SystemExit("nix_cache_fetch: unknown node type %r" % t)


def fetch(base, out):
    """base = <hash>-<name>; returns the narinfo's References (hash-names)."""
    h = base.split("-", 1)[0]
    info = narinfo(h)
    if info.get("StorePath") != "/nix/store/" + base:
        raise SystemExit("nix_cache_fetch: %s: the cache answers for %s" % (base, info.get("StorePath")))
    dst = os.path.join(out, "nix", "store", base)
    refs = [r for r in info.get("References", "").split() if r != base]
    if os.path.lexists(dst):
        print("%s  %s (present)" % (info["NarHash"], base))
        return refs
    if info.get("Compression") != "xz":
        raise SystemExit("nix_cache_fetch: %s: compression %s not supported" % (base, info.get("Compression")))
    tmp = dst + ".nar.xz.part"
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    fh = hashlib.sha256()
    with urllib.request.urlopen("%s/%s" % (CACHE, info["URL"]), timeout=300) as r, open(tmp, "wb") as f:
        for b in iter(lambda: r.read(1 << 20), b""):
            fh.update(b); f.write(b)
    if "sha256:" + nix32(fh.digest()) != info["FileHash"]:
        raise SystemExit("nix_cache_fetch: %s: FileHash mismatch" % base)
    with lzma.open(tmp, "rb") as raw:
        hf = Hashing(raw)
        n = Nar(hf)
        n.expect("nix-archive-1")
        unpack(n, dst)
        if hf.read(1):
            raise SystemExit("nix_cache_fetch: %s: trailing data after the archive" % base)
    os.unlink(tmp)
    if "sha256:" + nix32(hf.h.digest()) != info["NarHash"]:
        raise SystemExit("nix_cache_fetch: %s: NarHash mismatch (unpacked tree left at %s)" % (base, dst))
    print("%s  %s" % (info["NarHash"], base))
    return refs


def main(argv):
    closure = "--closure" in argv
    args = [a for a in argv if a != "--closure"]
    if len(args) < 2:
        raise SystemExit(__doc__)
    out, todo = args[0], [os.path.basename(p.rstrip("/")) for p in args[1:]]
    seen = set()
    while todo:
        b = todo.pop(0)
        if b in seen:
            continue
        seen.add(b)
        refs = fetch(b, out)
        if closure:
            todo += refs
    for p in SKIPPED:
        print("not written (its name differs from an earlier entry's only in case): %s" % p)


if __name__ == "__main__":
    main(sys.argv[1:])
