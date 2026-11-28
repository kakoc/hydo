# Which web-app bugs real projects report (Milestone 391)

hydo runs a web app, its browser, its backend, and its database in one VM. Its web-app side today
has two parts. `browser:` runs a team's Playwright tests, and `browser.explore` walks through the
pages itself, with page invariants and `browser.read_back` (`hydo guide web-apps`,
`docs/design/ui-exploration.md`). The lead's rule is to build what answers real problems. This
page measures which classes of web-app bugs the bug trackers of 15 popular self-hosted web apps
hold, and which new feature of hydo would reach each class. Nothing was run in a VM for this
page, and nothing was built.

## The question

Six candidate classes were named before the count. Each one is a feature hydo could build:

| class | label | what the bug needs to show |
|---|---|---|
| 1 | two users at once | two sessions. 1a is a lost update or a race of two writers. 1b is one user who sees or changes another user's object. 1c is shared data that one user's action handles wrong |
| 2 | answers in another order | one user's requests whose answers come late, twice, or refused, or a page that keeps an old state until a reload |
| 3 | a deploy during use | 3a is an old frontend against a new backend, or cached assets after a deploy. 3b is an upgrade of the server that fails or damages the data |
| 4 | offline and sync | a client that loses its link to the server, a local-first store, or a sync engine |
| 5 | the user's clock and time zone | a browser or a server in another time zone, a daylight-saving change, or a clock ahead or behind |
| 6 | backend crashes | a kill, a restart, a power cut, or a request cut in the middle, which hydo tests today |

The data added two classes that none of these covers:

- Class 7, live updates: a change that one session makes does not reach another open session
  until a reload. It needs two sessions, but no conflict.
- Class 8, an outside service fails: a calendar, a mail server, an object store, an LDAP server,
  or a webhook receiver answers late, wrong, or not at all. hydo's fakes and emulators answer
  such faults today (`H=`, `emulators:`).

Everything else is in the bucket "other": a bug that one user meets with one request at a time,
without a fault. Some issues are not correctness bugs: feature requests, questions, a program that
does not start, a failed build or install, documentation, and performance. They are counted apart,
and no share counts them.

## How the issues were collected

The projects are 15 web apps with a real backend and a public GitHub tracker. AFFiNE, AppFlowy,
Excalidraw, and Penpot are real-time collaborative or local-first. Outline and Plane also edit
documents together.

| project | what it is | bug filter | bug issues created 2024-01-01 to 2026-11-28 |
|---|---|---|---|
| go-gitea/gitea | Git forge | label `type/bug` | 2,180 |
| makeplane/plane | project tracker | its bug label, an emoji and `bug` | 748 |
| outline/outline | team wiki, collaborative editor | label `bug` | 1,415 |
| twentyhq/twenty | CRM | issue type `Bug` | 1,864 |
| calcom/cal.diy | scheduling (the cal.com repository) | its bug label, an emoji and `bug` | 2,094 |
| immich-app/immich | photo library with mobile apps | none: every issue | 7,922 |
| usememos/memos | notes | label `bug` | 815 |
| go-vikunja/vikunja | task manager | issue type `Bug` | 571 |
| AppFlowy-IO/AppFlowy | local-first workspace (Flutter clients) | label `bug` | 527 |
| toeverything/AFFiNE | local-first workspace (CRDT) | none: every issue | 2,573 |
| excalidraw/excalidraw | whiteboard with live collaboration | label `bug` | 193 |
| penpot/penpot | design tool, multi-user | label `bug` | 593 |
| nextcloud/server | file sync and sharing | label `bug` | 3,268 |
| documenso/documenso | document signing | label `type: bug` | 67 |
| mattermost/mattermost | team chat | label `Bug Report/Open` | 397 |

Immich and AFFiNE label almost no issue, so their whole tracker is the population, and a reader
left out the issues that were not bugs. The lists were read with `gh issue list --search
created:A..B`, split into date windows wherever a window passed 1,000 issues.

There are two samples, and the tables keep them apart:

- **The random sample.** For each project, Python's `random.sample` with seed 391 drew 16 issues,
  and seed 3912 drew 12 more from the rest. That is 28 issues per project, 420 in all. Each
  project has the same weight.
