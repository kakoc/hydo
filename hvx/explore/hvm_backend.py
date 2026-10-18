"""explore.py --backend hvm: run schedules on the Mac VMM (hvx-mac/explore/hvmx) instead of
hvx_explore on the Linux box. The worker protocol (READY / RUN / RESULT / QUIT) is the same;
only the engine's command line and the set of faults it can apply differ.

The guest is a manifest-built arm64 initramfs (hvx/onboard/hydo-build --arch arm64): --image
names it (a path, or a file under hvx-mac/explore/out), --hvm-kernel the arm64 kernel
(default hvx-mac/explore/out/Image-explore, from hvx-mac/explore/build-guest.sh).

The data disk (<NAME>-arm64-disk.img next to the initramfs, from hydo-build --arch arm64) is
attached as the virtio-blk device when it exists; then disk faults and power cuts work as on
the box (same seeded fault classes, same two-boot recovery). What the Mac backend cannot do
is removed from every schedule BEFORE it is rendered (hvm_supported), so a run never claims
a fault it did not apply: no tc on the arm64 root (no N=/O= network faults), no PMU clock (no
run budget), and without the disk no disk faults or power cuts. Tick jitter works, but its
delays are counted in VM exits instead of guest branches (hvx-mac/hvm.c), so a seeded jitter's
maximum delay is rescaled to HVM_TJ_MAX exits.
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
MAC = os.path.join(os.path.dirname(os.path.dirname(HERE)), "hvx-mac", "explore")
KERNEL = os.path.join(MAC, "out", "Image-explore")


def resolve_image(image):
    if os.path.isabs(image) or os.path.exists(image):
        return os.path.abspath(image)
    return os.path.join(MAC, "out", image)


def disk_for(image):
    """The data disk built next to an arm64 initramfs, or None."""
    p = resolve_image(image)
    d = p[:-len(".cpio.gz")] + "-disk.img" if p.endswith(".cpio.gz") else None
    return d if d and os.path.exists(d) else None


HAS_DISK = False        # set by configure()
HVM_TJ_MAX = 1000       # exits; a busy guest takes about 1,500 exits per 4 ms tick (64 virtual ticks each)


def configure(image):
    global HAS_DISK
    HAS_DISK = disk_for(image) is not None


def engine_cmd(image, consoles, kernel=None):
    env = ["env", "HVM_DISK=" + disk_for(image)] if disk_for(image) else []
    return env + [os.path.join(MAC, "hvmx"), "serve", kernel or KERNEL, resolve_image(image), consoles]


def hvm_supported(s):
    """The part of a schedule the Mac backend can run (a copy; s is not modified)."""
    s = dict(s)
    if not HAS_DISK:
        s["disk"] = "none"
        s.pop("C", None)
    for k in ("N", "O", "rb"):
        s.pop(k, None)
    tj = s.get("tj")
    if tj and tj[0] == "seed" and tj[3] > HVM_TJ_MAX:
        s["tj"] = tj[:3] + (HVM_TJ_MAX,) + tj[4:]
    return s


def repro_line(image, schedule, kernel=None):
    env = "HVM_DISK=%s " % disk_for(image) if disk_for(image) else ""
    return "%s%s one %s %s <outdir> \"%s\"" % (env, os.path.join(MAC, "hvmx"), kernel or KERNEL, resolve_image(image), schedule)


def embedded_manifest(image):
    """etc/hydo/hydo.yaml from an arm64 manifest initramfs (gzip'd newc cpio)."""
    import gzip
    data = gzip.open(resolve_image(image)).read()
    i = 0
    while data[i:i + 6] == b"070701":
        f = [int(data[i + 6 + 8 * k:i + 14 + 8 * k], 16) for k in range(13)]
        fsz, nsz = f[6], f[11]
        name = data[i + 110:i + 110 + nsz - 1].decode()
        o = (i + 110 + nsz + 3) & ~3
        if name == "etc/hydo/hydo.yaml":
            return data[o:o + fsz].decode()
        if name == "TRAILER!!!":
            break
        i = (o + fsz + 3) & ~3
    raise SystemExit("no etc/hydo/hydo.yaml in %s" % image)
