# Results: Memos explored through its own pages

This page records how hydo explored Memos 26.11.0 with its own walk through the pages
(Milestone 258) and what it found. The design is `docs/design/ui-exploration.md`, and `hydo
guide web-apps`, section "Let hydo walk through the pages", is the how-to.

## The system

Memos (https://github.com/usememos/memos, about 64,000 stars on 2026-11-15) is a self-hosted
note-taking app. One Go server serves the React frontend and the API (Connect RPC over HTTP, at
`/memos.api.<Service>/<Method>`), and keeps its data in SQLite (`memos_prod.db`, WAL journal)
by default. The example runs the image `neosmemo/memos:26.11.0` as its documentation runs it
with Docker, one container with a data volume.

| file | what it is |
|---|---|
| `docker-compose.yaml` | Memos as its documentation runs it with Docker. |
| `hydo/hydo.yaml` | Started with `hydo compose ../docker-compose.yaml`; the workload is `browser.explore`. |
| `hydo/page-rules/saved_memos.mjs` | The project's own rule: every memo that Memos answered as saved is in its database. |
| `hydo/fetch-images.sh`, `hydo/hydo-compose.lock.json` | The Memos image by digest, written by `hydo compose`. |

What changed from the import:

- The workload and the placeholder invariant of `hydo compose` gave way to `browser.explore`. The
  walk starts on `http://10.89.0.11:5230/`, the server's address on the guest's bridge. It has 6
  workload steps of 12 actions.
- The first step signs up the administrator: a fresh instance opens its setup page. The later
  steps keep the session.
- The walk never uses Log out or Sign out, nor the map, whose tiles come from the internet, nor
  audio recording.
- `watch.alerts` adds `[role=status]`, the role of the toasts of `react-hot-toast`, which Memos
  shows for errors. `watch.console: false`: Memos logs every error it handles with
  `console.error`, next to its toast (`web/src/lib/error.ts`, `handleError`).
- `watch.requests: /memos.api.` judges the API's requests and not the static files.
- `guest.ram_mib: 4096` for the browser, and `guest.outcomes_after_cut` so that the walk's
  acknowledged saves reach a recovery boot.
- The invariant `memos.saved_memos_survive` copies the database and runs the rule in the
  browser's service, whose Node.js reads SQLite (`node:sqlite`).

## How the runs were made

- Machine: the bare-metal machine, hvx, the main module (srcversion `91D86B693F4D4035C368F9B`).
- hydo: this branch's `hydo` over the 0.3.3 Linux release, fingerprint epoch 3.
- Image: `hydo build hydo/hydo.yaml`. `hydo build` fetched playwright-core 1.65.1 into
  `browser/` by its pinned sha256.
- One VM at a time: the machine was shared with a release check.

## One walk

`disk=none | run I=1` signed up the administrator and walked 72 actions in 6 steps: it wrote and
saved memos, opened the attachments and the calendar, reacted to a memo, and created a saved
view. It took 72 s of wall time, and every invariant held: "all 2 memos that Memos answered as
saved are in the database".

## What the first explorations taught the example and hydo

The first explorations showed six things that hydo or the example did wrong, and each fix is in
this milestone:

- **A background read is not a lost write.** With every request judged, disk I/O errors
  (`disk=ioerr`) and kills made Memos's background reads fail, such as the polling of
  `ListUserNotifications`, and the page showed nothing. A user loses nothing then: the reads
  run again. `browser.watch.failures_of` (new) names the requests whose failure the page must
  show; here the writes.
- **A ResizeObserver error is no exception.** Under a network fault the archive page raised
  "ResizeObserver loop completed with undelivered notifications", an error event without an
  error object that Chromium sends when a layout changes again inside a ResizeObserver
  callback. `hook.cjs` ignores it now.
- **A recovery boot has no pages.** After a power cut (`C=`) the page checks ran in the
  recovery boot, found no record, and ended the run in ERROR. `check.mjs` now says that no page
  ran in that boot. The pages of the first boot before the cut are not judged (open item 124).
- **A copy of a live SQLite database can be torn.** The rule copied the database while Memos
  wrote it and once read "no such table: memo". The invariant now copies the database and its
  WAL, without the shared-memory file, and copies again when the copy cannot be read. A walk
  that saved no memo holds the rule.
- **The acknowledged writes are words.** The controller carries `$HYDO_ACKED` across a power
  cut as a list of words. The walk first wrote one line per write (`walk POST <path> <names>`),
  and after a cut the rule found no line it could read and judged nothing. The walk now writes
  one word per resource name, `CreateMemo:memos/<id>`.
- **A pattern that matches nothing judges nothing.** The first `failures_of` was
  `/[A-Za-z]+/(Create|...)`, and Memos's paths have dots before the method
  (`/memos.api.MemoService/CreateMemo`): no failed request was judged in two explorations of
  60 schedules (seeds 1-20, and seeds 21-60 before this list's last two fixes). Nothing in hydo
  warned (open item 124). The
  pattern is now `/(Create|Update|Delete|Upsert|Set)[A-Za-z]*$`, and the explorations below ran
  with it.

## The blind exploration of all of Memos's pages

`hydo explore --seeds 1-20` (one worker) drew kills, pauses, network faults, power cuts, clock
steps and disk faults of the Memos server, and a walk for every schedule. It ran 20 schedules in
13 minutes of wall time (a mean of 38.3 s per run), and none failed. It ran with the first,
non-matching `failures_of` and with lines in `$HYDO_ACKED` (above). So it judged uncaught
errors, spinners, broken texts, and the memos saved in a run without a power cut, but no failed
write.

The walks spent most of their actions where Memos has many controls: 216 of them filled the
editor without saving it, and others went to the calendar's 30 day buttons, the saved views'
presets, the filters and the spaces. Only 16 clicked Save. So the example's `avoid:` now keeps
the walk on the memos themselves (the names are in `hydo.yaml`), and the next exploration used
that.

## The exploration of the memos

`hydo explore --seeds 21-60 --new-faults --durable-only` (one worker) ran on the image with
the final `avoid:`, `failures_of` and rule. `--durable-only` leaves out the disk that drops
acknowledged writes, whose losses are the disk's (below). The 40 schedules drew 14 kills, 19
network faults, 2 one-way network faults, 16 pauses, 10 clock steps and 19 power cuts of the
server, each with a walk of 6 steps of 12 actions. They ran in 42 minutes of wall time (a mean
of 43.1 s per run).

- 34 of the 40 walks saved between 1 and 6 memos, 85 in all. Every one was in the database at
  the end, also after each of the 19 power cuts.
- One schedule failed, and it reproduced: finding 1, below. Its shrink ran 10 candidates and
  kept the schedule as it was found; both confirmation runs gave `0d4aeb5af21bf82a`.

An exploration of the same seeds without `--durable-only` (stopped after 4 schedules for the
time) showed the rule at work. Seed 24, `disk=lying:3 ... C=4`, lost all 3 saved memos: "Memos
answered that it saved the memo memos/J2gBW9VwmmuwFTuefuZpLK, and the database does not have
it (3 of 3 saved memos are missing)." The disk of that schedule reports writes as done that it
dropped, so the loss is the disk's, not Memos's.

## Finding 1 (new candidate): "Copy link" of a share link throws over plain HTTP

**What a user meets.** A self-hosted Memos opened over plain HTTP from another machine, for
example `http://192.0.2.10:5230` on a home network as the Docker guide runs it, does not copy a
memo's share link. The user opens a memo, Share, Manage share links, creates a link and clicks
its copy button: nothing is copied, the button shows no check mark, and no message says why. The
page leaves a promise rejection unhandled:

```text
VIOLATION [browser.no_page_errors] The page /memos/b4R4VE2u3vE8gqW7sZTJtL left a promise
rejection unhandled during the test "walk step 2": TypeError: Cannot read properties of
undefined (reading 'writeText')
```

**Why.** `ShareLinkRow.handleCopy` in `web/src/components/MemoDetailSidebar/MemoSharePanel.tsx`
calls `navigator.clipboard.writeText(url)` without a check and without a `catch`. A browser
gives `navigator.clipboard` only to a secure context: HTTPS, or `localhost`. Over plain HTTP from
another host it is `undefined`, so the call throws. The rest of Memos copies with the package
`copy-to-clipboard` (the memo menu's Copy link and Copy content), which works there, and
`MediaMetadataDetails.copyCoordinates` checks `navigator.clipboard` first. The handler is the
same on Memos's `main` branch on 2026-11-15. The cause is plain in the code, so no reproduction
without hydo was made.

**How hydo found it.** Blind, in seed 46 of the focused exploration: `disk=none
dseed=baee56f32e223007 | run T=1:-2592000 I=fc8214f2a80db679`. The walk's step 2 saved a memo,
opened it, and clicked Share, Manage share links, Create new link, Revoke, and the copy button
twice. On an image of this milestone's final code (built after the last change to the walker,
so with other bytes) the schedule failed the same way on two runs, `93e46fd99f8d9b62`. Its run
with `,rec` kept the verdict and copied out `repro.spec.ts` and the last step's trace; the video,
382,926 bytes, stayed in the guest. `walk-repro/copy-share-link.spec.ts` is the walk as a
Playwright test. The guest's browser reaches Memos at `http://10.89.0.11:5230`, which is not a secure
context. The clock step back by 30 days is not the cause: it changes the names on the page (a
memo's age), and so the walk; without it the shrinker's walk took other actions and passed.

**Duplicate search** (usememos/memos, issues and pull requests, open and closed, and
discussions, 2026-11-15): `clipboard`, `copy link`, `share link copy`, `writeText`, `copy share
link`, `copy not working`, `http clipboard`, and pull requests `clipboard`, `writeText`, `share
link`. None names it. Issue 4928 ("Support for an alternate host for the Copy Link button?") is
about the host in the copied link.

## Finding 2 (new candidate, small): a reaction the server refuses disappears without a message

**What a user meets.** When the server fails to save an emoji reaction (it is down, or its disk
fails), the reaction picker closes and the reaction does not appear; no message says it failed.
Nothing is lost: the page and the server agree that there is no reaction. The user only does not
learn why the click did nothing.

**Why.** `handleReactionClick` in `web/src/components/MemoReactionListView/hooks.ts` wraps the
upsert in `try { ... } catch { // skip error }`. The pin of a memo does the same (`catch { // do
nothing }` in `MemoActionMenu/hooks.ts`); every other write of the memo menu shows a toast.

**How hydo found it.** In the first exploration, seed 1, `disk=ioerr:1 dseed=910a2dec89025cc1 |
run K=memos@0+2 I=a0ac048e4471decd`: a disk I/O error made `UpsertMemoReaction` answer 500 in
step 4, and `browser.failures_shown` failed: "The page / showed no error when POST
/memos.api.MemoService/UpsertMemoReaction answered 500 during the test "walk step 4"; nothing
appeared in the next 3.7 s." That run used the first image of the example.

On the image of the focused exploration, seed 33's walk adds a reaction (😄) as its action 1.11.
A timed kill of the server just before it, `disk=none dseed=2c0e0fedbe2218a8 | run
X=memos@1~10500 I=beedbb3199dc2417`, gave (`2c94d41aa3e49398`):

```text
VIOLATION [browser.failures_shown] The page / showed no error when POST
/memos.api.MemoService/UpsertMemoReaction got no answer (net::ERR_CONNECTION_REFUSED) during the
test "walk step 1"; nothing appeared in the next 3.7 s.
```

The cause is plain in the code, so no reproduction without hydo was made.

**Duplicate search** (the same places): `reaction`, `reactions`, `reaction error`, `reaction
silently`, `reaction fails`, `reaction toast`, `reaction not saved`, `reaction failed`, `emoji
reaction`, `upsertMemoReaction`, `handleReactionClick`, `skip error`, `toast`, `no feedback`,
`nothing happens`, `fails silently`, `pin silently`, `pin error`. Issue 6525 ("Quick second
change to a memo can leave the page showing the older state") is about the order of two answers
that both succeed; it names `handleReactionClick` for that race, not for a failed save.

## What held

- No memo that Memos answered as saved was lost: 85 memos in 40 walks, through kills, pauses,
  network faults, clock steps and 19 power cuts. Memos opens SQLite with the WAL journal and the
  default `synchronous` (FULL), so a commit it answers is on the disk.
- No spinner stayed for 10 s, and no page printed `NaN`, `undefined`, `[object Object]` or
  `Invalid Date`. The editor keeps its text when a save fails, and every other write of the memo
  menu shows its error.

## What hydo lacked

The fixes are in this milestone; the rest is open item 124 of `AGENTS.md`.

- Background reads made `failures_shown` fail; `watch.failures_of` (new) narrows it to writes.
- Chromium's ResizeObserver error event counted as an uncaught exception; `hook.cjs` ignores it.
- The page checks in a recovery boot ended the run in ERROR; `check.mjs` now says that no page
  ran there.
- The walk wrote lines to `$HYDO_ACKED`, which the controller carries across a power cut as
  words; it writes words now.
- A `failures_of` that matched nothing made two explorations judge no write, without a warning.
- The finding's message holds the memo's id, a random 22-letter word, so a shrink candidate with
  another memo gives another signature and is not taken. The shrink of finding 1 kept the
  schedule as found.
- A shrink candidate without the clock step took other actions, because the page's names (a
  memo's age) changed: a fault can steer the walk.
- A recording of a Memos step is too large to leave the guest within the check phase (its video
  passed 384 KB). A video or trace over 200 KB stays in the guest now.
- The walk spent its actions on the calendar, the saved views and the filters until `avoid:`
  named them.
