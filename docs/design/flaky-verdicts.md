# Test verdicts of coding tasks between identical runs (2026-11-08)

Status: a measurement on 31 tasks of SWE-bench Lite, with the scripts to run it again in
`hydo/explore/flaky-verdicts/`. No product change.

The question came from a two-day study of hydo as an environment for AI agents (2026-11-08). A
test verdict is the pass or fail of one test. When a coding task's tests give other verdicts on identical runs, a score of
an AI agent or a CI result moves for no reason in the code. The question is how often that
happens natively, and whether hydo removes it.

## The answer in short

No test verdict changed between identical runs, natively or under hydo:

- 31 tasks with 928 listed tests (1,107 tests in the logs);
- 310 native runs, and 190 native runs with a quarter of one CPU;
- 310 hydo runs from fresh boots, 57 from fresh boots on a loaded CPU, 310 from a held snapshot.

So in a prepared environment with the network off, the noise did not reach a verdict. The noise
was there below the verdict. In 27 of 31 tasks the native test logs differed between identical
runs. Under hydo every log of a task was the same, byte for byte, in every task (for the calls
from a snapshot, calls 1 to 9; Limits says why). Each task's
whole guest state at the end of a run (the fingerprint) was the same in every fresh boot. A
loaded host CPU did not change it either.

hydo changed a verdict once, for a reason outside the tests. The image builder sets every file
time to one fixed value, which made Python's byte-code caches stale. One task then failed all 52
tests under hydo and passed natively. After a preparation step made the caches independent of
file times, no verdict differed between native runs and hydo runs.

## Setup

**Tasks.** 31 of the 300 tasks of SWE-bench Lite, from the smaller repositories, with short test
commands. They are 9 of pytest, 4 each of pylint and sphinx, 3 each of flask and django, and 2
each of requests and sympy. One each comes from seaborn, xarray, scikit-learn, and astropy. The
table below lists them. The tests of a task are its `FAIL_TO_PASS` and `PASS_TO_PASS` lists.

**One environment per task.** `prepare.sh` starts from the task's evaluation image on Docker Hub
(`swebench/sweb.eval.x86_64.<task>:latest`). It applies the gold patch and the test patch. Then it
runs the evaluation script's install steps, with the network on. The script comes from the
`swebench` package 3.0.17. The image is flattened into one layer of 550 to 970 MiB (gzip). A
native run and a hydo run start from these same bytes.

**One test command per task.** `run-tests.sh` is the harness's evaluation script without its
patch steps. It activates the conda environment, sets the script's variables and runs the test
command between the harness's two markers. `collect.py` parses each log with the harness's own
parser for the repository. A test passes when the parser says `PASSED` or `XFAIL`. A missing
test fails. A run resolves the task when every listed test passes.

**Where.** Everything ran on the build machine: an x86_64 laptop with 24 threads, 31 GB of RAM,
NixOS with Linux 6.6. The native runs and the hydo runs share one architecture and one
machine. The machine had other work on it at the same time.

Five kinds of runs per task, each from the same state:

| kind | what runs | runs per task |
|---|---|---|
| native | `docker run --rm --network none <env> bash /fv/run-tests.sh`, a new container each time | 10 |
| native, 1/4 CPU | the same with `--cpus 0.25` | 10 (19 tasks) |
| hydo, fresh boot | `hydo run --backend kvm <image> "disk=none \| run"`: boot, start, run the tests | 10 |
| hydo, loaded CPU | the same with the hydo process slowed down (see Limits) | 3 (19 tasks) |
| hydo, from snapshot | `hydo inspect exec` from one stop held after provisioning, each call from that state | 10 |

The hydo image takes the task's environment as an image service with `writable: true` (a
container's writable root). Its workload runs `hydo-oci env bash /fv/run-tests.sh` and prints the
log to the console. The engine is the KVM engine, from `main` at `e384571a`. Guest RAM is 3,072
MiB, with one vCPU. `HYDO_KVM_SPIN_SECS=3600` lets a test compute for up to an hour without a VM
exit. `task-remote.sh` writes the image file `hydo.yaml`.

## Results per task

Each cell gives the resolved runs and the graded runs, the flipping tests, the median seconds of
one run, and the number of different logs. A flipping test is a listed test whose verdict was
not the same in every run of that kind. A graded run is a run whose log the parser could read.

