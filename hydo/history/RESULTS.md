# History checkers on real systems — results (2026-10-24, bare-metal test machine)

Images built on the build box from branch `history-checkers` with the pinned tools
(`hvx_seedrng` `c2a440c5…`, hvx/BUILD-PINS.md); each built twice, byte-identical. In
`~/hydo/build` on the box:

| image | manifest | sha256 |
|---|---|---|
| `explore-ob-redis-hc-disk.img` | `examples/redis/hydo-hist.yaml` | `343d20ba7059094e…` |
| `explore-ob-etcd-hc-3.5.5-disk.img` | `examples/etcd/hydo-hist.yaml`, `--set version=3.5.5` | `4f8aa317ec627344…` |
| `explore-ob-etcd-hc-3.5.6-disk.img` | the same, `--set version=3.5.6` | `2a4a5d4dadb7d8eb…` |

`hvx_explore` compiled from main (`hvx/hvx_explore.c` is unchanged on this branch). Runs one VM
at a time, through `~/bin/hvx-run`. "Fresh boot" = `hvx_explore one` then `hydo-check` on its
output directory; "snapshot" = explore.py's worker pool (`serve`, every schedule from the S1
snapshot), whose `host_checks()` hook produced the verdicts shown. Results:
`/var/tmp/m80-hist/` on the box (`r2`, `r3` fresh boots, `s55`, `s56` snapshot runs).

## Redis (`explore-ob-redis-hc`)

Each step: `SET k:<i> v<i>` (history `kv`, register per key), `RPUSH l<i mod 3> <i>` and
`LRANGE` (history `log`, list-append, `strict-serializable`); the `final:` commands read every
key and list back (in the recovery boot after a cut).

| schedule | runs | verdict | fingerprint |
|---|---|---|---|
| `disk=cut dseed=1 \| run C=12` (appendfsync always) | 2 fresh boots | OK: kv 32 operations on 20 keys linearizable; log 27 operations, no anomaly | `9c80ba97a7fc299b` both |
| `disk=cut dseed=1 \| run C=12 R=everysec` | 2 fresh boots | VIOLATION `[redis.kv_linearizable]` key k:2: write k:2=v2 acknowledged, a read in the recovery boot returned nil (10 of 20 keys); `redis.lists_append` also fails: lost-append, stale-read, G-single-realtime | `0a6151f7548cfeea` both |
| `disk=none dseed=1 \| run` | 1 fresh boot | OK (40 operations) | `63724a499b5d3ffe` |

The lost keys k:2–k:11 are the writes after Redis's last fsync; the same window as the
hand-written invariant's "k:2 lost" on the `-pin` image (REBASELINE). The list-append checker
names the lost element too: "l0: T6 appended 3 and was acknowledged, but T24, which began after
that, read [0] without it".

## etcd (`explore-ob-etcd-hc-3.5.5` / `-3.5.6`)

The workload of `examples/etcd/hydo.yaml`; every put is recorded with the revision etcd
acknowledged, every get with value and `mod_revision`; `final:` does one linearizable range read
through each member. The only oracle is the linearizability check.

