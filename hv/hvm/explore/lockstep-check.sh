#!/bin/sh
# The permanent test of hvmx's lock-step control (Milestone 119): builds hvmx and
# hydo-engine's hvm_lockstep_check, then runs
#   check  the M116 Mac experiment (timed input, reactions, mid-run branches) on the experiment's
#          image explore-ob-redis-oci-arm64, which must give M116's recorded fingerprints;
#   run    CTL runs whose host only answers CONT against plain RUNs on the release's Redis
#          example (the quickstart schedules), which must give the same fingerprints;
#   rewind (Milestone 130, with a fourth argument: the ledger-spin example's shell image) the same
#          for schedules in the rewind mode (spin=rewind): a counter loop resolved under lock-step
#          control gives RUN's fingerprint, from S1 and from a fresh boot.
#   lockstep-check.sh <dir with Image-explore and explore-ob-redis-oci-arm64{.cpio.gz,-disk.img}> \
#                     <Redis example image explore-ob-redis-arm64.cpio.gz> <out dir> \
#                     [<ledger-spin image explore-ob-ledger-spin-arm64.cpio.gz>]
# Exit status 0 when every group agrees. About 5 minutes on an M2 Pro.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
img=$1; redis=$2; out=$3; spin=${4:-}
mkdir -p "$out"
sh "$here/build.sh"
(cd "$root" && cargo build --release --locked -p hydo-engine --example hvm_lockstep_check)
bin=$root/target/release/examples/hvm_lockstep_check
rc=0
"$bin" check "$here/hvmx" "$img/Image-explore" "$img/explore-ob-redis-oci-arm64.cpio.gz" "$out/check" || rc=1
"$bin" run "$here/hvmx" "$img/Image-explore" "$redis" "$out/run" \
	"disk=none | run" "disk=cut | run C=12 R=everysec" "disk=none | run O=redis@5-10:resp:loss,100%" \
	"disk=none tj=7 tjpm=500 tjmax=500 | run" || rc=1
if [ -n "$spin" ]; then
	"$bin" run "$here/hvmx" "$img/Image-explore" "$spin" "$out/rewind" \
		"disk=none spin=rewind | run" "disk=none spin=rewind | run P=front@10+2" || rc=1
fi
exit $rc