| task | tests | native | native, 1/4 CPU | hydo, fresh boot | hydo, loaded CPU | hydo, from snapshot |
|---|---|---|---|---|---|---|
| astropy__astropy-14365 | 9 | 10/10, 0, 1.1 s, 10 | 10/10, 0, 3.8 s, 10 | 10/10, 0, 9.4 s, 1 | 3/3, 0, 19.0 s, 1 | 10/10, 0, 4.5 s, 1 |
| django__django-10924 | 2 | 10/10, 0, 0.7 s, 2 | 10/10, 0, 3.1 s, 3 | 10/10, 0, 8.5 s, 1 | 3/3, 0, 17.6 s, 1 | 10/10, 0, 4.2 s, 1 |
| django__django-13220 | 5 | 10/10, 0, 0.7 s, 2 | 10/10, 0, 2.2 s, 2 | 10/10, 0, 8.5 s, 1 | 3/3, 0, 17.2 s, 1 | 10/10, 0, 4.0 s, 1 |
| django__django-16046 | 7 | 10/10, 0, 0.5 s, 1 | 10/10, 0, 1.9 s, 4 | 10/10, 0, 8.9 s, 1 | 3/3, 0, 18.3 s, 1 | 10/10, 0, 4.0 s, 1 |
| mwaskom__seaborn-3010 | 3 | 10/10, 0, 1.3 s, 6 | 10/10, 0, 5.7 s, 8 | 10/10, 0, 10.1 s, 1 | 3/3, 0, 20.9 s, 1 | 10/10, 0, 4.9 s, 1 |
| pallets__flask-4045 | 52 | 10/10, 0, 3.6 s, 10 | - | 10/10, 0, 11.9 s, 1 | - | 10/10, 0, 7.2 s, 1 |
| pallets__flask-4992 | 19 | 10/10, 0, 0.5 s, 1 | - | 10/10, 0, 8.7 s, 1 | - | 10/10, 0, 3.7 s, 1 |
| pallets__flask-5063 | 56 | 10/10, 0, 0.7 s, 1 | - | 10/10, 0, 8.9 s, 1 | - | 10/10, 0, 4.0 s, 1 |
| psf__requests-2317 | 141 | 0/10, 0, 4.7 s, 10 | - | 0/10, 0, 12.3 s, 1 | - | 0/10, 0, 7.8 s, 1 |
| psf__requests-863 | 64 | 0/10, 0, 4.1 s, 10 | - | 0/10, 0, 12.0 s, 1 | - | 0/10, 0, 7.3 s, 1 |
| pydata__xarray-4248 | 19 | 10/10, 0, 2.2 s, 8 | 10/10, 0, 11.8 s, 9 | 10/10, 0, 15.3 s, 1 | 3/3, 0, 31.5 s, 1 | 10/10, 0, 8.0 s, 1 |
| pylint-dev__pylint-5859 | 11 | 10/10, 0, 0.9 s, 1 | 10/10, 0, 4.3 s, 5 | 10/10, 0, 8.7 s, 1 | 3/3, 0, 57.5 s, 1 | 10/10, 0, 4.3 s, 1 |
| pylint-dev__pylint-6506 | 8 | 10/10, 0, 1.0 s, 2 | 10/10, 0, 5.0 s, 5 | 10/10, 0, 9.2 s, 1 | 3/3, 0, 58.0 s, 1 | 10/10, 0, 4.6 s, 1 |
| pylint-dev__pylint-7228 | 12 | 10/10, 0, 1.0 s, 2 | 10/10, 0, 5.1 s, 8 | 10/10, 0, 9.1 s, 1 | 3/3, 0, 55.2 s, 1 | 10/10, 0, 4.5 s, 1 |
| pylint-dev__pylint-7993 | 11 | 10/10, 0, 0.9 s, 3 | 10/10, 0, 4.8 s, 6 | 10/10, 0, 8.9 s, 1 | 3/3, 0, 59.0 s, 1 | 10/10, 0, 4.4 s, 1 |
| pytest-dev__pytest-5103 | 65 | 10/10, 0, 2.6 s, 10 | - | 10/10, 0, 10.1 s, 1 | - | 10/10, 0, 5.5 s, 1 |
| pytest-dev__pytest-5227 | 37 | 10/10, 0, 1.1 s, 10 | - | 10/10, 0, 8.6 s, 1 | - | 10/10, 0, 4.0 s, 1 |
| pytest-dev__pytest-5413 | 93 | 10/10, 0, 1.3 s, 10 | - | 10/10, 0, 8.9 s, 1 | - | 10/10, 0, 4.3 s, 1 |
| pytest-dev__pytest-5692 | 70 | 10/10, 0, 1.8 s, 10 | - | 10/10, 0, 9.4 s, 1 | - | 10/10, 0, 4.7 s, 1 |
| pytest-dev__pytest-7168 | 11 | 10/10, 0, 0.5 s, 2 | - | 10/10, 0, 8.1 s, 1 | - | 10/10, 0, 3.5 s, 1 |
| pytest-dev__pytest-7220 | 12 | 10/10, 0, 0.5 s, 2 | - | 10/10, 0, 8.2 s, 1 | - | 10/10, 0, 3.5 s, 1 |
| pytest-dev__pytest-7432 | 78 | 10/10, 0, 1.8 s, 4 | - | 10/10, 0, 9.6 s, 1 | - | 10/10, 0, 4.9 s, 1 |
| pytest-dev__pytest-8365 | 33 | 10/10, 0, 0.8 s, 10 | 10/10, 0, 3.7 s, 10 | 10/10, 0, 8.5 s, 1 | 3/3, 0, 54.0 s, 1 | 10/10, 0, 3.8 s, 1 |
| pytest-dev__pytest-9359 | 55 | 10/10, 0, 0.6 s, 2 | 10/10, 0, 2.3 s, 4 | 10/10, 0, 8.1 s, 1 | 3/3, 0, 50.9 s, 1 | 10/10, 0, 3.5 s, 1 |
| scikit-learn__scikit-learn-13584 | 9 | 10/10, 0, 0.8 s, 5 | 10/10, 0, 3.1 s, 7 | 10/10, 0, 12.0 s, 1 | 3/3, 0, 24.4 s, 1 | 10/10, 0, 5.8 s, 1 |
| sphinx-doc__sphinx-11445 | 10 | 10/10, 0, 0.9 s, 9 | 10/10, 0, 4.5 s, 10 | 10/10, 0, 9.0 s, 1 | 3/3, 0, 56.6 s, 1 | 10/10, 0, 4.3 s, 1 |
| sphinx-doc__sphinx-7975 | 8 | 10/10, 0, 0.9 s, 10 | 10/10, 0, 4.4 s, 10 | 10/10, 0, 8.9 s, 1 | 3/3, 0, 55.4 s, 1 | 10/10, 0, 4.2 s, 1 |
| sphinx-doc__sphinx-8273 | 4 | 10/10, 0, 1.2 s, 10 | 10/10, 0, 6.2 s, 10 | 10/10, 0, 9.1 s, 1 | 3/3, 0, 57.7 s, 1 | 10/10, 0, 4.6 s, 1 |
| sphinx-doc__sphinx-8627 | 19 | 10/10, 0, 0.8 s, 6 | 10/10, 0, 3.7 s, 9 | 10/10, 0, 8.6 s, 1 | 3/3, 0, 54.3 s, 1 | 10/10, 0, 4.1 s, 1 |
| sympy__sympy-13043 | 2 | 10/10, 0, 1.1 s, 10 | 10/10, 0, 5.6 s, 10 | 10/10, 0, 8.6 s, 1 | 3/3, 0, 53.2 s, 1 | 10/10, 0, 4.7 s, 1 |
| sympy__sympy-24909 | 3 | 10/10, 0, 0.8 s, 10 | 10/10, 0, 3.6 s, 10 | 10/10, 0, 8.9 s, 1 | 3/3, 0, 54.4 s, 1 | 10/10, 0, 4.3 s, 1 |

