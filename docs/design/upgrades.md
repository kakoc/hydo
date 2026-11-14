# Upgrades: a service switched to its next version during a run

This page explains how hydo tests rolling upgrades (Milestone 238). In a rolling upgrade, an
operator replaces the members of a system one at a time with the next version. Old and new
versions then run side by side and read each other's data. The upgrade of one service can also
be rolled back: the service goes back to the version it ran before. Both are a frequent cause of
real outages, so hydo makes them a fault like a kill or a network partition.

## What a manifest declares

A service lists the versions it can run with two keys:

```yaml
services:
  - name: etcd1
    version: "3.5.5"                 # the version that cmd:, setup: and ready: run
    versions:                        # the other versions an upgrade can switch it to
      - name: "3.6.0"                # cmd:, setup: and ready: default to the service's own
      - name: "3.6.0-rc.2"
        cmd: /opt/etcd-rc/etcd --data-dir /data/etcd1
    cmd: /opt/etcd-$HYDO_VERSION/etcd --data-dir /data/etcd1
    ready: wget -q -O /dev/null http://127.0.0.11:2380/version
```

The image holds every version's programs, for example several release binaries in `root.files`.
Each command of the service (its setup, its process and its readiness check) sees the name of the
version it runs as `$HYDO_VERSION`. So one command can serve every version, and a version needs
its own `cmd:` only when its command line differs. A version name is a letter or digit followed by
letters, digits, `.`, `_` and `-`, at most 40 bytes. A `{{param}}` may name a version, so one
manifest builds the pair of versions to test (`params:`, MANIFEST.md). Versions of a service
from an image (`image:`) are not supported yet.

## The token

`U=<target>@<at>[/<gap>][+<back>]:<version>` switches services to `<version>` before step `at`:

| form | what it does |
|---|---|
| `U=etcd1@5:3.6.0` | switches `etcd1` to 3.6.0 before step 5 |
| `U=etcd1@5+4:3.6.0` | the same, and switches it back to its previous version before step 9 |
| `U=etcd1,etcd2@5:3.6.0` | switches `etcd1`, then `etcd2`, both before step 5 |
| `U=etcd.all@5/3:3.6.0` | a rolling upgrade of the group `etcd`: the k-th member before step 5+3k |
| `U=etcd1@5:3.6.0 U=etcd1@9:3.5.5` | an upgrade, then a downgrade written as its own token |

The target is a service, a list, or a group selector, as for `K=` (MANIFEST.md "Groups of
services and partitions"). A target with several services is never switched at once: the services
are switched one after another in the target's order. Each waits until the one before it is ready
again, as an operator's rolling upgrade waits. A schedule may hold several `U=` tokens. The
canonical form writes no `/0` and no `+0`.

## What a switch does

The host controller (`hydo/crates/hydo-control/src/schedule/upgrade.rs`) does each switch in
three steps:

1. It stops the service as an orchestrator does. It sends SIGTERM to the service's process
   tree, then SIGKILL after 10 seconds of guest time if the process has not ended.
2. It switches the service's version.
3. It starts the service on the same data with its setup, its process, and its readiness check.

A switch comes before the step's other faults, after the restarts of `K=`. A service that is down
at its switch, for example after a kill whose restart is still to come, only gets its new version.
Its restart then starts it on that version. A restart policy treats the stop as hydo's own, as it
treats a kill by `K=`.

A service that does not start on its new version ends the run with this verdict:

```text
HYDO-ORACLE: VIOLATION [upgrade.started] etcd1 did not start on version 3.6.0-rc.2 before step 5 (it ran 3.5.5)
```

The upgrade itself is the failure, for example when the new version cannot read what the old one
wrote. A version that starts but serves wrong answers fails the manifest's invariants, as any
other fault does.

## Power cuts and replays

A power cut ends the first boot, and the recovery boot starts again from the snapshot S0. Each
service must start there on the version it ran at the cut, because its data directory holds that
version's files. The run's report therefore gets one more line at the cut, in a run with `U=`:

```text
hydo-explore-versions: etcd1=3.6.0 etcd2=3.6.0
```

The recovery boot reads this line from the control transcript, as it reads the first boot's clock
(`RecoveryInputs.versions`). So `hydo repro` of a run's folder hands the recovery boot the same
versions.

## What the run records

Each switch writes a note, which the run's `result.json` lists as `upgrades` (a results
addition):

```text
HYDO-UPGRADE {"service":"etcd1","step":5,"from":"3.5.5","to":"3.6.0","rollback":false,"started":true}
```

`started` is absent when the service was down at its switch. A run with `U=` also declares and
reaches the assertion `fault.upgraded`, and the explorer's class of its failures is `upgrade`.

## The explorer

`hydo explore --upgrades` draws upgrades for the services with `versions:` from their own random
stream, so every other fault of a seed's schedule stays as it was. In 60% of the schedules it adds
one `U=` token:

- Half of the upgrades are rolling upgrades of a group whose members all have versions, with the
  members 0 to 3 steps apart. The others upgrade one service.
- The version is one of the service's other versions; for a group, one that every member has.
- A quarter of the upgrades roll back 1 to 4 steps after each switch.

The shrinker tries the schedule without each `U=`, without its rollback, and with every member
switched before one step.

## What stays the same

Every addition is opt-in. A manifest without `versions:` gets no `$HYDO_VERSION`, and a schedule
without `U=` sends the commands it sent before. So no recorded fingerprint moves and the
fingerprint epoch stays 2. The additions are:

- format 1 of the project: `services[].version` and `services[].versions`;
- the schedule language: the token `U=` (the letter `U` is reserved, so no knob may use it);
- the results format: `Run.upgrades`;
- the control transcript: the report line `hydo-explore-versions:` in runs with `U=`.

## Limits

- A version of an image service (`image:`) is not supported.
- The grace period after SIGTERM is fixed at 10 seconds.
- The explorer draws one `U=` per schedule and only the selector `all` for a group.
- The Mac and KVM engines run `U=` through the same controller; the proof of Milestone 238 ran on
  hvx only.
