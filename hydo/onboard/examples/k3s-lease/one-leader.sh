#!/bin/sh
# The rule "one leader at a time" over the pods' logs in /data/elect (elect.sh writes them).
# A pod that leads appends `L <uptime in centiseconds>` every half second. A run of L lines of
# one pod, not broken by a step-down (D) or a new start of the pod (S), is a time span in which
# that pod acted as leader. Two pods' spans that share an instant break the rule.
# Exit 0: the rule holds; 1: it is broken (the last line names the first two spans that
# overlap); 2: no logs yet.
cd /data/elect 2>/dev/null || exit 2
ls ./*.log > /dev/null 2>&1 || exit 2
awk '
FNR == 1 { pod = FILENAME; sub(/^\.\//, "", pod); sub(/\.log$/, "", pod); open = 0 }
$1 == "L" { t = $2 + 0; if (!open) { n++; P[n] = pod; S[n] = t; open = 1 } E[n] = t; next }
$1 == "D" || $1 == "S" { open = 0 }
END {
  bad = 0
  for (i = 1; i <= n; i++) for (j = i + 1; j <= n; j++)
    if (P[i] != P[j] && S[i] <= E[j] && S[j] <= E[i]) {
      if (!bad) first = sprintf("%s led from %.2f s to %.2f s and %s from %.2f s to %.2f s (guest uptime)", P[i], S[i] / 100, E[i] / 100, P[j], S[j] / 100, E[j] / 100)
      bad++
    }
  if (bad) { printf "%d overlapping pairs of leader spans; %s\n", bad, first; exit 1 }
  printf "%d leader spans, none overlap\n", n
}' ./*.log
