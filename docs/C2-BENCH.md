# C2: the bug benchmark on shell images and agent images (Milestone 129)

Step C2 of `docs/design/guest-control.md` asks whether a strategy finds each planted defect of the
bug benchmark (`hydo/bench`, defects d0–d12) as often on agent images as on shell images. This
file holds the answer for the bare-metal test machine (hvx). The criterion is a strategy's
detection rate, not single verdicts, because a change of guest timing changes which schedule
finds what.

## 1. The benchmark images cannot be driven by the host controller

The benchmark images (`explore-bench-*`, `explore-nf`, `explore-disk`/`explore-fix-disk` of the
pinned reproducers, and every other `explore-*` image that `hv/hvx/images/build-stack.sh` builds)
run the four-service stack under the stack's own hand-written init: `build-stack.sh`'s
`/sbin/init` sources `/etc/stack-explore.sh`, which reads the explorer's commands on the console.
They carry no `/etc/hydo/hydo.yaml`. The host controller (`hydo-control`) drives manifests: its
`ScheduleController` does what `hydo/onboard/hydo-init.sh` does, from the plan of the image's
embedded manifest.

A copy of `explore-bench-m59-control-pin-kr-disk.img` made into an agent image with
`hydo/bench/agent-patch.sh` (`/sbin/init` = the pinned hydo-agent, `/etc/hydo/control` =
`agent 1`) shows what happens:

- `hydo-bench` (before this milestone) and `hydo-explore` without a manifest start the plain hvx
  engine, which refuses the image: `hvx_explore: the guest's first process is hydo-agent (an agent
  image, /etc/hydo/control): boot it with --control boot`. Every worker fails to start.
- `hydo-explore` in manifest mode (an `explore-ob-*` name) refuses it earlier:
  `manifest_space: no /etc/hydo/hydo.yaml in … (not a hydo-build image?)`, and
  `hydo_control::agent::plan_of_image` gives the same refusal (no embedded manifest).

What C3 needs before these images can run under the agent (one of the two):

1. **A controller for the stack's init**, written from `stack-explore.sh` and the functions of
   `build-stack.sh`'s init that it calls, as `ScheduleController` was written from
   `hydo-init.sh`: its own start and readiness polls for Redis, MySQL, Kafka and the order service
   (with the `/tmp/*.log` files and the `hydo-stack:` lines), the client batches with
   `HYDO_CONC`/`HYDO_DUP` (`Q=`), the timed kill `X=`, `M=` that also sets `sync_binlog=0`, the
   restart of a dead order service before the retry pass with its `hydo-explore: front door log:`
   lines (the M59 crash oracle reads them), the M67 path where the order service cannot come back
   after a `cf=` cut, `explore_oracle`'s restarts of dead stores, and the recovery boot's oracle; or
2. **the stack images rebuilt from a manifest**, which needs the build machine (Nix store paths).
   `hydo/bench/manifest/hydo.yaml` (this milestone) is such a manifest; section 2 shows that the
   controller drives it.

Until one of the two is done, the benchmark images, the pinned reproducers' images and the
stack's explorer images stay shell-only.

## 2. What can be driven: the benchmark guest as a manifest
