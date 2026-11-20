# UI exploration: hydo walks through a web app's pages

This page explains how hydo explores a web frontend without tests that the project wrote, and
why it is built this way (Milestone 258). The reference for users is `hydo guide web-apps`, the
section "Let hydo walk through the pages". Milestones 253 and 254 (`docs/design/web-apps.md`)
run a team's own Playwright tests inside the guest and watch every page. This milestone adds a
walker: hydo itself drives the pages with a seeded walk, while it injects faults into the
backend, and the same watcher judges what the pages did.

## What the walk must do

A team without Playwright tests still has pages that fail under faults: a save that the server
lost, a spinner that stays, an error that the page never shows. A team with tests has the same
problem in every path that its tests do not take. The walk must:

- reach the app's pages and forms without a script per app;
- be one walk for one schedule, so that a failure reproduces from its schedule;
- shrink, so that a failing walk of 40 actions becomes the few actions that matter;
- combine with every fault hydo already has, on the backend and between the page and its API;
- leave evidence that a developer can use without hydo.

## Prior work

- Random input generation for user interfaces is old and strong. Android's Monkey sends random
  taps and key presses [1]. In a comparison of 7 input generators for Android apps, Monkey
  reached the highest code coverage on average [2].
- Model-based and search-based tools build a model of the app's states and steer towards new
  ones: Crawljax infers a state-flow graph of an Ajax application by firing events on its
  elements [3]; Stoat samples a stochastic model of the GUI [4]; Sapienz searches for short
  sequences that crash an app [5]. WebExplor rewards a web crawler for reaching pages it has not
  seen, and found 12 unknown failures in a commercial web application [6].
- Delta debugging reduces a failing input to a minimal one by removing parts and keeping the
  removals that still fail [7].
- Rendezvous hashing chooses, for a key, the member of a set with the highest hash of (key,
  member); removing another member does not change the choice [8].

## The design

### Where the walk runs

The walker, `walk.mjs`, runs in the pinned Playwright image of `browser:`, in the guest, with
playwright-core of the image's version. `hydo build` fetches playwright-core by a pinned sha256
from the npm registry and unpacks it into `browser/` beside the manifest, as it fetches the
Playwright image. The watcher of Milestone 253, `hook.cjs`, is loaded into the walker's process
with `NODE_OPTIONS=--require`, unchanged: a stand-in package `@playwright/test` in the walker's
folder gives it the walk's step as the test's title. So the five properties of the pages are
judged exactly as for tests, and a manifest without `explore:` builds an image with the same
bytes as before.

### One step, one part of the walk

`browser.explore.steps` (default 4) is the number of workload steps. Step `s` starts a fresh
browser, which keeps the cookies, the local storage and the page of step `s-1`
(`/tmp/hydo/web/walk/state.json`), and takes `explore.actions` actions (default 10). A fault
window of a schedule falls between two parts of the walk, as it falls between two operations of
any workload. The first step that has no saved session runs `explore.setup`, a short list of
hydo-native steps (`open`, `fill` with `with`, `click`, `press`, `see`, `wait`), for example the
sign-up page of a fresh instance. `explore.server` starts the app's web server for each step, as
Playwright's `webServer` does, for an app that a service does not serve.

### One action

Each action reads the page's accessibility tree with Playwright's `ariaSnapshot` [9]: the roles
and names of the elements a user can see. The controls are the links, buttons, menu items,
tabs, check boxes, text fields and selects. A link that leaves the app's origin, or the path
prefix `explore.within`, is not a control, nor is a control whose name matches `explore.avoid`
(default: log out and sign out). The action is one of:

| action | weight | what it does |
|---|---|---|
| click | 12 | a link or a button |
| fill | 6 | a text field, with a seeded value |
| commit | 4 | a text field, then the button that saves it (Save, Submit, Create, Send, ...) |
| submit | 2 | a text field, then Enter |
| click twice | 2 | a button, twice within 300 ms: a double submit |
| select | 2 | an option of a select |
| back, reload, open the start page, wait | 1 each | |

