# ---------------------------------------------------------------- error logs as findings (hydo-init-errlog.sh)
# errors: in hydo.yaml (hvx/onboard/MANIFEST.md, "Error logs as findings"). hydo-build inserts this
# file, plus renamed copies _el_orig_<fn> of the six init functions wrapped below, into /sbin/init
# just before "main" -- ONLY for a manifest with errors:. Every other image keeps its init byte for
# byte.
#
# Each covered service has log STREAMS: 0 = its stdout/stderr log (start_svc moves <svc>.log into
# <svc>.log.old on a restart, so the two together are one append-only stream), 1 = the output of its
# setup: command, 2.. = the files listed in its errors.files (for example MySQL's log-error file).
#
#  - el_mark <label> records every stream's line count when a workload range starts (label = the
#    range's first step) and when the end phase starts (label "end": healing, retry, final
#    invariants). A line therefore belongs to "s" (before the workload: boot, provisioning), to the
#    step range it was written in, to "end", or -- in the recovery boot after a power cut -- to "r".
#    Ranges are split at every fault boundary, so the host can tell whether a line was written
#    while a fault was on (hvx/onboard/hydo_errors.py, known-errors `during:`).
#  - el_report, once per boot, just before the verdict line (at a power cut: before the frozen
#    state is handed over, after the services are stopped), selects each stream's error lines
#    (HYDO_ERR_<svc>_level / _exc / _ignore), reduces each to a template (timestamps, numbers, ids,
#    hex, ephemeral ports replaced), and prints one record per (service, template) with its count,
#    its labels and its first line, at most HYDO_ERR_MAX of them, then one summary record:
#        HYDO-ERRLOG {"svc":..,"src":..,"n":..,"at":"s,3,end","tpl":..,"ex":..}
#        HYDO-ERRLOG-SUM {"boot":"first|recovery","steps":N,"lines":..,"kinds":..,"shown":..}
# The host turns each template into a signature and compares it with the project's known-errors
# list; the guest never sees that list, so accepting an error kind changes no image and no
# fingerprint. Text in the records has no backslash, double quote or control character (they are
# replaced by / ' and space) and no "hydo-"/"HYDO-" (written hydo_/HYDO_), so a service's line can
# never look like a harness marker.

ev() { eval "printf '%s' \"\${HYDO_ERR_${1}_$2}\""; }      # ev <svc> <field>
el_nfiles() { set -- $(ev $1 files); echo $#; }
el_file() { local k=$2; set -- $(ev $1 files); shift $((k - 2)); printf '%s' "$1"; }   # el_file <svc> <k>=2..
el_cat() {   # <svc> <k>: the stream's text
  case $2 in
    0) $BB cat $H/$1.log.old $H/$1.log 2>/dev/null;;
    1) $BB cat $H/$1.setup.log 2>/dev/null;;
    *) $BB cat "$(el_file $1 $2)" 2>/dev/null;;
  esac
}
el_streams() { local n; n=$(el_nfiles $1); echo 0 1; local k=2; while [ $k -lt $((n + 2)) ]; do echo $k; k=$((k + 1)); done; }
el_mark() {   # <label>: every covered stream's line count, under the label of what starts now
  local s k
  for s in $HYDO_ERR_SERVICES; do
    for k in $(el_streams $s); do echo "$1 $(el_cat $s $k | $BB wc -l)" >> $H/el.m.$s.$k; done
  done
}

