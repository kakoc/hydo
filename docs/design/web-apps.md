# Web apps: hydo for frontend developers

This page explains how hydo tests the frontend of a web app, and why it is built this
way (Milestone 253). The reference for users is `hydo guide web-apps`. Milestone 166
(`docs/design/gui-testing.md`) showed that headless Chromium runs deterministically inside an hvx
guest. This milestone makes a browser a part of the project format and runs a team's existing
Playwright tests instead of a hand-written driver.

## How frontend teams test today

Most teams test a frontend at four levels:

- **Unit tests** of functions and hooks, with Jest or Vitest, in Node without a browser.
- **Component tests** that render one component in a simulated DOM (jsdom with Testing Library)
  or in a real browser (Storybook, Playwright's and Cypress's component modes).
- **End-to-end tests** that drive the whole app in a real browser with Playwright,
  Cypress or Puppeteer. In the State of JavaScript 2024 survey, 31.5% of 11,667 respondents used
  Playwright at work, 30.9% Cypress and 16.0% Puppeteer. Jest had 62.2%, Vitest 34.2%,
  Storybook 39.9% and Mock Service Worker 8.0% [1].
- **Visual regression tests** that compare screenshots with recorded references, for example
  Playwright's `toHaveScreenshot` [2].

The API under an end-to-end test is either the real backend, started for the test, or a mock.
Mocks are written per test with Playwright's `page.route` [3], Cypress's `cy.intercept` [4], or
Mock Service Worker (MSW) [5], which serves the same handlers to the browser, to Node, and to
Storybook. A console-error check is usually a few lines in a fixture: listen to `pageerror` and
`console`, and fail the test on an error.

## What hurts most

- **Flaky end-to-end tests.** Google reported in 2016 that 1.5% of all test runs were flaky and
  that almost 16% of its tests had some flakiness [6]. In a study of 235 flaky UI tests from 62
  open source projects, 45% were caused by an asynchronous wait: the test read the page before
  the page had finished [7]. The usual answer is a retry (`retries: 2` in CI) [8], which hides
  the failure instead of reproducing it.
- **Tests that never see a slow or failing API.** A mock answers at once and with success unless
  the test author wrote the failure by hand, one test at a time. Few teams write a test for every
  route returning 500, for a lost answer, or for an answer that comes after the client's timeout.
- **UI states under partial failure.** These bugs pass every happy-path test:
  - a spinner that stays forever after an error answer;
  - an unhandled promise rejection that leaves the page half updated;
  - a form submitted twice because the client retried a request whose effect had happened;
  - an optimistic update that is never rolled back;
  - a value printed as `NaN` or `undefined` after a partial answer.
- **Time.** Timers, token expiry, and dates depend on the clock. Playwright's clock API [9]
  controls the page's clock, but not the backend's, and not the browser's own timers in the same
  way.

## Where hydo is different

- **The browser and the backend are in one deterministic machine.** A run of a schedule is the
  same run every time: the same network timing, the same timer order, the same random numbers in
  the page. A flaky test fails on every run of the schedule that makes it fail, and passes on
  every run of the one that does not.
- **Faults sit between the page and its API.** An `H=` token makes one route answer 500, answer
  late, or act and lose its answer, in a chosen workload step. `hydo explore` draws such tokens
  without being told which route matters.
- **The checks are about the user's screen, not one test's assertion.** hydo watches every page
  that the tests open: errors in the console, spinners that stay, failures the page never shows,
  and broken texts. A team's tests need no change to get these checks.
- **The clock is the guest's clock.** A clock step (`T=`) moves the time of the page, the test
  runner and the backend together.

## The design

### Run the team's own Playwright tests, unchanged

The first goal is that a team adds hydo without rewriting its tests. `browser:` names the folder
of the Playwright project, and hydo runs `playwright test` there inside the guest:

- The image is Microsoft's own Playwright image of the pinned version
  (`mcr.microsoft.com/playwright:v1.65.1-noble`), which teams already use in CI [10]. It has
  Node.js and the browsers of that Playwright version. `hydo build` fetches it by digest.