- **The targeted searches.** 16 queries, two to three per class, each run over three groups of
  five projects with GitHub's issue search, `created:>=2024-01-01`, top 10 results by relevance.
  That gave 408 issues. The queries used words such as `overwritten`, `concurrent edit`,
  `another user`, `permission` with `bypass`, `double click`, `stale`, `until refresh`,
  `ChunkLoadError`, `migration`, `offline`, `sync`, `timezone`, `daylight saving`, `restart` with
  `lost`, `queue` with `stuck`, and `websocket`. The exact queries are in the CSV's column
  `search`.

Each issue was classified by its text, never by a keyword alone. The reader saw its title,
labels, and state, the first 200 to 450 characters of its body without the template's headings,
and its last comment by a maintainer. Five issues are in both samples.

Every row of `docs/design/web-bug-benchmark.csv` records the project, the number, the title, the
link, and the sample. It records the class, its subclass, and whether it is a correctness bug. It
records what the bug needs to show: another user, a deploy, the network, the clock, or a crash.
The titles are the issues' own, with two changes. Long dashes are plain dashes, and one version
number that reads as an IPv4 address is spelled out, because the repository's content test
refuses such addresses. Each row also has three marks that a script set from the issue's state
and comments:

- `fixed`: the issue is closed as completed, with a merged pull request or a maintainer's comment
  that says fixed, merged, resolved, or released;
- `confirmed`: fixed, or a label such as `1. to develop`, `prio: medium` or `Medium priority`, or
  a maintainer's comment that is not a request for more information and not "cannot reproduce";
- `hard_to_reproduce`: the text or a comment says intermittent, sometimes, randomly,
  occasionally, or flaky, or it says that a person could not reproduce the bug.

The three marks are heuristics. A reader checked them on the examples below, not on every row.

## The counts in the random sample

Of the 420 issues, 343 are correctness bugs. The interval is the 95 % Wilson interval of the share.

| class | bugs | share | 95 % interval | confirmed | fixed | hard to reproduce |
|---|---|---|---|---|---|---|
| 1a two users: lost update or race | 2 | 0.6 % | | 2 | 1 | 0 |
| 1b two users: authorization | 1 | 0.3 % | | 0 | 0 | 0 |
| 1c two users: shared data handled wrong | 3 | 0.9 % | | 2 | 2 | 0 |
| 2 answers in another order, stale page | 12 | 3.5 % | 2.0 to 6.0 % | 6 | 6 | 4 |
| 3a deploy: version skew | 0 | 0 % | | | | |
| 3b deploy: an upgrade fails or damages data | 8 | 2.3 % | 1.2 to 4.5 % | 6 | 2 | 0 |
| 4 offline and sync | 4 | 1.2 % | | 3 | 3 | 0 |
| 5 clock and time zone | 7 | 2.0 % | 1.0 to 4.2 % | 3 | 1 | 0 |
| 6 backend crashes | 0 | 0 % | | | | |
| 7 live updates | 1 | 0.3 % | | 0 | 0 | 0 |
| 8 an outside service fails | 9 | 2.6 % | | 4 | 4 | 0 |
| other | 296 | 86.3 % | 82.3 to 89.5 % | 194 | 132 | 22 |

Three readings follow from it:

- The eight classes together hold 47 of the 343 bugs, 13.7 % (10.5 % to 17.7 %). No single class
  passes 4 %, so two classes of 5 and 8 bugs do not differ by more than chance.
- 310 of the 343 bugs need nothing to show: no second user, no deploy, no network fault, no
  clock, no crash. 8 need a second user, 8 an upgrade, 7 a time zone, and 10 the network or an
  outside service. None needs a backend crash.
- The class-2 bugs are the ones reported as hard to reproduce: 4 of 12, against 22 of the 296
  bugs of "other" (7 %). Over both samples it is 9 of 43 against 25 of 409.

The bucket "other" splits by what the bug is. This split is a reader's label from each issue's
text, in the CSV's column `subclass`:

| kind | bugs of the random sample | examples |
|---|---|---|
| `ui`: layout, rendering, input, keyboard, a browser's own behaviour, translations | 136 | a toolbar cut off, Ctrl+K that also opens the browser's search |
| `logic`: a wrong result, a missing validation, an API that ignores a field | 91 | an "Expired" filter that lists rejected documents |
| `error`: an error page, a 500, an uncaught exception during use | 41 | `TypeError` when archiving a cycle without an end date |
| `format`: import, export, a file format, or a media format | 22 | column widths lost in an HTML export |
| `security`: a server-side flaw of one user | 6 | an SSRF, files readable by every local user |

hydo's walk with its page invariants reaches the `error` kind today when it opens the page. An
uncaught error and a failed request that the page never shows are two of the five invariants.

## The counts in the targeted searches

Of the 408 issues, 278 are correctness bugs. The searches looked for the classes, so their shares
say nothing about frequency. They show how many reports of each class exist and what they look
like. 317 of the 408 issues were opened in 2026, and 399 are open, because GitHub's relevance
order prefers recent issues. So their fixed counts are low and do not compare with the random
sample's.

| class | bugs | subclasses | confirmed | fixed | hard to reproduce |
|---|---|---|---|---|---|
| 1a | 13 | edit conflict 5, race of two writers 6, two tabs 2 | 5 | 0 | 1 |
| 1b | 16 | leak 10, rule too strict 4, a removal not applied 2 | 5 | 0 | 1 |
| 1c | 3 | shared data 3 | 0 | 0 | 0 |
| 2 | 31 | stale page 15, background job races a user 6, order 5, autosave 2, double 1, optimistic 1, parallel uploads 1 | 10 | 1 | 5 |
| 3a | 4 | version skew 4 | 2 | 1 | 0 |
| 3b | 17 | upgrade 17 | 8 | 1 | 0 |
| 4 | 23 | sync 16, offline 3, reconnect 2, browser storage 2 | 2 | 0 | 3 |
| 5 | 27 | time zone 23, daylight saving 4 | 8 | 0 | 2 |
| 6 | 11 | restart 6, a request cut 3, a failed write 1, a backend link 1 | 5 | 0 | 0 |
| 7 | 5 | live update 5 | 1 | 0 | 0 |
| 8 | 15 | outside service 15 | 7 | 0 | 3 |
| other | 113 | | 40 | 1 | 3 |

Two notes on the targeted rows:

- 17 of the 23 class-4 reports concern AppFlowy's Flutter desktop and mobile clients and Immich's
  mobile apps. A browser does not run those clients, so no web-app feature of hydo reaches them.
  The 6 others are Excalidraw and Penpot in the browser.
- Many 2026 reports of classes 1b, 2, and 5 name a file and a line and read like the output of a
  code-reading tool. Immich's maintainers ask such reporters to write in their own words. They
  name real code paths, but a reader of the code found them, not a user who met them. So the
  targeted rows overstate what users see.

## Five example issues per class

Each link is the GitHub issue. A mark after it says `fixed` or `confirmed` where the CSV says so.

**Class 1a: a lost update or a race of two users.**