# the selection and the template (busybox awk; patterns come from the environment, not -v, so
# backslashes reach the regex engine as written)
EL_SCAN='
function clean(s) { gsub(/\\/, "/", s); gsub(/"/, Q, s); gsub(/[[:cntrl:]]/, " ", s)
                    gsub(/hydo-/, "hydo_", s); gsub(/HYDO-/, "HYDO_", s); return s }
function ishost(t,  i) { if (t == "localhost") return 1; for (i in HOST) if (HOST[i] == t) return 1; return 0 }
function nums(s,   out, tok, orig, pre, keep, prev, n) {
  out = ""; prev = ""
  while (match(s, /[0-9A-Za-z]+/)) {
    pre = substr(s, 1, RSTART - 1); orig = tok = substr(s, RSTART, RLENGTH); s = substr(s, RSTART + RLENGTH)
    out = out pre
    if (tok ~ /^[0-9]+$/) {
      keep = 0; n = tok + 0
      if (pre == "-" && prev ~ /^[A-Z][A-Z]+$/) keep = 1                       # MY-012592, ORA-00942
      else if (pre ~ /^[ :=]+$/ && prev ~ /^(errno|Errno|ERRNO|code|Code|CODE|error|Error|ERROR|status|Status|STATUS)$/) keep = 1
      else if (pre == ":" && length(tok) <= 5 && n < 32768 && (out ~ /#\.#\.#\.#:$/ || out ~ /\]:$/ || ishost(prev))) keep = 1
      if (!keep) tok = "#"                                                       # (a port below the ephemeral range)
    } else if (tok ~ /[0-9]/) {
      if (length(tok) >= 8 && tok ~ /^[0-9a-fA-F]+$/) tok = "<hex>"
      else if (length(tok) >= 16 && tok ~ /[A-Za-z]/) tok = "<id>"
      else if (tok !~ /^[A-Z]+[0-9]+$/) gsub(/[0-9]+/, "#", tok)               # HY000 stays, thread12 -> thread#
    }
    out = out tok; prev = orig
  }
  return out s
}
function norm(s,   i) {
  for (i = 1; i <= NR_NORM; i++) gsub(NPAT[i], NREP[i], s)
  gsub(/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][T ][0-9][0-9]:[0-9][0-9](:[0-9][0-9]([.,][0-9]+)?)?(Z|[+-][0-9][0-9]:?[0-9][0-9])?/, "<time>", s)
  gsub(/[0-9][0-9][0-9][0-9][-\/][0-9][0-9][-\/][0-9][0-9]/, "<date>", s)
  gsub(/[0-9]?[0-9]:[0-9][0-9]:[0-9][0-9]([.,][0-9]+)?/, "<time>", s)
  gsub(/[0-9]?[0-9][ -](Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[ -][0-9][0-9][0-9][0-9]/, "<date>", s)
  gsub(/((Mon|Tue|Wed|Thu|Fri|Sat|Sun),? +)?(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) +[0-9]?[0-9],?( [0-9][0-9][0-9][0-9])? <time>/, "<date> <time>", s)
  gsub(/[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F]+/, "<uuid>", s)
  gsub(/0[xX][0-9a-fA-F]+/, "<hex>", s)
  s = nums(s)
  gsub(/[ \t]+/, " ", s); sub(/^ /, "", s); sub(/ $/, "", s)
  return substr(s, 1, 200)
}
BEGIN {
  Q = sprintf("%c", 39); LEV = ENVIRON["EL_LEVEL"]; EXC = ENVIRON["EL_EXC"]; IGN = ENVIRON["EL_IGNORE"]
  NR_NORM = 0
  n = split(ENVIRON["EL_NORM"], L, "\n")
  for (i = 1; i <= n; i++) if (L[i] != "") { split(L[i], P, "\t"); NR_NORM++; NPAT[NR_NORM] = P[1]; NREP[NR_NORM] = P[2] }
  split(ENVIRON["EL_HOSTS"], HOST, " ")
  nm = 0
  while ((getline ml < ENVIRON["EL_MARKS"]) > 0) { split(ml, M, " "); nm++; MLAB[nm] = M[1]; MCNT[nm] = M[2] + 0 }
  base = ENVIRON["EL_BASE"] + 0; lab = ENVIRON["EL_FIRST"]; mi = 1; trace = 0
  svc = ENVIRON["EL_SVC"]; src = ENVIRON["EL_SRC"]
}
{
  while (mi <= nm && NR > MCNT[mi]) { lab = MLAB[mi]; mi++ }
  if (NR <= base) next
  line = $0
  if (line ~ /^HYDO-/ || (IGN != "" && line ~ IGN)) { trace = 0; next }
  if (LEV != "" && line ~ LEV) hit = 1
  else if (EXC != "" && line ~ EXC) { if (trace) next; hit = 1 }
  else { if (trace && (line ~ /^[ \t]/ || line ~ /^Caused by/)) next; trace = 0; next }
  trace = 1
  c = clean(line)
  printf "%s\t%s\t%s\t%s\t%s\n", svc, src, norm(c), lab, substr(c, 1, 240)
}'
EL_SUM='
function rank(l) { return l == "s" ? -1 : l == "end" ? 1000000 : l == "r" ? 1000001 : l + 0 }
BEGIN { FS = "\t"; max = ENVIRON["EL_MAX"] + 0; nk = 0; total = 0 }
{
  total++; k = $1 "\t" $3
  if (!(k in cnt)) { nk++; K[nk] = k; SVC[k] = $1; SRC[k] = $2; TPL[k] = $3; EX[k] = $5; NL[k] = 0 }
  cnt[k]++
  if (!((k SUBSEP $4) in seen)) { seen[k SUBSEP $4] = 1; NL[k]++; LB[k SUBSEP NL[k]] = $4 }
}
END {
  for (i = 1; i <= nk && i <= max; i++) {
    k = K[i]; n = NL[k]
    for (a = 1; a <= n; a++) T[a] = LB[k SUBSEP a]
    for (a = 2; a <= n; a++) { v = T[a]; b = a - 1; while (b >= 1 && rank(T[b]) > rank(v)) { T[b + 1] = T[b]; b-- } T[b + 1] = v }
    at = T[1]; for (a = 2; a <= n; a++) at = at "," T[a]
    printf "HYDO-ERRLOG {\"svc\":\"%s\",\"src\":\"%s\",\"n\":%d,\"at\":\"%s\",\"tpl\":\"%s\",\"ex\":\"%s\"}\n", SVC[k], SRC[k], cnt[k], at, TPL[k], EX[k]
  }
  printf "HYDO-ERRLOG-SUM {\"boot\":\"%s\",\"steps\":%d,\"lines\":%d,\"kinds\":%d,\"shown\":%d}\n", ENVIRON["EL_BOOT"], ENVIRON["EL_STEPS"], total, nk, (nk < max ? nk : max)
}'

el_report() {   # once per boot: every covered stream's error lines, grouped, on the console
  local s k first boot src
  [ -f $H/el.reported ] && return 0
  : > $H/el.reported
  first=s; boot=first
  case "$PHASE:$CMD" in serve:oracle*) first=r; boot=recovery;; esac
  : > $H/el.rec
  for s in $HYDO_ERR_SERVICES; do
    for k in $(el_streams $s); do
      case $k in 0) src=log;; 1) src=setup;; *) src=$(el_file $s $k);; esac
      el_cat $s $k | EL_SVC=$s EL_SRC=$src EL_MARKS=$H/el.m.$s.$k EL_FIRST=$first \
        EL_BASE=$($BB cat $H/el.b.$s.$k 2>/dev/null || echo 0) EL_LEVEL="$(ev $s level)" EL_EXC="$(ev $s exc)" \
        EL_IGNORE="$(ev $s ignore)" EL_NORM="$(ev $s norm)" EL_HOSTS="$HYDO_SERVICES" \
        $BB awk "$EL_SCAN" >> $H/el.rec 2>> $H/el.err
    done
  done
  [ -s $H/el.err ] && log "error-log scan: $($BB head -c 300 $H/el.err)"
  EL_MAX=$HYDO_ERR_MAX EL_BOOT=$boot EL_STEPS=$HYDO_STEPS $BB awk "$EL_SUM" $H/el.rec
}

# the wrapped init functions (originals: _el_orig_<fn>, copied by hydo-build)
mount_data() {   # a file a service appends to on the data disk: report only lines from this boot on
  local s k
  _el_orig_mount_data "$@"
  for s in $HYDO_ERR_SERVICES; do
    for k in $(el_streams $s); do [ $k -ge 2 ] && el_cat $s $k | $BB wc -l > $H/el.b.$s.$k; done
  done
  return 0
}
step_range() { el_mark "$1"; _el_orig_step_range "$@"; }
retry_workload() { el_mark end; _el_orig_retry_workload "$@"; }
failed_ids() {   # a power cut: stop the services (the base stops them right after), then report
  local s
  _el_orig_failed_ids "$@"
  [ "$cut" = 1 ] || return 0
  for s in $HYDO_SERVICES; do svc_sig $s STOP; done
  el_report
  return 0
}
final_verdict() {   # the verdict line stays the LAST line: the records go before it
  _el_orig_final_verdict "$@" > $H/el.verdict
  el_report
  $BB cat $H/el.verdict
}
die_oracle() { el_report; _el_orig_die_oracle "$@"; }
