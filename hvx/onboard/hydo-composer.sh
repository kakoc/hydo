#!/bin/busybox sh
# /etc/hydo/compose.sh -- the test composer inside the guest (MANIFEST.md "The test composer").
# hydo-build stages it, with /etc/hydo/compose.list (the plan's commands) and compose.checks,
# only for a manifest whose plan has test commands or checks; every other image is unchanged.
# The generic init (hydo-init.sh) does not know about it: the workload, provisioning and the
# invariants of a composed plan are compiled to call it.
#
#   compose.sh first            provisioning: every first command, in name order (no faults)
#   compose.sh step FROM TO     the workload for steps [FROM,TO): starts the composer daemon the
#                               first time, lets it run rounds FROM..TO-1 and returns when round
#                               TO is due (the init then applies the faults placed at TO); for
#                               TO = the step count it returns after the drain.
#   compose.sh daemon           the composition: rounds of commands, with overlap (below)
#   compose.sh run1 ...         one command (internal; its process is the "request in flight")
#   compose.sh end IDX          an eventually/finally command, as a final invariant
#   compose.sh check IDX        a final/always check, as an invariant (a timeout -> exit 2)
#   compose.sh verdict          the compose.commands invariant: no command exited 100
#
# The composition is a pure function of W= (schedule token, written to /tmp/hydo/compose.W by
# the init's knob path) and the plan: hydo_composer.plan() in Python, the awk below here.
# Round r starts r * round_ms of guest time after round 0 (later if a fault point held it);
# a command keeps running across later rounds and fault points until it ends. At most p commands
# run at once; a parallel driver waits while a serial or singleton driver runs; a serial or
# singleton driver waits until no other driver runs; anytime commands only respect p.
# Console lines (one write each): HYDO-COMPOSE plan|done ..., HYDO-CMD start|end|skip ...
# HYDO_ETC / HYDO_TMP / HYDO_CONSOLE / BB exist only for a dry run on a Linux host
# (tests/compose-dryrun.sh); the guest never sets them.
BB=${BB:-/bin/busybox}
E=${HYDO_ETC:-/etc/hydo}
H=${HYDO_TMP:-/tmp/hydo}; C=$H/compose
CON=${HYDO_CONSOLE:-/dev/console}
COMPOSE_SH=$E/compose.sh
L=$E/compose.list
. $E/manifest.sh
con() { echo "$*" >> $CON; }
tnow() { read _u _r < /proc/uptime; T=$(( ${_u%.*} * 100 + 1${_u#*.} - 100 )); }   # centiseconds
jesc() { printf '%s' "$1" | $BB tr -d '\000-\037' | $BB sed 's/\\/\\\\/g; s/"/\\"/g'; }

# the plan: lines "<entry> <round> <cmd index> <kind> <entry seed>"; mirrors hydo_composer.plan/kept
PLAN_AWK='
function nxt() { s = (s * 48271) % M; return s }
function below(n) { return nxt() % n }
function add(r, c) { er[ne] = r; ec[ne] = c; es[ne] = nxt(); ne++ }
function inlist(lst, v,   n, a, i, b) {
  n = split(lst, a, ".")
  for (i = 1; i <= n; i++) {
    if (a[i] == "") continue
    if (index(a[i], "-")) { split(a[i], b, "-"); if (v >= b[1] + 0 && v <= b[2] + 0) return 1 }
    else if (v == a[i] + 0) return 1
  }
  return 0
}
{ kind[$1 + 0] = $2; if ($1 + 1 > nc) nc = $1 + 1 }
END {
  M = 2147483647; np = ns = ng = na = 0; ne = 0
  for (i = 0; i < nc; i++) {
    k = kind[i]
    if (k == "parallel_driver") P[np++] = i
    else if (k == "serial_driver") SE[ns++] = i
    else if (k == "singleton_driver") SG[ng++] = i
    else if (k == "anytime") A[na++] = i
  }
  s = seed % (M - 1) + 1
  for (j = 0; j < 8; j++) nxt()
  single = 0
  if (ng > 0) { if (np + ns == 0) single = 1; else if (below(4) == 0) single = 1 }
  g = -1
  if (single) g = SG[below(ng)]
  for (r = 0; r < S; r++) {
    if (single) { if (r == 0) add(r, g) }
    else if (ns > 0 && (np == 0 || below(4) == 0)) { c = SE[below(ns)]; add(r, c) }
    else if (np > 0) { kk = 1 + below(q); for (j = 0; j < kk; j++) { c = P[below(np)]; add(r, c) } }
    if (na > 0 && ((np + ns + ng == 0) || below(3) == 0)) { c = A[below(na)]; add(r, c) }
  }
  out = 0
  for (i = 0; i < ne; i++) {
    if (inlist(X, i) || inlist(O, ec[i])) continue
    if (N > 0 && out >= N) break
    printf "%d %d %d %s %d\n", i, er[i], ec[i], kind[ec[i]], es[i]
    out++
  }
}'

parse_w() {
  WS=0 WP=$HYDO_COMPOSE_PARALLEL WQ=$HYDO_COMPOSE_PER_ROUND WR=$HYDO_COMPOSE_ROUND_MS
  WN=$HYDO_COMPOSE_MAX_COMMANDS WT=${HYDO_COMPOSE_BUDGET_MS:-0} WX= WO=
  W=$($BB cat $H/compose.W 2>/dev/null)
  [ "$W" = - ] && W=
  [ -n "$W" ] || return 0
  _ifs=$IFS; IFS=,; set -- $W; IFS=$_ifs
  WS=$((0x$1)); shift
  for f; do
    case "$f" in
      p*) WP=${f#p};; q*) WQ=${f#q};; r*) WR=${f#r};; n*) WN=${f#n};; t*) WT=${f#t};;
      x*) WX=${f#x};; o*) WO=${f#o};;
      *) con "HYDO-COMPOSE: W=$W: unknown field '$f' ignored";;
    esac
  done
}

line_of() {   # <list file> <index>: ci ck cto cid ccmd
  _l=$($BB sed -n "$(($2 + 1))p" $1)
  read -r ci ck cto cid ccmd <<EOF
$_l
EOF
}

tree() {   # <pid>: it and every descendant
  $BB cat /proc/[0-9]*/stat 2>/dev/null | $BB sed 's/ (.*) / x /' | $BB awk -v r="$1" '
    { p[$1] = $4 } END { s[r] = 1; ch = 1
      while (ch) { ch = 0; for (i in p) if (!(i in s) && (p[i] in s)) { s[i] = 1; ch = 1 } }
      for (i in s) print i }'
}

# ---------------------------------------------------------------- one command
run1() {   # <entry> <round> <cmd index> <entry seed>
  line_of $L $3
  tnow; _t1=$T
  HYDO_CMD=$cid HYDO_CMD_ID=$1 HYDO_ROUND=$2 HYDO_SEED=$4 \
    $BB timeout $cto $BB sh -c ". $E/prelude.sh; $ccmd" < /dev/null > $C/log.$1 2>&1
  rc=$?
  tnow
  case $rc in 0) st=ok;; 100) st=violation;; 143) st=timeout;; *) st=failed;; esac
  $BB grep -a '^HYDO-ASSERT' $C/log.$1 >> $CON 2>/dev/null
  con "HYDO-CMD end i=$1 cmd=$cid rc=$rc ms=$(( (T - _t1) * 10 )) status=$st"
  case $st in
    ok) $BB mkdir $C/ok.$cid 2>/dev/null && \
          con "HYDO-ASSERT {\"t\":\"sometimes\",\"id\":\"compose.$cid.succeeded\",\"ok\":true,\"msg\":\"$cid exited 0 at least once\"}";;
    violation)
      _d=$($BB grep -av '^HYDO-ASSERT' $C/log.$1 | $BB tail -1)
      if $BB mkdir $C/viol 2>/dev/null; then echo "$1 $cid" > $C/violation; echo "$_d" > $C/violation.detail; fi
      $BB mkdir $C/vio.$cid 2>/dev/null && \
        con "HYDO-ASSERT {\"t\":\"always_or_unreachable\",\"id\":\"compose.$cid.no_violation\",\"ok\":false,\"msg\":\"$cid never reports a violation (exit 100)\",\"details\":{\"entry\":$1,\"out\":\"$(jesc "$_d")\"}}";;
  esac
  echo $st > $C/rc.$1.tmp && $BB mv $C/rc.$1.tmp $C/rc.$1
}

