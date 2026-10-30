#!/usr/bin/env python3
"""Print the shared-library closure of some ELF files, as absolute paths.

Usage: elfdeps.py [--root DIR] FILE...   (files are dynamically linked nix-store binaries/libraries)

Reads PT_INTERP, DT_NEEDED, DT_RUNPATH/DT_RPATH straight from the ELF (no readelf, so it
works for foreign architectures), resolves each needed library through the RUNPATH of the
object that needs it, and recurses. Prints every resolved file (and every symlink on the
way to it) once, sorted, so the initramfs builder can copy exactly what the dynamic loader
will open. A library that cannot be resolved is an error.

--root DIR (Milestone 99): the files live under DIR (DIR/nix/store/..., e.g. a tree unpacked by
hydo/dist/linux/nix_cache_fetch.py on a Mac without /nix): FILE arguments and the printed paths are the
guest's absolute paths, every read and symlink hop is made under DIR. Without it nothing changes.
"""
import os
import struct
import sys

PT_INTERP, PT_DYNAMIC, PT_LOAD = 3, 2, 1
DT_NEEDED, DT_STRTAB, DT_RPATH, DT_RUNPATH = 1, 5, 15, 29


ROOT = ""          # --root: prefix of every file system access


def H(p):
    """The host path of a guest path."""
    return ROOT + p if ROOT else p


def realpath(p):
    """os.path.realpath of a guest path, every hop resolved under ROOT."""
    if not ROOT:
        return os.path.realpath(p)
    parts, out, hops = [x for x in p.split("/") if x], "", 0
    while parts:
        x = parts.pop(0)
        if x in ("", "."):
            continue
        if x == "..":
            out = os.path.dirname(out) if out else ""
            continue
        nxt = out + "/" + x
        if os.path.islink(H(nxt)):
            hops += 1
            if hops > 40:
                sys.exit(f"elfdeps: symlink loop at {p}")
            t = os.readlink(H(nxt))
            parts = [y for y in t.split("/") if y] + parts
            if t.startswith("/"):
                out = ""
            continue
        out = nxt
    return out or "/"


def parse(path):
    with open(H(path), "rb") as f:
        b = f.read()
    if b[:4] != b"\x7fELF" or b[4] != 2:
        return None
    phoff, = struct.unpack_from("<Q", b, 0x20)
    phentsize, phnum = struct.unpack_from("<HH", b, 0x36)
    loads, interp, dyn = [], None, None
    for i in range(phnum):
        p_type, _flags, p_off, p_vaddr, _pa, p_filesz, _ms, _al = struct.unpack_from("<IIQQQQQQ", b, phoff + i * phentsize)
        if p_type == PT_LOAD:
            loads.append((p_vaddr, p_off, p_filesz))
        elif p_type == PT_INTERP:
            interp = b[p_off:p_off + p_filesz].rstrip(b"\0").decode()
        elif p_type == PT_DYNAMIC:
            dyn = (p_off, p_filesz)
    if not dyn:
        return interp, [], []

    def v2o(va):
        for v, o, n in loads:
            if v <= va < v + n:
                return va - v + o
        raise ValueError(f"{path}: vaddr {va:#x} not in a PT_LOAD")

    ents, strtab = [], None
    for k in range(dyn[1] // 16):
        tag, val = struct.unpack_from("<qQ", b, dyn[0] + 16 * k)
        if tag == 0:
            break
        if tag == DT_STRTAB:
            strtab = v2o(val)
        ents.append((tag, val))

    def s(off):
        e = b.index(b"\0", strtab + off)
        return b[strtab + off:e].decode()

    needed = [s(v) for t, v in ents if t == DT_NEEDED]
    rp = []
    for t, v in ents:
        if t in (DT_RUNPATH, DT_RPATH):
            rp += [d for d in s(v).split(":") if d]
    origin = os.path.dirname(path)
    rp = [d.replace("$ORIGIN", origin).replace("${ORIGIN}", origin) for d in rp]
    return interp, needed, rp


def chain(p):
    """p and every symlink hop to its final target."""
    out = []
    seen = set()
    while p not in seen:
        seen.add(p)
        out.append(p)
        if not os.path.islink(H(p)):
            break
        t = os.readlink(H(p))
        p = os.path.normpath(os.path.join(os.path.dirname(p), t))
    return out


def main(files):
    todo = list(files)
    done, outs = set(), set()
    fallback = []          # the loader's own directory: glibc's libraries carry no RUNPATH
    while todo:
        f = todo.pop()
        real = realpath(f)
        if real in done:
            continue
        done.add(real)
        r = parse(real)
        if r is None:
            continue
        interp, needed, rp = r
        if interp:
            outs.update(chain(interp))
            todo.append(interp)
            d = os.path.dirname(interp)
            if d not in fallback:
                fallback.append(d)
        for n in needed:
            for d in rp + fallback + [os.path.dirname(real)]:
                c = os.path.join(d, n)
                if os.path.exists(H(c)):
                    outs.update(chain(c))
                    todo.append(c)
                    break
            else:
                sys.exit(f"elfdeps: {f}: cannot resolve {n} (runpath {rp})")
    for o in sorted(outs):
        print(o)


if __name__ == "__main__":
    a = sys.argv[1:]
    if a[:1] == ["--root"]:
        ROOT = os.path.abspath(a[1]).rstrip("/")
        a = a[2:]
    main(a)
