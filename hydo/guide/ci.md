# Running hydo in continuous integration (hydo ci run)

Use this page to run hydo on every pull request and every push of a repository that has a `hydo/`
folder. One command, `hydo ci run`, does all the work, the same way on a laptop and in every CI
system. The CI system only runs it, keeps its files and, if you want, posts its report.

`hydo ci run` does these steps in this order:

1. It builds the image, or takes it from the image cache when nothing the image is built from has
   changed.
2. It explores the image with the same seed range on every run.
3. It builds and explores the base commit's image the same way (a pull request's target).
4. It replays every finding's minimal schedule on the other commit's image, so each finding gets
   a state that a run proves (the table in section 7).
5. It replays the saved regressions in `hydo/regressions/`. A regression whose `# params:` line
   names other build parameters than the image's is left out, as `hydo repro --regressions`
   leaves it out (`hydo guide workflow`).
6. It marks the findings that `hydo/known-findings.yaml` accepts (section 9).
7. It writes `ci-run.json`, `report.md`, `comment.md` and `junit.xml` into its output
   directory (default `hydo-ci`).

Its exit status is 0 when the run passed and 1 when it failed by the project's policy
(`fail_on`). It is 2 when the run could not complete, for example when the image did not build.

## 1. Try it on your machine

In a repository with `hydo/hydo.yaml`, on a branch:

```sh
hydo ci run --base main
```

It prints the report at the end and writes it to `hydo-ci/report.md`. Without `--base` (and
outside a pull request) it explores this commit only and lists every finding without a
comparison. `hydo ci --help` lists every option. Add `hydo-ci/` to `.gitignore`.

## 2. Settings: hydo/ci.yaml

The file is optional, sits next to `hydo/hydo.yaml`, and is not put into the image: changing a
budget keeps the image and its fingerprints. A flag of `hydo ci run` wins over the file.

```yaml
format: 1
engine: kvm                     # kvm (Linux CI runners), hvx, or hvm (an Apple Silicon Mac)
build: ./build.sh               # builds the files hydo.yaml stages (at this commit and the base's)
inputs:                         # what those files are built from
  shop: [src/, build.sh]
set:                            # hydo build --set KEY=VALUE for the manifest's params:
  variant: fixed
pr:                             # what a pull request explores
  seeds: 1-60
  strategy: process
  options: [--protocol-faults]
  workers: 2
  budget: 10m                   # the wall time the whole run may take (section 10)
push: {seeds: 1-60}             # a push to a branch (default: as pr)
nightly: {seeds: 1-2000}        # a scheduled run (default: as push)
fail_on: [new, reappeared, regression]
```

- `seeds` is a fixed range, so the head and the base explore the same schedules. The default is
  `1-50` with the `process` strategy.
- `inputs` lets `hydo ci run` compute the image's input key before it runs `build`. When the
  inputs and every other file the image is made from are unchanged, the cached image is used and
  `build` does not run.
- `fail_on` takes finding states (section 7) and `regression` (a saved regression that did not
  give its expected answer). The default fails a pull request only on what it caused.

`hydo guide schema` does not cover this file; its schema is `hydo/schema/ci-v1.json` in hydo's
repository.

## 3. GitHub Actions

Add `.github/workflows/hydo.yml`:

```yaml
name: hydo
on:
  pull_request:
  push: {branches: [main]}
  schedule: [{cron: "17 2 * * *"}]
permissions: {contents: read, pull-requests: write}
jobs:
  hydo:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: kakoc/hydo/action@v1
        with:
          hydo-version: 0.8.0
```

The action installs that hydo release, checked against the release's `SHA256SUMS`. It caches
`~/.hydo` (the release and the image cache), fetches the pull request's base commit and runs
`hydo ci run`. It keeps the output directory as an artifact and writes the report into the job
summary. It also posts `comment.md` as one pull-request comment, which it edits on every push. The job fails
with `hydo ci run`'s exit status. A pull request from a fork gets a read-only token: the comment
is then skipped with a warning, and the report is still in the job summary. Section 13 has a
recorder workflow that posts the comment for such a pull request too.

The comment shows only what the pull request changed against its base: the findings in the
states `new`, `reappeared`, `changed` and `fixed` (section 7). Each finding has its minimal
schedule, the `hydo run` line that replays it, and the `hydo ci accept` line for it. Findings
already on the base and accepted findings are only counted there, and `report.md` lists them.
`hydo ci comment hydo-ci` prints the comment of a recorded run again.

