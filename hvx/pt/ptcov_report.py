#!/usr/bin/env python3
"""ptcov_report -- turn ptcov's executed-instruction list into a coverage report.

  ptcov_report.py funcs  SYMS INSNS                 per-function covered instruction counts
  ptcov_report.py lines  ELF  INSNS                 covered source lines (DWARF via addr2line)
  ptcov_report.py diff   SYMS|ELF A_INSNS B_INSNS   functions (and, for an ELF, lines)
                                                    covered in B but not in A, and vice versa

SYMS is a System.map / `nm -n` style file ("addr type name"); an ELF path makes the
script run `nm -n` on it. INSNS is ptcov's output (one hex address per line).
"""
import bisect
import os
import subprocess
import sys
from collections import Counter


def load_syms(path):
    if open(path, "rb").read(4) == b"\x7fELF":
        text = subprocess.run(["nm", "-n", path], capture_output=True, text=True).stdout
    else:
        text = open(path).read()
    syms = []
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[1] in "tTwW":
            syms.append((int(parts[0], 16), parts[2]))
    syms.sort()
    return [a for a, _ in syms], [n for _, n in syms]


def load_insns(path):
    return [int(x, 16) for x in open(path) if x.strip()]


def func_counts(syms, insns):
    addrs, names = syms
    c = Counter()
    for ip in insns:
        i = bisect.bisect_right(addrs, ip) - 1
        c[names[i] if i >= 0 else "?"] += 1
    return c


def lines(elf, insns):
    """Covered file:line set. Go binaries: set PTCOV_GO to a go binary and its
    `go tool addr2line` (which reads the Go line table) is used; else GNU addr2line."""
    out = set()
    if not insns:
        return out
    go = os.environ.get("PTCOV_GO")
    if go:
        p = subprocess.run([go, "tool", "addr2line", elf],
                           input="\n".join("0x%x" % a for a in insns), capture_output=True, text=True)
        res = p.stdout.splitlines()
        for i in range(1, len(res), 2):
            if not res[i].startswith("?"):
                out.add(res[i])
        return out
    p = subprocess.run(["addr2line", "-e", elf], input="\n".join("%x" % a for a in insns),
                       capture_output=True, text=True)
    for line in p.stdout.splitlines():
        if not line.startswith("??"):
            out.add(line.split(" (discriminator")[0])
    return out


def main():
    mode = sys.argv[1]
    if mode == "funcs":
        c = func_counts(load_syms(sys.argv[2]), load_insns(sys.argv[3]))
        print("covered functions: %d, instructions: %d" % (len(c), sum(c.values())))
        for name, n in c.most_common():
            print("%7d  %s" % (n, name))
    elif mode == "lines":
        ls = lines(sys.argv[2], load_insns(sys.argv[3]))
        print("covered source lines: %d" % len(ls))
        for l in sorted(ls):
            print(l)
    elif mode == "diff":
        src = sys.argv[2]
        syms = load_syms(src)
        a, b = load_insns(sys.argv[3]), load_insns(sys.argv[4])
        fa, fb = func_counts(syms, a), func_counts(syms, b)
        print("instructions: A %d, B %d; only in B %d, only in A %d" %
              (len(a), len(b), len(set(b) - set(a)), len(set(a) - set(b))))
        print("functions: A %d, B %d" % (len(fa), len(fb)))
        for title, x, y in (("only in B", fb, fa), ("only in A", fa, fb)):
            only = sorted(n for n in x if n not in y)
            print("\n%s (%d functions):" % (title, len(only)))
            for n in only:
                print("  %s  (%d instructions)" % (n, x[n]))
        if open(src, "rb").read(4) == b"\x7fELF":
            la, lb = lines(src, a), lines(src, b)
            for title, x, y in (("source lines only in B", lb, la), ("source lines only in A", la, lb)):
                only = sorted(x - y)
                print("\n%s (%d):" % (title, len(only)))
                for l in only:
                    print("  " + os.path.relpath(l) if l.startswith("/") else "  " + l)
    else:
        print(__doc__)
        sys.exit(2)


if __name__ == "__main__":
    main()