The values of a text field are edge values by the field's type: an empty text, a space, 300
characters, Unicode and an emoji, markup, a quote, numbers that are zero, negative or large,
dates at the ends of the calendar. `explore.values` adds the project's own. A value that contains
a watched text, such as `NaN`, is never typed, so that the page does not print it because the
walk typed it.

The walk then chooses the control in two levels. A family is the control's role and its name
with every number as `#`, so the 30 day buttons of a calendar are one family. Three times in
four the walk prefers the families it has used least in this step, so that it reaches new pages,
as a curiosity reward does [6]; the fourth time any family may come again. Within the family it
takes the control by rendezvous hashing of its role, name and place [8]. Before Milestone 267
the counts went on from step to step (below).

Before it chooses, the walk looks at the front of the page (Milestone 267): the controls of the
overlay that opened last, a `dialog`, `alertdialog`, `menu` or `listbox` of the accessibility
tree. Within it, the controls that the action before made appear come first, such as the row
that a dialog's Create button adds. By a draw from the action's own stream, the action stays in
the overlay seven times in eight, because an action elsewhere closes it. There it does not go
back, reload or open the start page, and three times in four it leaves out the controls that only
close the overlay (Close, Cancel, Skip, Not now). Model-based crawlers fire the events of the
state they have just reached [3]; the overlay is that state's part of the page.

The front is read from the page, so it keeps both properties of the shrink: an action's draw
does not depend on another action's draw, and an action left out changes the front only through
the page it leaves. Two parts of the first versions broke them, and both are left out. The
measure is how many of the 72 actions of Memos's seed 46 differ without its clock step
(`T=1:-2592000`): 24 for Milestone 258's walker and 9 for Milestone 264's, as Milestone 264
counted them.

- A front of the controls that were new on the whole page, without an overlay, depends on two
  pages in a row. With it, 38 actions differed.
- The counts of the least-used families depended on every choice before, in every step. Once
  two walks parted, their counts parted, and with the overlay front they did not meet again: 33
  actions differed. Without any counts 5 differed, but the walk went back to the sidebar's links
  again and again and reached a memo's Share in 2 of 26 walks. With counts that start at each
  step, 3 differ.

A text field is a control for a fill only when it takes text (Playwright's `isEditable`).
Memos shows each memo's text as a read-only textbox. Before Milestone 267, an eighth of the
actions of Memos's walks were fills of such a textbox, and each waited 3 s and did nothing.
A click goes to the first control, in the order of the rendezvous hashing, that takes Playwright's
trial click (visible, enabled, not covered), at most four of them. When none takes it, the
action presses Escape, as a user does in a dialog that does not answer. The unnamed buttons of
Memos's "Link memo" dialog never took a click, and with the front they held one walk for 8
actions in a row.

Milestone 258's walk of Memos reached a share link's copy button in 1 of 40 walks. The button
is behind four controls: Share, Manage share links, Create new link and the copy button. Each
of them opens a menu or a dialog of a few controls on a page of dozens. A choice among all the
page's families took one of Share's menu items in 3 of 24 actions right after Share. Milestone
264's walk took other actions at the same seeds and reached the button in no walk of the 40.

### The schedule names the walk

The token `I=<seed>[,n<actions>][,x<step>.<k>[-<last>]]...[,rec[<step>]]` names the walk. Action `k`
of step `s` draws from its own stream, made from `(seed, s, k)`. So the shrinker can cut the walk
in ways that leave every other action's draw as it was:

- `x<s>.<k>` leaves out one action, and `x<s>.<k>-<last>` the actions `k` to `last` of a step.
  A walk prints a run of four or more left-out actions of one step as a range (Milestone 264).
- `n<actions>` takes fewer actions in each step.

The shrinker's candidates come in this order (Milestone 264): all the actions of one step, the
last step first; the later and the earlier half of a step's actions; half and one less of the
actions in every step; the first and the last action of each step. A step after the one that
failed cannot change the failure, so the first rounds remove the steps after it at the cost of
one run each. Milestone 258 tried only fewer actions per step and the first action of each step,
which left a walk of 72 actions almost whole within the budget. A walk doubles the shrinker's
budget of runs, as a composed workload does.

This is delta debugging over actions [7], restricted to the cuts that keep the other draws. A cut
can still change a later action's choice, because the page is different without the action
that was cut, and the step's family counts change. Rendezvous hashing keeps that rare: the same
control wins whenever it is on the page. The shrinker runs every candidate, so a cut that
changes the failure is not taken.

Two things made the candidates of a real app fail although the failure stayed (Milestone 264):

- **Generated ids in the finding.** The shrinker takes a candidate when its verdict has the
  failure's signature: the verdict with every number as `#`. A page's finding names the page,
  and Memos's memo page has a random id, `/memos/b4R4VE2u3vE8gqW7sZTJtL`, which differs in every
  run that makes another memo. The signature of a page invariant's violation
  (`<browser>.no_page_errors` and the other five) now has `<id>` for every token that the error
  templates of Milestone 222 take for an id (hex of eight digits or more, sixteen characters or
  more with a digit, a shorter random mix), and for sixteen letters or more whose case changes
  often. Words such as `UpsertMemoReaction` keep their letters. Every other verdict keeps its
  signature, so no recorded exploration changes, and `hydo ci`'s finding keys are unchanged: a
  violation's key is its property id.
- **Times in the names of controls.** A clock step changes names such as a memo's age ("now",
  "1 minute ago", "in 30 days"), and the walk hashed the names, so a candidate without the
  clock step took other actions. The walk's choice now sees every relative time, date and clock
  time in a name as one word, `<time>`, and counts places among the controls with that name.
  The locator keeps the real name. A name without a time hashes as before.

