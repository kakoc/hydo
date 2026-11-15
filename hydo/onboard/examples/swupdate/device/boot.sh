#!/bin/sh
# setup: of the service `device`, run before every start of it. When the RAM (/tmp) says that the
# device has not booted yet, this is a cold boot: the first boot, the recovery boot after a power
# cut, or a reboot the agent asked for. Otherwise the update agent was killed and starts again on
# the running system, as systemd restarts a service.
. /opt/ota/device/lib.sh
mkdir -p $RUN
[ -f $RUN/booted ] && exit 0
rm -f $RUN/marked $RUN/ready
cold_boot
