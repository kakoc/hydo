"""hydo-build --arch arm64: the manifest compiled for the Mac VMM (hvx-mac/, Milestone 48).

Called by hydo-build's main() with its module namespace H (compile_shell, copy_into, mkfs,
zero_times, uuid_from, fix_mtimes, sha, write_mbr, ManifestError, FIXED_MTIME, HERE). Output, in
the build dir:
    <NAME>-arm64.cpio.gz    the root, as one initramfs
    <NAME>-arm64-disk.img   the data disk for the virtio-blk device (HVM_DISK): an MBR with a
                            1 MiB placeholder p1 and the journaled ext4 data partition p2, so
                            hydo-init.sh mounts /dev/vda2 unchanged
hvx-mac/explore/hvmx boots them with the arm64 kernel from hvx-mac/explore/build-guest.sh.

The root tree is the same as the x86 image's (busybox applets, the project's files and
closures, /etc/hydo/{manifest.sh,prelude.sh,hydo.yaml}, hydo-init.sh as /sbin/init), with two
differences:
  * the whole root is the initramfs (in guest RAM), not a read-only ext4 partition; a small
    /init waits for /dev/vda2 and then runs /sbin/init;
  * no tc (the pinned iproute2 is x86): network faults are skipped by hydo-init.sh as on an
    image without a port.
root.nix and root.files must name arm64 binaries (e.g. aarch64 store paths); nothing checks
the architecture. Byte-reproducible: sorted newc cpio, fixed mtimes and owners, gzip -n.
"""
import gzip, io, os, shutil, stat, subprocess

BUSYBOX_ARM64 = "/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1/bin/busybox"

SHIM = """#!/bin/busybox sh
# /init of an arm64 manifest image (hvx/onboard/hydo_build_arm64.py): wait for the data
# partition of the virtio disk, then the generic init (hydo-init.sh).
/bin/busybox mount -t devtmpfs none /dev
n=0
while [ ! -b /dev/vda2 ] && [ $n -lt 200 ]; do /bin/busybox usleep 10000; n=$((n + 1)); done
[ -b /dev/vda2 ] || echo "hydo-ob: no /dev/vda2 (run with the -arm64-disk.img attached: HVM_DISK)"
exec /sbin/init
"""


def _cpio(root, out_gz, mtime):
    """newc archive of root, entries sorted by path, uid/gid 0, fixed mtime; gzip -n."""
    paths = []
    for d, dirs, files in os.walk(root):
        dirs.sort()
        rel = os.path.relpath(d, root)
        if rel != ".":
            paths.append(rel)
        for f in sorted(files):
            paths.append(os.path.normpath(os.path.join(rel, f)))
    paths.sort()
    buf = io.BytesIO()

    def ent(name, mode, data=b"", nlink=1, rdev=(0, 0), ino=[1]):
        nb = name.encode() + b"\0"
        hdr = "070701" + "".join("%08x" % v for v in (ino[0], mode, 0, 0, nlink, mtime, len(data), 0, 0,
                                                       rdev[0], rdev[1], len(nb), 0))
        ino[0] += 1
        buf.write(hdr.encode()); buf.write(nb)
        buf.write(b"\0" * ((4 - (110 + len(nb)) % 4) % 4))
        buf.write(data); buf.write(b"\0" * ((4 - len(data) % 4) % 4))

    have_dev_console = False
    for p in paths:
        full = os.path.join(root, p)
        st = os.lstat(full)
        if p == "dev/console":
            have_dev_console = True
        if stat.S_ISLNK(st.st_mode):
            ent(p, stat.S_IFLNK | 0o777, os.readlink(full).encode())
        elif stat.S_ISDIR(st.st_mode):
            ent(p, stat.S_IFDIR | (st.st_mode & 0o7777), nlink=2)
        else:
            with open(full, "rb") as f:
                ent(p, stat.S_IFREG | (st.st_mode & 0o7777), f.read())
    if not have_dev_console:
        ent("dev/console", stat.S_IFCHR | 0o600, rdev=(5, 1))
    ent("TRAILER!!!", 0)
    with open(out_gz, "wb") as fo:
        with gzip.GzipFile(filename="", mode="wb", fileobj=fo, mtime=0, compresslevel=9) as g:
            g.write(buf.getvalue())


