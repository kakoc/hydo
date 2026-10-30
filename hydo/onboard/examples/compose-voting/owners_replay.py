#!/usr/bin/env python3
# M107: a research/measurement tool. It needs the Python explorer and builder, deleted in M107
# (replaced by the hydo-explore and hydo-build crates): restore them from commit 2a58c4b with
# git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history in a scratch checkout, and run it there.
"""M95 identity proof for a release build on a non-Nix machine: a plain hydo-build on the Nix build
box keeps the build user's file owners (1000:100) in the root image, while the Linux release
sets them to 0:0 (hvx/dist/hydo, own_by_root). This replays own_by_root on a copy of an image
built the plain way, then the time-zeroing hydo-build does after it, and compares the result with
the release's root image byte for byte.

    owners_replay.py <release dir> <old disk.img> <staged root dir> <new ext4.img> <scratch file>
"""
import hashlib, importlib.machinery, importlib.util, os, subprocess, sys

rel, old, staged, new, tmp = sys.argv[1:6]
spec = importlib.util.spec_from_loader("hb", importlib.machinery.SourceFileLoader("hb", os.path.join(rel, "hvx/onboard/hydo-build")))
hb = importlib.util.module_from_spec(spec); spec.loader.exec_module(hb)
size = os.path.getsize(new)
with open(old, "rb") as fi, open(tmp, "wb") as fo:      # partition 1 starts at LBA 2048
    fi.seek(2048 * 512)
    left = size
    while left:
        b = fi.read(min(left, 1 << 20)); fo.write(b); left -= len(b)
paths = [staged]
for d, dirs, files in os.walk(staged):
    dirs.sort()
    paths += [os.path.join(d, n) for n in sorted(dirs + files)]
lines = []
for p in paths:
    st = os.lstat(p)
    if st.st_uid == 0 and st.st_gid == 0:
        continue
    g = "/" + os.path.relpath(p, staged) if p != staged else "/"
    if g == "/oci" or g.startswith("/oci/"):
        continue    # the image roots: hydo_oci.fix_ext4 sets their owners from the layers AFTER own_by_root
    lines += ['sif "%s" uid 0' % g, 'sif "%s" gid 0' % g]
with open(tmp + ".owners", "w") as f:
    f.write("\n".join(lines) + "\n")
r = subprocess.run([os.path.join(rel, "libexec/faketime"), hb.FAKE_TS, os.path.join(rel, "tools/bin/debugfs"), "-w", "-f",
                    tmp + ".owners", tmp], env=hb.faketime_env(), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
os.unlink(tmp + ".owners")
errs = [x for x in r.stderr.splitlines() if x.strip() and not x.startswith("debugfs ")]
assert r.returncode == 0 and not errs, errs[:5]
hb.zero_times(tmp, all_inode_times=False)
h = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()
a, b = h(tmp), h(new)
print("re-owned %d paths; old root after own_by_root %s, release root %s: %s" % (len(lines) // 2, a[:16], b[:16],
      "IDENTICAL" if a == b else "DIFFERENT"))
os.environ.get("KEEP") or os.unlink(tmp)
