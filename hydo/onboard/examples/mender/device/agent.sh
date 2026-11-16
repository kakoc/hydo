#!/bin/sh
# cmd: of the service `device`: the device's update agent, which runs Mender in standalone mode
# for the operator (client.sh). After a cold boot it runs the health check of the booted system:
# a system that fails it rolls back with `mender-update rollback` and reboots, before the device
# counts as ready. Then it serves the operator's requests, one at a time: $RUN/req/<id> holds
# "install <version>", "commit" or "reboot", and the answer goes to $RUN/res/<id>: "<exit code>
# <last line of mender-update>", "rebooting", or "lost". An install runs
# `mender-update install /mnt/updates/v<N>.mender`; the Artifacts are on a medium mounted at
# /mnt/updates, as a USB stick would be (with the build parameter artifacts=plain, the Artifacts
# v<N>-plain.mender without state scripts). A commit runs `mender-update commit`, as Mender's
# documentation of standalone deployments says: install, reboot, commit.
# A reboot is a real one (guest.reboot, docs/design/devices.md): the agent asks hydo for it
# with the console line HYDO-REBOOT and waits to be stopped, as a program that calls `reboot`
# does. hydo shuts the guest down cleanly (sync, a cache flush of every disk) and boots the
# machine again; boot.sh then runs the cold boot of lib.sh.
. /opt/ota/device/lib.sh
mkdir -p $RUN/req $RUN/res
echo $$ > $RUN/agent.pid   # the operator stops waiting once this process is gone (a kill)

reboot_device() {   # reason
  event ota.reboot_started "a reboot started"
  c=/dev/ttyS0; [ -c $c ] || c=/dev/console
  echo "HYDO-REBOOT $1" > $c
  while :; do sleep 60; done
}

after_boot() {   # the health check; a system that fails it rolls back and reboots
  [ -f $RUN/bricked ] && return
  [ -f $RUN/checked ] && return
  d=$ROOT/data/mender/scripts
  if [ -d $d ]; then   # what the last install left of the Artifact's state scripts
    say "boot: Mender's Artifact scripts: $(ls $d | wc -l) files, the version file $(wc -c < $d/version 2>/dev/null || echo missing) bytes"
  fi
  if [ "$(booted health)" != ok ]; then
    say "boot: v$(booted version) fails its health check: mender-update rollback"
    say "boot: rollback: $(mender rollback)"
    reboot_device "v$(booted version) failed its health check"
  fi
  # the device's inventory: at the first boot this also installs Mender's bootstrap Artifact
  # (v1), which records the factory's version in Mender's database
  say "boot: mender-update show-artifact: $(mender show-artifact)"
  : > $RUN/checked
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
    case $id in *.tmp) continue ;; esac   # the operator is still writing it
    [ -f "$RUN/res/$id" ] && continue
    set -- $(cat "$r")
    case $1 in
      install)
        say "request $id: mender-update install v$2.mender"
        event ota.install_started "an install started"
        a=$(mender install /mnt/updates/v$2$ARTIFACT_SUFFIX.mender)
        ;;
      commit)
        say "request $id: mender-update commit"
        event ota.commit_started "a commit started"
        a=$(mender commit)
        ;;
      reboot)
        say "request $id: reboot"
        echo rebooting > "$RUN/res/$id"
        reboot_device "the operator's reboot $id"
        ;;
      *) a="unknown request" ;;
    esac
    say "request $id: $a"
    echo "$a" > "$RUN/res/$id.tmp" && mv "$RUN/res/$id.tmp" "$RUN/res/$id"
  done
  sleep 0.2
done
