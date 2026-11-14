# Explorer report

| | |
|---|---|
| schedules | 18 |
| strategy | sweep:turns |
| image | scripted-disk.img |
| workers | N |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 0.0 |
| worker_boot_s | 0.0 |
| total_wall_s | N |
| failures | 14 |
| reproduced | 2 |
| repro_runs | 4 |
| vcpus | 2 |

## vCPU turn lengths (`--sweep turns`)

The schedule `disk=none | run` ran with every turn length `q=` of a grid of 12 lengths from 1 to 4096 exits; each pair of neighbouring lengths with different verdict kinds was bisected until they were at most 1 exit apart.

### Boundary between q=7 (violation) and q=8 (ok)

- q=7: `HYDO-ORACLE: VIOLATION [ats.test_passes] pass=1 fail=1 3 vs 2`, fingerprint, two runs: `b4941741bb439f2d` / `b4941741bb439f2d`
  - by hand: `hydo run --backend kvm --vcpus 2 --image <dir>/scripted-disk.img "disk=none q=7 | run"`
- q=8: `HYDO-ORACLE: OK pass=2 of 2`, fingerprint, two runs: `88ba0a3f3c303635` / `88ba0a3f3c303635`
  - by hand: `hydo run --backend kvm --vcpus 2 --image <dir>/scripted-disk.img "disk=none q=8 | run"`
- reproduced: yes

### Boundary between q=32 (ok) and q=33 (violation)

- q=32: `HYDO-ORACLE: OK pass=2 of 2`, fingerprint, two runs: `30031144228340c7` / `30031144228340c7`
  - by hand: `hydo run --backend kvm --vcpus 2 --image <dir>/scripted-disk.img "disk=none q=32 | run"`
- q=33: `HYDO-ORACLE: VIOLATION [ats.test_passes] pass=1 fail=1 3 vs 2`, fingerprint, two runs: `850c26bda963b719` / `850c26bda963b719`
  - by hand: `hydo run --backend kvm --vcpus 2 --image <dir>/scripted-disk.img "disk=none q=33 | run"`
- reproduced: yes

| q | verdict kind | fingerprint |
|---|---|---|
| 1 | violation | `ea72cfa9afb48745` |
| 2 | violation | `16f4a11c9e2fb82a` |
| 5 | violation | `c5656c7b5d5df6e2` |
| 7 | violation | `b4941741bb439f2d` |
| 8 | ok | `88ba0a3f3c303635` |
| 10 | ok | `4b8beeec7fdbf4c2` |
| 21 | ok | `fccaa489c376d2d4` |
| 32 | ok | `30031144228340c7` |
| 33 | violation | `850c26bda963b719` |
| 35 | violation | `eb1b963d67fa994f` |
| 38 | violation | `14e5cca58d399bb2` |
| 44 | violation | `3161c1b9d3a1f708` |
| 93 | violation | `64ad64684a94508a` |
| 199 | violation | `352efa984822f582` |
| 424 | violation | `13a9e0c683d37a4a` |
| 903 | violation | `aef1ea71fe806109` |
| 1923 | violation | `2fa5eee1afa94d6d` |
| 4096 | violation | `743c2778511e3dbb` |

## Failures by class and signature

| class | signature | runs | seeds | reproduced |
|---|---|---|---|---|
| turn-length | `VIOLATION [ats.test_passes] pass=# fail=# # vs #` | 2 |   | 2/2 |

## Minimal reproducers

## Property table over every schedule (hydo guide sdks)

The guest emitted no HYDO-ASSERT records (an image built without the assertion SDK).

