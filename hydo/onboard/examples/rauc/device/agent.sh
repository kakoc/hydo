#!/bin/sh
# cmd: of the service `device`: the device's update agent.
# After a cold boot it runs the health check of the booted system and marks the booted slot good
# (`rauc status mark-good`, as RAUC's rauc-mark-good.service does). A system that fails its
# health check reboots without marking, so the boot counter rolls it back. Then it serves the
# client's requests, one at a time: $RUN/req/<id> holds "install <version>" or "reboot", and the
# answer goes to $RUN/res/<id> (rauc's exit code, "rebooting", or "lost").
# A reboot is a clean one (sync, then the cold boot of lib.sh) inside this process: the RAM
# under /tmp/ota is the boot's, the page cache survives as after a `reboot` that syncs first.
. /opt/ota/device/lib.sh
mkdir -p $RUN/req $RUN/res
echo $$ > $RUN/agent.pid   # the client stops waiting once this process is gone (a kill)

reboot_device() {
  event ota.reboot_started "a reboot started"
  sync
  rm -f $RUN/booted $RUN/marked $RUN/ready
  cold_boot
}

after_boot() {   # health check and mark-good; a failed health check reboots
  n=0
  while [ ! -f $RUN/bricked ] && [ ! -f $RUN/marked ]; do
    if [ "$(booted health)" = ok ]; then
      event ota.mark_good_started "mark-good of the booted slot started"
      if rauc_dev status mark-good; then
        booted gen > $RUN/marked
        say "boot $(booted gen): slot $(booted slot) v$(booted version) marked good"
      else
        say "boot $(booted gen): mark-good failed"
        echo failed > $RUN/marked
      fi
      return
    fi
    n=$((n + 1))
    if [ $n -gt $MAX_RESETS ]; then
      say "no healthy boot in $MAX_RESETS reboots: bricked"
      echo bricked > $RUN/bricked
      return
    fi
    say "boot $(booted gen): v$(booted version) fails its health check: reboot"
    reboot_device
  done
}

# the requests that the last run of the agent had not answered died with it
for r in $RUN/req/*; do
  [ -f "$r" ] || continue
  id=${r##*/}
  [ -f "$RUN/res/$id" ] || { echo lost > "$RUN/res/$id"; say "request $id lost"; }
done

after_boot
: > $RUN/ready
while :; do
  for r in $RUN/req/*; do
    [ -f "$r" ] || continue
    id=${r##*/}
    case $id in *.tmp) continue ;; esac   # the client is still writing it
    [ -f "$RUN/res/$id" ] && continue
    set -- $(cat "$r")
    case $1 in
      install)
        say "request $id: rauc install v$2"
        event ota.install_started "an install started"
        rauc_dev install $OTA/bundles/v$2.raucb > "$RUN/res/$id.log" 2>&1
        rc=$?
        tail -n 2 "$RUN/res/$id.log"
        say "request $id: rauc install v$2 exited $rc"
        echo $rc > "$RUN/res/$id.tmp" && mv "$RUN/res/$id.tmp" "$RUN/res/$id"
        ;;
      reboot)
        say "request $id: reboot"
        echo rebooting > "$RUN/res/$id"
        reboot_device
        after_boot; : > $RUN/ready
        ;;
      *) echo "unknown request" > "$RUN/res/$id" ;;
    esac
  done
  [ -f $RUN/bricked ] && { : > $RUN/ready; }
  sleep 0.2
done