Neither requests task resolves in any native run or in any hydo run. Their `FAIL_TO_PASS` tests call
httpbin.org, and the network is off in every run. The failure is the same in every run.

## Totals

| | tasks with a flipping listed test | tasks with any flipping test in the log | tasks whose logs differ |
|---|---|---|---|
| native | 0 of 31 | 0 of 31 | 27 of 31 |
| native, 1/4 CPU | 0 of 19 | 0 of 19 | 19 of 19 |
| hydo (all three kinds) | 0 of 31 | 0 of 31 | 0 of 31 |

- **Verdicts observed:** 36,265 (every test a log shows, times the graded runs of each kind),
  none of them different from the same test's other runs of that kind.
- **Fingerprints:** every task gave one fingerprint in its 10 fresh boots (31 of 31). The loaded
  runs gave the same fingerprint as the unloaded ones (19 of 19).
- **Native against hydo:** after the byte-code fix, every listed test of every task had the
  same verdict natively and under hydo (0 of 928 tests differ).

## What differs between native runs

The logs differ below the verdict. A diff of two native logs of one task shows these causes:

- Memory addresses in object names, such as `<... object at 0x7fa9074b3d30>`. Linux places a
  process's memory at random addresses on each start.
- Durations, such as `1 passed in 0.01 seconds` against `0.00 seconds`, and pytest's
  `--durations` table.