# ---------------------------------------------------------------- the daemon
reap() {
  _rnr=""
  for _re in $RUNNING; do
    _ri=${_re%%:*}; _rk=${_re#*:}
    if [ -f $C/rc.$_ri ]; then
      read _rst < $C/rc.$_ri
      case $_rk in parallel_driver) NP=$((NP - 1));; serial_driver) NS=$((NS - 1));;
                  singleton_driver) NG=$((NG - 1));; *) NA=$((NA - 1));; esac
      case $_rst in ok) NOK=$((NOK + 1));; violation) NVIO=$((NVIO + 1));; timeout) NTO=$((NTO + 1));;
                   *) NFAIL=$((NFAIL + 1));; esac
    else
      _rnr="$_rnr $_re"
    fi
  done
  RUNNING=$_rnr
}
can_start() {
  case $1 in
    parallel_driver) [ $((NP + NS + NG + NA)) -lt $WP ] && [ $((NS + NG)) -eq 0 ];;
    anytime) [ $((NP + NS + NG + NA)) -lt $WP ];;
    *) [ $((NP + NS + NG)) -eq 0 ] && [ $((NP + NS + NG + NA)) -lt $WP ];;
  esac
}
round_start() {   # round $r: wait for its time, then for the gate
  if [ $r -gt 0 ]; then
    t=$((t + RC))
    while reap; tnow; [ $T -lt $t ]; do $BB sleep 0.01; done
  fi
  read _g 2>/dev/null < $C/gate
  while [ $r -ge ${_g:-0} ]; do
    echo $r > $C/at.tmp && $BB mv $C/at.tmp $C/at
    reap; $BB sleep 0.01
    read _g 2>/dev/null < $C/gate
  done
  tnow; [ $T -gt $t ] && t=$T
}
launch() {   # <entry> <cmd index> <kind> <entry seed>
  while reap; ! can_start $3; do $BB sleep 0.01; done
  tnow
  if [ $WT -gt 0 ] && [ $(( (T - T0) * 10 )) -ge $WT ]; then
    con "HYDO-CMD skip i=$1 r=$r cmd=$(line_of $L $2; echo $cid) budget_ms=$WT"; NSKIP=$((NSKIP + 1)); return
  fi
  line_of $L $2
  con "HYDO-CMD start i=$1 r=$r kind=$3 cmd=$cid"
  $BB sh $COMPOSE_SH run1 $1 $r $2 $4 < /dev/null > /dev/null 2>&1 &
  echo "$! $cid" > $C/pid.$1
  RUNNING="$RUNNING $1:$3"
  NSTART=$((NSTART + 1))
  case $3 in parallel_driver) NP=$((NP + 1));; serial_driver) NS=$((NS + 1));;
             singleton_driver) NG=$((NG + 1));; *) NA=$((NA + 1));; esac
}
drain() {
  tnow; _dl=$((T + HYDO_COMPOSE_DRAIN_TIMEOUT * 100))
  while reap; [ -n "$RUNNING" ]; do tnow; [ $T -ge $_dl ] && break; $BB sleep 0.01; done
  NKILL=0
  for _e in $RUNNING; do
    _i=${_e%%:*}
    read _p _id < $C/pid.$_i
    $BB kill -9 $(tree $_p) 2>/dev/null
    con "HYDO-CMD end i=$_i cmd=$_id rc=137 status=killed (still running ${HYDO_COMPOSE_DRAIN_TIMEOUT}s after the last round)"
    NKILL=$((NKILL + 1))
  done
  [ $((NKILL + NTO)) -gt 0 ] && : > $C/incomplete
  _sum="started=$NSTART ok=$NOK failed=$NFAIL timeout=$NTO violation=$NVIO killed=$NKILL skipped=$NSKIP"
  echo "$_sum" > $C/summary
  con "HYDO-COMPOSE done $_sum"
  : > $C/done
}
daemon() {
  parse_w
  $BB awk -v seed=$WS -v q=$WQ -v S=$HYDO_STEPS -v X="$WX" -v O="$WO" -v N=$WN "$PLAN_AWK" $L > $C/plan
  con "HYDO-COMPOSE plan W=${W:--} seed=$WS p=$WP q=$WQ r=$WR n=$WN rounds=$HYDO_STEPS entries=$($BB wc -l < $C/plan)"
  while read -r _ci _ck _cto _cid _cc; do
    case $_ck in parallel_driver|serial_driver|singleton_driver|anytime)
      echo "HYDO-ASSERT-DECL {\"t\":\"sometimes\",\"id\":\"compose.$_cid.succeeded\",\"msg\":\"$_cid exited 0 at least once\"}" >> $CON
      echo "HYDO-ASSERT-DECL {\"t\":\"always_or_unreachable\",\"id\":\"compose.$_cid.no_violation\",\"msg\":\"$_cid never reports a violation (exit 100)\"}" >> $CON;;
    esac
  done < $L
  RC=$((WR / 10)); [ $RC -lt 1 ] && RC=1
  RUNNING=""; NP=0 NS=0 NG=0 NA=0 NSTART=0 NOK=0 NFAIL=0 NTO=0 NVIO=0 NSKIP=0
  tnow; T0=$T; t=$T; r=-1
  while read -r pe pr pc pk ps; do      # names no other function uses (reap/launch run inside)
    while [ $r -lt $pr ]; do r=$((r + 1)); round_start; done
    launch $pe $pc $pk $ps
  done < $C/plan
  while [ $r -lt $((HYDO_STEPS - 1)) ]; do r=$((r + 1)); round_start; done
  drain
}

