# Results: Gitea explored through its own pages

This page records how hydo explored Gitea 28.4.0 with its own walk through the pages and what it
found (Milestone 389, tentative number). The design of the walk is
`docs/design/ui-exploration.md`, and `hydo guide web-apps`, section "Let hydo walk through the
pages", is the how-to. The earlier apps explored this way are Memos
(`hydo/onboard/examples/memos/RESULTS.md`) and Vikunja (`hydo/onboard/examples/vikunja/RESULTS.md`).

<!-- SHORT ANSWERS -->

## The system

Gitea (https://github.com/go-gitea/gitea, about 59,000 stars and 2,192 open issues on
2026-11-28) is a self-hosted Git service. One Go server serves the pages, a REST API, and the
Git repositories, and keeps its data in SQLite by default (`gitea.db`). The example runs the
image `docker.gitea.com/gitea:28.4.0-rootless` as the documentation's "Installation with
Docker (rootless)" runs it: one container with a volume for the data and one for the
configuration.

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
| `hydo/regressions/` | The saved schedules of the findings. |

What changed from the import:

- The workload and the placeholder invariant of `hydo compose` gave way to `browser.explore`.
- The walk's setup registers the account `hydo`, the first account and so the administrator. It
  creates the repository `hydo/demo` with a README ("Initialize Repository"), the default label
  set, the milestone `v1`, and one issue, and it starts that issue's timer. The walk then starts
  on `/hydo/demo/issues`, and
  `within: /hydo/demo/issues` keeps it on the repository's issues. It has 6 workload steps of 12
  actions.
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
  names the issue writes.

## The rules

`gitea.mjs` runs in the browser's service, whose Node.js reads SQLite (`node:sqlite`), on a copy
of the database and its rollback journal. Gitea's setting `SQLITE_JOURNAL_MODE` is empty by
default (`modules/setting/database.go`, line 75), and its drivers then name no journal mode
(`models/db/driver_sqlite_mattn.go` and `driver_sqlite_modernc.go`). So the database uses
SQLite's default rollback journal with `synchronous=FULL`, and a commit is on the disk when Gitea
answers.

The walk writes one word to `$HYDO_ACKED` for each write that Gitea answered 2xx, such as
`POST:/hydo/demo/issues/new` or `POST:/hydo/demo/issues/2/comments`. Gitea's pages send their
forms with `fetch` and answer `{"redirect": "/hydo/demo/issues/3"}`, without an `id` field, so a
word names the kind of write and, for a comment, its issue, but not the new object.

- `gitea.saved_work_survives`: a repository has at least as many issues as the walk created there
  (the walk deletes no issue), and an issue has at least as many comments, closes and reopens as
  the acknowledged posts of its comment form. A post of that form makes a comment, a close or a
  reopen, and the word does not say which. When it fails, it prints the issue's state and every
  event of its timeline on a line before its verdict.
- `gitea.counters_match`: the numbers that Gitea keeps beside its rows agree with the rows. These
  are a repository's open and closed issues, an issue's comments, a label's and a milestone's open
  and closed issues, and the repository's last issue number. Gitea shows these numbers on its
  pages: the "Issues 3" tab, "2 Open 1 Closed", a label's and a milestone's counts.
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

<!-- RUNS -->

## What hydo lacked

The gaps are in the order of the time they cost. The ones of the walk go to open item 198 of
`AGENTS.md`.

- **No event at an acknowledged write.** The walk writes `$HYDO_ACKED` when the server answers a
  write, but it writes no console record. A reaction `A=<event>+<ms>:cut` acts at an SDK
  assertion record, so no schedule can cut the power a few milliseconds after a write that the
  walk saw acknowledged. The power cuts of this page came at the steps (`C=`) and at the k-th
  disk flush (`cf=`, hvx only).
- **No id in a redirect.** The walk takes ids only from a JSON field named `id`. Gitea answers
  each write of its fetch forms with `{"redirect": "/hydo/demo/issues/3"}`, which names the new
  issue in a path. So `$HYDO_ACKED` has no id, `browser.read_back` cannot be used, and the rule
  counts the issues of a repository instead of naming each one.
- **No body in a word.** A word names the method and the path, not what was sent. Gitea's
  comment form posts a comment, a close, or both, to one path, so the rule cannot tell which, and
  it counts comments, closes and reopens together.
- **The walk wanders.** Without `within`, the first native walks spent their actions in the API's
  Swagger pages, the settings, the code view and the search. With `within: /hydo/demo/issues` the
  walk stays on the issues, but `avoid` needs 13 patterns, most for the editor's toolbar, whose
  buttons insert text and save nothing. About a third of the actions still filled the comment
  field without sending it.
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