- The tests' `node_modules` go into the image, because the guest has no network. `hydo build`
  refuses a project whose `@playwright/test` is missing or of another version than the image.
- The workload runs one shard per step (`--shard=i+1/N`), so that a fault window can sit between
  test files. Retries are off and one worker runs, so that one schedule is one sequence of tests.
- The project's own `playwright.config` applies, with `CI=1`, including its `webServer`.

A hydo-native list of steps (open a page, click, read a value) was the fallback. It was not
needed for the first version, because the unchanged Playwright tests ran deterministically.

### Watch the pages without changing the tests

Playwright's test runner creates and closes browser contexts through instrumentation hooks that
it uses for its own traces (`runAfterCreateBrowserContext`, `runBeforeCloseBrowserContext`).
hydo's watcher, `hook.cjs`, is loaded into the test runner with `NODE_OPTIONS=--require` and
adds one listener to the same hooks. For each context it adds:

- an init script that runs in every page before the page's own scripts. It reports uncaught
  errors, unhandled rejections, `console.error` calls, the watched elements appearing, changing
  and going (a `MutationObserver`), and watched texts;
- a binding (`exposeBinding`) through which the script reports, so nothing is lost when the page
  navigates;
- listeners for the pages' fetch and XHR requests: sent, answered with a status, or failed;
- at the context's close, a screenshot of a page that still shows a busy or error element.

Everything goes to one file, `/tmp/hydo/web/events.jsonl`, with the guest's time. A small
reporter adds one line per test. `check.mjs` judges the file, once per invariant. The three files
are part of hydo (`hydo/crates/hydo-manifest/src/browser/`) and are written into the image by
the expansion of `browser:`. They are JavaScript because they run in the image's Node.js.

### The properties

The user asked for exceptions in the console, error chips and toasts, and specific printed
values. Each became an invariant with a default that works for an accessible page:

| property | default selectors or texts |
|---|---|
| no uncaught error, unhandled rejection or `console.error` | none needed |
| a busy element does not stay | `[aria-busy=true]`, `[role=progressbar]` |
| a failed request is shown | error elements `[role=alert]`, `[role=alertdialog]` |
| no broken text | `NaN`, `undefined`, `[object Object]`, `Invalid Date` |
| every test passes | the tests themselves |

The failure rule needed one refinement from the first runs. A client that retries a request after
a timeout is still handling the failure. So a failed request counts as handled when, within the
window, the page sends the same request again or shows an error element. A request cut off by
the end of its test, or by the page going to another document, is not judged.

"The screen agrees with the backend" stays a check over the system's data, as in Milestone 166,
or over a fake's record: `hydo-world check at-most-once` finds an order placed twice.

### Two ways to give the frontend an API

- **With its real backend**, from the services of the manifest (for example from `hydo compose`).
  Every fault of the services applies.
- **Alone**, with the API served by a fake, an emulator, or a fake generated from an OpenAPI
  description. `H=` faults then act on each route between the page and the API.

### Faults for a frontend

| what the user meets | how hydo makes it |
|---|---|
| an API error | `H=<api>.<route>@<steps>:status=500` |
| a slow answer | `H=...:delay=<ms>` |
| a lost answer after the effect | `H=...:drop` (the client gives up, the API acted) |
| an offline period | `H=<api>@<steps>:reset`, or `N=` on a service with the real backend |
| a clock step: timers, token expiry | `T=` |
| a starved browser | `P=workload@<step>~<offset>:<ms>`, which stops the test runner and the browser |

## What the prototype showed

The example is `hydo/onboard/examples/web-shop`: a React shop with three Playwright tests and a
fake API. Its `RESULTS.md` has every run. All ran on hvx on the bare-metal machine.

