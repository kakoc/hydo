# Shared by the checks.
#
# Q "<sql>": one query against the shop's database, unaligned, one row per line (fields separated
# by |); fails when PostgreSQL does not answer (a check then exits 2: cannot evaluate now).
Q() {
  timeout 30 hydo-oci postgres psql -X -q -At -U postgres -d medusa-store -v ON_ERROR_STOP=1 -c "$1"
}

# W <route>: the refs of the requests to the fake Stripe's <route> that took effect (fx=1), one per
# line, from hydo-world's record (/tmp/hydo/world.log; both boots after a power cut). For the
# intent routes the ref is the PaymentIntent id (pi_<seq>), for refunds the intent refunded.
# W_ALL <route>: every request to the route, effect or not (reads have no effect).
W() {
  awk -v r="$1" '{ e = ""; f = ""; x = ""; for (i = 1; i <= NF; i++) { split($i, kv, "=");
      if (kv[1] == "route") e = kv[2]; else if (kv[1] == "ref") f = kv[2]; else if (kv[1] == "fx") x = kv[2] }
    if (e == r && x == "1") print f }' /tmp/hydo/world.log 2>/dev/null
}
W_ALL() {
  awk -v r="$1" '$0 ~ /kind=req/ { e = ""; f = ""; for (i = 1; i <= NF; i++) { split($i, kv, "=");
      if (kv[1] == "route") e = kv[2]; else if (kv[1] == "ref") f = kv[2] }
    if (e == r) print f }' /tmp/hydo/world.log 2>/dev/null
}

# The intent a create record made: the fake answers pi_<seq> (the record's own seq number).
W_CREATED() {
  awk '{ e = ""; s = ""; x = ""; for (i = 1; i <= NF; i++) { split($i, kv, "=");
      if (kv[1] == "route") e = kv[2]; else if (kv[1] == "seq") s = kv[2]; else if (kv[1] == "fx") x = kv[2] }
    if (e == "intent_create" && x == "1") print "pi_" s }' /tmp/hydo/world.log 2>/dev/null
}

# minus A B: the lines of file A that are not in file B (awk, not grep -f: busybox grep -f with an
# empty pattern file matches every line)
minus() {
  awk 'FILENAME == ARGV[1] { seen[$0] = 1; next } $0 != "" && !($0 in seen) { print }' "$2" "$1"
}
