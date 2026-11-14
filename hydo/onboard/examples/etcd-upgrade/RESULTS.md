# etcd upgrade: results

This example is the proof of upgrade testing (Milestone 238, docs/design/upgrades.md). It shows
that hydo reaches a documented etcd upgrade bug with one schedule token, and that etcd's
documented upgrade path passes.

## The bug

etcd's issue 19557 ("too many learner members in cluster") hits clusters that were grown with
learners. A learner is a member that receives the log but does not vote; an operator adds a
member as a learner and then promotes it. From etcd 3.5.1 to 3.5.19, a promotion is stored only
in etcd's older membership store (v2store), and the bolt database (v3store) still marks the
member a learner. etcd 3.6 reads membership from v3store. So the first member upgraded to 3.6
sees two learners and stops at its start. The fix is in 3.5.20, and etcd's upgrade notes ask for
3.5.26 or later before 3.6.0 (the etcd blog, "How to Prevent a Common Failure when Upgrading etcd
v3.5 to v3.6", 2025).

## The setup

The manifest grows the cluster as operators do: `etcd1` starts alone, and `etcd2` and `etcd3`
join as learners and are promoted. The image holds etcd 3.5.5, 3.6.0-rc.2 and 3.5.26, built
by `build.sh` from the official release tarballs. A client writes 90 keys in 30 steps, and three
invariants check that acknowledged writes are readable, that revisions never go back, and that
the cluster ends with three voting members.

## The runs

All runs ran on hvx with this milestone's `hydo` on 2026-11-14, each once from a fresh boot. The
image `explore-ob-etcd-upgrade-disk.img` with old 3.5.5, new 3.6.0-rc.2 and mid 3.5.26 has the
sha256 `5506039765f748b7…`.

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=0 \| run` | OK, 90 acknowledged writes readable | `3b95c737a83238b8` |
| `disk=none dseed=0 \| run U=etcd1@5:3.6.0-rc.2` | `VIOLATION [upgrade.started] etcd1 did not start on version 3.6.0-rc.2 before step 5 (it ran 3.5.5)` | `55cbbb73b00549a1` |
| the same schedule, a second fresh boot | the same verdict | `55cbbb73b00549a1` |
| `disk=none dseed=0 \| run U=etcd.all@3/3:3.5.26 U=etcd.all@12/3:3.6.0-rc.2` | OK, 90 acknowledged writes readable | `95f11b3eacae70f6` |

`hydo inspect diagnose <run> --evidence` of the failing run replays it and reads etcd1's log:

```text
{"level":"error","caller":"etcdserver/server.go:308","msg":"bootstrap failed","error":"membership: too many learner members in cluster",...}
{"level":"fatal","caller":"etcdmain/etcd.go:183","msg":"discovery failed","error":"membership: too many learner members in cluster",...}
```

The passing run is etcd's documented path: every member to 3.5.26 first, one member every 3
steps, then every member to 3.6.0-rc.2. Its `result.json` lists six switches under `upgrades`,
each with `"started":true`.

## The same with etcd 3.6.0

The second image holds 3.6.0 in place of 3.6.0-rc.2 (`build.sh 3.5.5 3.6.0 3.5.26`, sha256
`4bb371e024b26d67…`). etcd's issue says that 3.6.0-rc.3 resolved the issue. The release 3.6.0
still stops on such data with the same two log lines, as etcd's blog post says:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=0 \| run U=etcd1@5:3.6.0` | `VIOLATION [upgrade.started] etcd1 did not start on version 3.6.0 before step 5 (it ran 3.5.5)` | `8c1b48147f614cc2` |
| `disk=none dseed=0 \| run U=etcd.all@3/3:3.6.0` | the same, before step 3: the rolling upgrade stops at its first member | `44591083908c663f` |
| `disk=none dseed=0 \| run U=etcd.all@3/3:3.5.26 U=etcd.all@12/3:3.6.0` | OK, 90 acknowledged writes readable | `6ed968a93c4eea92` |

## A blind exploration

`hydo explore --upgrades --seeds 1-24 --workers 1` on the 3.6.0 image drew a `U=` in 11 of the
24 schedules, among kills, network faults, power cuts and disk faults. It found 8 failures and
reproduced each. Two are the upgrade bug, found without a hand-written schedule:

| seed | schedule | class |
|---|---|---|
| 5 | `disk=none dseed=63033b0ca389c35a \| run U=etcd1@0:3.6.0` | `upgrade` |
| 14 | `disk=none dseed=6aa9d61435dbe63e \| run U=etcd.all@8/3:3.6.0` | `upgrade` |

The minimal schedule of the class is seed 5's, with the fingerprint `6c93dceb5756d55d` on two
runs. The other six failures come from the drawn disk faults (`io-errors`, `lying-disk`).
