# Web apps: run your Playwright tests under hydo

This page shows how to test a web app's frontend with hydo. Your Playwright tests run
unchanged inside the guest, with Playwright's own Chromium and no network. hydo watches every
page the tests open and turns what it sees into properties:

- an uncaught exception, an unhandled promise rejection or a `console.error` call;
- a spinner that stays on the screen;
- a failed request that the page never shows to the user;
- a text such as `NaN` or `undefined` printed on the page;
- a test that fails.

Because the whole machine is deterministic, a flaky test fails on every run of the schedule that
makes it fail. Faults between the page and its API (`H=` tokens) show how the frontend behaves
when the API is slow, fails, or loses an answer. The worked example is
`hydo/onboard/examples/web-shop` in hydo's repository: a React shop whose API is a fake. It runs
on every engine: hvx and KVM on Linux, and the Mac engine on an Apple Silicon Mac.

## The project: `browser:`

```yaml
name: my-shop
network: bridge                 # required: the browser runs in its own network namespace

root:
  files:                        # files the tests need, under /opt/hydo/web
    - {src: app/dist, dst: /opt/hydo/web/app/dist}
    - {src: app/serve.mjs, dst: /opt/hydo/web/app/serve.mjs}

browser:
  tests: e2e                    # the Playwright project: playwright.config.ts, tests, node_modules
  shards: 2                     # workload steps: step i runs shard i+1 of the tests (default 1)
  env:                          # variables for the tests and the web server their config starts
    APP_DIST: /opt/hydo/web/app/dist
    API_URL: http://10.89.0.1:9001
  watch:
    busy: [.spinner]            # spinners (default [aria-busy=true] and [role=progressbar])
    alerts: ["[role=alert]"]    # error toasts, chips, banners (default [role=alert], [role=alertdialog])
    requests: /api/             # judge only the pages' requests under this path

fakes:                          # the API, served in the guest (hydo guide outside-services)
  - name: api
    port: 9001
    addr: 10.89.0.1
    routes: [...]

faults: [protocol, clock, pause]
```

Before `hydo build`, run `npm ci` in the tests' folder: the guest has no network, so the
tests' `node_modules` go into the image. The tests' `@playwright/test` must be 1.65.1, the
version whose Playwright image this hydo pins. `hydo build` fetches that image
(`mcr.microsoft.com/playwright:v1.65.1-noble`, by its digest) into `browser/` next to the
manifest. Keep that folder out of git: the image is about 1 GB.

`browser:` writes the workload, as a test program (`workload.kind: test`, `hydo guide
flaky-tests`): do not write `workload:`. The explorer draws only the fault kinds that `faults:`
lists. Each step runs, inside the browser's service:

```sh
cd /opt/hydo/web/tests && playwright test --shard=<i+1>/<shards> --retries=0 --workers=1
```

With more than one shard, hydo adds `--fully-parallel`. Playwright then splits the tests one by
one instead of by file, and the one worker still runs them in the files' order. Step 0 of the
`web-shop` example runs its first two tests, and step 1 the third.

The tests' own `playwright.config` applies, including its `webServer`. The environment has
`CI=1`. Retries are off: under hydo a test that fails, fails on every run of its schedule, so a
retry would only hide it.

## Two ways to give the frontend an API

- **The real backend.** Add the backend's services, for example from `hydo compose`, and point
  the frontend at their addresses. Every fault of the services applies: kills, pauses, network
  faults, power cuts. The example `hydo/onboard/examples/compose-voting-browser` (Milestone
  166) checks that the screen agrees with the database.
- **The frontend alone.** Serve the API with a fake (`fakes:`), an emulator (`emulators:`), or a
  fake made from an OpenAPI description. `H=` tokens then act between the page and the API: an
  error status, a slow answer, a lost answer after the effect. This is the setup of the
  `web-shop` example.

## Let hydo walk through the pages

Without Playwright tests, or to go where your tests do not, let hydo drive the pages itself.
`browser.explore` replaces `browser.tests`:

