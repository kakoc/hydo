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
manifest builds the pair of versions to test (`params:`, MANIFEST.md). A service from an image
(`image:`) names an image for each version (Milestone 392, below).

## The token

`U=<target>@<at>[~<off>][/<gap>][+<back>]:<version>` switches services to `<version>` before step
`at`:

| form | what it does |
|---|---|
| `U=etcd1@5:3.6.0` | switches `etcd1` to 3.6.0 before step 5 |
| `U=etcd1@5+4:3.6.0` | the same, and switches it back to its previous version before step 9 |
| `U=etcd1,etcd2@5:3.6.0` | switches `etcd1`, then `etcd2`, both before step 5 |
| `U=etcd.all@5/3:3.6.0` | a rolling upgrade of the group `etcd`: the k-th member before step 5+3k |
| `U=etcd1@5:3.6.0 U=etcd1@9:3.5.5` | an upgrade, then a downgrade written as its own token |
| `U=web@2~8000:2.8.0` | switches `web` to 2.8.0 8 s of guest time into step 2, while the step runs (Milestone 392) |

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

## A switch while a step runs

With `~<off>` (Milestone 392) every switch of the token, its rollbacks too, comes `off` ms of guest
time after its step starts. A thread of the controller of its own (`upgrade::timer`) sleeps that
long in the guest and then makes the three steps above, while the step's workload goes on. The
main thread joins it before the next step's faults, and a power cut cancels it. Several timed
switches of one step run in the order of their offsets, each later one the difference after the
one before. The controller's words name the moment: `UPGRADING web from version 2.7.1 to 2.8.0
8000 ms into step 2: stopping it`.

A deploy comes while users work, and this is the form that shows it. A client in the middle of
its requests meets the stop, the downtime and the new version. A web app's walk keeps the pages
that the old version served open, so their code talks to the new version's API until the walk
loads a page again. Before a step, no page is open: each step of a walk is a fresh browser.

## Versions of an image service

A service from a Docker image gets each version from an image of its own:

```yaml
services:
  - name: vikunja
    image: {x86_64: images/vikunja-vikunja-2.7.1-amd64.docker.tar}
    image_ref: "vikunja/vikunja:2.7.1"
    version: "2.7.1"
    versions:
      - name: "2.8.0"
        image: {x86_64: images/vikunja-vikunja-2.8.0-amd64.docker.tar}
        image_ref: "vikunja/vikunja:2.8.0"
```

hydo-build (`hydo-build/src/oci.rs`) unpacks the version's image into `/oci/<service>@<version>`
and writes its launcher `/etc/hydo/oci/<service>@<version>/run`. The plan
(`hydo-control/src/plan.rs`) takes that launcher as the version's command, so the switch above
needs nothing else. The version keeps what a deploy keeps:

- the service's `mounts:`, so the new image opens the old one's data folders;
- its `workdir:`, `user:`, `env_file:` and `env:`, with the version's `env:` over them;
- its `cmd:` and `entrypoint:` unless the version gives its own;
- its `/tmp` (`/tmp/oci/<service>`), as a restart keeps it.

`@` is no character of a service's name, so a version's root never meets another service's. Each
root has its own mounts, bound once per boot by its own lock and done file. A one-service power
loss (`L=`) removes the versions' done files too, so the next start binds the new file system.

`hydo-oci <service>` runs a command in the root that runs. Each launcher of a service with
versions writes its root's key into `/tmp/hydo/oci-ver.<service>` before it starts the program.
`hydo-oci` reads the file, but a command of the service itself (`$HYDO_SVC` is the service) takes
the root of `$HYDO_VERSION`, so a readiness check right after the switch never runs in the old
root. An image without versions keeps the `hydo-oci` and the launchers it had.

`hydo compose <file> --upgrade-to <service>=<image>:<tag>` pins the version's image by digest in
the lock file and fetches it in `fetch-images.sh`, and writes `version:` with the service's tag
and the version named by the new tag.

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
fingerprint epoch stays 2. Milestone 392 keeps this: an image without versions of an image
service has the bytes it had, and a `U=` without `~` sends the commands it sent. The additions are:

- format 1 of the project: `services[].version` and `services[].versions`;
- the schedule language: the token `U=` (the letter `U` is reserved, so no knob may use it), and
  its `~<off>` (Milestone 392);
- format 1 of the project: `image:`, `image_ref:`, `entrypoint:` and `env:` of a version (Milestone
  392);
- the results format: `Run.upgrades`;
- the control transcript: the report line `hydo-explore-versions:` in runs with `U=`.

## Limits

- A version of an image service with `writable: true` is refused: each root would need its own
  writable layer.
- A timed switch comes when the controller's thread wakes, a few milliseconds of guest time
  after its offset, as a timed kill does.
- The grace period after SIGTERM is fixed at 10 seconds.
- The explorer draws one `U=` per schedule and only the selector `all` for a group.
- The Mac and KVM engines run `U=` through the same controller; the proof of Milestone 238 ran on
  hvx only.
