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
four the walk prefers the families it has used least in this walk, so that it reaches new pages,
as a curiosity reward does [6]; the fourth time any family may come again. Within the family it
takes the control by rendezvous hashing of its role, name and place [8].

### The schedule names the walk

The token `I=<seed>[,n<actions>][,x<step>.<k>]...[,rec]` names the walk. Action `k` of step `s`
draws from its own stream, made from `(seed, s, k)`. So the shrinker can cut the walk in two
ways that leave every other action's draw as it was:

- `n<actions>` takes fewer actions in each step; the shrinker tries half and one less.
- `x<s>.<k>` leaves out one action; the shrinker tries the first action of each step.

This is delta debugging over actions [7], restricted to the cuts that keep the other draws. A cut
can still change a later action's choice, because the page is different without the action
that was cut, and the family counts change. Rendezvous hashing keeps that rare: the same control
wins whenever it is on the page. The shrinker runs every candidate, so a cut that changes the
failure is not taken.

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
- **A video and a trace.** With `,rec` the walker records a video of the last step
  (`recordVideo`, 640 by 360 pixels) and a Playwright trace of it (`walk-step-<s>.trace.zip`,
  DOM snapshots without screenshots), which `npx playwright show-trace` opens with the DOM and
  the requests at every action [10]. Only the last step is recorded, because the controller
  copies the files through the agent within the check phase (below). The whole walk is in
  `repro.spec.ts`. Recording costs guest time, so a run with `rec` is a run of its own, with its
  own fingerprint. `hydo explore` does
  not record during the exploration. At its end it runs each minimal schedule with a walk once
  more with `,rec`, into `replays/rec-<k>/`, and says whether that run ended with the same
  verdict. `hydo report --html` of such a run shows the videos, the trace commands and the test.

The files leave the guest as the screenshots of Milestone 254 do: the walker lists them in
`/tmp/hydo/web/shots/index.jsonl`, and the controller copies them after the verdict, in reads
of 1,500 bytes through the agent. On hvx the check phase ended after about 386 reads, about
570 KB. The first recording replay of the web-shop example recorded a trace with screenshots
and a 1280 by 720 video of every step, about 600 KB, and the run ended `TIMEOUT check` in place
of its verdict. A later one with a 640 by 360 video of every step and one trace still made
about 340 KB of videos for a walk of 37 actions, and ended the same way. So only the last step
is recorded, and a video or a trace of more than 200 KB stays in the guest. The recording
replay of the Memos finding, before that bound, copied a trace of 105,758 bytes and then more
than 384,000 bytes of the last step's video, and ended `TIMEOUT check`. A copy that is too large
is open item 124.

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