def build(m, manifest_path, out, bdir, H):
    T = {
        "busybox": H["tool"]("HYDO_BUSYBOX_ARM64", BUSYBOX_ARM64, os.path.isfile),
        "mke2fs": H["tool"]("HYDO_MKE2FS", "/nix/store/24s0phlx9kly7x7qb8gal3mmn51ndmlx-e2fsprogs-1.47.0-bin/bin/mke2fs", os.path.isfile),
        "faketime": H["tool"]("HYDO_FAKETIME", "/nix/store/fs0gp7zyfjnbvaq82yvwbcsvqh7876d1-libfaketime-0.9.10/bin/faketime", os.path.isfile),
    }
    seedrng = os.environ.get("HYDO_SEEDRNG_ARM64")      # optional: the device tree's rng-seed seeds the CRNG
    mdir = os.path.dirname(os.path.abspath(manifest_path))
    root = m.get("root") or {}
    R = os.path.join(bdir, "%s-arm64-root" % out)
    if os.path.isdir(R):
        subprocess.run(["chmod", "-R", "u+w", R], check=False)
        shutil.rmtree(R)
    os.makedirs(os.path.join(R, "nix/store"))
    store = sorted(set(p for n in (root.get("nix") or []) for p in H["closure"](n)))
    for p in store:
        subprocess.run(["cp", "-a", p, os.path.join(R, "nix/store/")], check=True)
    for d in ["bin", "sbin", "proc", "sys", "dev", "data", "tmp", "etc/hydo", "lib/modules"] + [d.lstrip("/") for d in root.get("dirs") or []]:
        os.makedirs(os.path.join(R, d), exist_ok=True)
    shutil.copy(T["busybox"], os.path.join(R, "bin/busybox"))
    os.chmod(os.path.join(R, "bin/busybox"), 0o755)
    if seedrng:
        shutil.copy(seedrng, os.path.join(R, "bin/hvx_seedrng")); os.chmod(os.path.join(R, "bin/hvx_seedrng"), 0o755)
    # the applet list of an arm64 busybox cannot be asked from it on an x86 build box: use the
    # x86 build of the same applet set if HYDO_BUSYBOX is runnable, else a fixed list
    lister = os.environ.get("HYDO_BUSYBOX") or "/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox"
    try:
        applets = subprocess.run([lister, "--list"], stdout=subprocess.PIPE, text=True, check=True).stdout.split()
    except (OSError, subprocess.CalledProcessError):
        applets = ("sh ash cat echo grep egrep sed awk tr cut sort uniq wc head tail sleep date timeout pgrep pkill kill "
                   "mount umount mkdir rm cp mv ln ls test [ [[ true false printf env expr seq tee touch sync usleep "
                   "ifconfig ip wget nc httpd md5sum sha256sum xargs find basename dirname uname id hostname dd").split()
    for a in applets:
        p = os.path.join(R, "bin", a)
        if not os.path.lexists(p):
            os.symlink("busybox", p)
    for e in root.get("files") or []:
        if not isinstance(e, dict) or "src" not in e or "dst" not in e:
            raise H["ManifestError"]("root.files: each entry needs src and dst")
        src = os.path.expanduser(str(e["src"]))
        if not os.path.isabs(src):
            src = os.path.join(mdir, src)
        if not os.path.exists(src):
            raise H["ManifestError"]("root.files: %s does not exist" % src)
        dst = os.path.join(R, str(e["dst"]).lstrip("/"))
        H["copy_into"](src, dst)
        if "mode" in e:
            os.chmod(dst, int(str(e["mode"]), 8))
    for dst, content in (root.get("text") or {}).items():
        p = os.path.join(R, str(dst).lstrip("/"))
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(content)
    for name, content in (("etc/hosts", "127.0.0.1 localhost\n"), ("etc/passwd", "root:x:0:0:root:/tmp:/bin/sh\n"),
                          ("etc/group", "root:x:0:\n")):
        p = os.path.join(R, name)
        if not os.path.exists(p):
            with open(p, "w") as f:
                f.write(content)
    manifest_sh, prelude = H["compile_shell"](m, "")
    with open(os.path.join(R, "etc/hydo/manifest.sh"), "w") as f:
        f.write(manifest_sh)
    with open(os.path.join(R, "etc/hydo/prelude.sh"), "w") as f:
        f.write(prelude)
    shutil.copy(manifest_path, os.path.join(R, "etc/hydo/hydo.yaml"))
    shutil.copy(os.path.join(H["HERE"], "hydo-init.sh"), os.path.join(R, "sbin/init"))
    os.chmod(os.path.join(R, "sbin/init"), 0o755)
    with open(os.path.join(R, "init"), "w") as f:
        f.write(SHIM)
    os.chmod(os.path.join(R, "init"), 0o755)
    dmib = int((m.get("data") or {}).get("size_mib", 64))   # the 16 MiB journal needs >32 MiB
    data = os.path.join(bdir, "%s-arm64-data.img" % out)
    H["mkfs"](T, data, dmib, H["uuid_from"](m["name"], "data"), H["uuid_from"](m["name"], "data-hash"), journal=True)
    H["zero_times"](data, all_inode_times=True)
    disk = os.path.join(bdir, "%s-arm64-disk.img" % out)
    p1, n1 = 2048, 2048                                      # a 1 MiB placeholder: the root is the initramfs
    p2, n2 = p1 + n1, dmib * 2048
    with open(disk, "wb") as f:
        f.truncate((p2 + n2) * 512)
    H["write_mbr"](disk, [(p1, n1), (p2, n2)])
    with open(disk, "r+b") as fo, open(data, "rb") as fi:
        fo.seek(p2 * 512)
        shutil.copyfileobj(fi, fo, 1 << 20)
    H["fix_mtimes"](R)
    cpio = os.path.join(bdir, "%s-arm64.cpio.gz" % out)
    _cpio(R, cpio, H["FIXED_MTIME"])
    print("hydo-build: %s (arm64 initramfs) + %s (data %d MiB)" % (cpio, disk, dmib))
    for p in (cpio, disk):
        print("  sha256(%s)=%s" % (os.path.basename(p), H["sha"](p)))