`hydo explore` draws `I=` in every schedule of a manifest with `explore:`, from its own stream,
so the other fields of a seed's schedule are those of a seed without it. A schedule without `I=`
walks the seed 0. The token is not a fault: a schedule with only `I=` is a run without faults.

### Faults and rules

The explorer draws the faults that `faults:` lists for the backend's services: kills, pauses,
network faults between the browser and a service, power cuts, clock steps, disk faults. The
browser's own service is never a target. `H=` faults act on a fake's routes, as in Milestone
253. Nothing about the walk changes a fault: the faults and the walk meet at step boundaries.

The judges are:

- the five properties of the pages (`browser.<property>`, Milestone 253);
- the project's own invariants and checks over the backend's state. The walk records every
  write that the server answered 2xx. A write is a request whose path matches
  `watch.failures_of`, or without that key a request that is not a GET. `$HYDO_ACKED` is a list
  of words, so each resource name in the request or the answer becomes one word
  `<the path's last part>:<name>`, such as `CreateMemo:memos/T5k4x8o5`. The text of each write
  goes to `/tmp/hydo/web/walk/acks.jsonl`. A rule such as "every note the server answered as
  saved is in the database" reads the words. The recovery boot after a power cut starts with the
  first boot's `$HYDO_ACKED`.
- A REST API names a new object by a number in its answer, not by a resource name, and it keeps
  reads and writes on one path (`GET` and `PATCH /api/v2/tasks/4`). So since Milestone 273 each
  write also adds the word `<METHOD>:<path>=<id>` for every id of the objects that its JSON
  answer returns, such as `POST:/api/v2/projects/1/tasks/bulk=6`, or `<METHOD>:<path>` when it
  returns none, such as `DELETE:/api/v2/tasks/6`. `watch.failures_of` matches the path or
  `<METHOD> <path>`, so `^(POST|PATCH|DELETE) /api/` names the writes. Vikunja's rule reads
  these words (`hydo/onboard/examples/vikunja`).

`hydo rules scan` lists a project's promises from its schema, its API and its documentation, and
`hydo rules check` runs candidate rules without faults (Milestone 236). With a walk, the runs
without faults are walks without faults; the rules see the state that the walk left.

### The evidence of a failing walk

- **The walk as a Playwright test.** Every run of a walk writes `repro.spec.ts` into the run
  directory's `screens/`: each action as Playwright code, with the same locator the walker used
  (`getByRole(role, { name, exact: true }).nth(k)`). An `H=` fault on a fake's route becomes a
  `page.route` handler between the steps of its window: a status code, a delay, an aborted
  connection, or an answer that never comes after the server acted. A fault of the backend that
  a page cannot make (a kill, a clock step) is a comment at its step. A developer runs the file
  against their own instance with `npx playwright test`.
