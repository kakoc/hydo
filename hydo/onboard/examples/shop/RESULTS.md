# shop: results (fake outside services and protocol faults, 2026-10-24)

The demo project for `fakes:` / `H=` (hydo/onboard/MANIFEST.md, "Fake outside services and protocol
faults"). `shop` charges each order through a fake payment provider (`payments`, with idempotency
keys and a `charge.succeeded` webhook back to the shop) and sends a receipt through a fake email
service (`email`). One planted bug: `src/charge_buggy.go` gives every retry of a charge a new
idempotency key, so a charge that took effect but whose answer was lost is taken again.
`src/charge_fixed.go` keys the charge by the order.

All runs on the bare-metal test machine, one VM at a time, with `hvx_explore` built from this branch
(commit `ec099a3`, which includes the 3584-byte recovery command). Results under
`/var/tmp/m80-fakes/` on the box.

## Images

Built on the build box with `build.sh` (pinned Go 1.26.5, standard library only; pinned
`hvx_seedrng` `c2a440c5…`):

| image | sha256 | notes |
|---|---|---|
| `explore-ob-shop2-buggy-disk.img` | `f331c6be7bade742…` | the planted bug |
| `explore-ob-shop2-fixed-disk.img` | `6c6693200f80880e…` | the fix |
| `explore-ob-redis-proxy-disk.img` | `892810258f1e7c83…` | control: `examples/redis/hydo-proxy.yaml` |

A second build under other names gave the same bytes. (`explore-ob-shop-{buggy,fixed}` are the first
build, before the receipt check was corrected; see below.) Binaries: `hydo-world` `2da400f3…`,
`shop-buggy` `96019e2e…`, `shop-fixed` `4fa42299…`.

## The bug found blind, shrunk, reproduced

`explore.py --protocol-faults --strategy process --seeds 1-100` on the buggy image
(`explore-ob-shop-buggy`, first build): 6 failures, every one re-run with the same fingerprint.

- Four are the planted bug (`payments.charged_at_most_once`): seeds 46 (first found), 78, 84, 85.
  Each schedule carries a protocol fault on `payments.charge` that lets the charge happen while its
  answer is lost: a reset connection, a dropped answer, a 3000 ms delay (the shop waits 1 s).
- The shrinker reduced them to schedules that name only the protocol fault:
  - `disk=none dseed=efb4cfbc49052179 | run H=payments.charge@0-1:drop` -- order 0 charged 3 times
  - `disk=none dseed=fbb60485ae17b0ba | run H=payments.charge@6-9:reset,p=50` -- order 7 charged twice
- On the shop2 images, each of the two minimal schedules twice:

| schedule | buggy | fixed |
|---|---|---|
| `H=payments.charge@0-1:drop` | VIOLATION, o0 charged 3 times, `56657683c9287f08` ×2 | OK `b7417498e0a9ad72` ×2 |
| `H=payments.charge@6-9:reset,p=50` | VIOLATION, o7 charged twice, `3a4622ba213124e9` ×2 | OK `54b2cc1e822a7e53` ×2 |
| no fault (`disk=none dseed=1 \| run`) | OK `fc08af8dda7ff5e6` ×2 | OK `6eeb85c5f19ec59c` ×2 |

The bug needs a protocol fault: the probability that a generated schedule carries one that lets a
charge happen without its answer is about 9% (counted over seeds 1-200; seeds 1-40 hold none, which is
why the run covers 100 seeds).

## Both shop2 images on the same schedules

The same 100 process seeds with `--protocol-faults`, one worker:

- `explore-ob-shop2-buggy`: 4 failures, exactly the planted-bug seeds 46, 78, 84, 85 (fingerprints
  `bdfec48c0a261020`, `92995d9b771d8acd`, `10ff11872c2b94bf`, `250d55df657e11fe`), all re-run
  identically; the shrinker reached the same two minimal schedules and fingerprints as above.
- `explore-ob-shop2-fixed`: 0 failures (726 schedules per hour).

## The protocol sweep (every rule kind, every target, every step)

`--sweep protocol`: 228 one-fault schedules (19 rule variants on `payments.charge`,
`payments.refund` and `email.send`, one workload step at a time, 12 steps).

| image | failures | which |
|---|---|---|
| buggy | 48, all reproduced | exactly `payments.charge` with `delay=3000`, `drop`, `reset` and `status=500,effect=yes`, at each of the 12 steps: the four ways a charge takes effect while its answer is lost |
| fixed | 0 | |

A plain error answer (`status=500` without the effect), every webhook rule (duplicated, reordered,
late, lost) and every fault on refunds and emails pass on both images.

## A check that was too strong (found by the explorer)