```yaml
browser:
  explore:
    start: http://10.89.0.11:5230/   # the URL the walk starts on, as the guest's browser reaches it
    steps: 6                   # workload steps: a fault can fall between two parts of the walk
    actions: 12                # actions in each step
    setup:                     # signs in once; the later steps keep the session
      - {fill: Username, with: demo}
      - {fill: Password, with: demo-pass-1}
      - {click: Sign in}
    avoid: ["log ?out", "^Delete account$"]   # control names the walk never uses
  watch:
    requests: /api/            # judge the API's requests only
```

Each step starts a fresh browser that keeps the cookies and the page of the step before. Each
action reads the page's accessibility tree and does one of these:

- click a link or a button, or click a button twice;
- fill a field with an edge value: empty, 300 characters, Unicode, a negative number;
- fill a field and click Save, or fill it and press Enter;
- choose an option, go back, or reload. The schedule token `I=<seed>` chooses every action, so a walk is the same walk on every
run of its schedule. `hydo explore` draws a new walk for every schedule and shrinks a failing one
to fewer actions. It leaves out whole steps, halves of a step, and single actions. It writes the
result as `I=<seed>,x<step>.<first>-<last>`. A time in a control's name (such as `1 minute ago`)
does not steer the walk, so a clock step that a shrink drops changes fewer of its actions. When
an action opens a menu or a dialog, the next action most often acts in it, so a walk reaches
controls that sit behind several clicks. The run prints each action:

```text
hydo-web: walk 0.7: click button "Place order"
```

The five properties below judge every page of the walk. For your app's own promises, write
invariants over the backend's state. The walk writes each write that the server answered 2xx
to `$HYDO_ACKED`: one word per resource name in the request or the answer, after the last part
of the request's path. It also writes one word with the method, the path and each `id` that
the JSON answer returns. A write is a request that `watch.failures_of` matches, or without it
any request that is not a GET. For example:

```text
CreateMemo:memos/T5k4x8o58vBLJfJSXPXNS4
POST:/api/v2/projects/1/tasks/bulk=6
DELETE:/api/v2/tasks/6
```

A rule such as "every memo the server answered as saved is in the database" reads these words
(the examples `hydo/onboard/examples/memos` and `vikunja`). The recovery boot after a power cut
starts with them too.

Every run leaves the walk as a Playwright test, `screens/repro.spec.ts` in the run directory.
Run it against your own instance without hydo: `BASE_URL=http://localhost:5230 npx playwright
test repro.spec.ts`. An `H=` fault on a fake's route is a `page.route` handler in it; a fault of
the backend, such as a kill, is a comment at the step where hydo made it.

Every run also keeps a filmstrip of the walk. After the page opens and after every action, the
walker saves a small screenshot of the page (480 by 270 pixels, `screens/film-<step>-<k>.jpg`)
and one line in `screens/film.jsonl`. The line has the action's sentence, the page's path, and what went wrong
since the action before: an uncaught error, a failed request, an error shown. `hydo report
--html <run dir>` shows the filmstrip as the run's timeline, step by step, and marks the step
that the finding names. These are frames of the run itself, so they show the failure even when
it depends on the walk's timing. When the run fails, `screens/` also has a Playwright trace of
every step, `walk-step-<step>.trace.zip`: `npx playwright show-trace
screens/walk-step-2.trace.zip` opens step 2 with the DOM and the requests at every action. A
power cut does not lose them: hydo copies the filmstrip and the trace of the last step out of
the guest before it cuts the power. The page of an exploration shows each failure's filmstrip in
the failure's details.

At its end `hydo explore` runs each minimal schedule with a walk once more with recording on,
into `replays/rec-<k>/`. That run records a video of the step in which the finding happened,
`I=<seed>,rec<step>`. A finding that names no step records the last step that takes an action
(`I=<seed>,rec`). `hydo report --html` of that folder shows the video and the test.