- Random seeds. sympy's test runner prints its seed and the hash seed of each run. The 20
  native runs of sympy-13043 gave 20 different seeds, for example `random seed: 26160767` with
  `PYTHONHASHSEED=1862165417`. All 13 fresh hydo boots of that task gave `random seed: 90927612`
  and `PYTHONHASHSEED=4065091438`. All 10 calls from the snapshot gave `22405228`: another
  starting state, so another seed, but the same one every time.

None of these reached a verdict in these 31 tasks. A test that depends on a seed, on the
order of a hash table or on a time limit can flip natively. Under hydo it gives one answer
per image and schedule.

## The verdict hydo changed, and why

The first run of pallets__flask-4045 resolved the task in all 10 native runs and in none of
the 20 hydo runs. Every one of its 52 tests failed under hydo with this error:

```
E     File "/opt/miniconda3/envs/testbed/lib/python3.9/site-packages/blinker/base.py", line 93
E       """Connect *receiver* to signal events sent by *sender*.
E       ^
E   SyntaxError: invalid escape sequence \*
```

The cause is the image builder. `hydo-build` writes every file of an image with one fixed time,
so that two builds give the same bytes. Python's byte-code cache (`.pyc`) records the time of its
source file. Under hydo the times no longer match, so Python compiles the installed packages
again on every run. Compiling `blinker` gives a `DeprecationWarning` for an invalid escape
sequence. Flask's `setup.cfg` sets `filterwarnings = error`, so pytest turns the warning into an
error. Natively the caches in the image are valid, and nothing is compiled.

`prepare.sh` now rewrites the caches as checked-hash caches, which compare the source's hash
instead of its time:

```sh
python -m compileall -q -f --invalidation-mode checked-hash /opt/miniconda3/envs/testbed/lib /testbed
```

Both native runs and hydo runs use the new caches. With them, flask-4045 resolved in every run of
both kinds. The same effect showed in psf__requests-2317 as 22 warnings under hydo against 3
natively, with the same verdicts. With the new caches it shows 3 warnings in both.

This matters for every Python image service, not only for test suites. Any program in such an
image compiles its packages again at every start under hydo, and gets the compile-time warnings
that a cached run does not get. Two ways would remove it in hydo itself. `hydo-build` could keep
the file times of the image's layers, which are part of the image's content and so stay
reproducible. Or it could rewrite timestamp caches as checked-hash caches. Neither is done.

## Runtimes

The medians of the per-task medians, in seconds per run:

| native | native, 1/4 CPU | hydo, fresh boot | hydo, loaded CPU | hydo, from snapshot |
|---|---|---|---|---|
| 0.9 | 4.3 | 8.9 | 54.0 (12 tasks) and 18.3 (7 tasks) | 4.3 |

- A fresh hydo run also boots the guest and starts the service. A call from the snapshot skips
  both.
- A call from the snapshot costs a restore and the lock-step transfer of the command and its
  output. Its median is 4.8 times the native median for these short test commands.
- Once per task: building the image took 17 to 38 seconds, and `hydo inspect start` took 7 to 12
  seconds. The image is 2.6 to 4.6 GB, and the environment tar is 550 to 970 MiB.
- Making the environment took about 3 to 7 minutes per task on an Apple Silicon Mac, which ran
  the x86_64 container under emulation. This step is outside the comparison.

## What it means

