#!/bin/sh
# hydo: history=rg model=cas
# hydo-msg: the SQL registers are linearizable (no lost write, no stale read on a primary)
# A replica's reads are not recorded (they may be stale); its writes are.
# The final read-back: every register on n1. hand: tried up to 10 times, 3 s apart (as in
# list_append_sql.la).
i=0
while :; do
  hydo-sql register read-all n1 9997 && exit 0
  i=$((i + 1)); [ "$i" -lt 10 ] || exit 2
  sleep 3
done