Recording slows the browser, so a walk with `rec` does not choose its actions again: it repeats
the actions of the same schedule without `rec`. `hydo explore` takes them from the failing run's
console. `hydo run` of a schedule with `rec` first runs it without `rec`, into `walked/` of the
run directory, and prints how many actions that run took. Each action finds its control by the
same role and name. When a control is not on the page, the walker leaves the action out and
says so in the run's console, `hydo-web: walk <step>.<k>: ... not on the page`, and `hydo
explore`'s line about the recording names the action.

The recorded run repeats the actions, but not their timing: the video costs the guest time. A
failure that depends on the walk's timing can pass in the recording. `hydo explore` then says that
the recording did not fail, and `hydo report` and `hydo report --html` of the exploration say it
under the problem. The page links each problem's recording, its video, its trace and its
`repro.spec.ts`, in `replays/rec-<k>/screens/`. The minimal schedule without `rec` still shows
the failure, and its run's filmstrip and traces show it action by action.

### Check that the pages show what the server stored

The five properties below judge what a page did. `browser.read_back` adds a sixth check,
`browser.acked_shown`: every object whose id the server returned in a write that it answered as
done is shown on its page. It runs at the end of the run and, after a power cut, in the recovery
boot. It opens the pages in a fresh browser, with the walk's session or after its own setup
steps, and looks for each object's id.

```yaml
browser:
  explore: ...
  read_back:
    setup:                     # sign in again when the check has no session of the walk
      - {open: /login}
      - {fill: Username Or Email Address, with: hydo}
      - {fill: Password, with: hydo-pass-1}
      - {click: Login}
      - {see: Current Tasks}
    objects:
      - writes: '^(POST|PUT) /api/v[12]/projects/(?<project>\d+)/tasks(/bulk)?$'
        name: task             # the word for the object in the finding
        page: /                # the page that lists such an object
        shows: 'a[href$="/tasks/{id}"]'   # a Playwright selector: how the page shows it
        unless: '^DELETE /api/v[12]/(tasks/{id}|projects/{project})$'
    wait_ms: 10000             # how long a page may take to show its objects
