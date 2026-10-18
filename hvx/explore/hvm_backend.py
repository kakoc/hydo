"""explore.py --backend hvm: run schedules on the Mac VMM (hvx-mac/explore/hvmx) instead of
hvx_explore on the Linux box. The worker protocol (READY / RUN / RESULT / QUIT) is the same;
only the engine's command line and the set of faults it can apply differ.

The guest is a manifest-built arm64 initramfs (hvx/onboard/hydo-build --arch arm64): --image
names it (a path, or a file under hvx-mac/explore/out), --hvm-kernel the arm64 kernel
(default hvx-mac/explore/out/Image-explore, from hvx-mac/explore/build-guest.sh).

What the Mac backend cannot do yet is removed from every schedule BEFORE it is rendered
(hvm_supported), so a run never claims a fault it did not apply: there is no block device
(no disk faults, no power cuts), no tc on the arm64 root (no N=/O= network faults) and no
PMU clock (no tick-timing jitter, no run budget). Kills, pauses, clock jumps and the
manifest's durability knobs are applied as on the box.
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
MAC = os.path.join(os.path.dirname(os.path.dirname(HERE)), "hvx-mac", "explore")
KERNEL = os.path.join(MAC, "out", "Image-explore")


def resolve_image(image):
    if os.path.isabs(image) or os.path.exists(image):
        return os.path.abspath(image)
    return os.path.join(MAC, "out", image)


def engine_cmd(image, consoles, kernel=None):
    return [os.path.join(MAC, "hvmx"), "serve", kernel or KERNEL, resolve_image(image), consoles]


def hvm_supported(s):
    """The part of a schedule the Mac backend can run (a copy; s is not modified)."""
    s = dict(s)
    s["disk"] = "none"
    for k in ("C", "N", "O", "tj", "rb"):
        s.pop(k, None)
    return s


def repro_line(image, schedule, kernel=None):
    return "%s one %s %s <outdir> \"%s\"" % (os.path.join(MAC, "hvmx"), kernel or KERNEL, resolve_image(image), schedule)
