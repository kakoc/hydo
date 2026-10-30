# Branch report (marker `hydo-stack-marker:`)

| | |
|---|---|
| schedules | 12 |
| branch_points | 9 |
| runs | 36 |
| variants_per_point | 3 |
| tjpm | 100 |
| sweep_wall_s | N |
| runs_per_hour | N |
| mean_run_s | 5.5 |
| failures | 9 |
| reproduced_standalone | 9 |
| timing_only_failures | 1 |
| total_wall_s | N |

## Failures

| run | class | timing only | verdict | reproduced standalone |
|---|---|---|---|---|
| seed-1-v1 | io-errors | yes | `ERROR harness: guest did not finish (I/O error)` | yes |
| seed-7-base | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=11` | yes |
| seed-7-v1 | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=11` | yes |
| seed-7-v2 | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=11` | yes |
| seed-7-v3 | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=11` | yes |
| seed-12-base | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=27` | yes |
| seed-12-v1 | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=27` | yes |
| seed-12-v2 | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=27` | yes |
| seed-12-v3 | lying-disk | no | `HYDO-ORACLE: VIOLATION lost acknowledged write order=27` | yes |
