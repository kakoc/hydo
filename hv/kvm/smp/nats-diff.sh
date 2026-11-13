#!/bin/sh
# Where two runs of one schedule on several vCPUs differ (docs/design/kvm-vcpus.md, "Stage 4"):
# nats-server's flaky test (hydo/onboard/examples/nats-flaky-8363) on 2 vCPUs at q=12, whose
# fingerprint differed between a host's own runs in Milestone 219.
#
#   HYDO=<hydo of this tree> REL=<unpacked Linux release> SMP_KERNEL=<bzImage-kvm-smp> \
#     [VMLINUX=<its vmlinux>] [OUT=<dir>] [T=<seconds>] [Q=12] sh hv/kvm/smp/nats-diff.sh
#
# 1. two runs with the engine's diagnostics and RAM dumps (HYDO_KVM_DIAG, HYDO_KVM_DIAG_RAM):
#    every 4 KiB page that differs, its differing bytes, and the kernel symbols of differing
#    words (with VMLINUX);
# 2. three runs each with fewer looks of the spin observer (HYDO_KVM_SPIN_MS=2000), without its
#    full-state cycles (HYDO_KVM_SPIN_STATE=0), and with a trace of every exit (HYDO_KVM_TRACE).
# Every wait has a time limit. It writes only under OUT.
set -u
HYDO=${HYDO:?}
REL=${REL:?}
SMP_KERNEL=${SMP_KERNEL:?}
OUT=${OUT:-$PWD/nats-diff}
T=${T:-900}
Q=${Q:-12}
here=$(cd "$(dirname "$0")" && pwd)
ex=$(cd "$here/../../../hydo/onboard/examples/nats-flaky-8363" && pwd)
mkdir -p "$OUT"
cd "$OUT" || exit 2
LOG=$OUT/nats-diff.log
: > "$LOG"
say() { echo "$*" | tee -a "$LOG"; }
say "nats-diff: $(date -u) on $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
cp -f "$HYDO" "$REL/bin/hydo-nats-diff" || exit 2
H="$REL/bin/hydo-nats-diff"
mkdir -p "$REL/guest" "$REL/hv/kvm/linux/out"
cp -f "$SMP_KERNEL" "$REL/guest/bzImage-kvm-smp"
cp -f "$SMP_KERNEL" "$REL/hv/kvm/linux/out/bzImage-kvm-smp"
mkdir -p "$OUT/ex" && cp -R "$ex/." "$OUT/ex/"
(cd "$OUT/ex" && timeout "$T" sh build-test.sh >> "$LOG" 2>&1) || { say "build-test.sh failed"; exit 1; }
(cd "$OUT/ex" && timeout "$T" "$H" build hydo.yaml >> "$LOG" 2>&1)
IMG=$(ls "$OUT"/ex/hydo-out/*-disk.img | head -1)
S="disk=none q=$Q | run"

run() { # <dir> [VAR=value...]: one run, its fingerprint
  d=$1; shift
  rm -rf "$d"
  env HYDO_KVM_DIAG=1 "$@" timeout "$T" "$H" run --backend kvm --image "$IMG" --out "$d" --vcpus 2 "$S" > "$d.txt" 2>&1
  grep -m1 '^RESULT' "$d.txt" | sed -n 's/.* fp=\([0-9a-f]*\) .*/\1/p'
}

# 1. two runs with RAM dumps
a=$(run "$OUT/ram-a" HYDO_KVM_DIAG_RAM=4096)
b=$(run "$OUT/ram-b" HYDO_KVM_DIAG_RAM=4096)
say "1. $S with RAM dumps: $a $b"
for ph in w f; do
  ra=$(find "$OUT/ram-a" -name "*.$ph.ram" | head -1)
  rb=$(find "$OUT/ram-b" -name "*.$ph.ram" | head -1)
  [ -n "$ra" ] && [ -n "$rb" ] || { say "  phase $ph: no RAM dump"; continue; }
  python3 -I - "$ra" "$rb" "$ph" "${VMLINUX:-}" <<'PY' | tee -a "$LOG"
import bisect, subprocess, sys
a, b, ph, vml = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
syms = []
if vml:
    for line in subprocess.run(["nm", "-n", vml], capture_output=True, text=True).stdout.split("\n"):
        p = line.split()
        if len(p) == 3:
            syms.append((int(p[0], 16), p[2]))
addrs = [s[0] for s in syms]
def sym(v):
    i = bisect.bisect_right(addrs, v) - 1
    return f"{syms[i][1]}+{v - syms[i][0]:#x}" if i >= 0 and v - syms[i][0] < 0x100000 else ""
fa, fb = open(a, "rb"), open(b, "rb")
page, pages = 0, []
while True:
    x, y = fa.read(4096), fb.read(4096)
    if not x and not y:
        break
    if x != y:
        pages.append((page, x, y))
    page += 1
print(f"  phase {ph}: {len(pages)} pages differ")
for p, x, y in pages[:24]:
    words = [i for i in range(0, 4096, 8) if x[i:i + 8] != y[i:i + 8]]
    print(f"  page {p:#x} (gpa {p * 4096:#x}): {len(words)} words differ")
    for i in words[:6]:
        u, v = int.from_bytes(x[i:i + 8], "little"), int.from_bytes(y[i:i + 8], "little")
        # the kernel image is mapped at 0xffffffff80000000 + its physical address (no KASLR)
        at = sym(0xFFFFFFFF80000000 + p * 4096 + i)
        print(f"    +{i:#05x} [{at}]: {u:#018x} {v:#018x} xor {u ^ v:#x} {sym(u)} {sym(v)}")
PY
done

# 2. three runs of each variant
for v in "HYDO_KVM_SPIN_MS=2000" "HYDO_KVM_SPIN_STATE=0" "HYDO_KVM_TRACE=TRACE"; do
  fps=""
  k=1
  while [ "$k" -le 3 ]; do
    d="$OUT/v-$(echo "$v" | cut -d= -f1)-$k"
    case "$v" in
      HYDO_KVM_TRACE=*) fp=$(run "$d" "HYDO_KVM_TRACE=$d.trace") ;;
      *) fp=$(run "$d" "$v") ;;
    esac
    fps="$fps ${fp:-none}"
    k=$((k + 1))
  done
  say "2. $v: $S:$fps"
done
t1="$OUT/v-HYDO_KVM_TRACE-1.trace"; t2="$OUT/v-HYDO_KVM_TRACE-2.trace"
if [ -f "$t1" ] && [ -f "$t2" ]; then
  say "   the traces' first difference (run 1 / run 2):"
  diff "$t1" "$t2" | head -12 | tee -a "$LOG"
  grep -h 'STATE-CYCLE\|REWIND' "$t1" | head -20 | sed 's/^/   run 1: /' | tee -a "$LOG"
  grep -h 'STATE-CYCLE\|REWIND' "$t2" | head -20 | sed 's/^/   run 2: /' | tee -a "$LOG"
fi
say "nats-diff: done ($(date -u))"
