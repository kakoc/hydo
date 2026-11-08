#!/bin/bash
# task-remote.sh <task id> [runs]: one task's native and hydo runs on an x86_64 Linux machine with
# Docker (for the login user) and /dev/kvm. Expects $K/env/<id>.tar.gz (prepare.sh's output) and
# the hydo prefix $K/pfx. Writes $K/out/<id>/:
#   native/<i>.log, native/times   `docker run --rm --network none <env> bash /fv/run-tests.sh`
#                                  (since the 24th task with --cpus 8 --memory 8g, the machine's limits)
#   native-load/                   the same with a quarter of one CPU (`--cpus 0.25`)
#   fresh/<i>.log,  fresh/times    `hydo run --backend kvm` of the image from a fresh boot each
#   fresh-load/                    three of them on a loaded CPU (see 3b)
#   snap/<i>.json,  snap/times     `hydo inspect exec` from one held stop (the provisioned state);
#                                  the log is inside the JSON answer (collect.py takes it out)
#   build.log, info                the image build and the image's size and sha256
# then deletes the Docker image, the env tar and the hydo image. Stops (exit 3) when the home
# file system has less than $MIN_GB free. Every step has a time limit.
set -u
trap 'chmod -R u+w "/dev/shm/hydo-flaky-$ID" 2>/dev/null; rm -rf "/dev/shm/hydo-flaky-$ID"; [ -n "${HOG:-}" ] && kill "$HOG" 2>/dev/null' EXIT
ID=$1
N=${2:-10}
K=${K:-$HOME/hydo-flaky}
MIN_GB=${MIN_GB:-8}
H=$K/pfx/bin/hydo
O=$K/out/$ID
W=$K/work/$ID
TAR=$K/env/$ID.tar.gz
TAG=fv/$(echo "$ID" | tr 'A-Z_' 'a-z-'):1
export HYDO_KVM_SPIN_SECS=3600 HYDO_BACKEND=kvm
free_gb() { df -k ~ | awk 'NR==2{print int($4/1048576)}'; }
check_disk() { [ "$(free_gb)" -ge "$MIN_GB" ] || { echo "stop: $(free_gb) GB free, under $MIN_GB GB" | tee -a "$O/info"; exit 3; }; }
now() { date +%s.%N; }
el() { awk "BEGIN{printf \"%.2f\", $2 - $1}"; }
chmod -R u+w "$W" 2>/dev/null
rm -rf "$O" "$W"
mkdir -p "$O/native" "$O/fresh" "$O/snap" "$W"
echo "start $(date -u +%FT%TZ) free=$(free_gb)GB" > "$O/info"
check_disk

# 1. Natively: Docker, network off, a fresh container from the same image for every run.
timeout 900 docker load -q -i "$TAR" >> "$O/info" 2>&1
for i in $(seq 1 "$N"); do
  check_disk
  s=$(now)
  timeout 3600 docker run --rm --network none --memory 8g --memory-swap 8g --cpus 8 "$TAG" bash /fv/run-tests.sh > "$O/native/$i.log" 2>&1
  rc=$?
  echo "$i rc=$rc wall=$(el "$s" "$(now)")" >> "$O/native/times"
done
# 1b. Natively on a loaded machine: the same runs with a quarter of one CPU (--cpus 0.25).
mkdir -p "$O/native-load"
for i in $(seq 1 "$N"); do
  check_disk
  s=$(now)
  timeout 3600 docker run --rm --network none --memory 8g --memory-swap 8g --cpus 0.25 "$TAG" bash /fv/run-tests.sh > "$O/native-load/$i.log" 2>&1
  rc=$?
  echo "$i rc=$rc wall=$(el "$s" "$(now)")" >> "$O/native-load/times"
done
docker rmi "$TAG" > /dev/null 2>&1

# 2. The hydo image: the same environment as an image service with a writable root.
cat > "$W/workload.sh" <<'EOF'
#!/bin/sh
# Run the task's tests in the environment's root, then print the whole log to the console
# (gzip and base64 between two marker lines), where the host reads it from the run directory.
hydo-oci env bash /fv/run-tests.sh > /data/fv-test.log 2>&1
rc=$?
echo "FV-RC $rc" > /data/fv-rc
{ echo "FV-BEGIN $rc"; gzip -c /data/fv-test.log | base64; echo "FV-END"; } > /dev/console
EOF
ln "$TAR" "$W/env.tar.gz"
cat > "$W/hydo.yaml" <<'EOF'
# One SWE-bench task's environment (docs/design/flaky-verdicts.md): the evaluation image with the
# gold patch applied, flattened by prepare.sh; the workload runs the task's tests once.
format: 1
name: fv
description: one SWE-bench task's tests, run as they are
guest:
  ram_mib: 3072
data:
  size_mib: 512
root:
  files:
    - {src: workload.sh, dst: /opt/fv/workload.sh, mode: "0755"}
services:
  - name: env
    image: {x86_64: env.tar.gz}
    writable: true
    cmd: sleep infinity
    ready: "true"
workload:
  steps: 1
  run: /opt/fv/workload.sh
  timeout: 7200
  process: run-tests
invariants:
  - id: fv.tests_ran
    msg: the test command ran to its end
    cmd: cat /data/fv-rc
