#!/usr/bin/env python3
# M107: a research/measurement tool. It needs the Python explorer and builder, deleted in M107
# (replaced by the hydo-explore and hydo-build crates): restore them from commit 2a58c4b with
# git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history in a scratch checkout, and run it there.
"""Build the namespace-kernel boot test's initramfs (Milestone 99).

    mkinit.py <busybox> <store root> <out.cpio.gz>

<busybox>: the static arm64 busybox of the Mac release (guest/busybox, 1.36.1); <store root>: a
directory holding nix/store/... with the aarch64 iproute2 and iptables of hydo_build_arm64.py
(hydo-nix-fetch --closure <root> <path>, hydo/crates/hydo-tools, or the release's guest tree). The archive
holds /init (this directory's init, with the tool paths filled in), busybox and its applets, and
ip, tc, iptables-legacy with exactly the files their loaders open; sorted, fixed mtimes, uid 0,
gzip -n, so two builds are the same bytes. Boot it with the namespace kernel:

    hv/hvm/hvm print hv/hvm/explore/out/Image-explore-netns <out.cpio.gz> "hydo-boot-marker: launching shell"
    hv/hvm/hvm branch ... "hydo-netns: ns1 addresses" "hydo-boot-marker: launching shell"
"""
import os, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(HERE))))
sys.path.insert(0, os.path.join(TOP, "hydo", "onboard"))
import hydo_build_arm64 as a64      # noqa: E402  (_cpio, TC_ARM64, IPTABLES_ARM64, IPT_FILES)

APPLETS = ("sh mount echo cat uname seq md5sum sleep mkdir wget httpd ping nsenter unshare tr cut grep "
           "head tail ls true").split()


def main(busybox, store, out):
    store = os.path.abspath(store)
    iproute = os.path.dirname(os.path.dirname(a64.TC_ARM64))
    ip, tc = iproute + "/bin/ip", iproute + "/bin/tc"
    ipt = a64.IPTABLES_ARM64
    want = [ip, tc] + [ipt + "/" + f for f in a64.IPT_FILES]
    deps = subprocess.run([sys.executable, a64.ELFDEPS, "--root", store] + want, stdout=subprocess.PIPE,
                          text=True, check=True).stdout.split()
    tmp = tempfile.mkdtemp(prefix="hydo-netnsboot-")
    try:
        R = os.path.join(tmp, "root")
        for d in ("bin", "proc", "sys", "dev", "tmp"):
            os.makedirs(os.path.join(R, d))
        shutil.copy(busybox, os.path.join(R, "bin/busybox"))
        os.chmod(os.path.join(R, "bin/busybox"), 0o755)
        for a in APPLETS:
            os.symlink("busybox", os.path.join(R, "bin", a))
        for p in sorted(set(deps + want)):
            src, dst = store + p, os.path.join(R, p.lstrip("/"))
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            if os.path.islink(src):
                os.symlink(os.readlink(src), dst)
            else:
                shutil.copy(src, dst)
                os.chmod(dst, 0o755 if os.access(src, os.X_OK) else 0o644)
        os.symlink("xtables-legacy-multi", os.path.join(R, ipt.lstrip("/"), "bin/iptables-legacy"))
        init = open(os.path.join(HERE, "init")).read()
        init = init.replace("@IP@", ip).replace("@TC@", tc).replace("@IPT@", ipt + "/bin/iptables-legacy")
        with open(os.path.join(R, "init"), "w") as f:
            f.write(init)
        os.chmod(os.path.join(R, "init"), 0o755)
        for d, dns, fns in os.walk(R):
            for n in dns + fns:
                os.utime(os.path.join(d, n), (1600000000, 1600000000), follow_symlinks=False)
        a64._cpio(R, out, 1600000000)
    finally:
        subprocess.run(["chmod", "-R", "u+w", tmp])
        shutil.rmtree(tmp)
    print(out)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    main(*sys.argv[1:])
