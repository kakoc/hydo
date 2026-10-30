# Shared helpers of the hvx safety-net scripts (Milestone 97). Sourced, not run.

QUIET_FILE=${QUIET_FILE:-/var/tmp/hydo-QUIET}
SAFETY_NIXPKGS=/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source	# NixOS 24.05 @ 89c4987

# safety_build SCRIPT LOG [nix packages...]
# Runs the bash SCRIPT at nice 19 in its own process group, output to LOG. On a machine
# with nix the script runs inside nix-shell with the given packages (and without the
# cc-wrapper's userspace hardening flags, which the kernel sets itself); elsewhere it uses
# the system tools. While $QUIET_FILE exists (another session measures timing on the
# bare-metal test machine) the whole process group is stopped with SIGSTOP and continued after.
safety_build() {
	local script=$1 log=$2 pid rc stopped=0
	shift 2
	if command -v nix-shell >/dev/null 2>&1; then
		local pk=""
		for p in "$@"; do pk="$pk $p"; done
		setsid nice -n 19 nix-shell -I nixpkgs="$SAFETY_NIXPKGS" -p $pk \
			--run "export NIX_HARDENING_ENABLE=''; bash '$script'" > "$log" 2>&1 < /dev/null &
	else
		setsid nice -n 19 bash "$script" > "$log" 2>&1 < /dev/null &
	fi
	pid=$!
	echo "$pid" > "$log.pid"	# stop the build: kill -- -$(cat LOG.pid) (its own process group)
	while kill -0 "$pid" 2>/dev/null; do
		if [ -e "$QUIET_FILE" ] && [ $stopped = 0 ]; then
			kill -STOP -- "-$pid" 2>/dev/null && stopped=1 && echo "safety_build: paused ($QUIET_FILE exists)"
		elif [ ! -e "$QUIET_FILE" ] && [ $stopped = 1 ]; then
			kill -CONT -- "-$pid" 2>/dev/null; stopped=0; echo "safety_build: resumed"
		fi
		sleep 5
	done
	wait "$pid"; rc=$?
	[ $rc = 0 ] || { echo "safety_build: $script failed (rc=$rc); last lines of $log:" >&2; tail -20 "$log" >&2; }
	return $rc
}

# wait_free_mem MIB TIMEOUT_S : wait until MemAvailable >= MIB (true) or the timeout ends (false)
wait_free_mem() {
	local need=$1 to=$2 t=0 avail
	while :; do
		avail=$(awk '/^MemAvailable:/ {print int($2/1024)}' /proc/meminfo)
		[ "$avail" -ge "$need" ] && return 0
		[ "$t" -ge "$to" ] && { echo "wait_free_mem: only ${avail} MiB available after ${t}s (need $need)" >&2; return 1; }
		sleep 30; t=$((t+30))
	done
}