Inputs:

- `hydo-version` (required) and `project` (the directory that holds `hydo/hydo.yaml`).
- `mode`, `args` (more `hydo ci run` arguments) and `out`.
- `comment`, `artifact-name` and `github-token`.
- `merge` (section 11), `history` (section 12), `recorded` and `pull-request` (section 13).

## 4. GitLab CI

Copy hydo's template, `action/gitlab/hydo.gitlab-ci.yml` of https://github.com/kakoc/hydo at an
`action-v1.*` tag, into the repository (here as `ci/hydo.gitlab-ci.yml`). Include it, and extend
its hidden job:

```yaml
include:
  - local: ci/hydo.gitlab-ci.yml
hydo:
  extends: .hydo
  variables:
    HYDO_VERSION: "0.8.0"
  tags: [kvm]
```

GitLab shows `junit.xml` as the merge request's test report. To get the report as one merge
request note, add a project access token with the `api` scope as the CI/CD variable
`HYDO_GITLAB_TOKEN`. The runner must give the job `/dev/kvm` (section 6).

## 5. Any other CI system

Every other system uses the same command and files:

1. Install a hydo release on the runner (`hydo setup --get <version>`, or the release's
   `install.sh`).
2. Check out the repository with enough history to contain the base commit.
3. Run `hydo ci run --base <the target branch or commit>`.
4. Keep the `hydo-ci/` directory as the job's artifact.
5. Give the system `hydo-ci/junit.xml` as the test report.
6. Post `hydo-ci/comment.md` as a comment, if the system can, and edit it on the next push.
7. Fail the job with `hydo ci run`'s exit status.

Every finding and every saved regression is one test case in `junit.xml`. A finding in a state
that `fail_on` names is a failed test, and a finding already on the base is a skipped test. A
regression left out for its build parameters is a skipped test, and `report.md` names it. The
test's text has the minimal schedule and the command that reproduces it.

`hydo ci run` reads the commit, the base, the change number and the job's link from these
variables. A flag always wins: `--commit`, `--base`, `--base-branch`, `--change`.

| CI system | Detected by | Commit | Base | Change | Link |
|---|---|---|---|---|---|
| GitHub Actions | `GITHUB_ACTIONS` | `GITHUB_SHA` | `GITHUB_BASE_REF` | `GITHUB_REF` (`refs/pull/<n>/merge`) | `GITHUB_SERVER_URL`, `GITHUB_REPOSITORY`, `GITHUB_RUN_ID` |
| GitLab CI | `GITLAB_CI` | `CI_COMMIT_SHA` | `CI_MERGE_REQUEST_DIFF_BASE_SHA`, `CI_MERGE_REQUEST_TARGET_BRANCH_NAME` | `CI_MERGE_REQUEST_IID` | `CI_JOB_URL` |
| CircleCI | `CIRCLECI` | `CIRCLE_SHA1` | (none: give `--base`) | `CIRCLE_PR_NUMBER`, `CIRCLE_PULL_REQUEST` | `CIRCLE_BUILD_URL` |
| Jenkins | `JENKINS_URL` | `GIT_COMMIT` | `CHANGE_TARGET` | `CHANGE_ID` | `BUILD_URL` |
| Buildkite | `BUILDKITE` | `BUILDKITE_COMMIT` | `BUILDKITE_PULL_REQUEST_BASE_BRANCH` | `BUILDKITE_PULL_REQUEST` | `BUILDKITE_BUILD_URL` |
| Azure Pipelines | `TF_BUILD` | `BUILD_SOURCEVERSION` | `SYSTEM_PULLREQUEST_TARGETBRANCH` | `SYSTEM_PULLREQUEST_PULLREQUESTNUMBER` | `SYSTEM_COLLECTIONURI`, `SYSTEM_TEAMPROJECT`, `BUILD_BUILDID` |

With a base branch and no base commit, the base is the merge base of `HEAD` and
`origin/<branch>`. The run's kind (`--mode`) follows the event: a pull request is `pr`, a
scheduled run is `nightly`, anything else is `push`. Only a `pr` run finds its base by itself.

## 6. Runners

hydo runs guests with the KVM engine on Linux runners and needs `/dev/kvm` there. Check a runner
with:

```sh
hydo doctor --backend kvm
```

- GitHub's standard `ubuntu-24.04` runners have KVM. The action makes `/dev/kvm` usable for the
  runner's user.
