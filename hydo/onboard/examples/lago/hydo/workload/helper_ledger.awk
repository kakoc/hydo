# The business-result rule, written without any knowledge of Lago (checks/final_business_result).
#
# Input 1: the client's outcome lines "<op>,<key>,<amount> ack|failed|unknown" ($HYDO_OUTCOMES;
#          the last line of an operation wins).
# Input 2: what the system reports, "<key> <amount>" per line.
#
# For every key, the reported total must be the acknowledged operations' sum plus the amounts of
# some set of operations that got no answer (each counted at most once). Failed operations count
# nothing. Exit 0 when every key agrees, 1 otherwise.
FNR == NR { last[$1] = $2; next }
NF >= 2 { obs[$1] = $2 + 0; keys[$1] = 1 }
END {
  for (id in last) {
    n = split(id, p, ",")
    if (n < 3) continue
    k = p[2]; a = p[3] + 0; keys[k] = 1
    if (last[id] == "ack") { acked[k] += a; nack[k]++ }
    else if (last[id] == "unknown") { nu[k]++; u[k, nu[k]] = a; umax[k] += a }
  }
  missing = 0; over = 0; nk = 0
  for (k in keys) {
    nk++
    d = obs[k] - acked[k]
    if (d < 0) {
      printf "%s: reported %s, acknowledged %s (%d operations): %s acknowledged units missing\n", k, obs[k] + 0, acked[k] + 0, nack[k], -d
      missing++
      continue
    }
    if (d == 0) continue
    # d must be the sum of a subset of the unanswered operations' amounts
    split("", r); r[0] = 1
    for (i = 1; i <= nu[k]; i++) {
      a = u[k, i]
      for (s = umax[k]; s >= a; s--) if ((s - a) in r) r[s] = 1
    }
    if (!(d in r)) {
      printf "%s: reported %s, acknowledged %s, unanswered %s in %d operations: %s units more than any set of unanswered operations\n", k, obs[k] + 0, acked[k] + 0, umax[k] + 0, nu[k], d
      over++
    }
  }
  if (missing && over) { print missing " totals lack acknowledged units and " over " totals count units twice"; exit 1 }
  if (missing) { print missing " totals lack acknowledged units"; exit 1 }
  if (over) { print over " totals count units more than once"; exit 1 }
  print "all " nk " totals equal the acknowledged operations"
}
