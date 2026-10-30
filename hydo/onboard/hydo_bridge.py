#!/usr/bin/env python3
"""hydo_bridge -- the Python product code the Rust `hydo` command (hydo/crates/hydo-cli) still calls.

The Rust CLI owns the commands; where a command's logic is still Python, it runs

    python3 hydo/onboard/hydo_bridge.py <function> <request.json>

with {"args": {...}, "result": "<path>"} in the request file. A function that answers data writes
it as JSON to the result path; every function's own output goes to the inherited stdout/stderr
unchanged, and the exit status is the function's (or 0). Each function here is one seam: when its
module is ported to Rust (one crate per module), the Rust CLI stops calling it and the function is
deleted. The code below is what the two Python release CLIs (hydo/dist/{linux,mac}/hydo before
M101) did in-process, moved here unchanged.

    build_linux            hydo-build with the Linux release's tools      hydo build (Linux)
    build_mac              hydo-build --arch arm64 with the Mac's tools   hydo build (Mac)

Ported and removed here (M104): errors_cli and errors_for_run_dir (the hydo-errors crate), and
regression_status, regression_warning and saved_regression_text (the hydo-composer crate).
"""
import importlib.machinery
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import types

ONBOARD = os.path.dirname(os.path.realpath(__file__))
HYDO = os.path.dirname(ONBOARD)
EXPLORE = os.path.join(HYDO, "explore")


def die(msg, rc=1):
    print("hydo: " + msg, file=sys.stderr)
    sys.exit(rc)


def _onboard():
    if ONBOARD not in sys.path:
        sys.path.insert(0, ONBOARD)


# ---------------------------------------------------------------- hydo build
# Replaced by hydo-build (Milestone 103): the Rust `hydo build` calls the hydo-build crate, which
# carries these two release patches itself (hydo_build::Release). Kept until the lead deletes them.
def load_hydo_build():
    loader = importlib.machinery.SourceFileLoader("hydo_build", os.path.join(ONBOARD, "hydo-build"))
    spec = importlib.util.spec_from_loader("hydo_build", loader)
    m = importlib.util.module_from_spec(spec)
    loader.exec_module(m)
    return m


def use_bundled_store(hb, GUEST):
    """hydo-build copies the guest-side Nix closures (tc from iproute2, iptables-legacy for the
    freeze fault) from the host's /nix/store with `nix-store -qR`. The release ships exactly those
    closures under guest/store/nix/store/ (guest/store-closures.txt lists them). Point
    hydo-build's own view of os.path (only for /nix/store paths the host does not have) and its
    closure() at that tree: the image gets the same paths, bytes and modes as a build on the Nix
    build box. A path that is not bundled (a manifest's root.nix:) still needs nix-store here."""
    STORE = os.path.join(GUEST, "store")
    CLOSURES = os.path.join(GUEST, "store-closures.txt")
    closures = {}
    for line in open(CLOSURES):
        if line.strip():
            root, member = line.split()
            closures.setdefault(root, []).append(member)
    here = lambda p: os.path.join(STORE, p.lstrip("/"))
    bundled = lambda p: str(p).startswith("/nix/store/") and not os.path.lexists(p) and os.path.lexists(here(p))
    path = types.ModuleType("os.path")
    path.__dict__.update(os.path.__dict__)
    for fn in ("isfile", "isdir", "exists", "lexists"):
        real = getattr(os.path, fn)
        setattr(path, fn, (lambda real: lambda p: real(here(p)) if bundled(p) else real(p))(real))
    shim = types.ModuleType("os")
    shim.__dict__.update(os.__dict__)
    shim.path = path
    hb.os = shim                                    # only hydo-build's own view of os
    real_closure = hb.closure

    def closure(p):
        if p in closures and not os.path.exists(p):
            return sorted(here(x) for x in closures[p])     # `cp -a <bundle>/nix/store/X R/nix/store/`
        if shutil.which("nix-store"):
            return real_closure(p)
        raise hb.ManifestError("%s: not in the release's bundled closures, and this machine has no "
                               "nix-store (root.nix: needs Nix; list the files under root.files: instead)" % p)
    hb.closure = closure


