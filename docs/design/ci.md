# hydo in a team's continuous integration (design, 2026-11-08)

Status: a design only. Nothing in it is built. It answers the user's question of 2026-11-08 ("is
it scalable into a real solution? ... what was running, when, what's history, what was checked,
found ... I don't have the whole picture of its evolution") and replaces the short "CI mode (to be
discussed)" section of `hydo/PRODUCT.md` once the user agrees with it.

This document is about **a team using hydo in its own repository's CI**. It is not about hydo's own
CI (the workflows that build and test this repository), which `docs/CI.md` describes.

## 1. The short answer

Yes, it scales into a real product, and most of what it needs already exists inside `hydo`. A
deterministic run is a pure function of four things: the image, the schedule string, the engine
(with, on KVM, the host's CPU vendor) and the hydo version. Every design choice below follows from
that one fact:

- **History needs only small records.** A finding is fully described by its commit, its image's
  input key, its minimal schedule, its engine and its hydo version. Every console, transcript or
  report can be made again from those. So the permanent history is a few kilobytes per CI run,
  and the bulky evidence is a cache that may expire.
- **"New", "fixed" and "where it started" are proven by replay, not guessed from absence.** A
  finding seen on a pull request is replayed on the base commit's image: if the base passes, the
  pull request introduced it. An open finding is replayed on every new commit: if it passes, the
  commit fixed it. The commit that introduced a finding is found by bisection with one run per
  commit, because a deterministic run needs no repetitions.
- **The work splits cleanly.** The explorer's schedules are a function of the seed, so seed
  ranges shard across runners with no coordination, and the shards' results merge into one
  exploration.

The recommended first step (stage 1, section 9) is a `hydo ci run` command plus a small GitHub
composite action that runs the KVM engine on GitHub's standard Linux runners and posts one
pull-request comment: which findings are new (proven against the base), which saved regressions
failed, and what the run covered. The recommended history store (section 4.5) is a branch in the
team's own repository for the small records, with GitHub's artifacts holding the bulky evidence
for as long as they last.

## 2. What exists today, and what each piece gives CI

| Piece | What it gives a CI run |
|---|---|
| The KVM engine on GitHub-hosted runners (M159, M161; proven 2026-11-08) | Runs on ubuntu-24.04 runners with no module and no root. The published 0.2.0 Linux release built the Redis quickstart image on the runner to the recorded bytes; `disk=none \| run` gave OK and `disk=cut \| run C=12 R=everysec` gave VIOLATION, each twice identical, on four runners and two AMD EPYC models (9V45, 9V74), in about 12 seconds per whole job. Fingerprints are one per CPU vendor; verdicts equal those on Intel. |
| The results format (M115) | `exploration.json` (configuration, summary, failures with their reproductions, shrinks, error kinds, the property table, liveness), `events.jsonl` (one record per run), `result.json` per run. One finding shape (`kind`, `id`, `status`, `message`, `details`). Readers ignore unknown fields, so CI records can be added without breaking anything. |
| Failure signatures and shrinking (M38, M102) | The explorer groups failures by a normalized signature (`VIOLATION [redis.acked_writes_survive] key k:# lost`), re-runs each failure (fingerprints must match), and shrinks one failure per signature to a minimal schedule, which it runs twice. |
| The property table (M38, M59) | For every declared assertion (`always`, `sometimes`, `reachable`, `unreachable`, `eventually`): passes, fails and a status, including `not-exercised`. This is the "what was never reached" list a team needs. |
| Error kinds and `known-errors.yaml` (M94) | New log error kinds are findings with a stable 12-hex signature; accepted ones are silent. |
| Saved regressions (M86, M98) | `hydo/regressions/<name>.schedule` with `# expect: ok`, `violation` or `fp=`; `hydo repro --regressions` replays them and exits 1 when one is not as expected. |
| `hydo diff` and `hydo explain` (M165) | Where two runs first differ, and the first difference in what the checks judge. A pull request's new finding replayed on the base gives exactly such a pair. |
| Failure diagnosis and `hydo inspect` (M178) | Why a service failed to start, and a look inside a run at any stop. |
| No silent passes (M184) | A schedule that names something the image lacks is refused before any VM. CI depends on this: a replayed schedule that no longer applies must say so, not pass. |
| Image records and fingerprint epochs (M155) | Every image records its guest kernel; runs of one epoch compare fingerprints, runs across epochs compare verdicts. |
| The explorer cache (`HYDO_EXPLORE_CACHE`) | A deterministic exploration resumes from recorded results. Its key today is not safe to share between machines (section 7.2). |
| `hydo-bench rebaseline` and `recorded.json` (M164) | A precedent for keying builds by their inputs and runs by image and engine, and for "recorded values versus today's values". |
| The install cache (M162) | `hydo setup --get <version>` installs a pinned version from its GitHub release, verified by sha256. A CI job pins the version and caches `~/.hydo`. |

What does not exist: a CI command, a record of a CI run (commit, base, budget, shards, runner), a
finding identity that survives commits, a history store, comparison against a base, sharding and
merging, a time budget, bisection, a report for pull requests, and a GitHub action.

## 3. Where a CI run can run

| Runner | Engine | Notes |
|---|---|---|
| GitHub-hosted Linux runner (x86_64) | `kvm` | Proven. One vCPU per guest, the exit-count clock, guest CPU level v2. Fingerprints depend on the host's CPU vendor (AMD and Intel give different ones; verdicts agree). Only AMD runners were seen on 2026-11-08; the action must record the vendor and expect both. |
| GitHub-hosted larger Linux runner | `kvm` | The same engine with more memory and cores: the way to run the large systems (section 7.4). |
| Self-hosted Linux machine with VT-x | `hvx` (or `kvm`) | The only place for several vCPUs, the PMU branch clock and `guest.cpu: v3`. Needs the kernel module loaded once (`hydo module install`) and either root or the udev rule. |
| Self-hosted Apple Silicon Mac | `hvm` | Works as on a developer's Mac. |
| GitHub-hosted macOS runner | none | GitHub documents that its Apple Silicon macOS runners do not support nested virtualization, so Hypervisor.framework cannot run a guest there. Not tried. |
| Other CI systems | `kvm` where the runner exposes `/dev/kvm`, otherwise self-hosted | Not tried (section 8.3). |

A finding replays exactly (same fingerprint) only on the same engine and, for KVM, the same CPU
vendor. On another engine or vendor the same schedule string is a new run, and only its verdict is
compared. x86_64 images do not run on a Mac at all: a Mac developer rebuilds the arm64 image of the
same commit and runs the same schedule, which usually but not always gives the same verdict
(section 6.3).

## 4. The data model over time

### 4.1 What one CI run records

A **CI run** is one execution of hydo for one project at one commit, possibly split into shards.
It writes one new results-format document, `ci-run.json` (`"type": "ci-run"`, an addition to format
1). It holds only small values and refers to the bulky outputs by name:

```json
{
  "v": 1,
  "type": "ci-run",
  "id": "github:acme/shop:actions/11873625:1",
  "project": {"name": "shop", "manifest": "hydo/hydo.yaml", "manifest_sha256": "5f1c..."},
  "trigger": "pull_request",
  "commit": "9c41e07",
  "base": {"branch": "main", "commit": "4b2d11a", "ci_run": "github:acme/shop:actions/11870002:1"},
  "pull_request": 412,
  "hydo": {"version": "0.3.0", "commit": "a1b2c3d4", "epoch": 1},
  "engine": {"backend": "kvm", "host_cpu": {"vendor": "AuthenticAMD", "model": "AMD EPYC 9V74"},
             "ram_mib": 2048, "workers": 3},
  "image": {"input_key": "ik1-3e9a...", "sha256": "d255fb2b...", "reused": true},
  "budget": {"mode": "pr", "seeds": "1-200", "strategy": "process",
             "options": ["--more-faults"], "time_limit_s": 900, "completed_seeds": "1-200"},
  "shards": [{"index": 0, "seeds": "1-100", "runs": 113, "wall_s": 151},
             {"index": 1, "seeds": "101-200", "runs": 109, "wall_s": 147}],
  "summary": {"runs": 222, "failures": 9, "findings": 3, "new": 1, "present": 2, "fixed": 1,
              "regressions": {"replayed": 4, "as_expected": 4}, "wall_s": 312},
  "coverage": {
    "faults": {"K": 81, "P": 44, "N": 37, "C": 52, "disk": 66, "H": 19},
    "services": {"payments": 61, "orders": 77, "postgres": 70},
    "properties": {"declared": 14, "reached": 12,
                   "not_exercised": ["refund.partial_path", "fault.paused"]}
  },
  "findings": [
    {"key": "f1:invariant:b7d0e2a1c4", "title": "VIOLATION [shop.no_double_charge] charge # twice",
     "state": "new", "proof": "base-replay-ok",
     "minimal": "disk=none dseed=1 | run H=payments.charge@0-1:drop",
     "fp": "8cbe80f7b6acaf4e", "classes": ["protocol"],
     "repro": "hydo run hydo-out/explore-ob-shop-disk.img \"disk=none dseed=1 | run H=payments.charge@0-1:drop\""}
  ],
  "nondeterminism": [],
  "artifacts": {"exploration": "explore/exploration.json", "report": "summary.md",
                "bundle": "github-artifact:hydo-ci-412-9c41e07"}
}
```

Notes on the fields:

- `engine.host_cpu` is new. The results format records the engine's backend, module and options,
  but not the host's CPU vendor, and KVM fingerprints depend on it. It must be added to `Engine`
  in `hydo-results` (an addition) for every run, not only CI runs, because local reproduction
  (section 6.3) needs it too.
- `image.input_key` is new (section 7.2). The image's sha256 is already recorded.
- `coverage.faults` and `coverage.services` are counts of schedule tokens by kind and by target
  service. They need no new recording: they come from parsing the schedules in `events.jsonl`
  with `hydo-schedule`. They are counts, not percentages, because the fault space of a random
  strategy cannot be enumerated (section 10).
- `coverage.properties` is the existing property table reduced to counts and the list of ids
  that were never reached.
- `nondeterminism` lists failures whose re-run gave another fingerprint. With a correct engine it
  is always empty; when it is not, it is a hydo bug or an environment problem, reported apart from
  the project's findings and never counted as one.

### 4.2 What identifies "the same finding" across commits

Fingerprints cannot identify a finding across commits: a changed line of the service changes the
image and so every fingerprint. Schedules cannot either: several schedules hit the same bug, and
the minimal one changes when the code changes. What stays stable is **what the check said**.

The **finding key** is computed from the finding's kind and a normalized statement:

| Finding kind | The statement the key is made from | Example title |
|---|---|---|
| `invariant` (a check's verdict) | the verdict's signature as the explorer forms it today (numbers and ids replaced by `#`) | `VIOLATION [redis.acked_writes_survive] key k:# lost` |
| `assertion` (an SDK property) | the property id and the way it failed | `always shop.balance_never_negative failed` |
| `history-check` | the checker and the anomaly type | `linearizable register: stale read` |
| `error-log` | the existing error-kind signature | `704053f4c802` (`[mysql] ... Operating system error number #`) |
| `run-error` | the `ERROR` or `TIMEOUT` verdict's signature | `ERROR orders restart failed` |

The key is `f1:<kind>:<first 10 hex of sha256(statement)>`; `f1` is the key version, raised if the
normalization ever changes, so that old and new keys are never compared silently. The fault
classes that led to the finding (`lying-disk`, `cut`, `protocol`) are recorded as attributes, not
as part of the key: the team cares that an acknowledged write was lost, whichever fault lost it.
A team that wants them apart can say so per property later; that is not in the first stages.

Two weaknesses, and what limits them:

1. **A reworded check splits a finding.** If a developer edits a check's message, the key changes
   and the old finding looks fixed while a new one appears. The guide already asks checks to
   print a bracketed property id (`[shop.no_double_charge]`); when one is present, the statement is
   the id plus the verdict word only, so rewording the rest changes nothing. The report also
   pairs a "fixed" and a "new" finding of the same commit that share a minimal schedule and says
   "probably the same finding, renamed".
2. **Two bugs with one message merge.** Two different causes that print the same verdict are one
   finding. The minimal schedules differ, and the report lists every minimal schedule seen under
   a key, so a person sees two routes. Replay keeps the states honest regardless: each route is
   replayed on its own.

### 4.3 How runs relate across commits and branches

- **The line of truth is the default branch.** Every push to it and every nightly run is
  recorded in history (section 4.5). Its findings ledger (section 4.4) is the team's list of open
  findings.
- **A pull request is tested at GitHub's merge commit** (what the default branch would become)
  and compared with the CI run of that merge commit's first parent, the base branch's tip. If the
  tip has no run (a run is still going, or it failed to build), the nearest first-parent ancestor
  with a run is used, and the report names it.
- **The comparison is by finding key, then proven by replay** (section 6.1). The base's run may
  have used another seed range, another budget, another hydo version or epoch: keys and verdicts
  compare across all of them, fingerprints only within one engine, vendor and epoch.
- **Other branches** (release branches, long-lived feature branches) get their own ledger when
  their workflow runs on push; a pull request against them compares with their tip.

### 4.4 The findings ledger

The ledger is an append-only list of state changes per project, one JSON line each:

```json
{"v":1,"type":"finding-event","key":"f1:invariant:b7d0e2a1c4","project":"shop",
 "event":"new","commit":"9c41e07","branch":"main","ci_run":"github:acme/shop:actions/11873625:1",
 "title":"VIOLATION [shop.no_double_charge] charge # twice",
 "minimal":"disk=none dseed=1 | run H=payments.charge@0-1:drop","image_input_key":"ik1-3e9a...",
 "engine":"kvm/AuthenticAMD","hydo":"0.3.0","proof":"base-replay-ok"}
```

The current state of each finding is the fold of its events. The states:

| State | Meaning | How it is decided |
|---|---|---|
| `new` | First seen on this branch at this commit. | Found by exploration (or replay) here; not open on the base; **the base replay passes**. |
| `pre-existing` | First seen now, but already in the base. | Found here; not in the base's ledger; the base replay **also fails**. The base's exploration missed it. Recorded on the base as `new` with `proof: found-later`. |
| `present` | Still there. | Found again by exploration, or its minimal schedule replayed and failed with the same key. |
| `changed` | The replayed schedule now fails differently. | Replay gives another key. Reported with both titles. |
| `fixed` | Proven gone at this commit. | Its minimal schedules replayed and **passed**, and the exploration did not find the key. Records the commit. |
| `reappeared` | Fixed earlier, failing again. | A `fixed` key is found again. Records both commits. |
| `unverified` | Cannot be replayed here. | Every saved schedule is refused (M184: the image no longer has that service or fault point) and the exploration did not find it. It stays open, never turns into `fixed` by itself, and the report asks a person to look. |
| `accepted` | A person decided it is expected. | `hydo/known-findings.yaml` (key, reason, who), reviewed like code, in the spirit of `known-errors.yaml`. |

"Not found by this exploration" is never a state. Absence after a bounded random search proves
nothing, and the old way of reading it ("the test went green") is exactly what determinism lets
hydo avoid.

### 4.5 Where history lives

Options, judged for a team on GitHub:

| Option | Cost | Permissions | Privacy | Lifetime | Fit |
|---|---|---|---|---|---|
| GitHub Actions artifacts | Free within storage quotas | Written by any job | Same as the repository (public repositories: public) | 1 to 90 days on public repositories, up to 400 on private | Good for bulky evidence; not queryable; expires. |
| GitHub Actions cache | Free, 10 GB per repository by default, least recently used evicted, unused entries removed after 7 days | Written by any job; pull requests read their base branch's entries | Same as the repository | Days | Only for images and run caches, never for history. |
| **A branch in the repository** (`hydo-history`, no common history with the code) | Free; a few kilobytes per run | Needs `contents: write`; pull requests from forks get a read-only token | Same as the repository | Permanent, versioned | Good for small records; anyone can read it with `git`; no server. |
| Release assets | Free | `contents: write` | Same as the repository | Permanent | Made for files of releases; awkward for many small writes. Not recommended. |
| An object store the team owns (S3, GCS, R2 and the like) | Storage and requests, small | Credentials; on GitHub through OIDC, no stored secret | The team decides | The team decides | Good for bulky evidence at scale and for private data; one more thing to set up. |
| A hosted page or service with its own database | A server, or one person's account | Its own login | Data leaves the repository | As long as the service lives | Ties the team's records to something outside its control. Not for the first stages. |

**Recommendation.** Two layers, behind one small interface in `hydo` (a "history store" with a
`dir:` backend, so that `hydo` itself never calls GitHub's API):

1. **Small records in the `hydo-history` branch**: every `ci-run.json` of the default branch and
   of nightly runs, the ledger, and a generated summary per project. The action checks the branch
   out into a separate directory, `hydo ci record --store dir:<that directory>` writes files, and
   the action commits and pushes. Layout:

   ```
   v1/<project>/runs/2026/11/08/<ci-run id>.json     one file per run, about 5 KB
   v1/<project>/findings.jsonl                        the ledger, appended
   v1/<project>/state.json                            the fold of the ledger, rewritten
   v1/<project>/coverage.jsonl                        one line per run: counts for the trend
   page/index.html                                    the history page (section 5.3), generated
   ```

   Size: at 50 recorded runs a day and about 5 KB per run, about 90 MB a year before git's
   compression, which shrinks such JSON several times. A yearly roll-up can fold old run files into
   one file per month if it ever matters.

   Writes are serialized with one GitHub concurrency group (`hydo-history`), and only trusted
   workflows write: pushes to the default branch, nightly runs, and a follow-up job started by
   `workflow_run` that records pull-request runs from their artifacts. That follow-up job runs in
   the default branch's context, treats the artifact as data (it is written by untrusted code on a
   fork's pull request) and only copies validated JSON into the branch. Pull-request jobs
   themselves never get a write token.

2. **Bulky evidence in GitHub artifacts**: the exploration directory, consoles, transcripts and
   the minimal runs, kept as long as the repository's artifact retention allows. When they expire
   nothing is lost that matters: a finding's recipe (commit, input key, schedule, engine, hydo
   version) is in the branch, and `hydo ci reproduce` (section 6.3) makes the evidence again.
   This holds when the image builds again to the same bytes from the commit (section 7.2); where
   a team's service build is not reproducible, the image of each open finding is kept in an
   artifact, and the team is told why.

   An object-store backend (`store: s3://...`) is a later addition for teams that outgrow
   artifacts or must keep consoles private while the repository is public (section 7.5).

## 5. What a team sees

### 5.1 On a pull request

One comment, edited in place on every push (so a pull request never collects twenty of them),
and the same text as the check's summary:

```markdown
### hydo: 1 new finding, 4 of 4 regressions as expected

**New in this pull request** (the base 4b2d11a passes the same schedule)

| Finding | Minimal schedule | Reproduce |
|---|---|---|
| `VIOLATION [shop.no_double_charge] charge # twice` | `disk=none dseed=1 \| run H=payments.charge@0-1:drop` | `hydo ci reproduce 11873625 f1:invariant:b7d0e2a1c4` |

Where base and head first differ on that schedule (`hydo diff`): after the dropped answer to
`payments.charge` (step 1), the head retries with a new idempotency key; the base reuses it.
First judged difference: `world: charges for order 1: base 1, head 2`.

**Fixed by this pull request** (replayed and passed): `VIOLATION [shop.refund_once] ...`

**Still open on main** (replayed here, still failing; not caused by this change): 2 findings, see
the history page.

<details><summary>What ran</summary>

- 222 runs (seeds 1-200, strategy process, --more-faults), 2 shards, 5 min 12 s, KVM on AMD EPYC.
- Faults drawn: kill 81, pause 44, network 37, power cut 52, disk 66, protocol 19.
- Services targeted: payments 61, orders 77, postgres 70.
- Properties never reached in this run: `refund.partial_path`, `fault.paused`.
- Image reused from the base (same input key).
</details>
```

The check fails only by the team's policy (`fail_on` in section 8.2). The default is to fail on a
`new` or `reappeared` finding and on a regression that is not as expected; `present` findings do
not fail an unrelated pull request, because the change did not cause them.

### 5.2 In the job summary

The job summary carries the same text plus what only a person debugging the CI job needs: the
shard table (seeds, runs, time, exit status of each), the image build or reuse, the hydo version
and engine, any nondeterminism, and links to the artifacts.

### 5.3 The history page

A static page generated into the `hydo-history` branch (`hydo ci page`), which the team can serve
with GitHub Pages or open from a checkout. It needs no server. Sections:

- **Open findings**: key, title, state, first seen (commit and date), introduced by (when
  bisected, section 6.2), last replayed, minimal schedules, the reproduce command.
- **Timeline of findings**: one row per finding, one column per default-branch commit or day,
  showing new, present, fixed and reappeared. This answers "fixed since commit X" and "came back".
- **Coverage over time**: runs per day; fault kinds drawn per day; per property, the last time it
  was reached (a `sometimes` property not reached for 30 days is a blind spot worth a look).
- **Runs**: every recorded CI run with its trigger, commit, budget, runner and time, linking to its
  summary and, while they exist, its artifacts.
- **hydo's own health**: nondeterminism reports, run errors by kind, and hydo versions in use.

## 6. What determinism makes possible, and its limits

### 6.1 "New" and "fixed" proven by replay

For every finding of a pull request's exploration, `hydo ci run` replays its minimal schedule on
the base commit's image (one VM run per finding; the image usually comes from the cache,
section 7.2). Pass on base and fail on head proves that the change introduced it, on that schedule.
Fail on both proves it was already there. The two runs are exactly the pair `hydo diff` explains,
so the comment can say where their behaviour first differs.

In the other direction, every open finding of the base's ledger, and every saved regression, is
replayed on the head's image. This turns the ledger into a regression suite that maintains
itself: the team does not have to remember to save a reproducer for every bug. Saved regression
files stay useful as the reviewed, versioned part: when a finding becomes `fixed` on the default
branch, the action proposes the `hydo save` file (`# expect: ok`) in its comment, and the team
commits it.

Limits:

- **A schedule's meaning can move.** `K=redis@12+2` kills Redis at workload step 12. If the
  workload changed between base and head, step 12 is another moment, and a pass on base may only
  mean the schedule no longer hits the window. The report flags this when the workload commands or
  the manifest changed between the two images, and `hydo diff` shows whether the runs diverge
  before the fault.
- **A finding can be gone from one schedule and still exist.** "Fixed" means "every saved minimal
  schedule passes and this exploration did not find it". It is far stronger than absence, but it
  is not a proof that no schedule fails. Nightly runs keep exploring (section 7.3).
- **A schedule can stop applying.** Since M184 a refused schedule is reported, never a pass; the
  finding becomes `unverified`.

### 6.2 Finding the commit that introduced a finding

`hydo bisect <finding key> --good <commit> --bad <commit>` drives `git bisect run`: at each commit
it builds the image (reusing cached builds by input key), replays the minimal schedule once, and
answers good (OK), bad (the same key), or skip (the build failed, or the schedule is refused). One
run per commit suffices: a deterministic run gives the same answer every time, so no retries and no
statistics. 100 commits take about 7 builds and 7 runs. The nightly workflow bisects every finding
that became `new` on the default branch, between the last run without it and the first with it,
and records `introduced_by` in the ledger.

Limits:

- **The cost is the builds.** A run takes seconds; building the team's services and the image at
  each commit can take minutes. Builds keyed by input key are reused across bisections.
- **Introduced versus exposed.** Bisection finds the first commit where the schedule fails. That
  may be a commit that changed the workload or a check (which exposed an older bug), not the
  commit that wrote the bug. The report says which kind of files the blamed commit changed
  (service sources, `hydo/` folder, both).
- **Moving step numbers** (6.1) affect bisection the same way; when the workload changed inside
  the range, the result is labeled "the schedule's meaning changed in this range".

### 6.3 Reproducing a CI finding on a developer's machine

`hydo ci reproduce <ci run> <finding key>` fetches the run's record, checks out nothing (it tells
the developer which commit to check out if the working tree differs), builds the image or downloads
it, and runs the minimal schedule:

- **Same engine and, on KVM, same CPU vendor**: the fingerprint must match; it says REPRODUCED or
  DIFFERENT, exactly as `hydo repro` does today within an epoch.
- **Another engine or vendor** (an Intel laptop with KVM against an AMD runner; a bare-metal
  machine with hvx): it compares verdicts and says so, as `hydo repro` already does across epochs.
- **A Mac**: x86_64 images do not run there. It builds the arm64 image of the same commit and
  runs the same schedule. The verdict usually matches for logic bugs and may not match for
  timing-dependent ones. It says this plainly. A Mac developer who needs the exact run uses a
  Linux machine or `hydo inspect` on the CI artifacts (a later addition: an inspect session on a
  downloaded run directory).

## 7. Scale

### 7.1 Sharding the exploration

The explorer draws each schedule from its seed, the strategy, the options and the image's fault
space. So a seed range splits into disjoint ranges that run anywhere, in any order, with the same
results. The plan:

1. **Shard jobs** run `hydo explore --seeds <their range> --shrink none` (a new option): explore
   and re-run failures, but do not shrink, because every shard would shrink the same signature.
2. **A merge job** runs `hydo ci merge shard-*/ --out merged/`: it unions the `events.jsonl` files,
   sorts runs by seed, rebuilds one `exploration.json` (summary, failures, property table, error
   kinds), and for each signature picks the failure with the lowest seed, so that the choice does
   not depend on which shard finished first.
3. **The same job shrinks once** per signature (`hydo explore --shrink-from merged/`, a new mode),
   replays as in section 6.1, and writes `ci-run.json`.

Shrinking costs tens of runs per signature, and replays a few runs per finding, so the merge job
is short for a handful of findings. With many signatures it becomes the bottleneck; then the
shrinks themselves fan out as jobs, one per signature.

GitHub's matrix gives the shards (up to 256 jobs per workflow); the merge job waits for them with
`needs:`.

### 7.2 Reusing images and results

**The image input key.** An image is the same bytes when its inputs are the same: `hydo build` is
reproducible (fixed clock, no host state, pinned assets). The input key is a hash over:

- the hydo version and the pins that shape images (the builder's own identity),
- the target architecture,
- the manifest file and every file of the `hydo/` folder,
- for each `src:` path, its git tree hash at the commit (`git rev-parse <commit>:<path>`, which costs
  nothing and needs no file reads),
- for each OCI service, its pinned digest,
- for each service the team builds itself, either the sha256 of the archive it built, or, when
  that build is not reproducible, the git tree hashes of the paths the team names as its inputs
  in `hydo/ci.yaml` (section 8.2).

Images are kept in the Actions cache under `hydo-image-<input key>`. Pull requests read their base
branch's entries, so a pull request that changes no service input reuses the base's image, and
every shard of one run restores the same image instead of building it.

`hydo-bench rebaseline`'s build key (`stage.sh sources`) is a precedent, written by hand per stage;
this one is computed from the manifest.

**Run results.** A run's result can be reused when its image sha256, its schedule, its engine and
its fingerprint epoch are the same. The explorer's cache (`HYDO_EXPLORE_CACHE`) is not safe for
that yet: `hydo-explore/src/run.rs` keys it by the image *path* (`cx.image`) and the engine's
*arguments* (`cx.engine_args()`), not by the image's sha256, the backend, the engine program, the
loaded module or the host's CPU vendor. Two different images at the same path would share entries.
Before any cache is shared between machines, the key must become (image sha256, backend, engine
program sha256, module srcversion on hvx, host CPU vendor on KVM, epoch, schedule). With that key,
the main use in CI is a nightly run that resumes after a runner was lost, and pull requests whose
image did not change (a documentation-only change replays from the cache in seconds).

### 7.3 Budgets

The explorer takes a seed range today; it has no time limit. CI needs both a predictable time and
a reproducible set of schedules, so:

- **The budget is a number of seeds, chosen from history.** `budget: 10m` in `hydo/ci.yaml` is
  turned into a seed count using this project's mean run time on this runner kind from recent
  history (`summary.mean_run_s` exists today). The set of schedules is then fixed, and base and
  head compare like with like.
- **A hard time limit is only a safety stop** (`--time-limit`, new): when it fires, the run stops
  drawing seeds, finishes what started, records `completed_seeds`, and is marked partial. A
  partial run never decides `fixed`.
- **Pull requests use a fixed seed range** (for example `1-200`), the same on every pull request,
  so the comparison with the base is exact.
- **Nightly runs go deeper and rotate**: a larger count, more fault kinds (`--new-faults`,
  `--storage-faults`, `--concurrency`), and a seed range that moves every night (`--seeds` from an
  offset derived from the date), so that the search keeps covering new schedules. Whatever the
  nightly finds enters the ledger, and from then on every pull request replays it. That is how a
  small per-pull-request budget still benefits from a large search.

### 7.4 Targeting the services a change touched

The manifest knows which source paths build which service. A change that touches only the
payments service could get more faults aimed at payments and its links (a new explorer option,
`--focus <service>`). This is an experiment, not a stage-1 feature: the measurements of M41 to M57
showed that where faults land matters more than how the next schedule is chosen, which argues for
targeting, but no measurement of targeting by changed service exists. It should be measured on the
bug benchmark (`hydo/bench/`) before it is built into the action.

### 7.5 Runners, cost and limits

- **Memory decides how many workers a runner holds.** Measured on KVM (docs/design/kvm-engine.md):
  a Redis worker uses about 0.24 GB, a four-service stack worker about 3.2 GB (2.3 GB of it the
  image's shared page cache). GitHub's standard Linux runner for public repositories has 4 vCPUs
  and 16 GB, enough for 3 to 4 stack workers or many Redis workers; private repositories' standard
  runner is smaller (check GitHub's current documentation; not tried). Guests up to 8 GiB (M179)
  need larger runners.
- **Disk**: a standard runner guarantees about 14 GB. Multi-GB images plus OCI layers fit, but not
  many; the action deletes intermediate files as it goes.
- **Time**: Redis runs take about 0.6 s on KVM, the stack about 4.5 s. A pull-request budget of
  200 seeds is about a minute for Redis with 3 workers and about 6 to 8 minutes for the stack with
  3 workers, plus the image build. A nightly 5,000-seed stack exploration is about 2 hours on one
  runner, or 15 minutes on 8 shards.
- **Money**: standard runners are free for public repositories. Private repositories use the plan's
  included minutes, then pay per minute; larger runners are always paid. A self-hosted runner costs
  the machine. The report shows minutes spent per run so a team sees its cost.
- **GitHub's limits**: 6 hours per job, a concurrency limit per account (20 jobs on the free plan),
  256 jobs per matrix.

### 7.6 What breaks at 10 times and 100 times

At **10 times** (ten projects in one repository, or ten times the pull-request rate):

- The `hydo-history` branch takes many more pushes; one concurrency group serializes them, which
  is fine at this rate. Run files have unique names, so only the ledger can conflict, and the
  writer rebases and appends again.
- The 10 GB Actions cache evicts images. Images of the default branch's tip and the nightly must
  win; per-pull-request images are kept only briefly.
- Run minutes become a real line item for private repositories.

At **100 times**:

- A git branch is no longer a good database: thousands of run files a day, a ledger fold that reads
  everything. History moves to the object store backend with an index, or to a small service.
- The merge-and-shrink job is the bottleneck; shrinks fan out per signature.
- Finding identity errors (section 4.2) multiply with the number of checks and messages; the
  reviewed `known-findings.yaml` and the renamed-finding detection matter more.
- Hosted runners' concurrency limits and memory force self-hosted or larger runners; a team at this
  size wants a pool of its own machines, and hvx on bare metal (several vCPUs, the PMU clock)
  becomes worth its setup.

## 8. Packaging

### 8.1 All logic in `hydo`, a thin action around it

Every step is a `hydo` subcommand, written in Rust, tested like the rest of hydo, and shipped in
the release:

```
hydo ci run      --config hydo/ci.yaml --mode pr|nightly [--shard I/N] --out hydo-ci/
hydo ci merge    hydo-ci/shard-*/ --out hydo-ci/merged
hydo ci compare  <base ci-run.json> <head ci-run.json>        # the report, also offline
hydo ci record   --store dir:<checkout of hydo-history>       # append to history
hydo ci page     --store dir:<...> --out <dir>                # the history page
hydo ci reproduce <ci run> <finding key>
hydo bisect      <finding key> --good <commit> --bad <commit>
```

The action only installs hydo, restores caches, calls these, uploads artifacts, and posts the
comment with the `gh` program that GitHub's runners already have. This keeps the action small
enough to be a **composite action** (YAML and shell steps), with no JavaScript to build, version
and audit. Other CI systems then need only a few lines around the same commands.

### 8.2 What the team's repository needs

1. The `hydo/` folder they already have (`hydo.yaml`, workload, checks, regressions).
2. `hydo/ci.yaml`, a new file that is **not** staged into the image (if CI settings lived in
   `hydo.yaml`, changing a budget would change the image's bytes and every fingerprint):

   ```yaml
   format: 1
   engine: kvm
   build: ./ci/build-services.sh       # builds the services hydo.yaml names, from this commit
   inputs:                             # for services built here: what their archives depend on
     orders: [services/orders/]
     payments: [services/payments/, go.sum]
   pr:
     seeds: 1-200
     strategy: process
     options: [--more-faults]
     time_limit: 15m
   nightly:
     budget: 2h
     shards: 8
     strategy: process
     options: [--more-faults, --new-faults, --storage-faults]
   fail_on: [new, reappeared, regression]
   ```

3. A workflow file:

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
         - uses: actions/checkout@<pinned sha>
         - uses: <owner>/hydo/action@v1     # <owner>: hydo's GitHub account
           with:
             hydo-version: 0.3.0
             history: ${{ github.event_name != 'pull_request' }}
   ```

   Recording pull-request runs into history (section 4.5) adds a second, short workflow on
   `workflow_run`; the action documents it.

### 8.3 Versions and the release

- The action lives in hydo's repository under `action/` (`uses: <owner>/hydo/action@v1`, with
  hydo's GitHub account as the owner), released with tags `action-v1.x.y` and a moving `v1`. Its
  major version changes only when its inputs change incompatibly.
- The action installs hydo with `hydo setup --get <hydo-version>` (the install cache, M162) and
  caches `~/.hydo` by version, so a job downloads a release once per cache lifetime. The bootstrap
  is the release's `install.sh` or a small static `hydo` download, verified by the release's
  `SHA256SUMS`.
- The team pins `hydo-version`. Upgrading hydo is a pull request like any other: it is compared
  with the base under the old version. Across a fingerprint epoch, fingerprints are not compared,
  verdicts and finding keys are, and the report says so. A change of the finding key's
  normalization raises the key version (`f1` to `f2`), and `hydo ci record` maps old keys to new
  ones once, in a recorded ledger event, instead of letting every finding look new.
- `ci-run.json`, the ledger events and `ci.yaml` are versioned formats with schemas and the
  additions-only check of `hydo-schema-compat`, like every other hydo format.
- Other CI systems use the same command and files from the start (section 8.4); only the comment
  and the cache steps differ. Whether their hosted runners expose `/dev/kvm` was not checked;
  self-hosted runners always work.

### 8.4 Every CI system, with no per-system logic (user, 2026-11-08)

Teams use many CI systems (GitHub Actions, GitLab CI, CircleCI, Jenkins, Buildkite, Azure
Pipelines, Tekton and others), each with its own interface, and real projects have complicated
pipelines. hydo supports all of them without duplicating work, by keeping every decision in one
command and making the per-system parts as small as possible.

1. **One command, the same everywhere.** `hydo ci run` builds or reuses the image, explores,
   compares with the base by replay, replays the regressions and decides pass or fail. It takes
   its inputs from flags (`--commit`, `--base`, `--change`, `--mode`, `--project`) and
   `hydo/ci.yaml`, and it writes only files and an exit code:
   - `report.md`: the summary a person reads (a pull-request comment or a job summary).
   - `junit.xml`: every finding and every regression as a test case, with the minimal schedule
     and the reproduce command in its message. JUnit XML is the one test-report format that
     GitLab, CircleCI, Jenkins, Azure Pipelines, Buildkite and GitHub all show without extra
     code, as failed tests with their history.
   - `ci-run.json`: the record that history and later comparisons read (section 4.1).
   - the exit code: 0 or 1 by the team's `fail_on` policy, 2 for a run error.

   A CI system that runs a shell command and keeps files works with this alone. That is the
   generic recipe, and it covers every system hydo has no adapter for.
2. **The only per-system code is a table of environment variables.** Each CI system names the
   commit, the base branch, the change number and the run's URL in its own variables
   (`GITHUB_SHA` and `GITHUB_BASE_REF`; `CI_COMMIT_SHA` and `CI_MERGE_REQUEST_TARGET_BRANCH_NAME`;
   `CIRCLE_SHA1`; and so on). `hydo ci run` reads them from one table with a few lines per system,
   and the flags always win. Adding a CI system means adding its lines to the table.
3. **Comments and status checks stay outside hydo.** Posting a comment differs on every platform,
   so hydo never calls a platform's API. A thin adapter, about 30 to 50 lines with no decisions in
   it, posts `report.md`: the GitHub composite action (section 8.1), and a GitLab CI template as
   the second adapter. Any other adapter can be written by a user, because it contains no logic.
4. **History is independent of the CI system.** The store backends are a git branch (any git
   host), a directory (any cache or mounted volume) and S3-compatible object storage (available
   almost everywhere). None of them uses a CI system's API.
5. **Complicated pipelines.** hydo does not take part in how a team builds or deploys. It tests
   the system that the repository's `hydo/` folder describes, from the service images the team's
   pipeline already builds: `hydo ci run` is one more job after the build job, and it takes the
   image digests that job produced (`--image <service>=<digest>`, or the `build:` step of
   `ci.yaml`). A monorepo with several systems runs `hydo ci run --project <dir>` once per system.
   Argo CD deploys and does not test, so it is out of scope; Kubernetes-based CI (Argo Workflows,
   Tekton, Jenkins on Kubernetes) runs hydo as a pod.
6. **The real limit is the runner, not the interface.** hydo needs `/dev/kvm` (or the hvx module)
   on the runner. GitHub's standard Linux runners have it (proven on 2026-11-08). On other hosted
   CI it often needs a self-hosted runner, a larger machine type, or a Kubernetes node with a KVM
   device plugin. The documentation states this per platform, with the command that checks it
   (`hydo doctor --backend kvm`), so nobody has to find it out by trial.

What hydo's maintainers build and test: `hydo ci run` with its environment table, the generic
recipe as a documented page, the GitHub Action and the GitLab template. Everything else uses the
generic recipe.

## 9. The evolution in stages

| Stage | What a team gets | What hydo must add | Size |
|---|---|---|---|
| 0. Today, by hand | Install a release in a workflow, `hydo build`, `hydo explore --backend kvm`, `hydo repro --regressions`, upload `hydo-out/`. A red check on any failure. No comparison, no history, every pre-existing bug fails every pull request. | A documented example workflow. | Small: an example and a guide page. |
| 1. A useful pull-request check | One comment per pull request: findings new to the change (proven on the base), fixed by it, still open; saved regressions; what ran (fault kinds, services, properties never reached, runs, time); the `hydo diff` of each new finding against the base; fails only by policy. KVM on standard runners, images reused by input key, two shards at most. | `engine.host_cpu`; the explorer cache key fixed; the finding key; `ci-run.json`; `hydo ci run` and `hydo ci compare` (base-image replay, regressions, `report.md`, `junit.xml`, the CI environment table: section 8.4); `hydo/ci.yaml`; the image input key; the composite action and the generic recipe. The base's open findings come from the base's last `ci-run.json` artifact. | Medium: about 3 milestones (roughly 3,000 to 4,000 lines of Rust with tests, plus the action). |
| 2. History | The `hydo-history` branch: the ledger with new, present, fixed, reappeared, unverified and accepted; nightly deep runs with rotating seeds whose findings every pull request replays; `known-findings.yaml`; the history page (open findings, timeline, coverage trend, runs, hydo's health); proposed regression files when a finding is fixed. | `hydo ci record` and `page`; the store interface with the `dir:` backend; the ledger fold; the `workflow_run` recorder; budgets from history and `--time-limit`. | Medium: about 3 milestones. |
| 3. Scale and depth | Many-shard nightly runs merged and shrunk once; the commit that introduced each new finding (bisection); `hydo ci reproduce` on a laptop; targeting changed services (once measured); self-hosted hvx runners for several vCPUs; an object-store backend. | `--shrink none`, `hydo ci merge`, `--shrink-from`; `hydo bisect`; `hydo ci reproduce`; `--focus` with a benchmark measurement; the object-store backend; the shared run cache. | Large: about 4 to 6 milestones, mostly independent of each other. |
| 4. A hosted service | Cross-repository dashboards, a fleet of managed runners, a database, access control. | A server, its operation, its security, billing. | Very large, and a different kind of work. Not worth building until many teams use stages 2 and 3 and ask for it; the GitHub-native design covers a single team fully. |

Each stage is useful on its own, and none needs the next. A team that stops at stage 1 already
gets the main promise: a pull request is told only about what it caused, with proof and a command
to reproduce it.

## 10. What is not worth building

- **A dashboard server early.** The static history page and the branch answer the questions of
  one team. A server is stage 4 at the earliest.
- **Keeping every console forever.** Deterministic runs can be made again from their recipe;
  storing all evidence permanently costs much and adds little.
- **Flaky-test handling** (retries, quarantine, flakiness scores). Runs do not flake. A run that
  differs on re-run is a hydo or environment bug, reported as such, not managed as test noise.
- **Coverage percentages.** The fault space of a random strategy has no meaningful size, so any
  percentage would be invented. Counts of what was drawn and the list of properties never reached
  are honest and useful.
- **The same fingerprints on AMD and Intel under KVM.** The differences come from the host CPU
  (documented in docs/design/kvm-engine.md); verdict comparison across vendors is enough for CI.
- **Running on GitHub's hosted macOS runners.** Their virtualization does not allow it.
- **Plugins for many CI systems before anyone asks.** The commands are the interface.

## 11. Risks

1. **Finding identity.** If keys split (a reworded check) or merge (two causes, one message), the
   states shown to the team are wrong, and the team stops trusting the comment. Mitigations: keys
   from property ids where checks print them, the key version, replay-based states, the
   renamed-finding hint and the reviewed `known-findings.yaml`. It should be tried on the
   onboarded third-party examples (libSQL, Medusa, NATS) before stage 2.
2. **Build cost and latency.** Each pull request builds the team's services and the image, and a
   comparison needs the base image too. If service builds are slow or not reproducible, the image
   cache never hits, bisection is slow, and teams turn the check off. Mitigations: the input key
   by declared inputs (not by archive bytes), the base image from the base branch's cache, and a
   report of where the time went.
3. **Runner limits and the KVM engine's reach.** Hosted runners give one engine with one vCPU, the
   exit-count clock and the v2 CPU level, and a mix of CPU vendors that GitHub can change. Large
   systems need more memory than a standard runner has. Bugs that need several vCPUs or exit-free
   timing windows are out of reach on hosted runners and need self-hosted hvx. The report must say
   which engine ran so nobody reads a hosted pass as more than it is.

Also important: **security of pull requests from forks.** A fork's code runs in the job, so the
job never holds a write token; history is written only by the trusted follow-up job, which treats
artifacts as untrusted data (validated JSON, escaped text in comments).

## 12. Recommended first steps

In this order, each small and each useful alone:

1. **Correctness of what is recorded**: add `engine.host_cpu` to every result, and key the
   explorer cache by image sha256, backend, engine program, module or host CPU vendor and epoch.
   Both are small and needed by every later stage; the cache key is a latent bug today for anyone
   who reuses an image path.
2. **`hydo ci run` with the base comparison by replay** (stage 1 without the action): it writes
   `ci-run.json` and the comment text, and runs on a laptop as well as in CI. Prove it on the
   Redis quickstart and on one onboarded third-party example (the shop's planted double charge
   makes a good "new finding" pull request).
3. **The composite action** with the KVM engine, the image cache and the sticky comment, tried on a
   throwaway public repository with the same examples.

## 13. Questions for the user

- Whether pull requests should fail on `present` findings (the default here is no).
- Whether the history branch is acceptable in teams' repositories, or an object store should be
  the default from stage 2.
- Whether `hydo/ci.yaml` is the right place for CI settings (a new versioned file), or command-line
  inputs of the action are enough for stage 1.
- Where the action lives: under `action/` in hydo's repository (assumed here) or in its own
  repository.