- On other hosted CI systems a job often has no `/dev/kvm`. Use a self-hosted runner, a machine
  type with nested virtualization, or a Kubernetes node with a KVM device plugin.
- A self-hosted Linux machine with VT-x can run the `hvx` engine (`engine: hvx`), the only one
  with several vCPUs and the PMU branch clock.
- GitHub's macOS runners cannot run a guest. A self-hosted Apple Silicon Mac runs `hvm`.

A finding replays with the same fingerprint only on the same engine and, on KVM, the same host CPU
(`engine.host_cpu` in every result). On Intel that is the same CPU model; on AMD, the EPYC models
measured share their fingerprints. Elsewhere hydo compares verdicts.

## 7. The states of a finding

A finding is identified by its key, `f1:<kind>:<hash>`, made from what the check said: the
bracketed property id of the verdict (`VIOLATION [shop.no_double_charge]`), else the verdict with
its numbers as `#`. Rewording a check's message keeps the key when the check prints an id.

| State | Meaning | How the run decides it |
|---|---|---|
| `new` | This change introduced it. | Found here; its minimal schedule passes on the base's image (or the base cannot run the schedule: it lacks what the schedule names). |
| `pre-existing` | The base has it too. | Found here; the base's exploration missed it, but the base fails its schedule the same way. |
| `present` | Still there. | Found on the base and here. |
| `changed` | Both fail, differently. | One commit's schedule fails on the other commit with another finding key. |
| `fixed` | This change removed it. | Found on the base; its minimal schedules pass here, and this run's exploration did not find it. |
| `unverified` | Not decided. | A replay was refused or gave no result. A person should look. A run that its budget cut short (section 10) calls a finding whose replay passes `unverified`, not `fixed`. |
| `reappeared` | Fixed earlier, back now. | Found here, the base passes its schedule, and the history (section 12) recorded it fixed on the branch. |
| `accepted` | A person decided it is expected. | Its key is in `hydo/known-findings.yaml` (section 9). `ci-run.json` keeps the state it would have had. |

A finding is never called fixed because an exploration did not find it: only a passing replay
proves that.

## 8. Comparing two recorded runs

```sh
hydo ci compare base/ci-run.json head/ci-run.json --out compared/
```

It compares by finding key only and runs nothing, so it calls nothing `fixed`: a base finding
that the head did not find is listed as `unverified`.

## 9. Accepting a finding

A finding the team expects, for example a documented trade-off of a setting, would otherwise
fail every pull request that brings it back. Accept it by its key from a recorded run:

```sh
hydo ci accept f1:invariant:5ef6 --reason "appendfsync everysec: Redis documents the loss of up to one second of writes"
```

- The key comes from `ci-run.json` or `junit.xml`. A unique prefix of a key of the run is enough.
- `--from` names the run (default `hydo-ci/ci-run.json`). The run gives the entry its title and
  minimal schedule.
- `--who NAME` records who accepted it. Without the flag the field stays empty; hydo never fills
  it from a login or a host name.
- The entry goes into `hydo/known-findings.yaml`. Commit the file with the change it belongs to,
  so reviewers see it.

The file is not put into the image, so accepting a finding keeps the image and its fingerprints.
Its schema is `hydo/schema/known-findings-v1.json` in hydo's repository:

```yaml
format: 1
accepted:
  - key: f1:invariant:5ef675c8c9
    title: "VIOLATION [redis.acked_writes_survive] key k:# lost"
    schedule: "disk=cut dseed=70c9101f818fe74 | run C=14"
    reason: "appendfsync everysec: Redis documents the loss of up to one second of writes"
    who: ""
    date: "2026-11-13"
```

`hydo ci run` lists an accepted finding in the report's section `Accepted`, with the state it
would have had, and `fail_on` never fails on it. In `junit.xml` it is a skipped test. When the
exploration does not find an accepted key, the run replays the entry's schedule. If the schedule
passes, the report's section `Accepted findings this run did not find` names the entry, so that
the file can be cleaned.

## 10. Time budgets

`budget:` in the settings of a run's mode (`pr`, `push`, `nightly`), or `--budget 10m`, is the
wall time the whole run may take. It accepts `90s`, `10m`, `1h30m` or a number of seconds.

- The seed range stays fixed. The exploration stops drawing new schedules when the rest of the
  run would not fit: the base's exploration of the same seeds, the replays, and the regressions.
- The cut is at a seed boundary in seed order. The explored seeds are always the first ones of
  the range, so they do not depend on how fast the runner is.