def own_by_root(hb):
    """mke2fs -d records each source file's owner, i.e. the uid/gid of whoever ran the build, so
    two people building one manifest would get different images (and different fingerprints).
    After hydo-build's mkfs of the root, set every inode's owner to 0:0 with the bundled debugfs,
    under the same frozen clock (the hydo_oci.fix_ext4 pattern). HYDO_KEEP_OWNERS=1 skips it
    (then the image matches a plain hydo-build run by the same user on the Nix build box)."""
    orig = hb.mkfs

    def mkfs(T, out, mib, uuid, seed, src=None, journal=False):
        orig(T, out, mib, uuid, seed, src=src, journal=journal)
        if not src or os.environ.get("HYDO_KEEP_OWNERS"):
            return
        paths = [src]
        for d, dirs, files in os.walk(src):
            dirs.sort()
            paths += [os.path.join(d, n) for n in sorted(dirs + files)]
        lines = []
        for p in paths:
            st = os.lstat(p)
            if st.st_uid == 0 and st.st_gid == 0:
                continue
            g = "/" + os.path.relpath(p, src) if p != src else "/"
            if '"' in g or "\n" in g:
                raise hb.ManifestError("root: a file name with a quote or newline (%r) cannot be re-owned" % g)
            lines += ['sif "%s" uid 0' % g, 'sif "%s" gid 0' % g]
        if not lines:
            return
        dbg = os.path.join(os.path.dirname(T["mke2fs"]), "debugfs")
        cmdf = out + ".owners"
        with open(cmdf, "w") as f:
            f.write("\n".join(lines) + "\n")
        r = subprocess.run([T["faketime"], hb.FAKE_TS, dbg, "-w", "-f", cmdf, out], env=hb.faketime_env(),
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        errs = [x for x in r.stderr.splitlines() if x.strip() and not x.startswith("debugfs ")]
        os.unlink(cmdf)
        if r.returncode != 0 or errs:
            die("setting the root image's owners to root failed:\n  " + "\n  ".join(errs[:10]))
    hb.mkfs = mkfs


def build_linux(a):
    """hydo build on the Linux release: hydo-build with the bundled host tools (the Rust CLI set
    the HYDO_* tool variables, PATH and the umask's 022 is set here) and the two release patches."""
    R = a["root"]
    os.umask(0o022)     # the staged root's new directories and text files get the same modes for everyone
    _onboard()
    hb = load_hydo_build()
    use_bundled_store(hb, os.path.join(R, "guest"))
    own_by_root(hb)
    sys.argv = ["hydo-build"] + list(a["argv"])
    return hb.main()


def mac_mkfs(hb, R):
    """A replacement for hydo-build's mkfs(): the same mke2fs options, run by a static arm64
    mke2fs inside a helper VM (hvm) on a raw disk file, instead of the host's mke2fs under
    libfaketime. The guest clock is deterministic, and hydo-build zeroes every time field
    afterwards anyway, so the image is a pure function of the options."""
    import hydo_build_arm64 as a64
    GUEST = os.path.join(R, "guest")
    HVM = os.path.join(R, "hv", "hvm", "hvm")
    KERNEL = os.path.join(R, "hv", "hvm", "explore", "out", "Image-explore")

    def mkfs(T, out, mib, uuid, seed, src=None, journal=False):
        if src:
            die("internal: populating a filesystem (mke2fs -d) is not supported on the Mac path")
        with open(out, "wb") as f:
            f.truncate(mib * 1024 * 1024)
        feats = "^metadata_csum,^64bit,^flex_bg,^resize_inode" + (",has_journal" if journal else "")
        args = ["/bin/mke2fs", "-q", "-F", "-t", "ext4", "-b", "4096", "-O", feats]
        if journal:
            args += ["-J", "size=16"]
        args += ["-U", uuid, "-E", "hash_seed=%s,lazy_itable_init=0,lazy_journal_init=0" % seed, "/dev/vda"]
        tmp = tempfile.mkdtemp(prefix="hydo-mkfs-")
        try:
            root = os.path.join(tmp, "root")
            for d in ("bin", "dev", "proc", "sys", "etc"):
                os.makedirs(os.path.join(root, d))
            shutil.copy(os.path.join(GUEST, "busybox"), os.path.join(root, "bin", "busybox"))
            shutil.copy(os.path.join(GUEST, "mke2fs"), os.path.join(root, "bin", "mke2fs"))
            os.symlink("busybox", os.path.join(root, "bin", "sh"))
            with open(os.path.join(root, "init"), "w") as f:
                f.write("#!/bin/busybox sh\n"
                        "/bin/busybox mount -t devtmpfs none /dev\n"
                        "/bin/busybox mount -t proc none /proc\n"
                        "n=0; while [ ! -b /dev/vda ] && [ $n -lt 300 ]; do /bin/busybox usleep 10000; n=$((n+1)); done\n"
                        "if %s; then /bin/busybox sync; echo HYDO-MKFS-OK; else echo HYDO-MKFS-FAIL; fi\n"
                        "echo HYDO-MKFS-DONE\n"
                        "while :; do /bin/busybox sleep 3600; done\n" % " ".join("'%s'" % x for x in args))
            for p in ("init", "bin/busybox", "bin/mke2fs"):
                os.chmod(os.path.join(root, p), 0o755)
            initrd = os.path.join(tmp, "mkfs.cpio.gz")
            a64._cpio(root, initrd, hb.FIXED_MTIME)
            env = dict(os.environ, HVM_DISK=os.path.abspath(out), HVM_DISK_OUT=os.path.abspath(out))
            p = subprocess.run([HVM, "run", KERNEL, initrd, "HYDO-MKFS-DONE"], env=env,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            con = p.stdout.decode(errors="replace")
            if "HYDO-MKFS-OK" not in con:
                sys.stderr.write(con[-3000:])
                die("formatting the data disk in the helper VM failed (output above)")
            # mke2fs records whether `char` is signed on the machine that ran it (superblock
            # s_flags: 1 = signed directory hash, x86; 2 = unsigned, arm64). Record the x86
            # value, as a Linux build of the same manifest does, so both give the same bytes.
            # The filesystem is still empty (no hashed directories), so this changes nothing else.
            with open(out, "r+b") as f:
                f.seek(1024 + 0x160)
                fl = int.from_bytes(f.read(4), "little")
                f.seek(1024 + 0x160)
                f.write(((fl & ~0x3) | 0x1).to_bytes(4, "little"))
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
    return mkfs


def use_bundled_tc(R):
    """hydo_build_arm64 puts the pinned aarch64 tc (TC_ARM64, a /nix/store path) and the files
    its loader opens into the root, reading them from the host's /nix/store. The release ships
    exactly those files under guest/tc/<the same absolute paths> (guest/tc-closure.txt). Point
    the module's existence check and its copy at that tree; the image gets the same paths, the
    same bytes and the same modes as a Linux build. Nothing else in the module is affected."""
    import hydo_build_arm64 as a64
    GUEST = os.path.join(R, "guest")
    top = os.path.join(GUEST, "tc")
    listing = [l.strip() for l in open(os.path.join(GUEST, "tc-closure.txt")) if l.strip()]
    here = lambda p: os.path.join(top, p.lstrip("/"))

    real_isfile = os.path.isfile
    path = types.ModuleType("os.path")
    path.__dict__.update(os.path.__dict__)
    path.isfile = lambda p: (real_isfile(here(p)) if str(p).startswith("/nix/store/") and os.path.lexists(here(p))
                             else real_isfile(p))
    shim = types.ModuleType("os")
    shim.__dict__.update(os.__dict__)
    shim.path = path
    a64.os = shim                                   # only hydo_build_arm64's own view of os

    def copy_closure(binary, R):                    # the same rules as a64._copy_elf_closure
        for p in sorted(set(listing + [binary])):
            dst = os.path.join(R, p.lstrip("/"))
            if os.path.lexists(dst):
                continue
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            src = here(p)
            if os.path.islink(src):
                os.symlink(os.readlink(src), dst)
            else:
                shutil.copy(src, dst)
                os.chmod(dst, 0o755 if os.access(src, os.X_OK) else 0o644)
    a64._copy_elf_closure = copy_closure


def build_mac(a):
    """hydo build on the Mac release: hydo-build --arch arm64 with mke2fs in a helper VM and the
    bundled aarch64 tc closure (the Rust CLI set the HYDO_* tool variables)."""
    R = a["root"]
    _onboard()
    hb = load_hydo_build()
    hb.mkfs = mac_mkfs(hb, R)
    use_bundled_tc(R)
    sys.argv = ["hydo-build"] + list(a["argv"])
    return hb.main()


FUNCS = {f.__name__: f for f in (build_linux, build_mac)}


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in FUNCS:
        print("usage: hydo_bridge.py <%s> <request.json>" % "|".join(sorted(FUNCS)), file=sys.stderr)
        return 2
    req = json.load(open(sys.argv[2]))
    out = FUNCS[sys.argv[1]](req.get("args") or {})
    rc, data = out if isinstance(out, tuple) else (out, None)
    if req.get("result"):
        with open(req["result"], "w") as f:
            json.dump(data, f)
    sys.stdout.flush()
    return rc or 0


if __name__ == "__main__":
    sys.exit(main())