stop_all() {   # the step range ran into workload.timeout: stop the composition, record what ran
  $BB kill -9 $(tree $_dp) 2>/dev/null
  for _p in $C/pid.*; do
    [ -f "$_p" ] || continue
    _i=${_p##*.}
    [ -f $C/rc.$_i ] && continue
    read _pp _id < $_p
    con "HYDO-CMD end i=$_i cmd=$_id rc=137 status=killed (workload.timeout)"
    echo killed > $C/rc.$_i
  done
  _n() { $BB cat $C/rc.* 2>/dev/null | $BB grep -c "^$1\$"; }
  _sum="started=$($BB ls $C | $BB grep -c '^pid\.') ok=$(_n ok) failed=$(_n failed) timeout=$(_n timeout) violation=$(_n violation) killed=$(_n killed) skipped=0"
  echo "$_sum" > $C/summary; : > $C/incomplete
  con "HYDO-COMPOSE done $_sum (stopped: steps $1..$(($2 - 1)) ran into workload.timeout=${HYDO_WORKLOAD_TIMEOUT}s)"
  : > $C/done
}

step() {   # FROM TO
  $BB mkdir -p $C
  echo $2 > $C/gate.tmp && $BB mv $C/gate.tmp $C/gate
  if [ ! -f $C/started ]; then
    : > $C/started
    $BB sh $COMPOSE_SH daemon < /dev/null > $C/daemon.log 2>&1 &
    echo $! > $C/daemon.pid
  fi
  read _dp < $C/daemon.pid
  # the init kills this call after workload.timeout; stop the composition a little earlier,
  # so no command keeps running into the heal and the final checks
  tnow; _end=$((T + (HYDO_WORKLOAD_TIMEOUT > 10 ? HYDO_WORKLOAD_TIMEOUT - 5 : HYDO_WORKLOAD_TIMEOUT / 2 + 1) * 100))
  while :; do
    if [ $2 -ge $HYDO_STEPS ]; then
      [ -f $C/done ] && return 0
    else
      _a=""; read _a 2>/dev/null < $C/at
      [ "$_a" = "$2" ] && return 0
    fi
    if ! $BB kill -0 $_dp 2>/dev/null; then
      [ -f $C/done ] && return 0
      echo "compose.sh: the composer daemon died"; $BB tail -5 $C/daemon.log; return 1
    fi
    tnow
    if [ $T -ge $_end ]; then [ -f $C/done ] || stop_all $1 $2; return 1; fi
    $BB sleep 0.01
  done
}

first() {   # every first command in index (= name) order; a failure fails provisioning
  _rc=0; $BB mkdir -p $C
  while read -r ci ck cto cid ccmd; do
    [ "$ck" = first ] || continue
    con "HYDO-CMD start i=f$ci kind=first cmd=$cid"
    tnow; _t1=$T
    HYDO_CMD=$cid $BB timeout $cto $BB sh -c ". $E/prelude.sh; $ccmd" < /dev/null > $C/log.f$ci 2>&1
    rc=$?; tnow
    con "HYDO-CMD end i=f$ci cmd=$cid rc=$rc ms=$(( (T - _t1) * 10 ))"
    if [ $rc != 0 ]; then echo "$cid exited rc=$rc:"; $BB tail -5 $C/log.f$ci; _rc=1; fi
  done < $L
  return $_rc
}

endc() {   # IDX: an eventually or finally command (an invariant: 0 holds, 1 violated)
  line_of $L $1; $BB mkdir -p $C
  if [ "$ck" = finally ]; then
    if [ ! -f $C/started ]; then echo "skipped: no composed workload ran in this boot (recovery boot after a power cut)"; return 0; fi
    if [ ! -f $C/done ] || [ -f $C/incomplete ]; then echo "skipped: not every started command ran to completion"; return 0; fi
  fi
  con "HYDO-CMD start i=e$1 kind=$ck cmd=$cid"
  tnow; _t1=$T
  HYDO_CMD=$cid $BB timeout $cto $BB sh -c ". $E/prelude.sh; $ccmd" < /dev/null > $C/log.e$1 2>&1
  rc=$?; tnow
  con "HYDO-CMD end i=e$1 cmd=$cid rc=$rc ms=$(( (T - _t1) * 10 ))"
  $BB grep -a '^HYDO-ASSERT' $C/log.e$1 >> $CON 2>/dev/null
  if [ $rc = 0 ]; then echo "$cid ok ($(( (T - _t1) * 10 )) ms)"; return 0; fi
  echo "$cid exited rc=$rc: $($BB grep -av '^HYDO-ASSERT' $C/log.e$1 | $BB tail -1)"; return 1
}

check() {   # IDX: a final/always check; 0 holds, 1 violated, 2 not evaluable now (incl. a timeout)
  line_of $E/compose.checks $1
  HYDO_CHECK=$cid $BB timeout $cto $BB sh -c ". $E/prelude.sh; $ccmd" < /dev/null
  rc=$?
  case $rc in 0|1|2) return $rc;; 143) echo "check $cid timed out after ${cto}s"; return 2;; *) return 1;; esac
}

verdict() {
  if [ -f $C/violation ]; then
    read _i _id < $C/violation
    echo "command $_id (entry $_i) reported a violation (exit 100): $($BB cat $C/violation.detail)"; return 1
  fi
  if [ -f $C/summary ]; then $BB cat $C/summary; else echo "no composed workload ran in this boot"; fi
  return 0
}

case "$1" in
  first) first;; step) step "$2" "$3";; daemon) daemon;; run1) shift; run1 "$@";;
  end) endc "$2";; check) check "$2";; verdict) verdict;;
  plan) parse_w; $BB awk -v seed=$WS -v q=$WQ -v S=$HYDO_STEPS -v X="$WX" -v O="$WO" -v N=$WN "$PLAN_AWK" $L;;
  *) echo "usage: compose.sh first|step FROM TO|daemon|end IDX|check IDX|verdict|plan" >&2; exit 2;;
esac
