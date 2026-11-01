#!/usr/bin/env bash
# hydo/bench/kafka-diag.sh -- a diagnostic copy of a benchmark manifest image (Milestone 133,
# docs/C2-BENCH.md section 6 item 4): the copy's manifest gets one more final invariant,
# `diag.dump`, which always passes and prints, with the prefix `DIAG`, on the guest console:
# the client's outcomes and acknowledged ids, MySQL's `published` flags, every Kafka record batch
# of the `orders` topic with its producer id, epoch and sequence numbers (Kafka's own
# DumpLogSegments), Kafka's WARN/ERROR lines, the workload's log and the order service's log.
# It runs after the two real invariants, so their results do not change (the fingerprint does).
#
#   kafka-diag.sh <image> <out image>       (shell and agent images alike: both manifest files)
#
# Both /etc/hydo/hydo.yaml (an agent image's plan, read by the host) and /etc/hydo/manifest.sh
# (a shell image's plan) are rewritten with debugfs in the copy's root file system.
set -euo pipefail
img=$1; dst=$2
OFF=1048576
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
for f in hydo.yaml manifest.sh; do
  debugfs -R "dump /etc/hydo/$f $w/$f" "$img?offset=$OFF" > /dev/null 2>&1
  [ -s "$w/$f" ] || { echo "kafka-diag: no /etc/hydo/$f in $img" >&2; exit 1; }
done
cat > "$w/diag.sh" <<'EOF'
# M133 diagnostic: dump the run's evidence to the console (prefix DIAG), always passes
{
echo "DIAG outcomes: $(tr "\n" " " < /tmp/hydo/outcomes)"
echo "DIAG acked: $(tr "\n" " " < /tmp/hydo/acked)"
echo "DIAG mysql: $(SQL -e "SELECT id, published FROM stack.orders ORDER BY id" | tr "\t\n" ": ")"
for f in /data/kafka-logs/orders-0/*.log; do
  $JAVA $JCOMMON -Xms64m -Xmx64m -cp "/opt/kafka/libs/*" kafka.tools.DumpLogSegments --print-data-log --files $f 2>&1 |
    awk "{ delete v; for (i = 1; i < NF; i++) v[\$i] = \$(i + 1) }
      /^baseOffset/ { printf \"DIAG kb off=%s n=%s seq=%s-%s pid=%s ep=%s ct=%s\n\", v[\"baseOffset:\"], v[\"count:\"], v[\"baseSequence:\"], v[\"lastSequence:\"], v[\"producerId:\"], v[\"producerEpoch:\"], v[\"CreateTime:\"] }
      /^\| offset/ { printf \"DIAG kr off=%s key=%s val=%s\n\", v[\"offset:\"], v[\"key:\"], v[\"payload:\"] }
      !/^baseOffset|^\| offset|^Dumping|^Log starting/ { print \"DIAG kx \" substr(\$0, 1, 160) }"
done
for f in /data/kafka-logs/orders-0/*.snapshot; do echo "DIAG ksnap $f"; done
grep -a "WARN\|ERROR" /tmp/hydo/kafka.log | tail -n 30 | cut -c1-200 | sed "s/^/DIAG klog: /"
tail -n 80 /tmp/hydo/workload.log | cut -c1-240 | sed "s/^/DIAG wl: /"
grep -av "HYDO-ASSERT" /tmp/hydo/front.log | tail -n 20 | cut -c1-200 | sed "s/^/DIAG front: /"
} > /dev/ttyS0 2>&1
echo "diag dumped"
exit 0
EOF
python3 -I - "$w" <<'EOF'
import os, sys
d = sys.argv[1]
diag = open(os.path.join(d, "diag.sh")).read().rstrip("\n").split("\n")
y = open(os.path.join(d, "hydo.yaml")).read()
marker = "\nfaults: ["
assert y.count(marker) == 1, "hydo.yaml: no single faults: line after the invariants"
i = y.index(marker)
inv = "  - id: diag.dump\n    msg: M133 diagnostic dump (always passes)\n    final: true\n    cmd: |\n"
inv += "".join("      " + l + "\n" for l in diag)
open(os.path.join(d, "hydo.yaml"), "w").write(y[:i].rstrip("\n") + "\n" + inv + "\n" + y[i + 1:])
m = open(os.path.join(d, "manifest.sh")).read()
old = "HYDO_INVARIANTS='i0 i1'\n"
assert m.count(old) == 1, "manifest.sh: the invariants are not i0 i1"
q = "'" + "\n".join(diag).replace("'", "'\\''") + "\n'"
add = ("HYDO_INV_i2_id='diag.dump'\nHYDO_INV_i2_type='always'\n"
       "HYDO_INV_i2_msg='M133 diagnostic dump (always passes)'\n"
       "HYDO_INV_i2_cmd=" + q + "\nHYDO_INV_i2_every=''\nHYDO_INV_i2_final=1\n")
open(os.path.join(d, "manifest.sh"), "w").write(m.replace(old, add + "HYDO_INVARIANTS='i0 i1 i2'\n"))
EOF
sh -n "$w/manifest.sh"
cp --sparse=always "$img" "$dst.tmp"
for f in hydo.yaml manifest.sh; do
  printf 'rm /etc/hydo/%s\nwrite %s /etc/hydo/%s\nset_inode_field /etc/hydo/%s mode 0100644\nset_inode_field /etc/hydo/%s uid 0\nset_inode_field /etc/hydo/%s gid 0\n' \
    "$f" "$w/$f" "$f" "$f" "$f" "$f"
done | debugfs -w -f - "$dst.tmp?offset=$OFF" > /dev/null 2>&1
e2fsck -fn "$dst.tmp?offset=$OFF" > /dev/null 2>&1 || { echo "kafka-diag: e2fsck -fn found problems in $dst.tmp" >&2; exit 1; }
for f in hydo.yaml manifest.sh; do
  debugfs -R "dump /etc/hydo/$f $w/$f.chk" "$dst.tmp?offset=$OFF" > /dev/null 2>&1
  cmp -s "$w/$f" "$w/$f.chk" || { echo "kafka-diag: /etc/hydo/$f was not written" >&2; exit 1; }
done
mv "$dst.tmp" "$dst"
echo "kafka-diag: $dst (diag.dump added to hydo.yaml and manifest.sh)"
