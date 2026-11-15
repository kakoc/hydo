# Shared by the workload commands and checks (sourced, never run).
# C: the test's Saleor client (helper_client.py), run with the API image's own Python. It reaches
# the API by its Compose name (Saleor answers only the host names in ALLOWED_HOSTS: an IP
# address gets 400 Bad Request), as a storefront or app would.
C() { hydo-oci api env SALEOR_URL=http://api:8000/graphql/ python3 /hydo/workload/helper_client.py "$@"; }
# Q: one query on Saleor's PostgreSQL, one row per line (workload/helper_q).
Q() { /hydo/workload/helper_q "$1"; }

# record <outcome> <id>...: the client's view of each operation (hydo guide composer).
# A checkout's id is "co,<checkout token>,<total>"; a refund's is "rf,<order>,<app|gift>,<amount>".
record() {
  o=$1; shift
  for id in "$@"; do
    echo "$id $o" >> "$HYDO_OUTCOMES"
    [ "$o" = ack ] && echo "$id" >> "$HYDO_ACKED"
  done
  return 0
}
acked() { cat "$HYDO_ACKED" 2>/dev/null; }
outcomes() { cat "$HYDO_OUTCOMES" 2>/dev/null; }
