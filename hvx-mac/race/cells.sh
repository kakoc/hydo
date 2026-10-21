#!/bin/sh
# cells.sh <variant> <strategy> <first> <last>: one boot per seed, 4 at a time (one.sh)
#   strategies: none; jitter (every tick delayed 1..1000 exits); pct<m> (m forced preemption
#   points at seeded exits over the race, exits 14327..34800)
cd "$(dirname "$0")"
seq $3 $4 | xargs -P 4 -n 1 ./one.sh $1 $2
