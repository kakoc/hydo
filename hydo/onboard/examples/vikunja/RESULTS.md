# Results: Vikunja explored through its own pages

This page records how hydo explored Vikunja 2.8.0 with its own walk through the pages
(Milestone 273) and what it found. The design is `docs/design/ui-exploration.md`, and `hydo
guide web-apps`, section "Let hydo walk through the pages", is the how-to. The first app explored
this way was Memos (`hydo/onboard/examples/memos/RESULTS.md`).

## The system

Vikunja (https://github.com/go-vikunja/vikunja, about 6,100 stars on 2026-11-16) is a
self-hosted task manager. One Go server serves the Vue frontend and a REST API (`/api/v1` and
`/api/v2`), and keeps its data in SQLite by default (`vikunja.db`). The example runs the image
`vikunja/vikunja:2.8.0` as the documentation's "Example with SQLite" runs it with Docker: one
container with a volume for the database and one for the files.

Vikunja was chosen over the other candidates for these reasons:

- Actual Budget (about 30,000 stars) keeps the budget in the browser and syncs binary messages,
  so a rule over the server's state cannot read the money. Its pages also need a secure
  context, and the guest's browser reaches the server over plain HTTP.
- Docmost (about 22,000 stars) saves a page's text over a collaboration WebSocket, which the
  walk does not see, and it needs PostgreSQL and Redis besides the server.
- Linkding (about 11,000 stars) saves with HTML forms that answer with a redirect, so the walk
  records no acknowledged write.
- Mealie (about 14,000 stars) fits too. Vikunja's data, a user's tasks and projects, is lost
  for good when a write that it acknowledged is lost, and its default SQLite settings are the
  ones a self-hosted user runs.

| file | what it is |
|---|---|
| `docker-compose.yaml` | Vikunja as its documentation runs it with Docker and SQLite. |
| `hydo/hydo.yaml` | Started with `hydo compose ../docker-compose.yaml`; the workload is `browser.explore`. |
| `hydo/page-rules/vikunja.mjs` | The project's own rules over the database (below). |
| `hydo/fetch-images.sh`, `hydo/hydo-compose.lock.json` | The Vikunja image by digest, written by `hydo compose`. |
| `walk-repro/quick-add-new-task.spec.ts` | Finding 2 as a Playwright test that runs without hydo. |

What changed from the import:

- The workload and the placeholder invariant of `hydo compose` gave way to `browser.explore`. The
  walk starts on `http://10.89.0.11:3456/`, the server's address on the guest's bridge, and
  `VIKUNJA_SERVICE_PUBLICURL` names that address. It has 6 workload steps of 12 actions.
- The first step creates the account `hydo` on the registration page, and the later steps keep
  the session.
- The service's `setup:` gives both volumes to uid 1000, as the documentation's `chown 1000`
  does: the image runs as that user, and without it the server stops at its start ("Could not
  init file handler: ... permission denied").
- The walk never logs out, deletes the account, opens the importers (they reach Todoist, Trello
  and others on the internet), or changes the language.
- `watch.alerts` adds `.vue-notification`, Vikunja's toasts. `watch.console: false`: Vikunja
  logs every error that it shows as a toast with `console.error`.
- `watch.requests: /api/` judges the API's requests, not the static files.
  `watch.failures_of: "^(POST|PUT|PATCH|DELETE) /api/v[12]/(projects|tasks|labels|teams|filters)"`
  names the writes. The pattern matches `<METHOD> <path>` (new in this milestone), because
  Vikunja reads and writes a task on one path (`GET` and `PATCH /api/v2/tasks/4`).
- `guest.ram_mib: 4096` for the browser, and `guest.outcomes_after_cut` so that the walk's
  acknowledged writes reach a recovery boot.

## The rules

`vikunja.mjs` runs in the browser's service, whose Node.js reads SQLite (`node:sqlite`), on a
copy of the database and its WAL. Each rule reads the words that the walk wrote to
`$HYDO_ACKED`. Since this milestone the walk writes one word `<METHOD>:<path>=<id>` for every id
that a write's JSON answer returns, such as `POST:/api/v2/projects/1/tasks/bulk=6`, and
`DELETE:/api/v2/tasks/6` for an answer without an id. Before, it wrote only resource names of
the shape `memos/<id>`, and Vikunja's answers have none.

- `vikunja.saved_work_survives`: every task, project, comment, label, saved filter and team that
  Vikunja answered as created is a row of its table, unless a write that Vikunja answered
  deleted it. A task in the trash can be purged, and a deleted project takes its tasks along.
  The first image checked tasks, projects and comments only; the seed-18 walk below also saved
  a filter, so the second image checks labels, saved filters and teams too.
- `vikunja.kanban_matches_done`: on every Kanban view that keeps its own buckets, each task of
  the project that is not in the trash is in one bucket of the view. When the view has a done
  bucket, the task is in it exactly when it is done. Vikunja moves a task that is marked done
  into the done bucket, and marks a task done that is moved there
  (`pkg/models/tasks.go`, `moveTaskToDoneBuckets`).

## How the runs were made

- Machine: the bare-metal machine, hvx, the main module (srcversion `91D86B693F4D4035C368F9B`).
- hydo: this branch's `hydo` over the 0.3.3 Linux release, fingerprint epoch 3.
- Image: `hydo build hydo/hydo.yaml`, which fetched playwright-core 1.65.1 into `browser/` by its
  pinned sha256.
- One VM at a time: other sessions shared the machine.

## One walk

`disk=none | run I=1` created the account and walked 72 actions in 6 steps: it added five tasks
on the home page and in the Inbox, marked the Inbox as a favorite, opened the trash, the teams,
the labels and the keyboard shortcuts, and opened a task's page with its label and due date
controls. It took 75 s of wall time, and every invariant held: "all 5 tasks, 0 projects and 0
comments that Vikunja answered as saved are in the database". A second run gave the same
fingerprint, `e6e35a2ab56471e5`. The first steps of the walk were the same as a walk of the same
seed on this Mac against Vikunja in Docker, before the image was built.

## The blind exploration of all faults

`hydo explore --seeds 1-20` (one worker, on the first image) drew kills, network faults, power cuts and disk faults
of the Vikunja server, and a walk for every schedule. It ran in 106 minutes of wall time (a mean
of 40.1 s per run, and 129 runs to reproduce and shrink). Four schedules failed, and each
reproduced:

| class | what failed | seeds |
|---|---|---|
| a disk that keeps its write cache (`disk=cut`) | a task that Vikunja answered as saved was gone after a power cut: finding 1 | 18 |
| disk I/O errors | the page left a promise rejection unhandled: finding 2 | 9 |
| disk I/O errors | the rule could not read its copy of the database ("no such table: tasks") | 1, 8 |

The two runs of the last row end in ERROR, not in a finding: the rule exits 2, "cannot check
now", after five copies that it could not read. The data disk returns seeded I/O errors to every
reader, the rule's own `cp` of the database and its WAL included. The walks of both runs worked
with Vikunja after its restart, so Vikunja itself read its tables. Which read of the copy failed
was not traced.

## The exploration with every fault kind but the lying disk

`hydo explore --seeds 21-60 --new-faults --durable-only` (one worker) ran on the third image, whose
rule puts the list of lost objects on a line before its verdict. `--durable-only` leaves out the
disk that drops acknowledged writes and the disk I/O errors. The 40 schedules drew 14 kills, 19
network faults, 2 one-way network faults, 16 pauses, 10 clock steps and 19 power cuts of the
server, each with a walk of 6 steps of 12 actions. They ran in 114 minutes of wall time (a mean
of 41.5 s per run).

- Five schedules failed, all with finding 1, and each reproduced: seeds 24, 49, 55, 59 and 60.
  Their power cuts came before step 1, 3 or 4, and they lost 1 to 4 of the objects that Vikunja
  had answered as saved, 9 of 21 in all.
- The 35 passing walks saved 65 objects, and each was in the database at the end, also after the
  14 other power cuts.
- The five failures had one signature, so one shrink ran: seed 24, 47 runs, to `disk=cut
  dseed=aac8c00000a81e44 | run C=4
  I=772ff9303a3b7ac5,n2,x0.0,x1.0,x1.1,x2.0,x2.1,x3.0,x3.1,x4.0,x4.1,x5.0,x5.1`
  (`a6607ec0eb8fce80` twice). Its one action, 0.1, types `**bold** and a list:` with two list
  lines into "Add a task…" and presses Enter. Vikunja's quick add made a label and three tasks of
  it, and the cut before step 4 lost all four.
- The Kanban rule held in all 40. In 20 of the walks an action opened a Kanban view or a task's
  bucket button, but no walk marked a task done (below).

An exploration of the same seeds on the second image was stopped after 5 schedules. Its seed 24
failed with finding 1, and its verdict named every lost object, so a loss of four objects and a
loss of two would have had two signatures, and each would have been shrunk on its own.

## Finding 1 (new candidate): a power cut loses the tasks that Vikunja saved in the last seconds

**What a user meets.** A user of a self-hosted Vikunja with its default database, SQLite, adds a
task, and the page shows it as saved. The machine then loses power, or the host's kernel stops,
a few seconds later. After the restart the task is gone. In the runs below, the tasks saved in
the walk's step before the cut were lost, and the tasks saved a step earlier were kept. No message says that anything
was lost, and the user has no copy of it.

**How hydo found it.** Blind, in seed 18: `disk=cut dseed=1120b3d00955f032 | run
N=vikunja@0-1:reorder,29%,delay,17ms C=6 I=d94729ae6c47c1f6`. The walk saved a task in step 4
(`POST /api/v2/projects/1/tasks/bulk`, answered 201) and a saved filter in step 3, and the power
was cut after the last step. In the recovery boot `vikunja.saved_work_survives` failed: "Vikunja
answered that it saved the task 1, and the database does not have it." The shrinker took 40 runs
and kept one action:

| | schedule | actions |
|---|---|---|
| before | `disk=cut dseed=1120b3d00955f032 \| run N=vikunja@0-1:reorder,29%,delay,17ms C=6 I=d94729ae6c47c1f6` | 72 in 6 steps |
| after | `disk=cut dseed=1120b3d00955f032 \| run C=6 I=d94729ae6c47c1f6,n4,x0.0-3,x1.0-3,x2.0-3,x3.0-3,x4.0-3,x5.0,x5.1,x5.2` | 1 |

The minimal walk creates the account, then in step 5 fills "Add a task…" with `hydo` and presses
Enter. Vikunja answers the create, and the walk writes
`POST:/api/v2/projects/1/tasks/bulk=1` to `$HYDO_ACKED`. The power is cut a few seconds later,
after the step. Both confirmation runs failed the same way (`14dd446ea98dd932` twice). Every
shrink candidate without `C=6` passed, and so did every candidate whose walk saved no task.

**How much is lost.** On the second image (the rule with labels, saved filters and teams),
`disk=none | run C=5 I=1` cuts the power before step 5 of the walk of seed 1, which saves five
tasks: one each in steps 0, 1 and 3, and two in step 4. The recovery boot had the first three and
lacked tasks 4 and 5: "Vikunja answered that it saved the task 4, and the database does not have
it (2 of 5 saved objects are missing: the task 4, the task 5)." Both runs gave
`b9c4ca304679f322`. `disk=none` loses only what the guest's kernel had not written to the disk
yet, so the tasks of the last step before the cut were lost, and the older ones had reached the
disk. With a disk that keeps a write cache, `disk=cut | run C=6 I=1`, the same two tasks were lost
(`3cc65a908cfb8df3`).

The minimal schedule failed the same way on the second image (`a962e661cd56616f` twice), and
also with `disk=none` in place of `disk=cut` (`7f5502f1e8f91845`): the loss needs no disk cache,
only the guest's own page cache, which a commit in this mode leaves unsynced.

**Why.** Vikunja opens SQLite with `?_busy_timeout=5000&_journal_mode=WAL` (`pkg/db/db.go`,
`initSqliteEngine`) and sets no `synchronous`. Its driver, `github.com/mattn/go-sqlite3`
1.14.52, compiles SQLite with `-DSQLITE_DEFAULT_WAL_SYNCHRONOUS=1` (`sqlite3.go`, its cgo flags),
so a database in WAL mode runs with `synchronous=NORMAL`. SQLite's documentation says that in
this mode a committed transaction "might roll back following a power loss or system crash": the
WAL is synced only at a checkpoint, not at each commit. Vikunja answers a create after the commit,
so an acknowledged task can be lost. The WAL mode came with commit `98f2893f` ("fix(db): use WAL
mode for SQLite and temp file for ephemeral databases", 2026-03-02), first released in v2.2.0.
Before, SQLite's rollback journal ran with the driver's default `synchronous=FULL`. The image
that the documentation runs, `vikunja/vikunja`, is built with cgo and this driver. A data source
name with `&_synchronous=FULL` would sync the WAL at every commit. The cause is plain in the code,
so no reproduction without hydo was made.

**Duplicate search** (go-vikunja/vikunja, issues and pull requests, open and closed, 2026-11-16;
the repository has no discussions, and a web search of the forum community.vikunja.io and the
old tracker found no thread): `synchronous`, `_synchronous`, `power loss`, `power outage`, `power
failure`, `sqlite data loss`, `sqlite WAL`, `WAL`, `journal_mode`, `fsync`, `durability`, `lost
tasks`, `tasks disappeared`, `data lost`, `data loss`, `unclean shutdown`, `corrupt`, `sqlite
crash`, `busy_timeout`, `database is locked`. None names it. Pull request 2336 restored the busy
timeout, and issue 2379 is about locks under concurrent writes.

## Finding 2 (new candidate, small): a task of the quick action bar that the server refuses is lost and throws

**What a user meets.** The user opens the quick action bar (the search button, or Ctrl+K), picks
"New task", types a title and presses Enter, and the server fails to create the task. A toast
says "unexpected error occurred". The bar has closed, the typed title is gone, and the page
leaves the error unhandled. The task field of the home page is written to put the title back
after such an error (below); this one has no such step.

**Why.** `onEnter` in `frontend/src/components/quick-actions/QuickActions.vue` calls `doCmd()`
without `await` or `catch`. `doCmd` closes the bar first and then awaits the command's action,
`newTask`, which awaits `createNewTask` without a `catch`. The rejection of the create's mutation
reaches no handler. `newTeam` in the same file catches its error, and the bulk create of the home
page restores the title on an error (`useQuickAddTask.ts`).

**How hydo found it.** Blind, in seed 9 of the exploration of all faults: `disk=ioerr:2
dseed=aeaf52febe706064 | run K=vikunja@0+2 I=7e77dad23c62fc84`. Disk I/O errors made Vikunja
answer 500, and `browser.no_page_errors` failed: "The page /user/settings/general left a promise
rejection unhandled during the test "walk step 3": ApiError: unexpected error occurred (6 times in
this run; this is the first)". The shrinker took 81 runs to
`disk=ioerr:2 dseed=aeaf52febe706064 | run I=7e77dad23c62fc84,x1.0-11,x2.0,x3.0,x4.0-5,x5.3-11`
(`65f8e47405e63d25` twice), without the kill. Its step 3 opens the settings, then the quick
action bar, "New task", and types `it's` with Enter. The recording replay took other actions in
step 3 and failed on another page, `/tasks/1`, with the same error: the walker of these runs
chose its actions again under recording, which Milestone 270 changed (open item 131).

On the second image the minimal schedule passed (`5c203157a31ae2aa`). That image differs in the
rule and in `avoid` (the login page's "change" button), and a walk is the same walk only on the
same image. The walk parted at action 3.4, which clicked Inbox on the second image and Settings
on the first; why was not traced.

**Without hydo.** `walk-repro/quick-add-new-task.spec.ts` answers the create with a 500 through
`page.route`. On this Mac with Playwright 1.65.1 against `vikunja/vikunja:2.8.0` in Docker it
fails: the page threw `ApiError: unexpected error occurred`, and the bar with the typed title was
gone.

**Duplicate search** (the same places): `quick actions`, `quick action`, `quick add error`, `new
task command`, `unhandled rejection`, `unhandled promise`, `task title lost`, `quick add title`,
`QuickActions`, `Ctrl+K`. Issue 1048 ("Quick Action error when creating a new task") was a
`No_project` error of v0.24.6, closed in July 2025 when the unstable build worked.


## What the walk lacked on Vikunja

The first two are built in this milestone; the rest is open item 137 of `AGENTS.md`.

- The walk recorded no acknowledged write of Vikunja. It wrote words only for resource names of
  the shape `memos/<id>`, and Vikunja's answers name a new object by a number. Each write now
  also adds `<METHOD>:<path>=<id>` for the ids of its JSON answer.
- `watch.failures_of` matched the path only, and Vikunja reads and saves a task on one path. It
  now also matches `<METHOD> <path>`.
- No walk marked a task done. Vikunja's done check boxes are hidden inputs under a styled label,
  and none of the three on the home page took Playwright's trial click on this Mac, so the walk
  leaves them out. The walks opened a Kanban view and a task's bucket button, but drag and drop
  is no action of the walk, and no walk moved a task to another bucket. So the Kanban rule judged
  only tasks that are not done, in the Kanban view's default bucket.
- When the session ended, the walk did not sign in again: `setup` runs only in a step without a
  saved session. Under disk errors the walk of seed 9 was on the login page from step 4 on.
- The recording replay of finding 1 recorded nothing: the power cut ends the first boot, and the
  recording stays in its guest, as the page checks of that boot do (open item 124). Since
  Milestone 334 the run keeps its own frames across the cut (the section below).
- A first version of the saved rule named every lost object in its verdict, so a failure that
  lost four tasks and one that lost two had two signatures, and each was shrunk on its own. The
  third image puts the list on a line before the verdict.

## The pages read back after the power cut (Milestone 334)

Milestone 334 gave the walk two things that finding 1 lacked. Every run keeps a small screenshot
of every action, and a failing run the trace of every step, also across a power cut. And the
check `browser.acked_shown` reads the acknowledged tasks back from the pages instead of the
database. `hydo.yaml` names the tasks for it (`browser.read_back`):

- Each task that Vikunja answered as created, a `POST` or `PUT` under
  `/api/v[12]/projects/<id>/tasks`, is a link `a[href$="/tasks/<id>"]` under "Current Tasks" on
  the home page.
- A later acknowledged delete of the task or of its project leaves it out.
- In the recovery boot the check has no session of the walk, so it signs in on `/login` with the
  account that the walk created.

The runs used hvx on the bare-metal machine, with this branch's `hydo` over the 0.4.0 Linux
release, at the fingerprint epoch 5. The image was built from `hydo/hydo.yaml`, with M334's
walker.

| schedule | verdict | the read-back check | fingerprint |
|---|---|---|---|
| `disk=none \| run I=1` | OK: 7 invariants held | "every one of the 5 objects ... is shown, on the page /" | `626ca6e005521589` |
| `disk=none \| run C=5 I=1` | VIOLATION `vikunja.saved_work_survives`: 3 of 5 saved objects missing | failed: "The page / does not show the task 3 that the server acknowledged ...; 3 of 5 acknowledged objects are not shown" | `a4348eacc7ad3268`, twice |
| `disk=cut \| run C=6 I=1` | OK | every one of the 5 objects shown | `e6ee25b034d97130` |

- In the run with `C=5`, both checks named the same three tasks, 3, 4, and 5. The database rule
  found the rows missing. The read-back check, from the home page alone, found that no link
  led to them. Its screenshot `screens/read-back-1.png` shows "Current Tasks" with tasks 1 and 2.
  The verdict names the database rule, because a project's own invariants come before the page
  checks. The read-back check's sentence is in the run's `result.json` and in `hydo report
  --details`.
- The run directory kept the first boot's walk: 66 frames of 1.5 to 10 KB (445 KB in all), with
  one line per action in `screens/film.jsonl`. It also kept the trace of step 4, the last step
  before the cut (482 KB). The controller copied them before it cut the power, in 0.8 s of
  guest time. `hydo report --html` of the run shows the filmstrip by step, and the page was
  801 KiB.
- M334's walker spends more guest time per step, because it takes the frames and records a
  trace. So the walk of `I=1` meets the cuts at other moments than M273's. With `C=5` it lost
  task 3 too (two tasks before), and with `disk=cut` and `C=6` the WAL had reached the disk
  before the cut, and nothing was lost (two tasks before).

## Milestone 390: a sign-in again, done checkboxes, drags, and faults inside a step

Milestone 390 built four of the gaps above (open item 137). The runs used hvx on the bare-metal
machine, with that branch's `hydo` over the 0.7.0 Linux release, at the fingerprint epoch 7. Two
images were built from `hydo/hydo.yaml`: one with the walker of `main` before the milestone
("before"), and one with the milestone's walker and the new keys of `hydo.yaml` ("after"). A
run's 31-day clock step (`T=2:2678400`) ends the walk's session: Vikunja's token is past its
time, and the page goes to `/login`. The new walker and keys give the image new bytes and its
walks other actions, so the fingerprints of the sections above hold only for the images built
before this milestone.

| schedule | image | the walk | verdict | fingerprint |
|---|---|---|---|---|
| `disk=none \| run T=2:2678400 I=1` | before | 33 of its frames on `/login`; 2 tasks acknowledged | VIOLATION `browser.acked_shown`: the check opened `/` with the ended session | `b259153002792f54`, twice |
| the same | after | signed in again at the start of step 2; 5 tasks acknowledged; 2 tasks marked done through their labels | OK, 7 invariants held | `35b61bf045be4d2c`, twice |
| `disk=none \| run I=1` | after | the same 2 labels: at 3.0 a task done, at 4.6 not done again | OK | `4899b87e1dab99d8` |
| `disk=none \| run I=3` | after | 1 label, at 3.4 | OK | `6875aa5efa0b5397` |
| `disk=none \| run A=walk.action#3@1:kill-vikunja I=1` | before | the walk of seed 0; no record of the walk reached the console, so the reaction never fired | OK | `9751f31f588dc3e0`, twice |
| the same | after | `vikunja` killed when the walk started action 1.2; the rest of step 1 met a server that refused every request | VIOLATION `browser.no_page_errors`: "The page /trash/projects left a promise rejection unhandled during the test "walk step 1": Error: Error while refreshing user info:" | `4e58b966ae56621e`, twice |
| `disk=none \| run I=1` of `hydo-kanban.yaml` | after | 5 drags of a card between the buckets; one timed out after 3 s | OK | `7c734d670e182574`, twice |

- The sign-in is `browser.explore.sign_in` of `hydo.yaml`: the login form of `read_back.setup`.
  The first step still creates the account with `setup`.
- Each label's click sent `PATCH /api/v2/tasks/<id>`, and the Kanban rule held: every task that
  the walk marked done was in the done bucket of its board. Before, the done checkboxes took no
  click, and the Kanban rule judged only tasks that were not done.
- The read-back looks for each task on its own page now (`/tasks/<id>`, `.task-view`), because
  the home page hides a done task. With the old entry the run with `T=2:2678400 I=1` failed it
  falsely twice: once by the ended session, and once by the done task.
- No walk of `hydo.yaml` dragged a card: the walk reaches the Kanban view in few of its
  actions. `hydo-kanban.yaml` is `hydo.yaml` whose setup adds three cards and opens the
  Inbox's Kanban view, which is also its start page. Its walk of `I=1` dragged cards to the
  Doing and the Done bucket, and the Kanban rule held: a card dragged to Done was done.
- The rejection after the kill is a candidate of Vikunja's frontend, not traced and not posted:
  when the server goes away, the page's refresh of the user's information rejects without a
  handler. Before the milestone, a schedule with any reaction walked seed 0, because the
  controller gave such a run's workload no walk; that is fixed too.

## Upgrades during a walk (Milestone 392)

The variant `upgrade/` runs Vikunja 2.7.1 and upgrades it to 2.8.0, the next release, on the same
SQLite database, while the walk runs. It was imported with `hydo compose docker-compose.yaml
--upgrade-to vikunja=vikunja/vikunja:2.8.0`, which pins both images. The rest of its manifest is
this example's walk and read-back. 2.8.0 migrates the database at its start: it adds `deleted_at`
to projects and tasks. So the variant keeps its own copy of the rules, which read either schema;
the first run with this example's rules failed on 2.7.1 with `no such column: p.deleted_at`. Its
data disk has 256 MiB, so that the root with both Vikunja images and the Playwright image fits the
4 GiB of hvx.

`U=vikunja@2~<ms>:2.8.0` sends SIGTERM to 2.7.1 `<ms>` of guest time into step 2, starts 2.8.0 when
2.7.1 has ended, and waits for its readiness check, while the walk goes on. The walk's browser opens
its first page about 5 s into the step. Every schedule ran twice on hvx, from fresh boots of the image
`822fe52d…`, with one fingerprint:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run I=1` | OK, 2.7.1 throughout | `4afa173eb515893a` |
| `disk=none \| run I=1 U=vikunja@2:2.8.0` | OK, the upgrade between steps 1 and 2 | `6cde8fa517f6ec8e` |
| `disk=none \| run I=1 U=vikunja@2~8000:2.8.0` | OK, the upgrade with the page of 2.7.1 open | `a42af1d1b15fa97f` |
| `disk=none \| run I=1 U=vikunja@2~12000:2.8.0` | OK, the same 4 s later | `ac528bd552f0da8d` |
| `disk=none \| run I=1 U=vikunja@2~1500:2.8.0` | VIOLATION `browser.no_page_errors`: the page `/` left a promise rejection unhandled, `Error: Error while refreshing user info:` | `5ee92a684aa53c60` |
| `disk=none \| run I=1 U=vikunja@2~1500+2:2.8.0` | the same violation twice, and the rollback to 2.7.1 before step 4 started | `9db42de9179a05e4` |
| `disk=none \| run I=1 X=vikunja@2~1500` | the same violation, with a kill and no upgrade | `5fc1602ff4019c5b` |
| `disk=cut \| run I=1 U=vikunja@2~8000:2.8.0 C=4` | VIOLATION `vikunja.saved_work_survives`: task 3 lost (finding 1); the recovery boot started 2.8.0 | `942b51d922e4a490` |

What the runs show:

- The upgrade kept its promises. With the pages of 2.7.1 open across the switch, the old frontend
  went on with the API of 2.8.0, and every page check held. Every task that either version
  acknowledged was in the database and on the home page of 2.8.0.
- 2.7.1 started on the database that 2.8.0 had migrated, and the rollback's runs held but for the
  page error below. 2.7.1 does not know the column `deleted_at`, so a task that 2.8.0 keeps as
  deleted would come back after a rollback. No walk deleted a task, so no run showed it.
- A small candidate, not posted, of a deploy's downtime and not of the versions. A page opened while
  the server is down shows "Failed to fetch". Then `GET /api/v2/user` fails, and the frontend
  rejects its promise `Error while refreshing user info` without a handler. At 1.5 s into the step the
  switch had stopped 2.7.1 before the walk opened its page. A kill of 2.7.1 at the same moment, with
  no upgrade, gives the same error (`X=vikunja@2~1500`), and so does the example's own image of
  2.8.0 (`3bb7a1adba79889a`).
- After the power cut the recovery boot started the version that ran at the cut, 2.8.0, from the
  report line `hydo-explore-versions: vikunja=2.8.0`. The task that it lost is finding 1: the
  commit was not on the disk yet.

## What held

- Apart from the power cuts of finding 1, every object that Vikunja answered as saved was in its
  database: under kills, pauses, network faults, one-way network faults and clock steps, in 60
  walks. A kill loses nothing, because the commit is in the kernel's page cache when Vikunja
  answers; only a power cut loses that cache.
- Every task of a Kanban view was in one of its buckets.
- No spinner stayed for 10 s, and no page printed `NaN`, `undefined`, `[object Object]` or
  `Invalid Date`. Every failed write of a task, project, label, team or saved filter showed a toast
  but the one of finding 2. The page checks of a walk with a power cut ran in the recovery boot
  and judged no page (open item 124): 19 of the 40 walks of seeds 21-60.
