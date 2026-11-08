# Explorer report

| | |
|---|---|
| schedules | 21 |
| strategy | sweep:turns |
| image | scripted-disk.img |
| workers | N |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 0.0 |
| worker_boot_s | 0.0 |
| total_wall_s | N |
| failures | 13 |
| reproduced | 1 |
| repro_runs | 2 |
| vcpus | 2 |

## vCPU turn lengths (`--sweep turns`)

The schedule `disk=none dseed=8363 | run` ran with every turn length `q=` of a grid of 12 lengths from 64 to 2097152 branches; each pair of neighbouring lengths with different verdict kinds was bisected until they were at most 100 branches apart.

### Boundary between q=18699 (ok) and q=18756 (violation)

- q=18699: `HYDO-ORACLE: OK pass=2 of 2`, fingerprint, two runs: `1dfa0cd60b91dcb4` / `1dfa0cd60b91dcb4`
  - by hand: `hydo run --vcpus 2 --image <dir>/scripted-disk.img "disk=none dseed=8363 q=18699 | run"`
- q=18756: `HYDO-ORACLE: VIOLATION [ats.test_passes] pass=1 fail=1 3 vs 2`, fingerprint, two runs: `54cba22a6c4fc0a2` / `54cba22a6c4fc0a2`
  - by hand: `hydo run --vcpus 2 --image <dir>/scripted-disk.img "disk=none dseed=8363 q=18756 | run"`
- reproduced: yes

| q | verdict kind | fingerprint |
|---|---|---|
| 64 | ok | `06eeee590b989ad3` |
| 165 | ok | `0f3c7c13723fbd6f` |
| 424 | ok | `d28cd2ede4e3b4a3` |
| 1091 | ok | `bea2de09add7d98c` |
| 2806 | ok | `1f7d6c489c06978f` |
| 7222 | ok | `b65689c24557602f` |
| 18585 | ok | `5ed3c06b73c8cfaf` |
| 18699 | ok | `1dfa0cd60b91dcb4` |
| 18756 | violation | `54cba22a6c4fc0a2` |
| 18813 | violation | `ab20215f64231301` |
| 19041 | violation | `edec3223f9c2a938` |
| 19498 | violation | `95d756c0b7315693` |
| 20412 | violation | `7c4a2cc21bba7190` |
| 22239 | violation | `9de19c56ad6a3e20` |
| 25894 | violation | `2f91db6ec4834401` |
| 33204 | violation | `48414ceb4f6a5a95` |
| 47824 | violation | `0d6ee81151022f71` |
| 123068 | violation | `29f5c58c6b8216ea` |
| 316693 | violation | `5ce6293860cce85f` |
| 814957 | violation | `11a179d1e3ca3be3` |
| 2097152 | violation | `fddcd7bd41e92ccd` |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|
| turn-length | `VIOLATION [ats.test_passes] pass=# fail=# # vs #` | 1 |  | 1/1 |

## Minimal reproducers

## Property table over every schedule (hydo/ASSERTIONS.md)

The guest emitted no HYDO-ASSERT records (an image built without the assertion SDK).

