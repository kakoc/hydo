#!/bin/sh
# The final read of the messages check (hydo.yaml, checks: rp): every partition from the start
# with read_committed (the group -), then the consumer group g from its committed offsets. Each
# waits until every leader answers, no transaction is open and nothing arrives for 8 s; it gives
# up after 150 s and 230 s of guest uptime (below workload.timeout).
. /hydo/workload/helper_lib.sh
exec $RP drain
