# Results: Gitea explored through its own pages

This page records how hydo explored Gitea 28.4.0 with its own walk through the pages and what it
found (Milestone 389, tentative number). The design of the walk is
`docs/design/ui-exploration.md`, and `hydo guide web-apps`, section "Let hydo walk through the
pages" is the how-to. The earlier apps explored this way are Memos
(`hydo/onboard/examples/memos/RESULTS.md`) and Vikunja (`hydo/onboard/examples/vikunja/RESULTS.md`).

## The short answers

- **Finding 1, new, a clock effect.** A timer of an issue that runs while the server's wall clock
  goes back records a negative time when it stops. Found blind on KVM and on hvx; the regression
  is `timer-after-a-step-back-records-a-negative-time.schedule`. Not posted.
- **Finding 2, new.** A power cut within about half a minute after a repository is created with a
  README leaves its branch broken, while Gitea's database says it is not empty. Found by directed
  power cuts; the regression is `power-cut-after-a-new-repository-breaks-its-branch.schedule`. The
  symptom is that of #31834, which was closed as a power outage's damage. Not posted.
- **What held.** Every issue, comment, close, and reopen that Gitea answered as saved stayed
  saved. Gitea's counts matched its rows, and the search found every issue. That held in 200
  schedules of kills, pauses, network faults, clock steps, and power cuts.
- **What hydo lacked.** The section of that name: chiefly no console event at an acknowledged
  write, no id in a redirect answer, and power cuts only at the end of the walk.

## The system

Gitea (https://github.com/go-gitea/gitea, about 59,000 stars and 2,192 open issues on
2026-11-28) is a self-hosted Git service. One Go server serves the pages, a REST API, and the
Git repositories, and keeps its data in SQLite by default (`gitea.db`). The example runs the
image `docker.gitea.com/gitea:28.4.0-rootless` as the documentation's "Installation with
Docker (rootless)" runs it: one container with a volume for the data and one for the
configuration.

The version is Gitea 28.4.0 (tag `v28.4.0`, commit `0b5dfde`, released 2026-11-15). The image is
`docker.gitea.com/gitea:28.4.0-rootless`, index digest `sha256:15ab7fad…`, linux/amd64
`sha256:0a28ed70…` (the lock file `hydo/hydo-compose.lock.json` has the full digests). The
browser is Microsoft's Playwright image 1.65.1 that `hydo build` fetches.

Gitea was chosen by a fit check of 30 minutes over the five candidates. It was the first that
met every criterion:

- It takes bug reports as GitHub issues.
- It has an official image and one service, and its default database, SQLite, is a file that
  a rule can read.
- A fresh instance with the installation locked lets the first account register, so the walk
  needs no fixture.
- It promises state that a fault can break: issues, comments, counts, a search index, and Git
  repositories.

The other candidates were left for these reasons:

- Forgejo is a fork of Gitea whose tracker is on Codeberg, which needs an account (the
  playbook's first red flag).
- Plane runs about ten services (API, workers, web, PostgreSQL, Redis, a message queue, an
  object store), too many for one guest beside the browser.
- Outline needs an outside sign-in provider (Slack, Google, OIDC or mail), which the guest
  cannot reach.
- Twenty CRM needs PostgreSQL, Redis, a server and a worker, and saves through GraphQL, whose
  answers carry the new object's id deeper than the walk reads (below).
- Immich is built around uploads of photos, which the walk cannot make, and needs a machine
  learning service.

| file | what it is |
|---|---|
| `docker-compose.yaml` | Gitea as its documentation runs it with Docker, rootless, with SQLite. |
| `hydo/hydo.yaml` | Started with `hydo compose ../docker-compose.yaml`; the workload is `browser.explore`. |
| `hydo/page-rules/gitea.mjs` | The project's own rules over the database and the search (below). |
| `hydo/fetch-images.sh`, `hydo/hydo-compose.lock.json` | The Gitea image by digest, written by `hydo compose`. |
| `hydo/hydo-kvm.yaml` | The manifest for KVM: `guest.proc_io: false`, because the runners' release has no KVM kernels of epoch 7. |
| `hydo/hydo-gitfsync.yaml` | The contrast of finding 2: Gitea runs Git with `core.fsync=committed`. |
| `hydo/regressions/` | The saved schedules of the findings. |

What changed from the import:

- The workload and the placeholder invariant of `hydo compose` gave way to `browser.explore`.
- The walk's setup registers the account `hydo`, the first account and so the administrator.
  It creates the repository `hydo/demo` with a README ("Initialize Repository"). It adds the
  default label set, the milestone `v1`, and one issue, and it starts that issue's timer. The
  walk then starts on `/hydo/demo/issues`, and `within: /hydo/demo/issues` keeps it on the
  repository's issues. It has 6 workload steps of 12 actions.
- `GITEA__server__ROOT_URL` names the server's address on the guest's bridge, which the guest's
  browser uses. The update checker, which asks `dl.gitea.com`, is off.
- The service's `setup:` gives both volumes to uid 1000: the rootless image runs as that user.
- `data.size_mib` is 384, not the import's 512. The browser's root is 3,584 MiB, and hvx maps at
  most 4 GiB of root and data disk together (below, "What hydo lacked").
- `guest.ram_mib: 4096` for the browser.
- The walk never signs out, deletes, changes the language, opens the settings or the API's pages,
  stars, watches or forks, searches, uses the editor's toolbar, or opens the file chooser.
- `watch.alerts` adds Gitea's error toasts (`.toastify.toast-error`) and flash messages
  (`.flash-error`). `watch.requests: /hydo/` judges the repository's requests, not the static
  files or the notification stream `/user/events`. `watch.failures_of: "^POST /hydo/demo/issues"`
  names the issue writes. `watch.console: false`: Gitea logs each failed fetch with
  `console.error` and shows it as an error toast.
- `guest.clock_after_cut: 60`: the recovery boot after a power cut continues the first boot's
  clock, 60 s later, as a server's clock does.

## The rules

`gitea.mjs` runs in the browser's service, whose Node.js reads SQLite (`node:sqlite`), on a copy
of the database and its rollback journal. Gitea's setting `SQLITE_JOURNAL_MODE` is empty by
default (`modules/setting/database.go`, line 75), and its drivers then name no journal mode
(`models/db/driver_sqlite_mattn.go` and `driver_sqlite_modernc.go`). So the database uses
SQLite's default rollback journal with `synchronous=FULL`, and a commit is on the disk when Gitea
answers.

The walk writes one word to `$HYDO_ACKED` for each write that Gitea answered 2xx, such as
`POST:/hydo/demo/issues/new` or `POST:/hydo/demo/issues/2/comments`. Gitea's pages send their
forms with `fetch` and answer `{"redirect": "/hydo/demo/issues/3"}`, without an `id` field. So a
word names the write's path and, for a comment, its issue, but not the new object.

- `gitea.saved_work_survives`: a repository has at least as many issues as the walk created there
  (the walk deletes no issue). An issue has at least as many comments, closes, and reopens as the
  acknowledged posts of its comment form. A post of that form makes a comment, a close, or a
  reopen, and the word does not say which. When it fails, it prints the issue's state and every
  event of its timeline on a line before its verdict.
- `gitea.counters_match`: the numbers that Gitea keeps beside its rows agree with the rows. These
  are a repository's open and closed issues, an issue's comments, a label's and a milestone's open
  and closed issues, and the repository's last issue number. Gitea shows these numbers on its
  pages: the tab "Issues 3", the line "2 Open 1 Closed", and a label's and a milestone's counts.
- `gitea.issues_found_by_search`: the search of the repository's issues finds every issue of the
  database by the longest word of its title of 4 to 20 letters and digits. Gitea fills its issue
  index (bleve) from a queue of its own after the commit, so the rule asks the API
  (`GET /api/v1/repos/hydo/demo/issues?q=`) for up to 60 s.
- `gitea.repository_readable`: the repository `hydo/demo` has its branch `main`, and `git fsck
  --connectivity-only` reaches every object from it. The rule runs `git` in the server's image.
- `gitea.tracked_time_not_negative`: every time that Gitea tracked for an issue is zero or more.
  The rule first stops every timer that still runs through the API (`POST
  .../issues/<n>/stopwatch/stop`), as the user would, and then reads every tracked time back
  (`GET /api/v1/repos/hydo/demo/times`). Gitea computes a stopped timer's time as the wall clock
  minus the timer's start (`models/issues/stopwatch.go`, line 140).

## How the runs were made

- hvx on the bare-metal machine, the main module (srcversion `91D86B693F4D4035C368F9B`), one VM
  of this onboarding at a time beside another session's VMs.
- hydo: this branch's `hydo`, `hydo-hvx` and `hydo-build` over the published 0.7.0 Linux release,
  at the fingerprint epoch 7.
- KVM: three explorations of `hydo/hydo-kvm.yaml` on GitHub's runners in 20 parts each
  (`ci/remote-explore.sh`), on AMD EPYC 7763, 9V45 and 9V74 hosts and on Intel Xeon 6975P-C and
  Platinum 8573C hosts.
- The image was built five times as the rules and the settings changed. Each section names its
  image by the rule it had.

## Without faults

`disk=none | run I=1` registers the account, creates the repository, its labels, its milestone
and its first issue, and starts the issue's timer. Then the walk takes 72 actions in 6 steps. In
them it creates two more issues, closes and comments on one, starts and stops a timer, sets a due
date, and opens the labels, milestones, assignees and dependencies of the sidebar. A run takes
about 100 s of wall time. On the final image both runs gave FP_CLEAN and every invariant held.
The first image gave `b8cd01e4d6f4acea` twice, the image with the search rule `990b215e5399e488`
twice, and the image with the clock across a power cut `fd2cd864ae5628c2` twice.

## Finding 1 (new candidate, a clock effect): a timer stopped after the clock went back records a negative time

**What a user meets.** A user starts the timer of an issue ("Start timer"). The server's wall
clock then goes back, for example when NTP corrects a clock that ran fast, or a virtual machine
is restored. When the user stops the timer, Gitea records a negative time. The issue's timeline
says "hydo worked for now", and the issue's "Total Time Spent" counts the negative time against
the times tracked before it: on this Mac, with 2 s tracked before, the total was empty. The
repository's times, the user's times, and the API (`GET /repos/{owner}/{repo}/times`) show the
negative number. The form "Add Time" refuses a total of zero or less, so a user cannot enter
such a time by hand.

**How hydo found it.** Blind, on KVM in the second exploration (seeds 72 and 79, `T=2:-2592000`
and `T=2:-300`), and on the final manifest in 3 of 60 KVM schedules (seeds 122 and 169, steps
back of an hour) and in 1 of 20 hvx schedules (seed 37 of e2, `T=4:-300`: −286 s). The rule
`gitea.tracked_time_not_negative` stops a timer that still runs at the end through the API, as
the user would, and reads every tracked time back.

**The regression.** `regressions/timer-after-a-step-back-records-a-negative-time.schedule`:
`disk=none | run T=2:-3600 I=1,x0.0-11,x1.0-11,x2.0-11,x3.0-11,x4.0-11,x5.0-11`. The walk
leaves out every action, so only the setup's timer runs, from step 0 to the end. It failed twice
with FP_BACK: "the issue #1 of hydo/demo has a tracked time of −3,5xx s". Without the step
(FP_NONE) and with a step forward of an hour (FP_FWD) it passes; the step forward records an hour
that nobody worked, as a timer on the wall clock does.

**Why.** `finishIssueStopwatch` (`models/issues/stopwatch.go`, line 140, the same on `main`)
computes the time as `time.Now().Unix() - int64(sw.CreatedUnix)` and stores it with no lower
bound, in a tracked time and in the comment's text. `Stopwatch.Seconds()` (line 32) shows the
running timer the same way. Both stops of a timer reach it: "Stop timer"
(`routers/web/repo/issue_stopwatch.go`, line 48) and a close of the issue, which stops the timer
in the close's transaction (`services/issue/status.go`, line 32). The manual "Add Time" refuses
`total <= 0` (`routers/web/repo/issue_timetrack.go`, line 39). The cause is plain in the code, so
no reproduction without hydo was made. On this Mac a check of the page only set a running timer's
start an hour ahead in the database of Gitea in Docker and stopped it: Gitea stored −3,586 s and
showed the entry as "worked for now".

**Duplicate search** (go-gitea/gitea, issues and pull requests, open and closed, 2026-11-28; the
table below): no issue names a negative tracked time or a timer after a clock change.

## Finding 2 (new candidate): a power cut right after a repository is created leaves its branch broken

**What a user meets.** A user creates a repository with "Initialize Repository", so Gitea commits
a README, a license and a `.gitignore`, and shows the new repository. The machine loses its power
within about half a minute. After the restart Gitea's database says the repository has its
branch `main`, and the branch's ref file in the Git repository is broken: `git` says "warning:
ignoring broken ref refs/heads/main" and finds no commit. INSPECT_PAGE

**How hydo found it.** Directed, by the rule `gitea.repository_readable` and the power cuts that
the blind explorations did not draw: the process strategy put all 47 of its power cuts at the end
of the walk (`C=6`), and no such cut came within half a minute of the setup. `disk=none | run C=1
I=1` cuts the power after step 0, about 20 s of guest time after the setup's commit. It failed
twice with FP_CUT1 on the final image. With a disk that keeps its write cache (`disk=cut`) it
fails the same way (FP_DCUT1), and so does the walk without actions (FP_CUT1X). A cut after step 1
(`C=2`) passes (FP_CUT2): by then the guest's kernel had written the files back.

**The regression.** `regressions/power-cut-after-a-new-repository-breaks-its-branch.schedule`:
`disk=none | run C=1 I=1`.

**Why.** Gitea writes the first commit with Git: `initRepoCommit` (`services/repository/init.go`)
runs `git add`, `git commit` in a temporary folder and pushes to the bare repository
(`git.PushFromLocal`). `git receive-pack` stores the objects of a small push as loose objects and
updates `refs/heads/main` by a rename. With Git's default `core.fsync`, which leaves out loose
objects and refs, nothing of it is synced. Gitea then commits the repository's row with
`is_empty = false` to SQLite, which syncs. Gitea sets no `core.fsync` of its own
(`modules/git/config.go` sets other keys), and its documentation names none. With Gitea's
`[git.config]` section setting `core.fsync=committed` (`hydo/hydo-gitfsync.yaml`), the same cut
passes: FSYNC_CONTRAST.

**Duplicate search.** #31834 ("Power outage during merge request, causing the main branch to be
unavailable", Gitea 1.21) has the same symptom after a merge. It was closed with "Power outage
could cause any damage to your system". No issue names the missing sync of Git's refs and objects
before Gitea answers, or `core.fsync`. #19551 mentions `core.fsyncObjectFiles` in a user's own
configuration. The cause is plain in the code and in Git's documentation of `core.fsync`, so no
reproduction without hydo was made.

## The first failures: mistakes of the rules

Each exploration's first failures were the rules' own:

- e1 seed 16 and KVM seeds 37, 41 and 57 failed `gitea.saved_work_survives`. Each walk had closed
  and reopened an issue, and the rule counted Gitea's comment types 0, 2 and 3. A reopen is type
  1 (`models/issues/comment.go`, line 71). The trace of KVM seed 41 showed both posts
  (`status=close`, `status=reopen`) answered `{"redirect": "/hydo/demo/issues/1"}`, and Gitea's
  log showed both `200 OK` without an error. Fixed to 0, 1 and 2.
- Four failures of the timer rule in the second KVM exploration (seeds 78, 86 and 106, and 66)
  had a power cut at the end. Without `guest.clock_after_cut` the recovery boot's clock starts at
  `clock_start` again, before the timer's start (MANIFEST.md "Clock after a power cut"), so the
  rule's stop recorded a negative time. Seed 106 had no clock step at all. The manifest now
  continues the first boot's clock 60 s after the cut.
- KVM seed 174 of the last exploration failed `browser.no_page_errors` with `Fetch action
  request error: TypeError: Failed to fetch`, during a loss of 81 % of the requests. Gitea logs
  each failed fetch with `console.error` and shows it as an error toast at once
  (`web_src/js/modules/fetch-action.ts`, lines 171 and 172), so the error was shown. The final
  manifest sets `watch.console: false`, as the Vikunja example does.

## The duplicate searches

Searched in go-gitea/gitea, issues and pull requests, open and closed, on 2026-11-28, with `gh
search issues --include-prs`. The repository has no discussions.

| phrase | results | relevant |
|---|---|---|
| `stopwatch`, `tracked time`, `time tracking` | 25 each | none names a negative time; #38568 (spent-on dates) and #30787 (stopwatch rework) are other changes |
| `negative tracked time`, `stopwatch negative`, `negative time spent`, `time tracking negative`, `stopwatch clock`, `tracked time wrong` | 0 | none |
| `power outage` | 4 | #31834, the same symptom after a merge, closed as a power outage's damage; #12880 and #7235, broken repositories without a cause |
| `power failure`, `unclean shutdown` | 3 each | #19933 (a project file damaged), #38210 (bleve), #30781 (a GPG lock file); none about refs |
| `fsync`, `core.fsync` | 5 and 1 | #19551, a user's own `core.fsyncObjectFiles`; no issue about Gitea's Git writes |
| `ignoring broken ref`, `broken ref`, `refs/heads empty`, `Initial commit missing`, `repository corrupted power`, `crash repository broken`, `durability` | 0 to 2 | none |
| `repository is broken`, `git fsck`, `object not found`, `auto init` | 6 to 30 | #34443 (marking broken repositories), #39022 (health checks against bitrot); none about a power cut after a creation |
| `num_issues wrong count`, `issue count incorrect`, `num_comments`, `label count wrong`, `milestone count`, `check_repo_stats` | 0 to 6 | #11164 (the doctor's recount); nothing broke the counters here |
| `issue indexer queue`, `indexer not updated`, `issue not searchable`, `bleve corrupt`, `levelqueue` | 0 to 6 | #33882 and #40219, a queue broken by an interrupted push, fixed in levelqueue 0.5.1, which 28.4.0 has |

## The explorations

| exploration | rules | seeds | schedules | wall time | failures |
|---|---|---|---|---|---|
| e1, hvx, `--strategy process --no-shrink`, 1 worker | the first three rules and the search | 1-20 | 20 | 76 min (144 s a run) | 1: a mistake of the rule (seed 16) |
| k1, KVM, 20 parts | the same | 21-60 | 40 | 12 min | 3: the same mistake (37, 41, 57) |
| k2, KVM, 20 parts | the reopen fixed, the timer rule, no clock across a cut | 61-120 | 60 | 16 min | 6: finding 1 (72, 79), and the recovery boot's clock (66, 78, 86, 106) |
| e2, hvx, 1 worker | the clock across a cut | 21-40 | 20 | 34 min (84 s a run) | 1: finding 1 (37) |
| k3, KVM, 20 parts | the same | 121-180 | 60 | 16 min | 3: finding 1 (122, 169), and `console.error` (174) |

Of the 200 schedules, 150 had a kill of the server, 156 a pause, 138 a network fault, 67 a step
back and 75 a step forward of the clock, and 47 a power cut. Every power cut came at the end of
the walk (`C=6`). No exploration shrank: the failures were judged by directed runs.

## What held

- Every issue, comment, close and reopen that Gitea answered as saved was in its database, under
  kills, pauses, network faults, one-way network faults, clock steps and power cuts at the end of
  the walk. Gitea's SQLite runs with its rollback journal and `synchronous=FULL`, so a commit is
  on the disk when Gitea answers.
- The counts that Gitea keeps beside its rows (issues, closed issues, comments, labels,
  milestones, the last issue number) matched the rows in every run.
- The search found every issue by a word of its title, also after kills and power cuts at the end
  of the walk.
- No page left a spinner on the screen for 10 s or printed `NaN`, `undefined` or `Invalid Date`,
  and every failed write showed an error toast.


## What hydo lacked

The gaps are in the order of the time they cost. The ones of the walk go to open item 198 of
`AGENTS.md`.

- **No event at an acknowledged write.** The walk writes `$HYDO_ACKED` when the server answers a
  write, but it writes no console record. A reaction `A=<event>+<ms>:cut` acts at an SDK
  assertion record, so no schedule can cut the power a few milliseconds after a write that the
  walk saw acknowledged. The power cuts of this page came at the steps (`C=`) and at the k-th
  disk flush (`cf=`, hvx only).
- **Power cuts only at the end of the walk.** The process strategy put all 47 of its power cuts
  after the last step (`C=6`), as in the SeaweedFS example (open item 186). Finding 2 needs a cut
  within half a minute of the setup's push, so only directed runs reached it.
- **The recovery boot's clock.** Without `guest.clock_after_cut` a power cut set the clock back to
  `clock_start`, and the timer rule then recorded a negative time in four KVM schedules, one of
  them without any clock step. The import does not write the key, and the guide names it for
  checks that compare times across a cut, not for a running timer of the system.
- **No id in a redirect.** The walk takes ids only from a JSON field named `id`. Gitea answers
  each write of its fetch forms with `{"redirect": "/hydo/demo/issues/3"}`, which names the new
  issue in a path. So `$HYDO_ACKED` has no id, `browser.read_back` cannot be used, and the rule
  counts the issues of a repository instead of naming each one.
- **No body in a word.** A word names the method and the path, not what was sent. Gitea's
  comment form posts a comment, a close, or both, to one path, so the rule cannot tell which, and
  it counts comments, closes and reopens together.
- **The walk wanders.** Without `within`, the first native walks spent their actions in the API's
  Swagger pages, the settings, the code view and the search. With `within: /hydo/demo/issues` the
  walk stays on the issues, but `avoid` needs 13 patterns. Two of them cover the editor's toolbar,
  whose 15 buttons insert text and save nothing. In the 60 walks of the last KVM exploration, 489 of 3,408
  actions (one in seven) still filled a field without sending it, 222 of them the comment field.
- **A setup step's failure does not name the step.** The first walk's setup stopped with
  `locator.click: Timeout 10000ms exceeded`. The button was `Register Account`, not `Create
  Account`, and the message did not say which of the eleven setup steps failed. The labels are
  `Username *` in the accessibility tree, and `{fill: Username}` still matches them.
- **A disk 1 MiB over hvx's limit passes `--check`.** The first image's root of 3,584 MiB and the
  import's data disk of 512 MiB made a disk of 4 GiB and 1 MiB, the partition table. `hydo build
  --check` printed `OK`, and every run ended at once with `ERROR harness boot vm`, with
  `HVX_SET_DISK: Invalid argument (os error 22)` only in `engine.log`.
- **The release tree's iproute2.** `hydo build` of an unpacked release tree stopped with
  `HYDO_IPROUTE2 not found at /nix/store/...` until `guest/store.tar` was unpacked into
  `guest/store` by hand (open item 98).
- **The sync audit misses writes of a program that the system runs.** `hydo rules scan --kind
  sync` listed 24 places of Gitea's Go code. Gitea's Git repositories are written by `git`, which
  it runs as a child process, and the audit cannot see those writes.

## Time

| phase | wall time |
|---|---|
| Scouting and the fit check | 30 minutes |
| Setup to the first clean run, with native walks of Gitea in Docker on this Mac | 45 minutes |
| The explorations e1 and e2 on hvx, and k1 to k3 on KVM | 2 hours of machine time, partly in parallel |
| Judging: the rules' mistakes, the trace of KVM seed 41, the directed runs and the contrast | 3 hours, much of it builds of 15 to 40 minutes beside another session's builds |
| Duplicate search and write-up | 1 hour |

Three restarts of the agent's process on this Mac, two of them reboots, stopped the waits on this
Mac. The jobs on the bare-metal machine ran on and were read from their logs.