- The base explores exactly the seeds the head explored, so the comparison stays like for like.
- The report says how far the run got, for example `Partial run: 7 of 20 seeds explored (seeds
  1-7)`. `--seeds 1-7` explores the same schedules again on any machine.
- A partial run never calls a finding `fixed`, because part of the range was not explored.

The explorer's own option is `hydo explore --time-limit SECONDS`, with the same cut.

### Seed counts from history

With `seeds_from: history` in a mode's settings, or `--seeds-from history`, and a history store
(`--history`, section 12), the budget chooses the seeds before the run starts:

```yaml
pr: {seeds: 1-500, strategy: process, budget: 10m, seeds_from: history}
```

- The store's recorded runs give the seconds per seed: the exploration time of each of the last
  10 runs with the same engine, strategy and options, divided by its seeds.
- When the base is explored too, the base's seconds per seed are added.
- The run explores the first seeds of the range that fit into the budget's time, for example
  seeds 1-60 of 1-200. The time limit stays on as a safety stop.
- The report says where the count came from: `The seed count came from the history: 6 recorded
  runs measured 2.5 s per seed`. `ci-run.json` has it in `budget.seeds_from`.
- Without a measured run, the run keeps the range and the time limit, and the report says so.
- A shard keeps its part of the range, because the shards of one run must split one range.

A recorded run's time includes its shrinks and its worker's boot, so a store of small runs gives
a high estimate. In one measured store the history said 2.5 s per seed, and the 60 seeds it
chose took 71 s.

## 11. Shards

`--shard I/N` explores the I-th of N contiguous parts of the seed range, so N jobs share one run.
Each shard compares its part with the base and replays its own findings. Only the first shard
replays the saved regressions. `hydo ci merge` makes one `ci-run.json`, one `report.md` and one
`junit.xml` from the shards, by finding key, in shard order. It runs no VM:

```sh
hydo ci run --base main --shard 1/2 --out shard-1
hydo ci run --base main --shard 2/2 --out shard-2
hydo ci merge shard-1 shard-2 --out hydo-ci
```

A missing shard makes the merged run partial, and the merged run then fails. On GitHub Actions a
matrix gives the shards, and the action's `merge` input runs the merge:

```yaml
jobs:
  shard:
    runs-on: ubuntu-24.04
    strategy: {fail-fast: false, matrix: {shard: [1, 2, 3]}}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: kakoc/hydo/action@v1
        with:
          hydo-version: 0.8.0
          args: --shard ${{ matrix.shard }}/3
          comment: "false"
          artifact-name: hydo-shard-${{ matrix.shard }}
  merge:
    needs: shard
    if: ${{ !cancelled() }}
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with: {pattern: hydo-shard-*, path: shards}
      - uses: kakoc/hydo/action@v1
        with:
          hydo-version: 0.8.0
          merge: shards/*
```

A shard job fails by `fail_on` for its own part of the range. The merge job's result is the
run's result.

## 12. History across runs

The history store is a directory that keeps the findings of a branch across runs. A team keeps it
in a branch of its own repository, for example `hydo-history`, checked out next to the code. hydo
writes only files there; the workflow commits and pushes them.

```sh
hydo ci record hydo-ci --store ../hydo-history          # after a push or a nightly run of main
hydo ci run --base main --history ../hydo-history       # in a pull request
```

- `hydo ci record` adds `ci-run.json` to `v1/<project>/runs/`. It appends each change of a
  finding's state to `v1/<project>/findings.jsonl` and rewrites `v1/<project>/state.json`.
- A finding that failed is `new` on the branch the first time, and `reappeared` when the history
  had it fixed. A finding the run proved fixed by replay becomes `fixed`. A finding the run did not
  find keeps its state: absence after a bounded search proves nothing.
- It records the runs of the branch itself. A pull request's run needs `--branch NAME`, and a
  shard's run must be merged first.
- `hydo ci run --history DIR` reads the ledger of the pull request's target branch. It replays
  every open finding that neither exploration found, so a finding of a nightly run is checked on
  every pull request. A finding the history has fixed that this change brings back is
  `reappeared`.

- `hydo ci record --report` also keeps the run's report page in the store, as
  `v1/<project>/reports/<run id>.html`. hydo makes the page from the run's files.
- `hydo ci page --store DIR` writes the history page, `DIR/index.html` (section 14).

Record only from trusted runs: a pull request from a fork must not get a token that can push to
the history branch. Section 13 does this with a second workflow.

