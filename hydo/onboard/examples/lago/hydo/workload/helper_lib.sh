# Shared by the workload commands and checks (sourced, never run).
# C: the test's Lago client, run with the API image's own Ruby.
C() { hydo-oci api ruby /hydo/workload/helper_client.rb "$@"; }
# Q: one query on Lago's PostgreSQL with psql from its own image, one row per line.
Q() { timeout 30 hydo-oci db env PGPASSWORD=changeme psql -h 10.89.0.11 -U lago -d lago -Atq -c "$1"; }

# record <outcome> <id>...: the client's view of each operation. An id is
# "<op>,<key>,<amount>": which total the operation adds to, and by how much, so that the
# business-result check needs nothing but these lines (checks/final_business_result).
record() {
  o=$1; shift
  for id in "$@"; do
    echo "$id $o" >> "$HYDO_OUTCOMES"
    [ "$o" = ack ] && echo "$id" >> "$HYDO_ACKED"
  done
  return 0
}

# acked, outcomes: the client's records of the whole test, the seed's (first_seed, on the data
# disk) and the run's ($HYDO_ACKED, $HYDO_OUTCOMES), seed first.
acked() { cat /data/seed/acked "$HYDO_ACKED" 2>/dev/null; }
outcomes() { cat /data/seed/outcomes "$HYDO_OUTCOMES" 2>/dev/null; }
