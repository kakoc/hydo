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
