#!/bin/sh
# An AI agent's session in the workspace, as numbered steps (docs/design/agent-environments.md):
#   agent-steps.sh <from> <to>   runs steps [from, to); each step's output goes to /data/agent.log
# Step 0 runs the tests (one bug: two fail), 1 fixes the bug, 2 runs the tests again (all pass),
# 3 commits, 4 builds the wheel, 5 prints what the session left (commit, wheel, random values).
cd /data/ws || exit 1
step() {
  echo "== step $1: $2" >> /data/agent.log
  sh -c "$2" >> /data/agent.log 2>&1
  echo "== step $1 status $?" >> /data/agent.log
}
i=$1
while [ "$i" -lt "$2" ]; do
  case $i in
  0) step 0 'python -m pytest -q -p no:cacheprovider' ;;
  1) step 1 "sed -i 's/lot.qty = take/lot.qty -= take/' stockbook/ledger.py && git diff --stat" ;;
  2) step 2 'python -m pytest -q -p no:cacheprovider' ;;
  3) step 3 'git commit -q -am "ledger: keep the rest of a partly shipped lot" && git log --oneline' ;;
  4) step 4 'python -m pip wheel . --no-deps --no-build-isolation -q -w dist && ls dist' ;;
  5) step 5 'git rev-parse HEAD; sha256sum dist/*.whl; python -c "import random, uuid, time; print(random.random(), uuid.uuid4(), time.time())"' ;;
  esac
  i=$((i + 1))
done
