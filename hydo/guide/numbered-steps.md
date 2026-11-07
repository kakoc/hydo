# The numbered-steps workload (`workload.run`)

Before the test composer, a workload was one program run over numbered steps, with faults placed
between steps. It is still supported (`hydo guide manifest`, "Schema"), and `hydo compose`
writes this form as a starting point. Use it instead of commands only when:

- the checks need exactly one operation per step with ids `0..steps-1` (an existing oracle
  written that way);
- an existing deterministic load generator can run a step range and is cheaper to reuse than to
  split into commands;
- a generated gRPC driver runs in steps mode (`hydo-grpc steps $HYDO_FROM $HYDO_TO`).

```yaml
workload:
  steps: 30                               # 1..1000; faults are placed at step numbers
  run: /bin/loadgen $HYDO_FROM $HYDO_TO   # runs steps [HYDO_FROM, HYDO_TO); called once per fault-free range
  retry: /bin/loadgen --retry-unacked     # after the faults heal (no power cut), optional
  timeout: 300                            # seconds per call; a client without its own deadline hangs on a paused service
invariants:                               # the checks, as shell commands (same exit codes as check files)
  - id: kv.acked_durable
    msg: every acknowledged write reads back with its value
    cmd: /bin/check-acked $HYDO_ACKED
    final: true                           # at the end (default unless every: is given)
    every: 2                              # also every 2 s of guest time during the workload
    type: always                          # always | always_or_unreachable | eventually (with within:, after:)
```

Step `i` does a fixed operation derived from `i` (no random data, no wall-clock values), appends
`i` to `$HYDO_ACKED` on success and `i ack|failed|unknown` to `$HYDO_OUTCOMES`. 20–50 steps is a
good size. A workload must not have both `workload.run` and commands in `hydo/workload/`.
`hydo guide examples/example-miniflux.yaml` prints a complete example of this form for a project that hydo's
authors did not write (miniflux with PostgreSQL).
