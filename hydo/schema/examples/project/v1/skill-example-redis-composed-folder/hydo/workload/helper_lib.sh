# sourced by the commands (helper_* files are not commands)
R() { timeout 2 /bin/redis-cli "$@" 2>/dev/null; }   # a 2 s client deadline
pick() {   # from $HYDO_SEED (the composer's per-command seed): two distinct accounts and an amount
  A=$((HYDO_SEED % ACCOUNTS)); B=$(((HYDO_SEED / ACCOUNTS) % (ACCOUNTS - 1)))
  [ $B -ge $A ] && B=$((B + 1))
  AMT=$(((HYDO_SEED / 100) % 5 + 1))
}