```

- `writes` is a regular expression over `<METHOD> <path>` of an acknowledged write whose JSON
  answer returned an id, the `<METHOD>:<path>=<id>` words of `$HYDO_ACKED`.
- `page`, `shows` and `unless` take `{id}` and the named groups of `writes`, such as
  `{project}`.
- `unless` names a later acknowledged write that removes the object on purpose.
- `read_back: true` takes the defaults: every object that a POST or a PUT returned an id of,
  on the walk's start page, shown by a link whose path ends with `/<kind>/<id>`. The kind is
  the last word of the write's path, such as `orders`.

A finding names the object, the write and the page, and the run directory keeps a screenshot of
that page:

```text
The page / does not show the task 4 that the server acknowledged (POST
/api/v2/projects/1/tasks/bulk answered with its id): no element matches a[href$="/tasks/4"] after
10.0 s; 2 of 5 acknowledged objects are not shown. Screenshot: screens/read-back-1.png in the
run directory.
```

What the check cannot judge:

- an object that the app shows only on a page it does not open, such as a second page of a long
  list, a closed folder, or a filter;
- an object that the app hides on purpose, such as a done or archived one, or one that another
  object took along when it was deleted, unless `unless` names that write;
- an object whose write returned no id, and a change to an object that keeps its id, such as a
  new title;
- that the page shows the object's content right: only its presence.

## The properties

Each one is an invariant named `<browser>.<property>`; an empty list in `watch:` turns it off.

| invariant | fails when |
|---|---|
| `browser.tests_pass` | a test fails (turn it off with `must_pass: false` when faults make the tests' own assertions fail by design) |
| `browser.no_page_errors` | a page throws an uncaught exception, leaves a promise rejection unhandled, or calls `console.error` (`watch.console: false` leaves `console.error` out) |
| `browser.busy_ends` | a busy element stays for `busy_max_ms` (default 10000), or is still on the screen when its test ends after a failed request that the page did not send again. A busy element ends with its document: a reload, a back to another page, or a link out ends it |
| `browser.failures_shown` | a request fails (an answer of 500 or more, or no answer) and within `failures_within_ms` (default 5000) the page shows no error element and does not send the request again. With `watch.failures_of`, a regular expression over the path or over `<METHOD> <path>`, only the requests it matches count: `^(POST|PATCH|DELETE) /api/` names the writes of an API whose reads use the same paths. When the pattern matches none of the requests the pages sent, the run warns: `hydo: warning: ... judged no failed request`. |
| `browser.no_bad_text` | a page prints a watched text (default `NaN`, `undefined`, `[object Object]`, `Invalid Date`) |
| `browser.acked_shown` | with `browser.read_back`: an object whose id the server returned in an acknowledged write is not on its page. The check runs at the end of the run and after a power cut ("Check that the pages show what the server stored") |

A finding reads like this:

```text
The page /orders/o5 showed the busy element .spinner ("Loading your order…") for 5.1 s, until the
test ended, after GET /api/orders/o5 answered 500 during the test "checkout.spec.ts › places an
order"; it showed no error. Console: HttpError: HTTP 500 for /api/orders/o5
```

The check that the API acted once per order is the fake's own record: `hydo-world check
at-most-once api.place_order` (`hydo guide checks`, "Side effects at fake outside services").

## Faults for a frontend

| fault | token | what the page meets |
|---|---|---|
| an API error | `H=api.get_order@0-1:status=500` | an error answer to every matching request in step 0 |
| a slow API | `H=api.add_item@1-2:delay=300` | the answers come 300 ms late |
| a lost answer | `H=api.place_order@0-1:drop` | the API acts, and the answer never comes |
| a clock step | `T=...` (`hydo guide faults`) | the page's `Date.now()` and its timers jump, for example past a token's expiry |
| a stalled browser | `P=workload@0~4:500` | every process of the tests, the browser too, stops for 500 ms |
| a deploy of the backend | `U=web@2~8000:2.8.0` | 8 s into step 2 the server stops and its next version starts on the same data, while the pages stay open |

`hydo explore --protocol-faults` draws `H=` tokens on every route of the fakes.

## Upgrade the backend while the pages are open

A deploy replaces the server while people use the app. Their open pages keep the frontend of
the old version, and their next requests reach the new one. During the restart they fail.
Afterwards the old frontend talks to the new API. To test it, give the backend's service both
versions' images and upgrade it in the middle of a step.

1. Put the next version's image in the manifest as a version of the service. With a Compose file,
   `hydo compose docker-compose.yaml --upgrade-to <service>=<image>:<tag>` writes it and pins the
   image, and `sh fetch-images.sh` fetches both images. By hand:

   ```yaml
   services:
     - name: web
       image: {x86_64: images/web-2.7.1-amd64.docker.tar, arm64: images/web-2.7.1-arm64-oci}
       image_ref: "example/web:2.7.1"
       version: "2.7.1"
       versions:
         - name: "2.8.0"
           image: {x86_64: images/web-2.8.0-amd64.docker.tar, arm64: images/web-2.8.0-arm64-oci}
           image_ref: "example/web:2.8.0"
   ```

   The new version runs with the service's data, mounts, `env:`, `setup:` and `ready:`. A version
   may set its own `cmd:`, `entrypoint:` and `env:`.

2. Run a walk with the upgrade inside a step: `hydo run "disk=none | run I=1 U=web@2~8000:2.8.0"`.
   The switch comes 8 s into step 2 while the walk's pages are open. A walk opens its first page
   some seconds into a step, after its browser starts, so an earlier switch meets no open page. hydo sends SIGTERM, starts
   2.8.0 when the old process has ended (SIGKILL after 10 s), and waits for its readiness check.
   The walk goes on meanwhile.
3. Read the verdict as for any fault. The page properties judge what the old pages did against
   the new server. A write may fail during the restart without a word on the page
   (`browser.failures_shown`). The old frontend's code may throw on a new answer
   (`browser.no_page_errors`), or a spinner may stay. `browser.read_back` then opens the pages of
   the new version and looks for every object that either version acknowledged.
4. Compare with `U=web@2:2.8.0`, the same upgrade between two steps, where no page is open, and
   with the run without `U=`. A finding of the first schedule only is a finding of the open pages.
5. `hydo explore --upgrades` draws such upgrades: for a manifest with `browser:`, half of them
   come 3 to 12 s into their step. `+<back>` rolls the service back to its own image, on the
   data that the new version migrated.

The example is `hydo/onboard/examples/vikunja/upgrade`, Vikunja 2.7.1 to 2.8.0, and its results
are in the section "Upgrades during a walk" of the example's RESULTS.md.

## How it works

- The browser's service runs Microsoft's Playwright image, idle. The workload runs the tests in
  it with `hydo-oci`.
- `NODE_OPTIONS=--require /opt/hydo/web/hydo/hook.cjs` loads hydo's watcher into the test
  runner. The watcher adds a script and a binding to every browser context the tests create. The
  tests stay unchanged.
- What the pages did goes to `/tmp/hydo/web/events.jsonl` in the guest. Each invariant runs
  `node /opt/hydo/web/hydo/check.mjs <property>` over it. The lines `hydo-web: test ...` on the
  console show each test's result.
- A passing test counts as work the workload did, so a clean run is not reported as a pass that
  checked nothing.

## Screenshots

When a page ends its test with a spinner or an error on the screen, the watcher takes a
screenshot. After the verdict hydo copies each screenshot into the run directory, under
`screens/`, and a finding names its file:

```text
... it showed no error. Console: HttpError: HTTP 500 for /api/orders/o3 Screenshot:
screens/checkout-spec-ts-places-an-order-1.png in the run directory.
```

`hydo report --html <run dir>` shows each screenshot under the finding that names it. The page
carries the images itself, so it still opens without the run directory.

`browser.screens` chooses which pages get a screenshot:

| value | screenshots |
|---|---|
| `failures` (default) | a page that ends its test with a busy or error element |
| `all` | every page at the end of its test |
| `compare` | as `all`, and each one is compared with its reference |

A file is named after the test and the page's number in the test, so the same page has the same
name in every run. A deterministic run draws the same pixels every time, so `compare` checks
for exact equality. To use it:

1. Build with `screens: all` and run a schedule that shows the pages as they should look, for
   example `disk=none | run`.
2. Copy the run directory's `screens/*.png` into `hydo-screens/` next to the manifest
   (`browser.screens_ref` names another folder).
3. Build with `screens: compare`. The references go into the image.

The invariant `browser.screens_match` then fails when one pixel of a page differs. Its finding
gives the number of changed pixels and the box around them, and `screens/<file>.diff.png` shows
the reference in grey with the changed pixels in red. In the `web-shop` example on KVM, the order
page under `H=api.get_order@0-1:status=500` gave:

```text
The screenshot screens/checkout-spec-ts-places-an-order-1.png of the page /orders/o3 at the
end of the test "checkout.spec.ts › places an order" differs from its reference in 5,505 of
921,600 pixels (0.60 %), inside the box from x=15, y=56 to x=311, y=138. The changed pixels are
red in screens/checkout-spec-ts-places-an-order-1.diff.png.
```

It also fails when a screenshot has no reference, and when a reference has no screenshot. In
the example, the screenshots had the same bytes on Intel and AMD hosts. Other guest CPUs or
kernels can draw differently, so record the references on the engine that runs the comparison.

## Limits

- On a Mac the image's root file system is on its disk (`guest.root: disk`, the default with
  `browser:`). The Playwright image is about 2.5 GB of files, more than guest RAM should hold.
  `hydo build` makes that file system in a short helper VM and needs about 10 GB of free disk
  while it runs; the image itself is about 3.4 GB.
- A Mac run and a Linux run of one schedule give different fingerprints, and their walks can take
  other actions. Record screenshot references on the engine that compares them.
- One pinned Playwright version (1.65.1) and its Chromium. Firefox and WebKit are in the image
  but were not tried.
- Tests that call outside sites (a CDN, a login provider) need a fake under that name.
- A screenshot is taken when a test's browser context closes. A page that a test leaves earlier
  has no screenshot.