- **Determinism needed no change.** Node.js, Playwright's runner, the config's web server and
  Chromium's headless shell gave one fingerprint per schedule: every schedule that ran twice gave
  the same fingerprint both times, on three builds of the image. Fonts, the GPU, timers, random numbers and `Date.now()` needed nothing: hvx makes the
  guest's clock and its random seed the same on every run, and Chromium draws in software.
- **A flaky test, made exact.** "adds two products to the cart" waits 250 ms and reads the cart
  count once. On a Mac it failed 1 of 20 runs against an API with 0 to 300 ms of latency. Under
  hydo it passes on every run of `disk=none | run` and fails on every run of `disk=none | run
  H=api.add_item@1-2:delay=1000`; 300 ms and 500 ms fail too.
- **What only an API fault shows.** In the buggy build, `H=api.get_order@0-1:status=500` leaves
  the order page's spinner on the screen and a promise rejection unhandled. Four properties fail,
  each with a sentence that names the page, the request, the time and the console line.
  `H=api.place_order@0-1:drop` makes the client retry without a key, and the fake's record shows
  one cart ordered twice.

- **Found blind.** `hydo explore --seeds 1-20 --protocol-faults` on the buggy build found the
  stuck spinner at seed 7 and shrank it to `H=api.get_order@0-1:reset` (fingerprint
  `e405de9b8e7299de` on the explorer's two runs and on a `hydo run` by hand). The other three
  failures were the tests' own assertions under API faults. The fixed build passes every watched
  property under the same tokens. Seeds 21 to 40 gave 9 more failures of the tests' own
  assertions. The double order was not found blind: it needs a fault on the order route in step
  0, and the drawn ones fell into step 1.

What did not work at first, and what changed because of it:

- hydo-build refused the Playwright image: two systemd unit names in Ubuntu contain a
  backslash. hydo-build now leaves such entries out and lists them in the image record.
- Playwright shards whole files, each into the shard of its first test. With two files of one
  and two tests, both went into shard 1 and step 1 ran nothing. hydo adds `--fully-parallel` when
  there is more than one shard.
- The explorer needs 4 workload steps unless the workload is a test program, so `browser:` writes
  `workload.kind: test`.
- A retry after a timeout is the page handling the failure. The first rule judged the first
  failed request alone and accused the fixed build; the rule now accepts a repeat of the request.
- The verdict names the first invariant that fails. `tests_pass` comes last, so that the page's
  own failure (an unhandled rejection) names the finding, not the test that it made fail.
- The cart's id was a random UUID, which made each run's failure signature different; the
  example now uses digits, which the signature folds.

## Screenshots and acknowledged work (Milestone 254)

- **A passing test is acknowledged work.** The explorer warns of a pass whose workload
  acknowledged nothing, because a broken client passes the same way. A `browser:` workload writes
  nothing to `$HYDO_ACKED`, so every clean run got the warning. A console line `hydo-web: test
  "<title>" passed` now counts as acknowledged work. The check reads the console on the host, so
  the guest and the fingerprint do not change.
- **Screenshots leave the guest after the verdict.** The watcher saves each screenshot in the
  guest's `/tmp/hydo/web/shots/` with a line in `index.jsonl`. After the verdict the host
  controller reads the index and each file through the agent, in hex, and sends each file as one
  `HYDO-SCREEN` note. The host writes the files into the run directory's `screens/` and keeps a
  short line without the bytes in the console and the transcript. A note is covered by no
  fingerprint and no control-transcript hash, and the reads come after the verdict, so they
  decide nothing. A manifest without `browser:` sends the same commands as before. Since
  Milestone 334 an image whose walker keeps a filmstrip copies more per boot, in windows of
  reads, and also at a power cut (docs/design/ui-exploration.md, the last section).
- **The comparison is exact and runs in the guest.** `screens: compare` puts the reference folder
  into the image, and the invariant `<name>.screens_match` compares each screenshot with the file
  of the same name. A deterministic run draws the same pixels on every run, so the rule needs no
  tolerance: one changed pixel is a difference. The check decodes the PNG files with the PNG
  library that Playwright bundles, and writes `<file>.diff.png` with the changed pixels in red.
  As an invariant, the comparison takes part in the verdict, the exploration and the shrinking
  like every other property.
- **The file name is the test's title path and the page's number in the test.** A run and its
  reference then name the same page alike. Before, the files were numbered in the order they were
  taken, and each shard restarted the count.

## The Mac engine (Milestone 284)

Most web developers work on a Mac, so the Mac engine must run `browser:`. Until this milestone it
could not. The Mac engine's arm64 image keeps its whole root file system in the initramfs, which
the guest kernel unpacks into guest RAM. The Playwright image's files are 2.4 GB, so the root
would take most of a 4 GiB guest before Chromium started.

- **The root moves to the disk.** `guest.root: disk` makes `hydo build` put a read-only ext4
  file system of the root into the disk's first partition, as the x86_64 images always have it.
  The initramfs keeps only busybox and an `/init` that mounts `/dev/vda1` and hands PID 1 to the
  agent with `switch_root`. It is the default with `browser:`. Every other image keeps its root
  in memory, so no image built before changes its bytes, and nothing recorded moves.
- **The file system is made in a helper VM.** A Mac has no mke2fs that fills a file system from a
  folder, and the release has no debugfs. `hydo build` already formats the data disk in a short
  VM of the Mac engine with the release's static mke2fs. For the root it writes the tree as one
  cpio archive with every owner and mode in its headers. The helper VM gets a disk with two
  partitions: the new file system and the archive. The helper formats the first, mounts it, and
  unpacks the archive into it with busybox as root. The 3,200 MiB root of web-shop took about one
  minute.
- **The guest kernel did not change.** Chromium's headless shell, Node.js and Playwright's runner
  started under `Image-explore-netns-cg`, the kernel of every image with `network: bridge`.
  Playwright starts Chromium without its sandbox, so the guest needs no user namespaces and no
  seccomp.
- **No loop without VM exits stopped a run.** No run ended in `SPIN`. Chromium's and Node.js's
  waits make system calls, which the engine's clock counts.

The arm64 Playwright image is the one that the catalog pinned in Milestone 253 and never ran. Its
platform manifest is `sha256:56be08aca06b…`, with 7 layers and 35,741 entries; the archive is
988,750,336 bytes.

## Answers in another order, double submits, and offline pages (Milestone 393)

Milestone 391 counted the bug reports of 15 web apps (`docs/design/web-bug-benchmark.md`). Pages
that show an older answer than the newest one, or keep an old state, were 12 of 343 random bugs
and 31 found by targeted searches. Reporters call that class hard to reproduce more often than
any other: 9 of 43 such bugs against 25 of 409 others. A cut of the link between the browser and
the server reached 9 targeted bugs. These faults depend on timing that a developer's machine
gives by chance, and hydo gives it on every run of one schedule.

### The faults

- **Answers in another order.** A rule `order=<k>` of `H=` on a fake's or an HTTP proxy's route
  (hydo-world's `order.rs`) holds the answers of k matching requests in a pen. When k are in it,
  they go out 30 ms apart, in a permutation drawn from the token, never the order of arrival.
  For two the second request's answer goes first. An answer that waited `wait=` ms (default
  3000) goes with the ones that came. The order is a pure function of the token and the order in
  which the requests arrived, so a schedule gives one order on every run.
- **A double submit.** A rule `twice` hands the request to the fake or the service a second time
  after its answer, with no rule, recorded with `copy=<seq>`. The client gets the second answer.
  On an idempotent route with a key the copy is a replay; without a key the world acts twice.
- **An offline page.** `N=<svc>@<at>~<off>:<dur>` cuts one service's bridge port while a step
  runs, from a timer of the controller, as the strict power loss of Milestone 245 cuts a node's
  links. With `browser:` the service is the browser. Chromium's network goes away, so the page
  sees itself offline, and its requests to the server and to the fakes fail or wait.

The browser reaches a fake API through the fake, so the first two act on a fake at once. For a
real backend the manifest puts an HTTP proxy in front of the server, and the page opens the
proxy's address. Its routes name the API's reads and writes as `H=` targets
(`examples/vikunja/hydo/hydo-proxy.yaml`).

### The check

`browser.watch.answers` loads `answers.cjs` beside the watcher. It records the requests that the
setting names and the JSON body of each answer. When an answer comes after the answer of a newer
request of the same method and path, it reads the page's visible text nodes 300 ms later. Each
answer's values are the strings and numbers of its body. A value that only the older answer has,
shown on the page, with no value that only the newer answer has, is a stale page. The property
`<browser>.newest_answer_shown` fails on it. A comparison is left undecided, and named so, when a
newer request still has no answer, an answer is not JSON, the two answers hold the same values,
or the page shows neither. The check knows no app: it compares the page with the server's own
answers. An input's value is not a text node, so a search box that shows the newest query does
not hide a list of the older one.

The files and the property are staged only with the setting. An image without it keeps its bytes:
`hook.cjs` and `check.mjs` did not change.

### The explorer

`hydo explore --answer-faults` draws, from a stream of its own, `order=<k>` (k = 2 three times in
four, else 3) on a GET route (80 % of the schedules) and `twice` on one of the other routes (40
%). `--link-cuts` draws a cut of the browser's link in 60 % of the schedules, 0 to 4 s into a
step, for 0.5 to 15 s. Without the options every seed draws what it drew before. The shrinker
drops an `H=` token and narrows its window as for every rule, and leaves a link cut out, halves
it, and moves it to its step's start.

### What the proofs showed

On hvx, each schedule twice with one fingerprint (the web-shop and Vikunja RESULTS.md pages have
every run):

- web-shop's search box, `H=api.search@0-4:order=2 I=1`: the buggy build fails
  `browser.newest_answer_shown`, because the answer of the page's first search (`q=`) came after
  that of `q=%23tag`. The fixed build holds, and its check judged two reordered answers.
- `H=api.place_order@0-1:twice` with the Playwright tests: the buggy build, which sends no key,
  fails `api.order_placed_once`; the fixed build's copy is a replay.
- `N=browser@1~500:5000 I=1`: the page was offline for 5 s in step 1, and every page check held.
- The blind exploration of the buggy build (`--answer-faults --link-cuts`, seeds 1 to 20) found
  the search bug (seed 7), the order sent twice (seeds 14 and 16), and the unhandled rejection of
  a held order answer (seed 18).
- Vikunja through its proxy: 31 answers of the task and project lists held and reordered, 4 bulk
  creations sent twice, and a cut of 5 s. Every check held. No check of the example judges a
  duplicate task.

Images and schedules without the new tokens kept their bytes and fingerprints: web-shop's walk
image is one file from this branch and from `main`, with one fingerprint per schedule, and the
pinned reproducers gave their values.

## Sources

1. State of JavaScript 2024, testing libraries: https://2024.stateofjs.com/en-US/libraries/testing/
2. Playwright, visual comparisons: https://playwright.dev/docs/test-snapshots
3. Playwright, mock APIs: https://playwright.dev/docs/mock
4. Cypress, `cy.intercept`: https://docs.cypress.io/api/commands/intercept
5. Mock Service Worker: https://mswjs.io/
<!-- vale Google.We = NO -->
<!-- The title of a cited article. -->
6. Google Testing Blog: Flaky Tests at Google and How We Mitigate Them (2016),
   https://testing.googleblog.com/2016/05/flaky-tests-at-google-and-how-we.html
<!-- vale Google.We = YES -->
7. Romano, Song, Grandhi, Yang, and Wang: An Empirical Analysis of UI-based Flaky Tests, ICSE
   2021, https://arxiv.org/abs/2103.02669
8. Playwright, test retries: https://playwright.dev/docs/test-retries
9. Playwright, clock: https://playwright.dev/docs/clock
10. Playwright, Docker: https://playwright.dev/docs/docker