- [plane#9995](https://github.com/makeplane/plane/issues/9995): the last of two people to save a
  work item's description overwrites the other's text and images without a warning.
- [memos#3475](https://github.com/usememos/memos/issues/3475) (fixed): the editor on a second PC
  starts from its own old copy, not the server's.
- [cal.diy#13397](https://github.com/calcom/cal.diy/issues/13397) (confirmed): two people book the
  same slot at once, and both bookings are accepted.
- [mattermost#31650](https://github.com/mattermost/mattermost/issues/31650) (confirmed): two replies
  to one post at the same moment race.
- [excalidraw#11738](https://github.com/excalidraw/excalidraw/issues/11738): a drawing returns to an
  old version after a switch between tabs.

**Class 1b: authorization between two users.**

- [twenty#31045](https://github.com/twentyhq/twenty/issues/31045) (confirmed): a group-by answers a
  relation that the role may not read.
- [immich#33680](https://github.com/immich-app/immich/issues/33680): a call that a user makes on
  their own assets clears another user's duplicate group.
- [plane#10401](https://github.com/makeplane/plane/issues/10401): a guest removed from every
  project still opens them from the "Recents" tab.
- [gitea#32370](https://github.com/go-gitea/gitea/issues/32370): the list of watchers shows users
  who are hidden.
- [documenso#3436](https://github.com/documenso/documenso/issues/3436): the document setting
  "Require 2FA to access" does not stop a recipient who views it.

**Class 1c: data shared by two users, handled wrong.**

- [cal.diy#21945](https://github.com/calcom/cal.diy/issues/21945) (fixed): a change to a managed
  event type resets the availability that its members set.
- [vikunja#1170](https://github.com/go-vikunja/vikunja/issues/1170) (fixed): the assignee gets no
  notification when another member is subscribed.
- [immich#32292](https://github.com/immich-app/immich/issues/32292): a merge of people across
  accounts drops the other user's photos.
- [nextcloud#65935](https://github.com/nextcloud/server/issues/65935): recipients can no longer copy
  a folder's public link.
- [nextcloud#47694](https://github.com/nextcloud/server/issues/47694): a user who leaves a calendar
  shared with a group cannot undo it.

**Class 2: answers in another order or a stale page.**

- [documenso#3164](https://github.com/documenso/documenso/issues/3164) (confirmed): an older search
  answer overwrites a newer one.
- [immich#32243](https://github.com/immich-app/immich/issues/32243): results of the previous search
  mix into the new one while it loads.
- [twenty#31562](https://github.com/twentyhq/twenty/issues/31562) (confirmed): a card stays in the
  lane where it was dropped after the server refused the move.
- [outline#10328](https://github.com/outline/outline/issues/10328) (fixed, hard to reproduce): one
  new document is sometimes created two to six times.
- [immich#32208](https://github.com/immich-app/immich/issues/32208) (fixed): an asset removed from
  an album stays on the page until a reload.

**Class 3a: version skew after a deploy.** Only four were found.

- [outline#14630](https://github.com/outline/outline/issues/14630) (fixed): after an upgrade the
  browser keeps the old translation file until a hard refresh.
- [excalidraw#8142](https://github.com/excalidraw/excalidraw/issues/8142) (confirmed): an open app
  fails to load a module that the new deploy deleted.
- [excalidraw#9151](https://github.com/excalidraw/excalidraw/issues/9151): the same error stops a
  live-collaboration session from saving.
- [penpot#12729](https://github.com/penpot/penpot/issues/12729): a lazy module that fails to load
  shows the internal error page.

**Class 3b: an upgrade that fails or damages data.**

- [twenty#31373](https://github.com/twentyhq/twenty/issues/31373) (fixed): an upgrade from 2.40.0
  to 2.54.0 stops at a workspace step. The upgrade works through 2.42.6.
- [plane#4988](https://github.com/makeplane/plane/issues/4988) (fixed): the database migration
  fails from v0.14.0 to v0.21.0.
- [gitea#35289](https://github.com/go-gitea/gitea/issues/35289) (confirmed): a migration fails when
  SQLite runs in WAL mode.
- [immich#29540](https://github.com/immich-app/immich/issues/29540): v3.0.1 calls a correct
  migration history corrupted and does not start.
- [penpot#4368](https://github.com/penpot/penpot/issues/4368) (confirmed): designs lose their
  labels after the update to Penpot 2.0.

**Class 4: offline and sync.**

- [excalidraw#9706](https://github.com/excalidraw/excalidraw/issues/9706) (fixed): joining a room
  moves the local drawing into it.
- [affine#14289](https://github.com/toeverything/AFFiNE/issues/14289) (fixed): an edit of a whole
  headline breaks the sync.
- [appflowy#8455](https://github.com/AppFlowy-IO/AppFlowy/issues/8455): checklist items added on a
  phone are lost between two devices.
- [excalidraw#12567](https://github.com/excalidraw/excalidraw/issues/12567): a share link that
  cannot load while offline replaces the drawing with an empty canvas.
- [penpot#12885](https://github.com/penpot/penpot/issues/12885): after a tab closes without its
  WebSocket's close, the server refuses every reconnect for about 108 s.

**Class 5: clock and time zone.**

- [cal.diy#30021](https://github.com/calcom/cal.diy/issues/30021): on a daylight-saving date the
  afternoon slots come one hour late.
- [plane#10292](https://github.com/makeplane/plane/issues/10292): a cycle that ends on the day of
  the change ends on the next day and blocks the next cycle.
- [documenso#3463](https://github.com/documenso/documenso/issues/3463) (confirmed): a date-only
  field shows the previous day to viewers west of the document's zone.
- [cal.diy#30266](https://github.com/calcom/cal.diy/issues/30266): duration limits are checked in
  the server's time zone, not the organizer's.
- [plane#3438](https://github.com/makeplane/plane/issues/3438) (fixed): the Gantt chart shows every
  date one day later than the work item.

**Class 6: backend crashes.**

- [immich#32726](https://github.com/immich-app/immich/issues/32726) (confirmed): queued jobs vanish
  when Redis restarts, and nothing queues them again.
- [twenty#27788](https://github.com/twentyhq/twenty/issues/27788) (confirmed): a server restart
  during a mail import drops the import's queue.
- [immich#27973](https://github.com/immich-app/immich/issues/27973) (confirmed): a transcode killed
  mid-write leaves a broken video file.
- [nextcloud#62321](https://github.com/nextcloud/server/issues/62321) (confirmed): an interrupted
  WebDAV PUT answers 400 but keeps the cut body as the file.
- [gitea#40279](https://github.com/go-gitea/gitea/issues/40279): after a PostgreSQL crash loop,
  dependent Actions jobs never start.

**Class 7: live updates.**

- [mattermost#38673](https://github.com/mattermost/mattermost/issues/38673): another user's message
  does not appear in the open channel.
- [penpot#12089](https://github.com/penpot/penpot/issues/12089): comments do not reach a second
  user's browser.
- [excalidraw#8661](https://github.com/excalidraw/excalidraw/issues/8661): pasted text does not
  reach the other client of a session.
- [penpot#13372](https://github.com/penpot/penpot/issues/13372): path editing stays open after
  another user deletes the path.
- [twenty#28759](https://github.com/twentyhq/twenty/issues/28759) (confirmed): records created
  elsewhere are missing from a table until a full reload.

**Class 8: an outside service fails.**

- [cal.diy#30592](https://github.com/calcom/cal.diy/issues/30592): a failed external calendar sync
  removes all of a member's availability.
- [twenty#30384](https://github.com/twentyhq/twenty/issues/30384) (confirmed): an IMAP connection
  dropped during folder discovery clears every folder's selection.
- [nextcloud#55814](https://github.com/nextcloud/server/issues/55814) (hard to reproduce): uploads
  fail with "Object not found after writing" on object storage.
- [vikunja#2569](https://github.com/go-vikunja/vikunja/issues/2569): when the first webhook fails,
  the others never fire.
- [gitea#40549](https://github.com/go-gitea/gitea/issues/40549): one slow pull mirror holds all 20
  mirror workers.

## What each feature would reach

Each feature below says what hydo would add and which sampled bugs it would reach. A bug is
reached when the feature can make it happen in a run and a check can tell it from a pass. The
counts are of correctness bugs: 343 in the random sample, 278 in the targeted searches. Each bug
is counted under one feature.

| feature | what hydo would add | classes | random | targeted |
|---|---|---|---|---|
| a reload check | after each action of a walk, reload the page and compare the objects it shows before and after; compare them with the server's answers | 2, stale page | 6 | 15 |
| two browsers | two Playwright contexts with two accounts, or two tabs of one account, whose walks run in one schedule. It adds three checks. One account's pages and replayed requests never show the other's private ids. A change of one reaches the other within a time. No saved edit vanishes without a conflict | 1a, 1b, 1c, 7, and two-client sync of class 4 | 8 | 37 |
| a browser time zone | the browser's time zone and locale per walk, the guest's own zone, a clock step to a daylight-saving change, and a walk run in two zones and compared | 5 | 7 | 27 |
| an upgrade under data | a walk on the old version, `U=` of the image services to the new one, and the walk's read-back after it. An open tab of the old frontend runs against the new backend | 3a, 3b | 8 | 21 |
| reordered answers | a proxy that holds one answer and releases it after a later one, a request sent twice, a refusal of one write, and walk actions that double-click and type fast | 2, order, double, optimistic, autosave, parallel uploads | 5 | 10 |
| a browser partition | the link between the browser and the server cut and restored, a tab closed with work queued, the browser's storage cleared | 4 in a browser, 6 request cut | 0 | 9 |
| nothing new | kills, restarts, power cuts, backend link faults, fakes and emulators of outside services, and the walk's page invariants | 6, 8, and one sync bug that one walk shows | 10 | 23 |
| nothing new in the browser | the backend's own timing with rules over the stored state | 2, a job races a user | 1 | 6 |
| no browser feature | AppFlowy's and Immich's desktop and mobile clients | 4 | 2 | 17 |

The bucket "other" holds the rest: 296 bugs of the random sample and 113 of the targeted
searches. Its 41 `error` bugs need no new feature. Its `ui`, `logic`, and `format` bugs need a
check that knows what the page or the answer should be, which a team's own tests or rules give.

## The recommended order

1. **A reload check.** It needs no fault and no engine work: the walker reloads and compares, as
   `browser.read_back` already reads the pages. It reaches 6 of the 343 random bugs and 15
   targeted ones. Stale pages are half of the random class-2 bugs (6 of 12). The comparison of a
   page with the server is also the check that the reordered answers and the two browsers need.
2. **Two browsers.** It reaches the most targeted bugs, 37, and 8 random ones. The leak check is
   generic: `browser.read_back` already knows every id that one account's acknowledged writes
   returned. The second account's pages and its replay of the first account's requests must
   never show them. It needs a second Playwright context with its own sign-in, and walks
   interleaved by the schedule.
3. **A browser time zone.** It reaches 7 random bugs and 27 targeted ones, and it costs the least
   after the reload check. Playwright sets `timezoneId` and `locale` per context. The guest's zone
   and the clock steps to a time (`T=<at>:@<point>`, Milestone 225) exist. Its check is a
   comparison of one walk in two zones, in the way `hydo compare-versions` compares two versions
   (Milestone 298): the stored instants must be the same.
4. **An upgrade under data.** It reaches 8 random bugs, as many as two browsers, and 21 targeted
   ones. `U=` exists (Milestone 238), but it refuses image services (open item 109), and most
   self-hosted web apps run as images. So it needs `U=` for image services first. The checks are
   the services' start after the upgrade and the walk's read-back.
5. **Reordered answers.** It reaches 5 random bugs and 10 targeted ones. Its class is the one
   reported as hard to reproduce, 9 of 43 against 25 of 409, so a replay that repeats the order
   is where determinism adds the most. hydo's `H=` faults already delay and refuse routes of a
   fake; this feature needs them between the browser and the app's own backend, per request.
6. **A browser partition.** It reaches no random bug and 9 targeted ones. Most offline and sync
   reports concern clients that do not run in a browser, so a browser feature reaches few of
   them.

Backend crashes need no new feature: hydo makes them today. Their share in the sample is the
lowest. They are 0 of 343 random bugs, and 11 targeted ones. That says how seldom users report
them, not how seldom they happen.

## What the sample cannot tell

- A tracker holds the bugs that users noticed and reported. A silent loss, such as a lost update
  or a job that never ran, is noticed late or never. So classes 1a, 4, and 6 are under-counted by
  an unknown amount. Vikunja's tracker does not hold the tasks that a power cut lost in hydo's
  onboarding (Milestone 273).
- The random sample weights every project the same, 28 issues each. A count by bug volume would
  give Immich and Nextcloud more weight.
- One reader classified every issue from its first paragraphs, with no second reader. The marks
  `confirmed`, `fixed`, and `hard_to_reproduce` come from a script over labels and comments, and a
  reader checked them only on the examples.
- Excalidraw's and Memos's labeled issues end in March 2026, so their populations end there.
  Mattermost keeps most of its bugs outside GitHub, and Penpot moves many to another tracker.
- The searches were in English, and their top 10 results prefer recent and open issues.
- The reach counts are this page's reading of what each feature would show. No feature exists,
  and no bug was reproduced for this page.
