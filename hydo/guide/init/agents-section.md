## Testing with hydo

This repository is tested with hydo, a deterministic hypervisor that runs the whole system under
seeded faults and replays every failure exactly. The test lives in `hydo/`: `hydo/hydo.yaml`
(the services and allowed faults), `hydo/workload/` (one command per client operation),
`hydo/checks/` (the properties) and `hydo/regressions/` (saved failing schedules).

Before you change anything in `hydo/`, follow the hydo-onboard skill (`hydo guide` says how to
get it; `hydo guide skill` prints it). Read hydo's facts from the installed hydo: `hydo guide
workflow`, `hydo guide manifest` and `hydo guide schema`. After a change, run
`hydo build hydo/hydo.yaml --check`; `hydo migrate` rewrites an old file in the current format.
