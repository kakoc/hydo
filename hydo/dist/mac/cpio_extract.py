#!/usr/bin/env python3
"""Extract a gzip'd newc cpio (as written by hydo_build_arm64.py) into a directory, without
root: regular files, directories and symlinks only (device nodes are skipped)."""
import gzip, os, stat, sys


def entries(path):
    data = gzip.open(path).read()
    i = 0
    while data[i:i + 6] == b"070701":
        f = [int(data[i + 6 + 8 * k:i + 14 + 8 * k], 16) for k in range(13)]
        mode, fsz, nsz = f[1], f[6], f[11]
        name = data[i + 110:i + 110 + nsz - 1].decode()
        o = (i + 110 + nsz + 3) & ~3
        if name == "TRAILER!!!":
            return
        yield name, mode, data[o:o + fsz]
        i = (o + fsz + 3) & ~3


def extract(path, out, only=None):
    for name, mode, body in entries(path):
        if only and not only(name):
            continue
        dst = os.path.join(out, name)
        if stat.S_ISDIR(mode):
            os.makedirs(dst, exist_ok=True)
        elif stat.S_ISLNK(mode):
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            if os.path.lexists(dst):
                os.unlink(dst)
            os.symlink(body.decode(), dst)
        elif stat.S_ISREG(mode):
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(dst, "wb") as fo:
                fo.write(body)
            os.chmod(dst, mode & 0o7777 | 0o200)


if __name__ == "__main__":
    extract(sys.argv[1], sys.argv[2])