**M78's schedule does not trigger 14685 on these images.** `disk=none dseed=1 | run K=etcd1@10+1
P=etcd3@26+1` gives OK on 3.5.5-hc (`b185be4330b0b398`, fresh boot and snapshot identical): the
three members' final reads agree on every key's mod_revision, so the bug did not fire. The
recording adds console writes to the workload, which moves guest timing (the same effect as M77's
longer init, which moved which schedules hit 14685's window). On 3.5.6-hc it is OK too
(`364b85ccc7e0b175`).

**A sweep of 36 schedules on 3.5.5-hc** (from the snapshot): M60's hand-written kill check
(`K=etcd<n>@<5,10,15,20,25>+2`), M60's cut reproducer (`disk=cut dseed=4127cc1347fae6ba | run
C=30`), and M78's variants (kill etcd1 before step 10, pause etcd2 or etcd3 before step 23..29;
kill etcd1 before step 7..13, pause etcd3 before step 26). **10 of 36 violate
`etcd.kv_linearizable`:**

| schedule | fingerprint | witness |
|---|---|---|
| `K=etcd1@20+2` | `81ca75fb137c936c` | k61: an unknown-outcome put, the retry acknowledged as v61@96, etcd1 reads v61@97, etcd2 reads v61@96 (29 keys) |
| `K=etcd2@15+2` | `d2be35fece6bd43f` | k45: put acknowledged with version 52, a later read through etcd2 returned 53 (45 keys) |
| `K=etcd2@25+2` | `c9f7b1225575ce6a` | k77: acknowledged 84, read 85 (15 keys) |
| `K=etcd1@10+1 P=etcd2@{23,24,25,26,27,28}+1` | `92a33886…`, `c5b42155…`, `525750ee…`, `b5d6c926…`, `a105d5cc…`, `0ee14835…` | k32 (k30 for 26): unknown-outcome put, retry acknowledged @97, etcd1 reads @98 |
| `K=etcd1@10+1 P=etcd3@29+1` | `cbdf3ba5ca184c9a` | k31: unknown-outcome put, retry @96, etcd1 reads @97 |

The other 26 pass, among them every kill of etcd3, the cut reproducer, and M78's exact schedule.
In every failure one member's mod_revisions are one ahead of the others for every key written
after some point — the off-by-one revision of etcd issue 14685 (a member re-applies entries after
a crash that followed an online defrag). M78's variant `P=etcd3@29+1` fails here as it did there.

**3.5.6:** the same 10 schedules on 3.5.6-hc (from the snapshot) all pass: `K=etcd1@20+2`
`d3a0e363d4eef091`, `K=etcd2@15+2` `a4d762b561fd1db8`, `K=etcd2@25+2` `3c920a0ecb8e9e61`,
`K=etcd1@10+1 P=etcd2@{23..28}+1` `5decf979…`, `22969f75…`, `177b5ecc…`, `f96966e3…`, `ab5d15de…`,
`6be12988…`, `K=etcd1@10+1 P=etcd3@29+1` `efbd507be25999ef`; M78's exact schedule `364b85ccc7e0b175`
(the same value as its fresh boot).

**Reproduction** (each fresh boot twice; the snapshot value from the sweep in brackets):

| schedule | 3.5.5-hc | 3.5.6-hc |
|---|---|---|
| `K=etcd1@10+1 P=etcd2@26+1` | VIOLATION k30, `b5d6c926ec87896e` ×2 (snapshot `b5d6c926…`) | OK, `f96966e395dede51` ×2 (snapshot `f96966e3…`) |
| `K=etcd1@10+1 P=etcd3@29+1` | VIOLATION k31, `cbdf3ba5ca184c9a` ×2 (snapshot `cbdf3ba5…`) | OK, `efbd507be25999ef` ×2 (snapshot `efbd507b…`) |
| `K=etcd2@15+2` | VIOLATION k45 (acknowledged version 52, read 53), `d2be35fece6bd43f` ×2 (snapshot `d2be35fe…`) | OK, `a4d762b561fd1db8` ×2 (snapshot `a4d762b5…`) |
| `K=etcd1@10+1 P=etcd3@26+1` (M78) | OK, `b185be4330b0b398` (fresh boot and snapshot) | OK, `364b85ccc7e0b175` (fresh boot and snapshot) |

Every fresh boot gave the same fingerprint twice, and the same as the run from the snapshot; the
host verdict is a pure function of the console, so it was the same too. A typical witness
(`K=etcd1@10+1 P=etcd3@29+1`): "k31: p1 write k31=v31 (unknown outcome, the kill at step 10);
p10 write k31=v31@96 (the retry, acknowledged); p20 read k31 -> v31@97 (etcd1); p21 read k31 ->
v31@96 (etcd2)" -- two members return different versions for the same value, which no order of
these four operations explains, whether or not the unknown-outcome put took effect.

## Cost and size

A history line is 40–60 bytes; the etcd run records about 370 events and its whole console is 22 KB (64 KiB allowed),
including the three ~1.1 KB range-read lines), the Redis run about 120. The host check takes
under a second per run (Python, the box's 3.12). The recording itself moves guest timing, which
is why the M78 schedule stopped hitting 14685's window on the new image.

## The test composer with history checks (`explore-ob-redis-composed-hc`)

`examples/redis-composed-hist/` (image `cd399d0b…`, two builds identical): three composed
commands (`parallel_driver_set` = SET as a register history, `parallel_driver_append` = RPUSH,
`anytime_read` = GET and LRANGE, each recorded with process `$HYDO_CMD_ID`) and two history checks
given as `hydo/checks/linearizable_redis.kv_linearizable` and
`hydo/checks/list_append_redis.lists_append` (each file is its read-back). Fresh boots, engine
compiled from main at `b2eea37`:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=cut dseed=1 \| run C=12 W=0000002a,p4,q2,r100` | OK (kv: 19 operations on 10 keys; log: no anomaly) | `df3918851b213951` twice |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | VIOLATION `[redis.kv_linearizable] key k:16`: acknowledged write lost (witness: p16's write, the recovery boot's read of nil); `redis.lists_append`: lost-append (l0 lost the acknowledged 14) | `ea9aa4a58a3517c7` twice |

With the same engine binary, `explore-ob-redis-hc` `disk=cut dseed=1 | run C=12` still gives
`9c80ba97a7fc299b`, and that image rebuilt from the merged tree is byte-identical (`343d20ba…`).
