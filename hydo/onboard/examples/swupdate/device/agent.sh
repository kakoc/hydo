#!/bin/sh
# cmd: of the service `device`: the device's update agent.
# After a cold boot it runs the health check of the booted system and, when it passes, confirms
# the update in one environment write (upgrade_available=0, bootcount=0), as SWUpdate's
# documentation asks of the application. A system that fails its health check reboots without
# confirming, so U-Boot's boot counter rolls it back. Then it serves the client's requests, one
# at a time: $RUN/req/<id> holds "install <version>" or "reboot", and the answer goes to
# $RUN/res/<id> (swupdate's exit code, "rebooting", or "lost"). An install runs
# `swupdate -i <bundle> -e stable,copy1|copy2`: the copy that is not booted.
# A reboot is a real one (guest.reboot, docs/design/devices.md): the agent asks hydo for it
# with the console line HYDO-REBOOT and waits to be stopped, as a program that calls `reboot`
# does. hydo shuts the guest down cleanly (sync, a cache flush of every disk) and boots the
# machine again; boot.sh then runs the cold boot of lib.sh.
. /opt/ota/device/lib.sh
mkdir -p $RUN/req $RUN/res
echo $$ > $RUN/agent.pid   # the client stops waiting once this process is gone (a kill)

reboot_device() {   # reason
  event ota.reboot_started "a reboot started"
  c=/dev/ttyS0; [ -c $c ] || c=/dev/console
  echo "HYDO-REBOOT $1" > $c
  while :; do sleep 60; done
}

after_boot() {   # health check and confirmation; a failed health check reboots
  n=0
  while [ ! -f $RUN/bricked ] && [ ! -f $RUN/marked ]; do
    if [ "$(booted health)" = ok ]; then
      event ota.mark_good_started "the confirmation of the booted slot started"
      if [ "$(envvar upgrade_available 0)" = 0 ] || \
         { printf 'upgrade_available=0\nbootcount=0\n' > $RUN/confirm.env && fw_setenv -s $RUN/confirm.env; }; then
        booted gen > $RUN/marked
        say "boot $(booted gen): slot $(booted slot) v$(booted version) confirmed"
      else
        say "boot $(booted gen): the confirmation failed"
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
    reboot_device "v$(booted version) failed its health check"
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
        copy=copy2; [ "$(booted slot)" = B ] && copy=copy1
        say "request $id: swupdate -i v$2.swu -e stable,$copy"
        event ota.install_started "an install started"
        swupdate -v -i $OTA/bundles/v$2.swu -e stable,$copy > "$RUN/res/$id.log" 2>&1
        rc=$?
        tail -n 2 "$RUN/res/$id.log"
        say "request $id: swupdate exited $rc"
        echo $rc > "$RUN/res/$id.tmp" && mv "$RUN/res/$id.tmp" "$RUN/res/$id"
        ;;
      reboot)
        say "request $id: reboot"
        echo rebooting > "$RUN/res/$id"
        reboot_device "the operator's reboot $id"
        ;;
      *) echo "unknown request" > "$RUN/res/$id" ;;
    esac
  done
  [ -f $RUN/bricked ] && { : > $RUN/ready; }
  sleep 0.2
done
