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
5. It replays the saved regressions in `hydo/regressions/`.
6. It writes `ci-run.json`, `report.md` and `junit.xml` into its output directory (default
   `hydo-ci`).

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
          hydo-version: 0.3.0
```

The action installs that hydo release, checked against the release's `SHA256SUMS`. It caches
`~/.hydo` (the release and the image cache), fetches the pull request's base commit and runs
`hydo ci run`. It keeps the output directory as an artifact and writes the report into the job
summary. It also posts the report as one pull-request comment, which it edits on every push. The job fails with
`hydo ci run`'s exit status. A pull request from a fork gets a read-only token: the comment is
then skipped with a warning, and the report is still in the job summary.

Inputs: `hydo-version` (required), `project` (the directory that holds `hydo/hydo.yaml`), `mode`,
`args` (more `hydo ci run` arguments), `out`, `comment`, `artifact-name`, `github-token`.

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
    HYDO_VERSION: "0.3.0"
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
6. Post `hydo-ci/report.md` as a comment, if the system can.
7. Fail the job with `hydo ci run`'s exit status.

Every finding and every saved regression is one test case in `junit.xml`. A finding in a state
that `fail_on` names is a failed test, and a finding already on the base is a skipped test. The
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
vendor (`engine.host_cpu` in every result). Elsewhere hydo compares verdicts.

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
| `unverified` | Not decided. | A replay was refused or gave no result. A person should look. |

A finding is never called fixed because an exploration did not find it: only a passing replay
proves that.

## 8. Comparing two recorded runs

```sh
hydo ci compare base/ci-run.json head/ci-run.json --out compared/
```

It compares by finding key only and runs nothing, so it calls nothing `fixed`: a base finding
that the head did not find is listed as `unverified`.