`hydo findings record` writes the same records from a plain `hydo run` or `hydo explore` on a
laptop (`hydo guide workflow`, "Keep the findings of your runs"). Such a record has the trigger
`manual` and names its command, and `hydo ci page` shows it among the runs. It goes to the line
of the branch that git names in that folder. Give `--branch` another name to keep laptop records
out of the line that `hydo ci run --history` reads.

## 13. History, the page, and comments from forks

Two workflows keep the history in the branch `hydo-history` of the team's repository. hydo's
repository has both as examples, in `action/examples/` at an `action-v1.*` tag. They need hydo
0.4.0 or later. Copy them to `.github/workflows/`:

- `hydo.yml` runs `hydo ci run` on every pull request, every push to `main` and every night. It
  has a read-only token, so a pull request from a fork runs in it safely. It reads the history
  branch, so each run replays the branch's open findings. It posts no comment and writes no
  history.
- `hydo-record.yml` starts when a run of `hydo` ends (the `workflow_run` event). It runs in the
  default branch's context, with a token that can write. It downloads the run's artifact, posts
  or edits the pull request's comment, records the run in the history branch with
  `hydo ci record --report`, writes the page with `hydo ci page`, and pushes the branch.

The recorder treats the artifact as data. It runs no code from the pull request, and hydo checks
that `ci-run.json` is a CI run document. Every text from the run is escaped in the comment and on
the page. The pull request's number comes from the event, not from the artifact. A pull
request's run goes to its own line in the history, `pull/<number>`. The line `main` is written
only by the repository's own pushes and nightly runs.

The action's input `recorded` makes it run no VM: it makes the comment from a `ci-run.json` that
another job wrote. `pull-request` names the pull request to comment on.

## 14. The history page

`hydo ci page --store hydo-history` writes one web page, `hydo-history/index.html`. It loads
nothing from the network, so it opens from a checkout, and GitHub Pages can serve the branch. For
each project of the store it shows:

- Each finding across commits and branches: where it was first seen, the commit where a replay
  proved it fixed, the commits where it came back, and whether it is accepted.
- Each finding's minimal schedule, the `hydo run` line that replays it, the `hydo ci accept`
  line, and a link to the report page of the run that last saw it, when the store keeps one.
- A timeline per branch: one row per finding, one column per recorded run. A cell says `new`,
  `fails`, `fixed`, `back`, or `·` for a run that did not find a finding that stays open.
- The runs, the latest first: when, the branch and commit, the seeds, the budget, the engine,
  the findings by state, the result, and links to the run's report and job.

## 15. Explore on many runners

A plain exploration splits into parts the same way as a CI run. `hydo explore --shard I/N`
explores the I-th of N contiguous parts of `--seeds`, with the split of section 11. Each part
re-runs and shrinks its own failures. `hydo explore --merge` then makes one exploration of the
whole range from the parts' folders, without a VM:

```sh
hydo explore --seeds 1-40 --shard 1/2 --out part-1
hydo explore --seeds 1-40 --shard 2/2 --out part-2
hydo explore --merge part-1 part-2 --out explore
hydo report explore
```

- The merged folder has one `exploration.json`, `events.jsonl` and `report.md`, and the consoles
  of the runs it keeps. `hydo report` and `hydo report --html` read it like any exploration.
- The merge decides what one exploration of the range decides. It puts the failures in seed
  order and keeps the shrink of the first failure of each class and signature. It also merges
  the error kinds and the property table.
- On the scripted engine the merged files equal one exploration's. Only the wall times and the
  worker counts differ.
- `shards` in `exploration.json` lists each part with its seeds, its counts, its host CPU and
  its wall time. `missing_seeds` names the seeds of a part that did not finish.
- The merge refuses parts of two explorations: another image, another engine, another hydo, or
  other options.

A part's failure is re-run on the part's own machine. On GitHub's runners the CPU is an AMD EPYC
or an Intel Xeon, and KVM fingerprints differ between the two vendors (section 6). So the merge
compares verdicts and signatures across parts, and fingerprints never.

hydo's own repository runs its examples this way in the workflow `explore.yml`. One job builds
hydo and the image, up to 20 jobs explore one part each, and one job merges them. The script
`ci/remote-explore.sh` starts it, waits, and downloads the merged folder:

```sh
sh ci/remote-explore.sh --example hydo/onboard/examples/redis --seeds 1-40 --shards 8
```
