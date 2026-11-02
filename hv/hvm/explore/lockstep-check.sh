#!/bin/sh
# The permanent test of the Mac engine's lock-step control (Milestone 119): builds the engine and
# hydo-engine's hvm_lockstep_check (and hydo-control's hvm_agent_check), then runs
#   check  the M116 Mac experiment (timed input, reactions, mid-run branches) on the experiment's
#          image explore-ob-redis-oci-arm64 (a shell-init image), which must give M116's recorded
#          fingerprints;
#   run    on the Redis example image: for a shell-init image (built before M136), CTL runs whose
#          host only answers CONT against plain RUNs (the quickstart schedules), which must give
#          the same fingerprints; for an agent image (every image hydo builds since M136, which
#          runs only under the host controller), every schedule driven by the controller from the
#          host's S1 twice and from a fresh boot twice, which must give one fingerprint, one ctl=
#          and one verdict (hvm_agent_check, M143);
#   rewind (Milestone 130, with a fourth argument: the ledger-spin example's image) the same for
#          schedules in the rewind mode (spin=rewind): a counter loop resolved under lock-step
#          control gives one fingerprint (shell image: RUN's), from S1 and from a fresh boot.
#   lockstep-check.sh <dir with Image-explore and explore-ob-redis-oci-arm64{.cpio.gz,-disk.img}> \
#                     <Redis example image explore-ob-redis-arm64.cpio.gz> <out dir> \
#                     [<ledger-spin image explore-ob-ledger-spin-arm64.cpio.gz>]
# The engine is the Rust program hydo-hvm (M143, the default; signed here); ENGINE=c tests the C
# hvmx instead. Exit status 0 when every group agrees. About 5 to 15 minutes on an M2 Pro.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
img=$1; redis=$2; out=$3; spin=${4:-}
mkdir -p "$out"
(cd "$root" && cargo build --release --locked -p hydo-hvm -p hydo-engine --example hvm_lockstep_check \
	&& cargo build --release --locked -p hydo-control --example hvm_agent_check)
if [ "${ENGINE:-rust}" = c ]; then
	sh "$here/build.sh"
	engine=$here/hvmx
else
	engine=$root/target/release/hydo-hvm
	codesign --force --entitlements "$root/hv/hvm/hv.entitlements" -s - "$engine"
fi
bin=$root/target/release/examples/hvm_lockstep_check
agent=$root/target/release/examples/hvm_agent_check
is_agent() {   # an agent image says "agent <protocol>" in /etc/hydo/control (M121)
	gzip -dc "$1" | LC_ALL=C grep -aq 'etc/hydo/control'
}
rc=0
"$bin" check "$engine" "$img/Image-explore" "$img/explore-ob-redis-oci-arm64.cpio.gz" "$out/check" || rc=1
if is_agent "$redis"; then
	"$agent" "$engine" "$img/Image-explore" "$redis" "$out/run" \
		"disk=none | run" "disk=cut | run C=12 R=everysec" "disk=none | run O=redis@5-10:resp:loss,100%" \
		"disk=none tj=7 tjpm=500 tjmax=500 | run" || rc=1
else
	"$bin" run "$engine" "$img/Image-explore" "$redis" "$out/run" \
		"disk=none | run" "disk=cut | run C=12 R=everysec" "disk=none | run O=redis@5-10:resp:loss,100%" \
		"disk=none tj=7 tjpm=500 tjmax=500 | run" || rc=1
fi
if [ -n "$spin" ]; then
	if is_agent "$spin"; then
		"$agent" "$engine" "$img/Image-explore" "$spin" "$out/rewind" \
			"disk=none spin=rewind | run" "disk=none spin=rewind | run P=front@10+2" || rc=1
	else
		"$bin" run "$engine" "$img/Image-explore" "$spin" "$out/rewind" \
			"disk=none spin=rewind | run" "disk=none spin=rewind | run P=front@10+2" || rc=1
	fi
fi
exit $rc
