# Example: a hydo/ folder with composed commands

A copy of `hydo/onboard/examples/redis-composed/hydo/` from hydo's repository: Redis with an
append-only file, driven by composed commands (SETs, transfers between accounts, an AOF rewrite
as a serial driver, a read as an anytime command, seed accounts as a `first_` command) and three
checks, plus one saved regression. Its `root.files` point at Redis binaries on hydo's build
machine (`~/hydo/build/redis-server`); in a project, point them at the project's own files.

Print a file with `hydo guide examples/example-redis-composed/hydo/<file>`:

```
hydo.yaml
workload/helper_lib.sh            shared shell code (not a command)
workload/first_accounts           seed accounts, once, before any fault
workload/parallel_driver_set      a SET, several at once
workload/parallel_driver_transfer a transfer between two accounts
workload/serial_driver_rewrite    an AOF rewrite, never beside other drivers
workload/anytime_read             a read at any time
workload/eventually_serves        an end-of-run command
workload/finally_summary          an end-of-run command
checks/final_acked_writes_survive every acknowledged SET is still there
checks/final_total_conserved      the transfers kept the total
checks/eventually_answers         Redis answers again after the faults
regressions/lost-update.schedule  a saved minimal schedule
```

Try: `disk=none | run W=0000002a,p4,q2,r100` (OK with the default atomic transfer; built with
`--set transfer=racy`, two overlapping transfers lose an update) and `disk=cut | run C=12
R=everysec` (Redis's documented fsync window loses acknowledged SETs).
