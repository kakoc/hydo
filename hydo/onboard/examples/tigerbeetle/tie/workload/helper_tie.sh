# Shared by the commands and checks (sourced, not a command): TigerBeetle's REPL, every call
# with a time limit (a TigerBeetle client never gives up on a request).
TB=${TB:-/opt/tb/tigerbeetle}
TB_ADDRESSES=${TB_ADDRESSES:-127.0.0.1:3001}

# tb <seconds> <statement>: one REPL call (a new client session); its output on stdout
tb() {
  timeout "$1" $TB repl --cluster=0 --addresses="$TB_ADDRESSES" --command="$2" 2>/dev/null
}

# ts_of <id>...: "id timestamp" of each transfer that exists (lookup_transfers)
ts_of() {
  _s="lookup_transfers"; _sep=" "
  for _i in "$@"; do _s="$_s$_sep id=$_i"; _sep=","; done
  tb 20 "$_s" | awk '/"id":/ { gsub(/[",]/, "", $2); id = $2 }
                     /"timestamp":/ { gsub(/[",]/, "", $2); print id, $2 }'
}

# say <text>: on the console and on stdout
say() { echo "tie: $*"; echo "tie: $*" > /dev/console 2>/dev/null; }

# One REPL session for many statements (session_start, rq, session_out): a new REPL process
# takes most of a second to start in the guest, and its start time varies by milliseconds. The
# REPL edits its line in a terminal, so it runs under `script` (a pseudo-terminal) and gets an
# answer to each of its two cursor-position queries per prompt.
SESS=/tmp/hydo/repl
session_start() {
  [ -c /dev/ptmx ] || return 1
  mkdir -p /dev/pts; mountpoint -q /dev/pts || mount -t devpts -o ptmxmode=0666 devpts /dev/pts
  rm -f $SESS.in $SESS.out; mkfifo $SESS.in; : > $SESS.out
  $(command -v script >/dev/null && echo script || echo busybox script) -q -c "$TB repl --cluster=0 --addresses=$TB_ADDRESSES" /dev/null < $SESS.in > $SESS.out 2>&1 &
  exec 7> $SESS.in
  NQ=0
  session_prompt
}
_queries() { grep -o '\[6n' $SESS.out | wc -l; }
_waitq() {   # _waitq <n>: until the REPL has asked n times (120 s at most)
  _i=0
  while [ "$(_queries)" -lt "$1" ]; do
    _i=$((_i + 1)); [ $_i -gt 60000 ] && return 1; sleep 0.002
  done
}
session_prompt() {   # wait for the prompt and answer its two queries (cursor, then screen size)
  _waitq $((NQ + 1)) || return 1; printf '\033[1;3R' >&7
  _waitq $((NQ + 2)) || return 1; printf '\033[9999;30000R' >&7
  NQ=$((NQ + 2))
}
rq() {   # rq <statement>: run it in the session; its output (without the echo) in $SESS.last
  _off=$(wc -c < $SESS.out)
  printf '%s;\r' "$1" >&7
  session_prompt || return 1
  tail -c +$((_off + 1)) $SESS.out | tr -d '\r' | grep -v -e '\[6n' -e '^\[' > $SESS.last
}