- **A video and a trace.** With `,rec<s>` the walker records a video of step `s`
  (`recordVideo`, 640 by 360 pixels) and a Playwright trace of it (`walk-step-<s>.trace.zip`,
  DOM snapshots without screenshots), which `npx playwright show-trace` opens with the DOM and
  the requests at every action [10]. With `,rec` alone it records the last step that takes an
  action. A shrunk walk often leaves its last steps without actions; Milestone 258 recorded the
  last workload step all the same. Only one step's video is recorded, because the controller
  copies the files through the agent after the verdict (below). The whole walk is in
  `repro.spec.ts`, and since Milestone 334 in the filmstrip of every run (the last section). Recording costs guest time, so a run with `rec` is a run of its own, with its
  own fingerprint. `hydo explore` does not record during the exploration. At its end it runs
  each minimal schedule with a walk once more with recording on, into `replays/rec-<k>/`, and
  says whether that run ended with the same verdict. A page's finding names the test of its
  step ("during the test \"walk step 2\""), and the replay records that step, `,rec2`
  (Milestone 267). Milestone 264's replay of Memos's share link recorded step 5, which had no
  action left, and the video showed nothing of the failure in step 2. A finding that names no
  step, such as a rule over a fake's records, records the last step that takes an action. An
  image built before Milestone 267 takes `rec<s>` for no recording. `hydo report --html` of
  such a run shows the videos, the trace commands and the test.
- **The recording repeats the failing run** (Milestone 270). Recording slows the browser, so
  the pages answer later. A walk that chose its actions again from what the pages showed took
  other actions than the run it replayed: two of Memos's share-link walks passed when recorded
  (Milestone 267). So a walk with `rec` does not choose. `I=<walk>,rec<s>` means the actions
  that `I=<walk>` takes, with the recording of step `s`:
  - Every run prints each action it took as one console line, `hydo-web: walk-act <step>.<k>
    <json>`: the kind of action, the control's role, accessible name and place among the
    controls of that name, the value typed, the option chosen, or the time waited.
  - Before a run with `rec`, the host takes those lines from the run of the same schedule
    without `rec`. `hydo explore` takes them from the console of the minimal schedule's
    confirmation run. `hydo run` and `hydo repro` run that schedule first, into the run
    directory's `walked/`. The lines go to `walk-script.txt` in the run directory, and the
    controller writes them into the guest before the walk's first step.
  - Each action finds its control by the same role, name and place, and waits up to 10 s for it
    to be visible, because a recorded page answers later. A name with a time in it ("1 minute
    ago") may read otherwise when the clock moved on; then the control with the same name
    without its times and the same place among them is the same control (Milestone 264's
    rule).
  - A control that is not on the page is left out. The walker's line says so, nothing takes its
    place, and `hydo explore`'s line about the recording names each action it left out.

  So a schedule with `rec` reproduces exactly: the actions are those of its schedule without
  `rec`, which hydo runs deterministically. An image built before Milestone 270 prints no
  actions, and its walker chooses again.

The files leave the guest as the screenshots of Milestone 254 do: the walker lists them in
`/tmp/hydo/web/shots/index.jsonl`, and the controller copies them after the verdict, in reads
of 1,500 bytes through the agent. In Milestone 258 the first recording replay of the web-shop
example recorded a trace with screenshots and a 1280 by 720 video of every step, about 600 KB,
and the run ended `TIMEOUT check` in place of its verdict, after about 386 reads. So did the
recording replay of the Memos finding, after a trace of 105,758 bytes and more than 384,000
bytes of the last step's video.

The cause was not time (Milestone 264). Each read's reply carries its bytes as hex, and on hvx
the agent's lines travel through the host-call port into a buffer of the engine that kept at
most 1 MiB per boot, the console's limit. The copy filled it: the engine dropped the later
replies, the controller waited for a reply that never came, and the check phase ran until the
engine's wall limit of 900 s. A read that arrives costs about a tenth of a second. Now:

- The hvx engine keeps up to 16 MiB of the agent's lines per boot (`PBUF_CAP`), and says once
  in `engine.log` when a boot fills them. A boot below 1 MiB keeps every byte, and so its
  fingerprint, as before.
- The controller copies at most 1 MiB of files per run (`screens::MAX_BYTES`, 16 MiB before).
  It is a number of bytes, not a time, so that a run copies the same files on every replay and
  keeps its fingerprint. It fits the KVM engine's 4 MiB console with the run's own lines, and
  takes about two minutes on hvx. A file that does not fit what is left costs one read and stays
  in the guest, and a `HYDO-SCREEN-LEFT` line in the console names it.
- The walker keeps a video or a trace of up to 640 KB (200 KB before); the last-step video of
  Memos's seed 46 had 524,990 bytes. Only one step is recorded still: the videos of every step
  of a Memos walk would pass 2 MB.

### The failing run's own frames, and the pages read back (Milestone 334)

A recording replay is a second run. It repeats the failing run's actions but not their timing,
so a failure that depends on the timing can pass in it, and then no video shows the failure
(Milestone 301). A walk that a power cut ends left nothing at all, because its files stay in the
first boot's `/tmp`, which the recovery boot does not have (Vikunja's finding 1, Milestone 273).
Since this milestone the failing run carries its own evidence:

- **A frame after every action.** After the page opens and after each action, the walker takes a
  JPEG of the viewport at three eighths of its size (480 by 270 pixels, quality 50) through the
  page's DevTools session, `Page.captureScreenshot` with a clip that scales. It is not
  Playwright's `page.screenshot`, which a trace records as an action of the walk. A frame of a
  Vikunja page has 3 to 8 KB. Each frame gets one line in `film.jsonl`: the step, the action's
  number, the file, its size, the action's sentence, the page's path, the guest's time, and the
  `notes` of what the watcher recorded since the frame before. The notes are the uncaught
  errors, the failed requests and the error elements that appeared, read from `events.jsonl`,
  which the watcher writes in the same process.
- **A trace of every step.** Every step records a Playwright trace with DOM snapshots and
  without screenshots. Its index line says `only: failed`, so the controller copies it only when
  the boot's verdict is not OK. A Vikunja step's trace had 400 to 600 KB.
- **The copy is pipelined and larger.** The controller reads `film.jsonl`, then every frame
  whose size its line gives in windows of 16 reads, before it waits for their replies; a read
  that waits alone costs about a tenth of a second on hvx. A boot of such an image copies at
  most 5 MiB (`screens::BOOT_BYTES`), and one file up to 3 MiB (`screens::FILE_MAX`). As hex
  with its notes, that is about 10.5 MiB of the agent's channel. The hvx engine keeps 16 MiB of
  agent lines per boot. The Mac and KVM engines keep 32 MiB of console since this milestone (4
  MiB before). A boot that wrote more than 4 MiB of console lost the agent's later lines before
  and never reached its verdict, so no recorded value moves.
- **A power cut takes the frames along.** At a host power cut (`C=`, a reaction's `:cut`, and the
  flush of `cf=`), after the ids are read and with every service frozen, the controller copies
  the filmstrip and the trace of the last step before the engine cuts the power. `/tmp` is a
  tmpfs, so the reads touch no disk, also when the disk has lost its power at a `cf=` flush.
  The recovery boot copies its own files after its verdict, and the run directory gets both:
  `film.jsonl` of the recovery boot follows the first boot's.
- **Only an image that has the walker copies.** The plan of a run comes from the image's
  manifest, read by the running hydo. So the controller cannot tell an image's walker from the
  manifest. The expansion of `browser.explore` writes `/etc/hydo/walk-film`, and `plan_of_image`
  reads it, in the ext4 root of an x86_64 image and in the small initramfs of a Mac image whose
  root is on its disk. An image without the file sends the commands that it sent before, with
  the copy of Milestone 264. The fingerprint epoch could not decide: an image built at epoch 5
  before this milestone has the older walker.

