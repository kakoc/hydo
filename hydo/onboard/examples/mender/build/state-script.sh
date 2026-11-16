#!/bin/sh
# A Mender state script (the slots' /etc/mender/scripts and every Artifact's scripts): it prints
# the state it runs in as a console event, which a reaction of hydo's schedule can wait for
# (A=<event>[@<step>][+<ms>]:cut|kill-device). It changes nothing on the device.
case ${0##*/} in
  Download_Enter_*) id=mender.download_started ;;
  ArtifactInstall_Enter_*) id=mender.install_started ;;
  ArtifactCommit_Enter_*) id=mender.commit_started ;;
  ArtifactCommit_Leave_*) id=mender.commit_leave ;;
  ArtifactRollback_Enter_*) id=mender.rollback_started ;;
  *) exit 0 ;;
esac
c=/dev/ttyS0; [ -c $c ] || c=/dev/console
echo "HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"$id\",\"ok\":true,\"msg\":\"${0##*/}\",\"details\":{}}" > $c
exit 0
