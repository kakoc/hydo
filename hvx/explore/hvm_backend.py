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
a fault it did not apply: no PMU clock (no run budget, no vCPU turn tokens q=/qs=), without the disk no disk faults or
power cuts, and without tc in the image (an arm64 image built with HYDO_TC_ARM64= , or one built
before tc was added) no N=/O= network faults. Whether the image has tc is read from its compiled
manifest (etc/hydo/manifest.sh, HYDO_TC), not assumed. Tick jitter works, but its delays are
counted in VM exits instead of guest branches (hvx-mac/hvm.c), so a seeded jitter's maximum
delay is capped at HVM_TJ_MAX exits.

Guest RAM: explore.py passes ["--ram-mib", N] (its ram_args(), from the manifest's
guest.ram_mib or --ram-mib) to engine_cmd/repro_line; hvmx accepts 256..4096 MiB.
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
HAS_TC = False
HVM_TJ_MAX = 1000       # exits; a busy guest takes about 1,500 exits per 4 ms tick (64 virtual ticks each)


def configure(image):
    global HAS_DISK, HAS_TC
    HAS_DISK = disk_for(image) is not None
    HAS_TC = image_has_tc(image)


def image_has_tc(image):
    """True if the image's compiled manifest names a tc binary (HYDO_TC non-empty)."""
    for line in embedded_file(image, "etc/hydo/manifest.sh").splitlines():
        if line.startswith("HYDO_TC="):
            return line[len("HYDO_TC="):].strip("'\"") != ""
    return False


def engine_cmd(image, consoles, kernel=None, ram=()):
    """ram: ["--ram-mib", N] for a non-default guest RAM size (explore.py's ram_args())."""
    env = ["env", "HVM_DISK=" + disk_for(image)] if disk_for(image) else []
    return env + [os.path.join(MAC, "hvmx")] + list(ram) + ["serve", kernel or KERNEL, resolve_image(image), consoles]


def hvm_supported(s):
    """The part of a schedule the Mac backend can run (a copy; s is not modified)."""
    s = dict(s)
    if not HAS_DISK:
        s["disk"] = "none"
        s.pop("C", None)
    for k in ("rb", "pp", "xt", "q") if HAS_TC else ("N", "O", "rb", "pp", "xt", "q"):   # pp/xt/q need the PMU clock; the Mac's lever is pct
        s.pop(k, None)
    tj = s.get("tj")
    if tj and tj[0] == "seed" and tj[3] > HVM_TJ_MAX:
        s["tj"] = tj[:3] + (HVM_TJ_MAX,) + tj[4:]
    return s


def repro_line(image, schedule, kernel=None, ram=()):
    env = "HVM_DISK=%s " % disk_for(image) if disk_for(image) else ""
    return "%s%s %sone %s %s <outdir> \"%s\"" % (env, os.path.join(MAC, "hvmx"), "".join(x + " " for x in ram),
                                               kernel or KERNEL, resolve_image(image), schedule)


def image_ram_args(image):
    """["--ram-mib", N] for an image whose embedded manifest sets guest.ram_mib != 1024, else []
    (for callers that start hvmx on an image directly, e.g. `hydo run`)."""
    import manifest_space
    try:
        m = manifest_space.Space(embedded_manifest(image), "hydo.yaml").m
    except SystemExit:
        return []
    r = (m.get("guest") or {}).get("ram_mib", 1024)
    return [] if r == 1024 else ["--ram-mib", str(r)]


def embedded_manifest(image):
    """etc/hydo/hydo.yaml from an arm64 manifest initramfs (gzip'd newc cpio)."""
    return embedded_file(image, "etc/hydo/hydo.yaml")


def embedded_file(image, path):
    """One file from an arm64 manifest initramfs (gzip'd newc cpio)."""
    import gzip
    data = gzip.open(resolve_image(image)).read()
    i = 0
    while data[i:i + 6] == b"070701":
        f = [int(data[i + 6 + 8 * k:i + 14 + 8 * k], 16) for k in range(13)]
        fsz, nsz = f[6], f[11]
        name = data[i + 110:i + 110 + nsz - 1].decode()
        o = (i + 110 + nsz + 3) & ~3
        if name == path:
            return data[o:o + fsz].decode()
        if name == "TRAILER!!!":
            break
        i = (o + fsz + 3) & ~3
    raise SystemExit("no %s in %s" % (path, image))