`hydo report --html` of a run shows the filmstrip as "The walk, action by action": one row of
frames per step, each with its action and its path. The step that a failing check names ("during
the test "walk step 3") is marked, and so is every frame with notes. The video of `,rec`
follows when there is one. The page of an exploration shows each failure's filmstrip from
`consoles/<tag>.screens/` in the failure's details, under its timeline.

The walk also checks the server against the pages. A rule over the database says whether the
server kept what it acknowledged; it does not say whether a user can still see it. `browser.read_back`
names the objects and how a page shows them:

- The walker already writes `<METHOD>:<path>=<id>` to `$HYDO_ACKED` for each id of a write's
  JSON answer (Milestone 273). An entry of `objects` takes the words whose `<METHOD> <path>` its
  regular expression `writes` matches. Its named groups, such as `(?<project>\d+)`, and the id
  fill `page`, the path of the page that lists the object, and `shows`, a Playwright selector.
  An object stays out when a later acknowledged write matches `unless`, by default a `DELETE`
  of `<kind>/<id>`.
- The invariant `<name>.acked_shown` runs `node walk.mjs --read-back "$HYDO_ACKED"` in the
  browser's service, after the other page checks: in a recovery boot it makes the record folder
  that they read as "no page ran in this boot". It runs at the end of a run and in the recovery
  boot, whose `$HYDO_ACKED` starts with the first boot's words.
- A fresh browser takes the walk's saved session. A recovery boot has none, so the check runs
  `read_back.setup`, or the walk's own setup. A walk whose setup creates an account (Vikunja's)
  needs a setup that signs in.
- Each page is opened once, and the check waits up to `wait_ms` (10 s) for all of its objects,
  so a page that loads its list late does not count as a loss. A page that lacks an object gets
  a screenshot, `read-back-<n>.png`, named in the finding, and a frame in the filmstrip.
- The default entry (`read_back: true`) guesses: every object a POST or a PUT returned, on the
  start page, as a link that ends with `/<kind>/<id>`. An app that shows its objects elsewhere
  needs its own entry, or the default gives a false finding. So the check is not on by default
  for every walk.

The check cannot see an object that the app shows only on a page it does not open, such as a
second page of a long list, or an object that the app hides on purpose, such as a done task. A
cascade of a delete, such as a project's tasks, needs `unless`. It judges that an object is
there, not that its content is right.

## Sources

1. Android Developers, UI/Application Exerciser Monkey:
   https://developer.android.com/studio/test/other-testing-tools/monkey
2. Choudhary, Gorla, and Orso: Automated Test Input Generation for Android: Are We There Yet?,
   ASE 2015, https://arxiv.org/abs/1503.07217
3. Mesbah, van Deursen, and Lenselink: Crawling Ajax-Based Web Applications through Dynamic
   Analysis of User Interface State Changes, ACM TWEB 6(1), 2012,
   https://doi.org/10.1145/2109205.2109208
4. Su, Meng, Chen, Wu, Yang, Yao, Pu, Liu, and Su: Guided, Stochastic Model-Based GUI Testing
   of Android Apps, ESEC/FSE 2017, https://doi.org/10.1145/3106237.3106298
5. Mao, Harman, and Jia: Sapienz: Multi-objective Automated Testing for Android Applications,
   ISSTA 2016, https://doi.org/10.1145/2931037.2931054
6. Zheng, Liu, Xie, Liu, Ma, Hao, and Liu: Automatic Web Testing Using Curiosity-Driven
   Reinforcement Learning, ICSE 2021, https://arxiv.org/abs/2103.06018
7. Zeller and Hildebrandt: Simplifying and Isolating Failure-Inducing Input, IEEE TSE 28(2),
   2002, https://doi.org/10.1109/32.988498
8. Thaler and Ravishankar: Using Name-Based Mappings to Increase Hit Rates, IEEE/ACM ToN 6(1),
   1998, https://doi.org/10.1109/90.663936
9. Playwright, aria snapshots: https://playwright.dev/docs/aria-snapshots
10. Playwright, videos and the trace viewer: https://playwright.dev/docs/videos,
    https://playwright.dev/docs/trace-viewer