- **For agent evaluation:** in a prepared, offline environment, run-to-run noise did not change a
  verdict of these 31 tasks. The paper Large Language Monkeys (Brown et al., 2024,
  arXiv:2407.21787) reports 34 of the 300 SWE-bench Lite problems with flaky test suites, which
  is 11.3%. That paper traces the flakiness to unpinned dependencies in the Docker environments
  that run the tests. Such drift is a difference between two builds of an environment, not
  between two runs of one build. This experiment held the environment fixed, so it measured
  only the second kind. hydo pins an image by its bytes, but this experiment did not measure
  drift between builds.
- **For CI:** the same holds. A suite whose verdicts depend on seeds, timing, or load gets one
  answer per image and schedule under hydo, also on a loaded machine. Here no verdict depended on
  them, but 27 of 31 logs did.
- **Where hydo adds noise of its own:** a file property that `hydo-build` normalises (file times)
  changed one task's verdict until the environment was prepared for it. An evaluation
  environment on hydo needs that check, or a fix in `hydo-build`.

## Limits

- **Sample:** 31 small tasks with short test commands, the gold patch only, no agent's patch,
  10 runs per kind. Zero flaky tasks of 31 still allows a true rate of up to about 10% (the 95%
  bound).
- **The flaky tasks of the paper:** the paper does not list its 34 problems, so they could not
  be chosen on purpose.
- **Images not pinned by digest:** the evaluation images were pulled by the tag `latest` on
  2026-11-08, and their digests were not recorded.
- **Two changes of conditions during the run:** the build machine's owner asked for limits. The
  last 7 tasks (django, seaborn, xarray, scikit-learn, astropy) ran their containers with
  `--cpus 8 --memory 8g` instead of no limit. Their hydo runs ran inside the machine's shared
  budget for hydo work.
- **The loaded-CPU hydo runs:** 12 tasks used a systemd scope with `CPUQuota=25%`. The last 7
  pinned hydo to one CPU together with a busy loop, because a scope of its own would leave the
  machine's shared budget. Both slow the host down. Neither changed a fingerprint.
- **The quarter-CPU native runs:** only 19 tasks, the ones after this kind was added.
- **Snapshot calls:** `hydo inspect exec` writes each call's output to `/tmp/hydo-inspect/<k>.out`,
  where `k` is the call's number. Call 10's filename is one character longer than those of calls 1
  to 9, so its guest work differs. In 15 of 31 tasks, call 10 took 1 to 24 engine stops more or
  fewer than calls 1 to 9. In sphinx-7975, tox printed `0.13 seconds` instead of `0.12`. Calls 1 to
  9 had the same stops and the same log in every task, and the "different logs" counts cover them.
- **One engine:** only the KVM engine ran. hvx on the bare-metal machine and the Mac engine were
  not used.

## How to run it again

On a computer with Docker that runs `linux/amd64` containers, and an x86_64 Linux machine with
Docker, `/dev/kvm` and a hydo release at `~/hydo-flaky/pfx`:

```sh
uv venv -p 3.12 $WD/venv && VIRTUAL_ENV=$WD/venv uv pip install swebench==3.0.17
# SWE-bench Lite's 300 rows as one JSON list
for o in 0 100 200; do
  curl -s "https://datasets-server.huggingface.co/rows?dataset=princeton-nlp/SWE-bench_Lite&config=default&split=test&offset=$o&length=100" > $WD/lite-$o.json
done
python3 -c 'import json,sys; print(json.dumps([r["row"] for o in (0,100,200) for r in json.load(open(f"'$WD'/lite-{o}.json"))["rows"]]))' > $WD/lite.json
bash hydo/explore/flaky-verdicts/run-all.sh $WD hydo/explore/flaky-verdicts/tasks.txt 10
$WD/venv/bin/python hydo/explore/flaky-verdicts/report.py $WD/out
```

The scripts:

- `tasks.txt`: the 31 tasks.
- `spec.py`: one task's patches, `setup.sh`, `run-tests.sh` and `task.json`.
- `prepare.sh`: one task's environment, as a docker-save tar.
- `task-remote.sh`: one task's runs on the Linux machine.
- `run-all.sh`: the loop over the tasks, which also makes the next environment during a task's runs.
- `collect.py`: grades the runs.
- `report.py`: prints the table and the totals.
- `logs.py`: writes one kind's logs to files, to compare with `diff`.
- `bm.sh`, `wait-remote.sh`: reach the build machine through `machines.sh`.
- `imgsize.sh`, `list.py`, `peek.py`: choose tasks.
