#!/bin/sh
# The permanent test of the Mac engine's lock-step control (Milestone 119; on agent images since
# Milestone 145, when the shell-init images' engine path was deleted): builds the engine,
# hydo-engine's hvm_lockstep_check and hydo-control's hvm_agent_check, then runs
#   check  the M116 Mac experiment (timed input, reactions, mid-run branches) on the Redis
#          example's agent image, which must give the recorded fingerprints (hvm_lockstep_check);
#   run    on the same image, every schedule driven by the host controller from the host's S1
#          twice and from a fresh boot twice, which must give one fingerprint, one ctl= and one
#          verdict (hvm_agent_check, M143);
#   rewind (Milestone 130, with a fourth argument: the ledger-spin example's agent image) the same
#          for schedules in the rewind mode (spin=rewind): a counter loop resolved under lock-step
#          control gives one fingerprint, from S1 and from a fresh boot.
#   lockstep-check.sh <dir with Image-explore> <Redis example image explore-ob-redis-arm64.cpio.gz> \
#                     <out dir> [<ledger-spin image explore-ob-ledger-spin-arm64.cpio.gz>]
# The engine is the Rust program hydo-hvm (M143; signed here; the C hvmx was deleted in M147).
# Exit status 0 when every group agrees. About 5 to 15 minutes on an M2 Pro.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
img=$1; redis=$2; out=$3; spin=${4:-}
mkdir -p "$out"
(cd "$root" && cargo build --release --locked -p hydo-hvm -p hydo-engine --example hvm_lockstep_check \
	&& cargo build --release --locked -p hydo-control --example hvm_agent_check)
engine=$root/target/release/hydo-hvm
codesign --force --entitlements "$root/hv/hvm/hv.entitlements" -s - "$engine"
bin=$root/target/release/examples/hvm_lockstep_check
agent=$root/target/release/examples/hvm_agent_check
rc=0
"$bin" "$engine" "$img/Image-explore" "$redis" "$out/check" || rc=1
"$agent" "$engine" "$img/Image-explore" "$redis" "$out/run" \
	"disk=none | run" "disk=cut | run C=12 R=everysec" "disk=none | run O=redis@5-10:resp:loss,100%" \
	"disk=none tj=7 tjpm=500 tjmax=500 | run" || rc=1
if [ -n "$spin" ]; then
	"$agent" "$engine" "$img/Image-explore" "$spin" "$out/rewind" \
		"disk=none spin=rewind | run" "disk=none spin=rewind | run P=front@10+2" || rc=1
fi
exit $rc
