#!/bin/sh
# Measure both findings on a fresh stack, then remove it:
#   sh measure.sh [blnk-binary]     # default work/blnk
# - 20 bursts of 5 draws of 30.00 at once against overdraft_limit 50.00, each on a new balance:
#   how many bursts end below -50.00 (-5000 in cents)
# - 20 atomic bulks whose second item has a used reference: how many leave the first applied
# RUNS changes the 20.
cd "$(dirname "$0")"
RUNS=${RUNS:-20}
trap 'sh stack.sh down >/dev/null' EXIT INT TERM
sh stack.sh up "${1:-work/blnk}" >/dev/null || exit 1
over=0 worst=0
i=1
while [ $i -le "$RUNS" ]; do
  b=$(MODE=burst N=5 sh probe-limit.sh | sed -n 's/^{"balance":\(-*[0-9]*\),.*/\1/p')
  [ -n "$b" ] || { echo "measure.sh: no balance in burst $i" >&2; exit 1; }
  [ "$b" -lt -5000 ] && over=$((over + 1))
  [ "$b" -lt "$worst" ] && worst=$b
  i=$((i + 1))
done
echo "overdraft limit: $over of $RUNS bursts end below -5000 (lowest balance $worst)"
partial=0
i=1
while [ $i -le "$RUNS" ]; do
  sh probe-bulk.sh | grep -q 'status APPLIED' && partial=$((partial + 1))
  i=$((i + 1))
done
echo "atomic bulk: $partial of $RUNS leave the first transfer applied after the answer said nothing was applied"
