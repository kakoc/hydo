#!/usr/bin/env python3
"""nar_extract -- take files out of a Nix binary-cache archive (.nar.xz) without Nix.

    nar_extract.py <file.nar.xz> <FileHash nix32> <out dir> <path in the archive>...

Checks the compressed file against the narinfo's FileHash (sha256, Nix base-32), then streams
the NAR and writes each named regular file (e.g. lib/modules/6.6.35/kernel/drivers/net/veth.ko.xz)
under <out dir> with the same relative path, and prints its sha256. Used in Milestone 95 to add
the guest kernel's llc/stp/bridge/veth modules (network: bridge) to a release tree on a machine
without Nix, from https://cache.nixos.org/<hash>.narinfo of the guest kernel's store path.
Standard library only.
"""
import hashlib, lzma, os, struct, sys

NIX32 = "0123456789abcdfghijklmnpqrsvwxyz"


def nix32(h):
    n = (len(h) * 8 - 1) // 5 + 1
    out = []
    for k in range(n - 1, -1, -1):
        b = k * 5
        i, j = b // 8, b % 8
        c = h[i] >> j
        if i + 1 < len(h):
            c |= h[i + 1] << (8 - j)
        out.append(NIX32[c & 0x1f])
    return "".join(out)


class Nar:
    def __init__(self, f):
        self.f = f

    def raw(self, n):
        b = self.f.read(n)
        if len(b) != n:
            raise SystemExit("nar_extract: truncated archive")
        return b

    def num(self):
        return struct.unpack("<Q", self.raw(8))[0]

    def bytes_(self, sink=None):
        n = self.num()
        left = n
        data = [] if sink is None else None
        while left:
            b = self.raw(min(left, 1 << 20))
            left -= len(b)
            if sink is None:
                data.append(b)
            else:
                sink(b)
        pad = (8 - n % 8) % 8
        if pad:
            self.raw(pad)
        return b"".join(data) if data is not None else None

    def s(self):
        return self.bytes_().decode()

    def expect(self, t):
        x = self.s()
        if x != t:
            raise SystemExit("nar_extract: expected %r, got %r" % (t, x))

    def node(self, path, want, out):
        self.expect("(")
        self.expect("type")
        t = self.s()
        if t == "regular":
            x = self.s()
            if x == "executable":
                self.expect("")
                x = self.s()
            assert x == "contents"
            if path in want:
                h = hashlib.sha256()
                dst = os.path.join(out, path)
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                with open(dst, "wb") as f:
                    def sink(b):
                        h.update(b); f.write(b)
                    self.bytes_(sink)
                print("%s  %s" % (h.hexdigest(), path))
                want.discard(path)
            else:
                self.bytes_(lambda b: None)
            self.expect(")")
        elif t == "symlink":
            self.expect("target"); self.s(); self.expect(")")
        elif t == "directory":
            while True:
                x = self.s()
                if x == ")":
                    break
                assert x == "entry"
                self.expect("("); self.expect("name"); name = self.s(); self.expect("node")
                self.node(name if not path else path + "/" + name, want, out)
                self.expect(")")
        else:
            raise SystemExit("nar_extract: unknown node type %r" % t)


def main():
    src, filehash, out = sys.argv[1:4]
    want = set(sys.argv[4:])
    h = hashlib.sha256()
    with open(src, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    got = nix32(h.digest())
    if got != filehash.split(":")[-1]:
        raise SystemExit("nar_extract: FileHash mismatch: %s, narinfo says %s" % (got, filehash))
    print("FileHash ok: sha256:%s" % got)
    with lzma.open(src, "rb") as f:
        n = Nar(f)
        n.expect("nix-archive-1")
        n.node("", want, out)
    if want:
        raise SystemExit("nar_extract: not in the archive: %s" % ", ".join(sorted(want)))


if __name__ == "__main__":
    main()
