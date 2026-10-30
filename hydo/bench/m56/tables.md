| defect | portfolio/cut | process | random+nf/cut | random/cut | swarm+nf/cut | swarm-pos | swarm/cut |
|---|---|---|---|---|---|---|---|
| d0-old-front-door | 3/3 (39, 24, 18) med 24 | 3/3 (29, 18, 1) med 18 | 2/3 (-, 6, 8) med 7 | 2/3 (-, 6, 8) med 7 | 3/3 (17, 19, 38) med 19 | 3/3 (49, 51, 10) med 49 | 3/3 (17, 19, 38) med 19 |
| d1-kafka-first | 3/3 (4, 11, 2) med 4 | 3/3 (2, 2, 4) med 2 | 3/3 (2, 3, 6) med 3 | 3/3 (2, 3, 6) med 3 | 3/3 (2, 8, 1) med 2 | 3/3 (3, 16, 4) med 4 | 3/3 (9, 8, 1) med 8 |
| d2-batched-publish-flag | 3/3 (12, 14, 3) med 12 | 3/3 (6, 1, 1) med 1 | 3/3 (5, 6, 5) med 5 | 3/3 (2, 6, 5) med 5 | 3/3 (5, 4, 4) med 4 | 3/3 (3, 1, 17) med 3 | 3/3 (2, 4, 4) med 4 |
| d3-blind-retry | 2/3 (9, -, 2) med 5.5 | 3/3 (4, 4, 4) med 4 | 3/3 (2, 18, 14) med 14 | 3/3 (2, 58, 14) med 14 | 3/3 (5, 13, 1) med 5 | 2/3 (-, 19, 19) med 19 | 3/3 (18, 13, 1) med 13 |
| d4-produce-timeout-definite | 3/3 (18, 27, 21) med 21 | 3/3 (13, 1, 1) med 1 | 2/3 (-, 6, 32) med 19 | 2/3 (-, 6, 32) med 19 | 3/3 (39, 19, 38) med 38 | 3/3 (49, 51, 38) med 49 | 3/3 (39, 19, 38) med 38 |
| d5-myisam | 3/3 (12, 15, 3) med 12 | 3/3 (15, 1, 1) med 1 | 3/3 (3, 4, 5) med 4 | 3/3 (3, 4, 5) med 4 | 3/3 (2, 4, 4) med 4 | 3/3 (3, 1, 10) med 3 | 3/3 (2, 4, 4) med 4 |
| d6-client-deadline-definite | 3/3 (9, 28, 2) med 9 | 3/3 (4, 1, 1) med 1 | 3/3 (2, 6, 14) med 6 | 3/3 (2, 6, 14) med 6 | 3/3 (5, 13, 1) med 5 | 3/3 (49, 19, 19) med 19 | 3/3 (18, 13, 1) med 13 |
| d7-skip-last-record | 0/3 (-, -, -) | 3/3 (28, 27, 4) med 27 | 0/3 (-, -, -) | 0/3 (-, -, -) | 0/3 (-, -, -) | 1/3 (-, 30, -) med 30 | 0/3 (-, -, -) |
| defects found (any rep) | 7 | 8 | 7 | 7 | 7 | 8 | 7 |

| defect | portfolio | random | random+nf | swarm | swarm+nf |
|---|---|---|---|---|---|
| d0-old-front-door | 3/3 (39, 24, 18) med 24 | 2/3 (-, 6, 8) med 7 | 2/3 (-, 6, 8) med 7 | 3/3 (17, 19, 38) med 19 | 3/3 (17, 19, 38) med 19 |
| d1-kafka-first | 3/3 (4, 15, 2) med 4 | 3/3 (2, 7, 6) med 6 | 3/3 (2, 7, 6) med 6 | 3/3 (9, 8, 1) med 8 | 3/3 (2, 8, 1) med 2 |
| d2-batched-publish-flag | 3/3 (12, 14, 3) med 12 | 3/3 (2, 6, 5) med 5 | 3/3 (5, 6, 5) med 5 | 3/3 (2, 4, 4) med 4 | 3/3 (5, 4, 4) med 4 |
| d3-blind-retry | 2/3 (9, -, 2) med 5.5 | 3/3 (2, 58, 14) med 14 | 3/3 (2, 58, 14) med 14 | 3/3 (23, 54, 1) med 23 | 3/3 (5, 54, 1) med 5 |
| d4-produce-timeout-definite | 3/3 (18, 27, 21) med 21 | 2/3 (-, 6, 32) med 19 | 2/3 (-, 6, 32) med 19 | 3/3 (39, 19, 38) med 38 | 3/3 (39, 19, 38) med 38 |
| d5-myisam | 3/3 (12, 15, 3) med 12 | 3/3 (3, 4, 5) med 4 | 3/3 (3, 4, 5) med 4 | 3/3 (2, 4, 4) med 4 | 3/3 (2, 4, 4) med 4 |
| d6-client-deadline-definite | 3/3 (9, 28, 2) med 9 | 3/3 (2, 6, 14) med 6 | 3/3 (2, 6, 14) med 6 | 3/3 (23, 19, 1) med 19 | 3/3 (5, 19, 1) med 5 |
| d7-skip-last-record | 0/3 (-, -, -) | 0/3 (-, -, -) | 0/3 (-, -, -) | 0/3 (-, -, -) | 0/3 (-, -, -) |
| defects found (any rep) | 7 | 7 | 7 | 7 | 7 |
