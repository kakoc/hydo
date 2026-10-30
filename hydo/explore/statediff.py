#!/usr/bin/env python3
"""statediff.py -- byte diff of two hvx_explore state dumps (HVX_EXPLORE_DUMP).

  statediff.py ram  <a.ram>  <b.ram>  [System.map]   differing pages/bytes of guest RAM; kernel
                                                     image addresses are named from System.map
  statediff.py disk <a.disk> <b.disk>                differing 4 KiB blocks of the data disk
  statediff.py snap <a.snap> <b.snap>                differing bytes of the snapshot struct

Guest RAM is guest-physical; the kernel runs at virt = phys + 0xffffffff80000000 (nokaslr).
"""
import bisect
import mmap
import sys

KBASE = 0xffffffff80000000
PG = 4096


def load_map(path):
    syms = []
    for line in open(path):
        p = line.split()
        if len(p) >= 3:
            try:
                a = int(p[0], 16)
            except ValueError:
                continue
            if a >= KBASE:
                syms.append((a, p[2], p[1]))
    syms.sort()
    return syms


def name(syms, addrs, va):
    i = bisect.bisect_right(addrs, va) - 1
    if i < 0:
        return "?"
    a, n, t = syms[i]
    return "%s(%s)+%#x" % (n, t, va - a)


def ranges(a, b, gran):
    """yield (offset, length) of differing runs, compared in `gran`-byte blocks first"""
    n = min(len(a), len(b))
    off = 0
    while off < n:
        e = min(off + gran, n)
        if a[off:e] != b[off:e]:
            blk_a, blk_b = a[off:e], b[off:e]
            i = 0
            while i < e - off:
                if blk_a[i] != blk_b[i]:
                    j = i
                    while j < e - off and blk_a[j] != blk_b[j]:
                        j += 1
                    yield off + i, j - i
                    i = j
                else:
                    i += 1
        off = e


def main():
    kind, fa, fb = sys.argv[1:4]
    syms = load_map(sys.argv[4]) if len(sys.argv) > 4 else []
    addrs = [s[0] for s in syms]
    fha, fhb = open(fa, "rb"), open(fb, "rb")
    ma = mmap.mmap(fha.fileno(), 0, access=mmap.ACCESS_READ)
    mb = mmap.mmap(fhb.fileno(), 0, access=mmap.ACCESS_READ)
    if len(ma) != len(mb):
        print("size differs: %d vs %d" % (len(ma), len(mb)))
    pages = {}
    nbytes = 0
    for off, ln in ranges(ma, mb, PG if kind != "snap" else 64):
        nbytes += ln
        pages.setdefault(off // PG, []).append((off, ln))
    print("%s: %d differing bytes in %d %s" % (kind, nbytes, len(pages), "pages" if kind != "snap" else "4K-blocks"))
    for pg in sorted(pages):
        runs = pages[pg]
        tot = sum(l for _, l in runs)
        hdr = "page %#x (%d bytes, %d runs)" % (pg * PG, tot, len(runs))
        if kind == "ram" and syms:
            hdr += "  %s" % name(syms, addrs, pg * PG + KBASE)
        print(hdr)
        for off, ln in runs[:12]:
            s = "  %#011x +%-4d a=%s b=%s" % (off, ln, ma[off:off + min(ln, 16)].hex(), mb[off:off + min(ln, 16)].hex())
            if kind == "ram" and syms:
                s += "  " + name(syms, addrs, off + KBASE)
            print(s)
        if len(runs) > 12:
            print("  ... %d more runs" % (len(runs) - 12))


if __name__ == "__main__":
    main()
