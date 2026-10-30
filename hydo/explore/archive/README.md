These research strategies, runners and their helpers (guided.py, richguided.py, portfolio.py,
abranch.py, compare.py, evaluate.sh, covgrowth.py from M41, M44 and M45; run_list.py and
bench_more_faults.py, moved here in M107) are kept as history and are not shipped. They import
../explore.py, the Python explorer that hydo-explore (hydo/crates/hydo-explore) replaced in M102
and that was deleted in M107. To run one, restore the Python explorer and the modules it imports
from commit 2a58c4b (`git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history` in a scratch checkout).