EOF
check_disk
s=$(now)
# The image and the build's intermediates (the layer tree, the data and root file systems: about
# twice the image) live in a tmpfs, so the shared home file system only holds the Docker image
# and the environment tar, never the hydo image.
B=/dev/shm/hydo-flaky-$ID
(cd "$W" && timeout 3600 flock -w 3000 ~/hydo-onboard-build.lock timeout 2400 "$H" build hydo.yaml --build-dir "$B") > "$O/build.log" 2>&1
echo "build rc=$? wall=$(el "$s" "$(now)")" >> "$O/info"
for f in "$B"/*; do
  case "$f" in */explore-ob-fv-disk.img|*/explore-ob-fv-disk.img.json) ;; *) chmod -R u+w "$f" 2>/dev/null; rm -rf "$f" ;; esac
done
rm -f "$W/env.tar.gz"
[ -n "${KEEP_TAR:-}" ] || rm -f "$TAR"
IMG=$B/explore-ob-fv-disk.img
[ -f "$IMG" ] || { echo "no image" >> "$O/info"; exit 2; }
echo "image $(du -h "$IMG") sha256=$(sha256sum "$IMG" | cut -c1-16)" >> "$O/info"
check_disk

# Pull the test log out of a console file.
extract() { awk '/^FV-BEGIN/{p=1;next} /^FV-END/{p=0} p' "$1" | base64 -d 2>/dev/null | gunzip 2>/dev/null; }

# 3. hydo, fresh boots.
for i in $(seq 1 "$N"); do
  s=$(now)
  timeout 7200 "$H" run --backend kvm --image "$IMG" --out "$W/r$i" "disk=none | run" > "$W/r$i.txt" 2>&1
  rc=$?
  r=$(grep -m1 '^RESULT' "$W/r$i.txt")
  echo "$i rc=$rc wall=$(el "$s" "$(now)") $(echo "$r" | grep -o ' fp=[0-9a-f]*' | tr -d ' ') $(echo "$r" | grep -o 'ctl=[0-9a-f]*')" >> "$O/fresh/times"
  c=$(grep -l '^FV-BEGIN' "$W/r$i"/*.log 2>/dev/null | head -1)
  [ -n "$c" ] && extract "$c" > "$O/fresh/$i.log"
  [ "$i" = 1 ] || rm -rf "$W/r$i"
done

# 3b. hydo, fresh boots on a loaded machine: three runs with hydo's process pinned to one CPU
# that a busy loop also runs on (FV_LOAD=0 skips them). The guest's results must not change.
# (The first tasks of the experiment used a systemd scope with CPUQuota=25% instead; a scope of
# its own would leave the build machine's shared budget, so it is not used any more.)
if [ "${FV_LOAD:-1}" = 1 ]; then
  mkdir -p "$O/fresh-load"
  taskset -c 2 sh -c 'exec yes > /dev/null' &
  HOG=$!
  for i in 1 2 3; do
    s=$(now)
    timeout 7200 taskset -c 2 "$H" run --backend kvm --image "$IMG" --out "$W/q$i" "disk=none | run" > "$W/q$i.txt" 2>&1
    rc=$?
    r=$(grep -m1 '^RESULT' "$W/q$i.txt")
    echo "$i rc=$rc wall=$(el "$s" "$(now)") $(echo "$r" | grep -o ' fp=[0-9a-f]*' | tr -d ' ') $(echo "$r" | grep -o 'ctl=[0-9a-f]*')" >> "$O/fresh-load/times"
    c=$(grep -l '^FV-BEGIN' "$W/q$i"/*.log 2>/dev/null | head -1)
    [ -n "$c" ] && extract "$c" > "$O/fresh-load/$i.log"
    rm -rf "$W/q$i"
  done
  kill "$HOG"
fi

# 4. hydo, from one held stop: the first stop of the run conversation (the services are up,
# the workload has not started), each call from that same state.
pt=$("$H" inspect events "$W/r1" --from run:1 --limit 1 2>/dev/null | grep -o '"point": *"run:[0-9]*"' | head -1 | grep -o 'run:[0-9]*')
echo "held stop ${pt:-none}" >> "$O/info"
s=$(now)
sess=$(timeout 3600 "$H" inspect start "$W/r1" --at "${pt:-run:1}" 2> "$W/start.err" | grep -o '"id": *"i-[0-9a-f]*"' | head -1 | grep -o 'i-[0-9a-f]*')
echo "inspect start ${sess:-failed} wall=$(el "$s" "$(now)")" >> "$O/info"
if [ -n "$sess" ]; then
  for i in $(seq 1 "$N"); do
    s=$(now)
    timeout 7200 "$H" inspect exec "$sess" --timeout 36000000 --max-bytes 1048576 --sh \
      'hydo-oci env bash /fv/run-tests.sh > /tmp/fv-test.log 2>&1; echo "FV-BEGIN $?"; gzip -c /tmp/fv-test.log | base64; echo FV-END' \
      > "$W/s$i.json" 2> "$W/s$i.err"
    rc=$?
    echo "$i rc=$rc wall=$(el "$s" "$(now)") $(grep -o '"wall_ms": *[0-9]*' "$W/s$i.json" | head -1) $(grep -o '"truncated": *true' "$W/s$i.json" | head -1)" >> "$O/snap/times"
    cp "$W/s$i.json" "$O/snap/$i.json"
  done
  "$H" inspect stop "$sess" > /dev/null 2>&1
fi

# Keep the first run directory's small files, delete the image.
mkdir -p "$O/r1"
cp "$W/r1.txt" "$W/start.err" "$W"/s*.err "$O/r1/" 2>/dev/null
find "$W/r1" -maxdepth 1 -type f -size -2M -exec cp {} "$O/r1/" \; 2>/dev/null
chmod -R u+w "$W" 2>/dev/null
rm -rf "$W"
echo "end $(date -u +%FT%TZ) free=$(free_gb)GB" >> "$O/info"