Seeds 20 and 54 of the first run failed `email.receipt_eventually` ("acknowledged o2 has no
receipt"); shrunk to `H=email.send@2-4:status=500`. Order 2's receipt failed (500), then the
workload cancelled order 2 at step 3, and the shop correctly sends no receipt for a cancelled order.
The invariant was wrong, not the shop: the workload now records `hydo-world event cancel-requested
o<id>` before each cancellation and the check is `effect-for-acked email.send --ref 'o{id}' --unless
event:cancel-requested`. The shop2 images carry the corrected check; both seeds pass there.

## Control: Redis behind a fault proxy

`examples/redis/hydo-proxy.yaml` puts a `tcp` proxy (`redisp`, port 6380) in front of Redis; the
workload talks to the proxy. `--protocol-faults --strategy process --durable-only --seeds 1-40`:
40 of 40 OK. 26 schedules carried an `H=redisp@...` fault (reset, drop, delay with and without `p=`),
44 connections were actually faulted; the client saw unknown outcomes there, which the invariant
allows. No false violation.

## Power cut

`disk=cut dseed=1 | run H=payments.charge@2-5:drop C=8` (fixed, first build): at the cut the world
carried 18 records (599 bytes) into the recovery boot's command; the recovery world started from
them and the checks saw both boots (OK).

## On the Mac backend (arm64, Milestone 96, 2026-10-30)

The same `hydo.yaml`, built for arm64 with a Mac release made from branch `arm64-parity` (plus
`guest/hydo-world`), on the M2 Pro. Programs cross-built with the pinned Go 1.26.5 of the build
box (`SHOP_GOARCH=arm64 build.sh`): `hydo-world-arm64` `7bc58006…`, `shop-buggy` `5d0411e8…`,
`shop-fixed` `1163302d…`, each twice byte-identical. Images (`hydo build hydo.yaml --set
variant=<v> --set bin=<dir>`, two builds each identical): buggy `explore-ob-shop-arm64.cpio.gz`
`219efa57…`, fixed `665eeb8b…`, data disk `359c73c4…` (both).

Each schedule twice from a fresh boot (`hvmx one`), both runs identical:

| schedule | buggy | fixed |
|---|---|---|
| `disk=none \| run` | OK `b761926ded3e9718` | OK `5b33fba9fd0e82bc` |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION `ref o0 took effect 3 times` `683360fa139cd823` | OK `416ab87f752ed4eb` |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION `ref o0 took effect 3 times (records 1,2,3)`, seen in the recovery boot `46b752e737c4e1c8` | OK `dbff293a9e68486c` |

The power cut: the first boot froze the world and carried 20 records (794 bytes; fixed: 19
records, 636 bytes) on the acknowledged-ids line; `hvmx`'s recovery command (now 3584 bytes, as
`hvx_explore`'s) typed them into the recovery boot, whose world started from them ("recovery
boot: the fake world starts from 20 carried records"), and the checks there counted the three
first-boot charges of order 0.

`hydo explore --protocol-faults --strategy process --seeds 41-50 --workers 2`: buggy 1 failure
(seed 46, as on x86_64), reproduced, shrunk to `disk=none dseed=fbb60485ae17b0ba | run
H=payments.charge@6-9:reset,p=50` (`30208696ee189cba`, the same minimal schedule as on x86_64);
fixed 0 of 10.

## Milestone 136: agent images with hydo-agent 0.2.0

Since M136 every image is an agent image (the guest agent `hydo-agent` as the first process, the
host controller driving it; docs/HISTORY.md "Milestone 136"), so every fingerprint above this
section belongs to a shell-init image of an earlier milestone. Its values, each schedule
run twice with the same fingerprint (docs/REBASELINE.md section 8, which also says how each image
was made):

**x86_64 (hvx), shop-buggy**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `b9568c33cf16930e` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `c0b64b0991adc479` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 25,26) | `fbf9e64b1e159b9d` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `5a8193e66693ed9a` | yes |

**x86_64 (hvx), shop-fixed**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `a38984a542210bd8` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `92e91caf0502fe49` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `a9b788c96aa2ccf3` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `5c628c155c3f0b77` | yes |

**Mac (hvm), shop-buggy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `287ec924a7c24266` | `cb5e8cab8f6ad1e8` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `3ca443aabec98486` | `8c5be03f3908e8e4` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `9496d19fc5de2a84` | `4e904702332fb368` | yes |
| `disk=none \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 24,26) | `36de7a870a2f929b` | `88ac58547d058752` | yes |

**Mac (hvm), shop-fixed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `4837e65705116cdb` | `42df466b523f61a8` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `61a13ebe6297ead4` | `c22306de0f9f6947` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `a6999e49d410101a` | `e1634bd4c544b877` | yes |

## Milestone 140: hydo-agent 0.3.0 and the host-call port (the current values)

Since M140 `hydo-build` stages hydo-agent 0.3.0 and the hvx engine offers the host-call port by
default (docs/HISTORY.md "Milestone 140"), so every image has new bytes and every fingerprint of
the M136 section above is superseded. The current values, each schedule run twice with the same
fingerprint (docs/REBASELINE.md section 9, which also says how each image was made; on x86_64 the
agent's lines travel over the host-call port, on the Mac over the serial line):

**x86_64 (hvx), shop-buggy**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `4bb0a1fe876cdb85` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `8fc62dc461277c3f` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 25,26) | `9671f2b17a161583` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `9d12c1710fb3caf3` | yes |

**x86_64 (hvx), shop-fixed**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `b53ecc59b80008fc` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `ea6404c9dd299dde` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `ae0a30fbdbf638ed` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `944f214df3babac2` | yes |

**Mac (hvm), shop-buggy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `b21d94a70f547510` | `0927ec66fcfd9b81` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `4918bf71ad909f1f` | `a95a5e731424d505` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `c9ebb0c89f804f43` | `66806064cf1f0f7a` | yes |
| `disk=none \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 23,26) | `065c010bd255c768` | `c9bf99c5c985c965` | yes |

**Mac (hvm), shop-fixed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `6f9ef8fe3c5b9aa5` | `0707ac0014b93d7a` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `884e05caed8206ce` | `95eb07beb9a9a85b` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `d30f33a552194f9b` | `ec7c5b8d2145b6f9` | yes |
