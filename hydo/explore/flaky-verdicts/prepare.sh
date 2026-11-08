#!/bin/bash
# prepare.sh <task dir> <out.tar.gz>: make one task's environment as a docker-save tar.
#
# Runs where Docker can run linux/amd64 containers (here: this Mac's Colima, emulated). From the
# task's SWE-bench evaluation image (task.json "image", pulled from Docker Hub) it applies the gold
# patch and the test patch, runs setup.sh (the evaluation script's own install steps, with the
# network on), rewrites the byte-code caches as checked-hash ones (for the reason below), puts
# run-tests.sh at /fv/run-tests.sh, deletes conda's package cache, and flattens
# the container into one layer with the original image's environment variables, working
# directory /testbed and the command `sleep infinity`. The result is the state every verdict
# starts from, natively (docker load) and in hydo (an image service).
set -euo pipefail
T=$1
OUT=$2
IMG=docker.io/$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["image"])' "$T/task.json")
ID=$(basename "$T")
C=fv-prep-$(echo "$ID" | tr 'A-Z_' 'a-z-')
N=fv/$(echo "$ID" | tr 'A-Z_' 'a-z-'):1
docker pull -q --platform linux/amd64 "$IMG" > /dev/null
docker rm -f "$C" > /dev/null 2>&1 || true
tar -C "$T" -cf - gold.patch test.patch setup.sh run-tests.sh |
  docker run -i --platform linux/amd64 --name "$C" "$IMG" bash -c '
    set -e
    mkdir -p /fvin /fv && tar -C /fvin -xf -
    cp /fvin/run-tests.sh /fv/run-tests.sh
    cd /testbed
    git apply -v /fvin/gold.patch
    git apply -v /fvin/test.patch
    set +e
    bash -x /fvin/setup.sh
    echo "FV-SETUP-RC $?"
    # Byte-code caches that do not depend on file times: hydo-build normalises every file time
    # in the image, which makes timestamp-checked .pyc files stale, and Python then compiles the
    # installed packages again on every run (with compile-time warnings that a project running
    # with filterwarnings=error turns into errors). Checked-hash caches stay valid in both places.
    /opt/miniconda3/envs/testbed/bin/python -m compileall -q -f --invalidation-mode checked-hash \
      /opt/miniconda3/envs/testbed/lib /testbed > /dev/null 2>&1
    echo "FV-COMPILE-RC $?"
    rm -rf /opt/miniconda3/pkgs /root/.cache /fvin
    true' > "$T/prepare.log" 2>&1
grep -q "FV-SETUP-RC 0" "$T/prepare.log" || echo "warning: setup.sh did not end with 0 ($T/prepare.log)"
# Flatten and save inside Docker's own machine (here the Colima VM, `DOCKER_VM_SH`), so the
# 1.5 GB tree does not cross the Docker socket twice; only the compressed tar comes out.
envs=$(docker image inspect -f '{{range .Config.Env}}--change "ENV {{.}}" {{end}}' "$IMG")
# DOCKER_VM_SH runs a command inside that machine; the default is Colima's Lima instance.
VM_SH=${DOCKER_VM_SH:-env LIMA_HOME=$HOME/.colima/_lima limactl shell colima}
timeout 1800 $VM_SH sh -c "docker export $C | docker import --platform linux/amd64 $envs \
  --change 'WORKDIR /testbed' --change 'CMD [\"sleep\", \"infinity\"]' - $N > /dev/null &&
  docker save $N | pigz -1" > "$OUT"
docker rm "$C" > /dev/null
docker rmi "$N" "$IMG" > /dev/null
# Give the freed blocks of Docker's disk back to this computer (Colima's disk file only grows).
$VM_SH sudo fstrim /var/lib/docker > /dev/null 2>&1 || true
ls -l "$OUT"
