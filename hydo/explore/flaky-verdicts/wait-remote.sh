#!/bin/bash
# wait-remote.sh <max seconds> <remote pid file>: wait until the process named in the pid file on
# the build machine has ended (or the pid file is gone), at most <max seconds>. Exit 0 when it
# ended, 1 on the time limit.
. "$(dirname "$0")/../../../machines.sh"
need_build_machine
end=$(($(date +%s) + $1))
while [ "$(date +%s)" -lt "$end" ]; do
  on_build_machine "p=\$(cat $2 2>/dev/null) && [ -n \"\$p\" ] && kill -0 \$p 2>/dev/null" || exit 0
  sleep 20
done
exit 1
