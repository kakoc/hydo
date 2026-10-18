#!/usr/bin/env python3
"""Determinism check for a list of schedules on the Mac backend: each schedule runs in two
separate `hvmx serve` processes (restored from the S1 snapshot) and once as `hvmx one` (a
fresh boot). All three fingerprints must be equal.

    check.py <image.cpio.gz> <outdir> <schedules-file>      (one schedule per line, # comments)
"""
import os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
EXP = os.path.dirname(HERE)
OUT = os.path.join(EXP, "out")


def run_serve(img, outdir, scheds, env):
    p = subprocess.Popen([os.path.join(EXP, "hvmx"), "serve", os.path.join(OUT, "Image-explore"), img, outdir],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env)
    res = {}
    for line in p.stdout:
        if line.startswith("READY"):
            break
    for i, s in enumerate(scheds):
        p.stdin.write("RUN s%d %s\n" % (i, s)); p.stdin.flush()
        for line in p.stdout:
            if line.startswith("RESULT"):
                res[i] = line.strip(); break
    p.stdin.write("QUIT\n"); p.stdin.flush(); p.wait()
    return res


def field(r, k):
    for t in r.split():
        if t.startswith(k + "="):
            return t[len(k) + 1:]
    return "?"


def main():
    img, outdir, sf = os.path.abspath(sys.argv[1]), sys.argv[2], sys.argv[3]
    scheds = [l.strip() for l in open(sf) if l.strip() and not l.startswith("#")]
    env = dict(os.environ)
    disk = img[:-len(".cpio.gz")] + "-disk.img"
    if os.path.exists(disk):
        env["HVM_DISK"] = disk
    a = run_serve(img, os.path.join(outdir, "a"), scheds, env)
    b = run_serve(img, os.path.join(outdir, "b"), scheds, env)
    bad = 0
    for i, s in enumerate(scheds):
        o = subprocess.run([os.path.join(EXP, "hvmx"), "one", os.path.join(OUT, "Image-explore"), img,
                            os.path.join(outdir, "one%d" % i), s], stdout=subprocess.PIPE, text=True, env=env).stdout
        one = [l for l in o.splitlines() if l.startswith("RESULT")][0]
        fps = (field(a[i], "fp"), field(b[i], "fp"), field(one, "fp"))
        ok = len(set(fps)) == 1 and fps[0] not in ("0", "?")
        bad += not ok
        print("%s %s | %s | %s" % ("IDENTICAL" if ok else "DIFFER   ", fps[0] if ok else fps, s,
                                   a[i].split("verdict=", 1)[-1][:120]))
    print("%d of %d identical" % (len(scheds) - bad, len(scheds)))
    sys.exit(1 if bad else 0)


main()
