#!/bin/sh
# start.sh: start one Garnet node of the cluster, with its data and its cluster configuration in
# /data/garnet (the node's own volume).
#
# The durability settings come from the environment, the build parameters of hydo.yaml:
#   GARNET_COMMIT_WAIT  yes: --aof-commit-wait, a write is answered after its AOF commit is on
#                       the disk; no: Garnet's default, the answer does not wait for the commit
#   GARNET_COMMIT_FREQ  --aof-commit-freq in ms: 0 (Garnet's default) commits after every write
#   GARNET_CKPT_FREQ    --checkpoint-freq in s: 0 (Garnet's default) takes no checkpoint by itself
#   GARNET_DEVICE       --device-type: Default (Garnet's default) or FileStream, which opens the
#                       AOF and the checkpoint files with O_SYNC (microsoft/garnet#2281)
#   GARNET_REEST        --cluster-replication-reestablishment-timeout in s: 0 (Garnet's default)
#                       never attaches a replica to its primary again by itself (RESULTS.md,
#                       finding 1); 5 tries every 5 s
# GARNET_ADDR is the node's own address, which it binds and announces to the cluster.
#
# The memory settings are small, so that six nodes fit in one guest: a hash index of 1 MiB, a log
# of 64 MiB in pages of 1 MiB, and an AOF buffer of 8 MiB in pages of 2 MiB.
set -eu
d=/data/garnet
mkdir -p "$d"
set -- --cluster --bind "$GARNET_ADDR" --port 6379 \
  --checkpointdir "$d" --recover \
  --aof --aof-commit-freq "${GARNET_COMMIT_FREQ:-0}" --checkpoint-freq "${GARNET_CKPT_FREQ:-0}" \
  --device-type "${GARNET_DEVICE:-Default}" \
  --cluster-replication-reestablishment-timeout "${GARNET_REEST:-0}" \
  --lua \
  --index 1m --memory 64m --page 1m --aof-memory 8m --aof-page-size 2m \
  --minthreads 8
if [ "${GARNET_COMMIT_WAIT:-yes}" = yes ]; then
  set -- "$@" --aof-commit-wait
fi
exec /app/GarnetServer "$@"
