# M107: a research/measurement tool. It needs the Python explorer and builder, deleted in M107
# (replaced by the hydo-explore and hydo-build crates): restore them from commit 2a58c4b with
# git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history in a scratch checkout, and run it there.
# Run explore.py against a side module: the workers' hvx_explore gets HVX_DEV (default
# /dev/hvxft), so its /dev/hvx opens go to the side module (hv/hvx/FASTTURN.md).
#   python3 explore_dev.py <explore.py arguments>
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "hydo", "explore"))
import explore  # noqa: E402

explore.EXTRA_ENV.append("HVX_DEV=" + os.environ.get("HVX_DEV", "/dev/hvxft"))
sys.argv = ["explore.py"] + sys.argv[1:]
explore.main()
