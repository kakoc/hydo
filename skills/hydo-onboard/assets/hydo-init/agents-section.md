## Testing with hydo

This repository is tested with hydo, a deterministic hypervisor that runs the whole system under
seeded faults and replays every failure exactly. The test lives in `hydo/`: `hydo/hydo.yaml`
(the services and allowed faults), `hydo/workload/` (one command per client operation),
`hydo/checks/` (the properties) and `hydo/regressions/` (saved failing schedules).

Before you change anything in `hydo/`, run `hydo guide` and follow it; `hydo guide manifest`
is the schema. After a change, run `hydo build hydo/hydo.yaml --check`.
