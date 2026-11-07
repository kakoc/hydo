# compose-voting-browser: the voting app tested through a real browser (Milestone 166)

This is Docker's example voting app from `../compose-voting`, with two more services: headless
Chromium, which plays the voters, and a small web server, which stands in for the public CDNs the
vote page needs. A vote counts as acknowledged only when the vote page **shows** it as cast after
the click. The design, the costs and what did not work are in `docs/design/gui-testing.md`.

## What is in the example

| file | what it is |
|---|---|
| `gui.yaml` | The manifest for x86_64. It is not named `hydo*.yaml`, so the repository's identity corpora, which run every `examples/*/hydo*.yaml`, stay unchanged. It is `../compose-voting/hydo.yaml` with the two extra services, a browser workload and four invariants. Guest RAM is 3072 MiB. |
| `gui-mac.yaml` | The same manifest for the Mac (arm64). Its first comment line calls the x86_64 manifest `hydo.yaml`, which was its name when the Mac image was built. Guest RAM is 4096 MiB, and it uses the arm64 build of the driver. |
| `gui-driver/` | `hydo-gui`, the voter at the screen. It is a static Rust program with no dependencies outside the standard library. It drives Chromium over `--remote-debugging-pipe`. Build it with `gui-driver/build.sh`. |
| `fetch-inputs.sh` | Fetches the six images and the two CDN scripts. Each one is pinned by its sha256. |
| `screens/` | Three screenshots that Chromium took inside the guest (see "Screenshots" below). |
| `healthchecks/` | A copy of the voting app's healthcheck scripts. |

### The workload

At step i, voter i opens `http://vote/` with the cookie `voter_id=voter<i>`. The voter clicks
Cats on even steps and Dogs on odd steps. The driver sends real mouse events through
`Input.dispatchMouseEvent` at the center of the button.

The driver then waits for the answer page to load and reads the vote app's own confirmation from
the page. The confirmation is: the chosen button is disabled and carries the check icon, and the
other button is shown at opacity 0.5. The vote app's template draws this with jQuery.

- If the page shows the vote the voter clicked, the id `voter<i>-<option>` goes to `$HYDO_ACKED`.
- If the page shows anything else, the outcome is `unknown`. This includes an error page, no
  answer within 15 s, or a page without the confirmation.

Each voter's line on the console shows what was clicked and what the screen showed, plus the
first 16 hex digits of the screenshot's sha256. For example:
`hydo-gui: voter5 clicked b, screen shows none page=500_Internal_Server_Error png 534a4542a1660953`.

### The invariants

| id | kind | holds when |
|---|---|---|
| `screen.confirmed_counted_soon` | eventually, within 60 s after the heal | Every vote that the screen showed as confirmed is a row in PostgreSQL, with the option the screen showed. |
| `results.screen_agrees_soon` | eventually, within 60 s after the heal | The results page, as Chromium renders it, shows the total and both percentages that PostgreSQL holds (for example "20 votes", Cats 50.0%, Dogs 50.0%). |
| `screen.confirmed_counted` | final | The same check as the first row, at the end of the run. |
| `results.page_live` | `always_or_unreachable`, every 2 s of guest time while the workload runs | Whenever PostgreSQL answers, the results page loads and shows a vote total. |

## Inputs

- **The voting app's images:** the five digests of `../compose-voting` (amd64 and arm64).
- **Headless Chromium:** `chromedp/headless-shell:156.0.8078.48`.
  - index digest `sha256:3da74b0945e4f80f00f0310bcda9f76cee283539ebd200a053a7bebf3de89423`;
  - linux/amd64 manifest `sha256:21eb1d28a091ed52a382359a35a82c5233af7387d9e822206f6b1abef4bc5d3e`, config `sha256:6b9ec6bd80da…`;
  - linux/arm64 manifest `sha256:3aa177f0d221ae85bc483407d2f4f2644a148066029d314f4c5ca99db1f39d8b`, config `sha256:45e1945d18ef…`;
  - the docker-save archive that `hydo-fetch-image` writes for amd64 has sha256 `9dc037679d92ed3b…` (157,400,064 bytes).
  - The image is Debian 12 slim with Chromium's headless shell (a 242,863,328-byte binary) and the DejaVu fonts. Unpacked it is 363 MiB.
- **The CDN scripts:**
  - jQuery 1.11.1 (`http://code.jquery.com/jquery-latest.min.js`, frozen since 2014), sha256 `540bc6dec1dd4b92…`;
  - jquery-cookie 1.4.1 from cdnjs, sha256 `b84161c9fbf7520c…`.
- **The driver:**
  - `build/hydo-gui` built on this Mac with Rust 1.90.0: 567,824 bytes, sha256 `1309e324c3bc92b4…`.
  - The same source built on the bare-metal machine gives other bytes. Its first build there was 8 bytes shorter (`deef7a2c…` against the Mac's `eb28a723…`). Like `hydo-read`, the bytes depend on the build host; docs/BUILD-PINS.md has the details.
  - The image above contains the Mac-built binary.

## x86_64 (the bare-metal machine)

- **hydo:** the Linux release `hydo-linux-0.1.0.tar.gz` (sha256 `58aab3b1…`, commit `1a88c31`), installed with `--no-module`.
- **Module:** the loaded main `hvx.ko`, srcversion `14D39A96F319623EC2AD8D1`.
- **Build:** `hydo build gui.yaml` gave the image `explore-ob-compose-voting-browser-disk.img`.
  - sha256 `f84c341213c1e65f0b9c31a8c7a3bc503a3cd03dd55f599d39f304d575e0ee49`, 3,356,491,776 bytes.
  - data disk `caa0a03f…`; build record `7a9c6a96…`; build time 165 s.
  - It was built twice, and the two builds gave the same bytes. The runs below used those builds,
    made when the manifest was still called `hydo.yaml`.
  - A third build after the rename to `gui.yaml` gave the same image bytes. Only the build record
    changed, because it names the manifest's path: `781091d0…` instead of `7a9c6a96…`. The record
    is not part of any fingerprint.
- **Runs:** each schedule was run with `hydo run --image … "<schedule>"` from a fresh boot, under `~/bin/hvx-run`, with one VM at a time. Every schedule was run twice.

| schedule | verdict | fingerprint | twice | wall time |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | OK. The screen confirmed all 20 votes, and all 20 are counted. The results page shows "20 votes", Cats 50.0%, Dogs 50.0%, 1.26 s after the heal. | `f4c8cd57de61b337` | yes | 42 s, 46 s |
| `disk=none dseed=1 \| run K=worker@5+10 C=12` | VIOLATION `screen.confirmed_counted_soon`: "the screen confirmed 12 votes, PostgreSQL counts 5 of them; voter5 (its screen showed the vote for b as cast) is not counted". In the recovery boot, the results page agrees with PostgreSQL ("5 votes", Cats 60.0%, Dogs 40.0%). | `0d240a50bec1aa87` | yes | 50 s, 51 s |
| `disk=none dseed=1 \| run K=db@5+5` | VIOLATION `results.page_live`: "the results page did not load (net::ERR_CONNECTION_REFUSED) while PostgreSQL answers". All 20 votes that the screen confirmed are counted. | `b22f460ebda8308c` | yes | 41 s, 41 s |
| `disk=none dseed=1 \| run C=12` | OK. All 12 votes that the screen confirmed are counted (see below). | `e5a45f361ca3f0fa` | yes | 47 s, 47 s |
| `disk=none dseed=1 \| run K=worker@5+5` | OK. All 20 are counted. | `61e4268bee3514ec` | yes | 41 s, 41 s |
| `disk=none dseed=1 \| run N=vote-redis@5-12:loss,100%` | OK. The screen confirmed 13 votes and all 13 are counted. For voters 5 to 11 the screen showed `500 Internal Server Error` and no confirmation. | `d335d99370201477` | yes | 47 s, 48 s |

The engine's other values were the same in both runs of each pair. Only `secs`, the run's wall
time inside the engine, differed:

| schedule | wfp | stops | typed | inh | ctl |
|---|---|---|---|---|---|
| clean | `c9dbce7e272238b3` | 63,168 | 95 | `32f77ba095ceaf42` | `dbab63652d7ddf4f` |
| `K=worker@5+10 C=12` | `21c19a89c8368d2c` | 125,252 | 221 | `e4d005c6eebde894` | `e8615a176166fd2f` |
| `K=db@5+5` | `db4fef19d0462584` | 72,018 | 118 | `33edb6af95453d3e` | `8137b790db0d2e2f` |
| `C=12` | `45716e6b912c1c30` | 77,170 | 141 | `6af55f62ff5a1906` | `266b23ca316b16e3` |
| `K=worker@5+5` | `355063aae50ef637` | 63,459 | 100 | `5a1ca3c91e0a3e1a` | `65cc50e065069f23` |
| `N=vote-redis@5-12:loss,100%` | `a237d5bbd05c0b28` | 116,889 | 140 | `e86944a3be53ee5d` | `1de3b1504df6da79` |

The run directories, each with its `result.json`, are in `/var/tmp/gui/c1-*` on the bare-metal
machine.

**An earlier version of the manifest** had no `results.page_live` invariant and lacked a
`mkdir` in its check helpers. Its image was `9fdefaad…`, built twice with the same bytes. It gave:

- `disk=none dseed=1 | run` OK `e923c6a7e6709266`, twice;
- `K=db@5+5` OK `c3e107e35c19113f`, twice. The heal had restarted the result app before the
  after-the-run checks ran, which is why the live check was added.
- `C=12` VIOLATION `a931ce016ca59e61`, twice. The cause was the manifest's own bug: the
  recovery boot had no `/tmp/gui`, so the results check could not write its log. That bug is
  fixed.

### What each result means

- **The power cut with a stalled worker:** this is the bug told by the screen. The vote app
  shows a vote as cast as soon as the vote is in a Redis list, and this Redis has no
  persistence.
  - While the worker is down (steps 5 to 14), the votes of steps 5 to 11 wait in that list.
  - The voters' screens showed every one of them as cast.
  - The power cut at step 12 lost them.
  - After recovery, the results page shows 5 votes, while 12 voters were told that their vote
    was cast. The screens of the voters and the results page disagree.
- **`C=12` alone passes.** At a browser's pace, a vote takes about 0.3 s of guest time. The .NET
  worker moves each vote from Redis to PostgreSQL within its 100 ms poll, and PostgreSQL makes it
  durable before the next vote arrives. So the loss window has closed by the time of the cut.
  - The HTTP client of `../compose-voting` sends all 12 votes within about 0.15 s, so it hits
    the window at every cut.
  - With real users, the window is open only while the worker falls behind. This example makes
    that happen with a kill.
- **The database restart:** this is the second class. When PostgreSQL restarts, the result app
  (Node, one `pg` client that it never replaces) and the .NET worker both stop.
  - After the restart, the database answers, but the results page cannot be opened
    (`ERR_CONNECTION_REFUSED`).
  - The check made during the workload sees this.
  - At the heal, hydo restarts every stopped service ("result is down; restarting it"). So the
    checks made after the heal pass, and an after-the-run check alone would miss the outage.
- **The network fault:** this shows the screen being right. The vote app cannot reach Redis, so
  after its 5 s socket timeout it answers 500. The screen shows the error page, and the voter is
  not told that the vote was cast. Nothing that the screen confirmed is lost.
- **The second class from the design discussion, a double submit under delay:** it was not
  reproduced as a bug. The vote app keys votes by the voter's cookie, and the worker upserts by
  that key. So a second click of the same voter can never count twice.

### Screenshots

- Every Cats confirmation in every run is the same PNG, `d390623351ec3f40…` (13,557 bytes,
  640×400). Every Dogs confirmation is the same PNG, `b6f25aab779d577a…`, and every
  "500 Internal Server Error" page is the same PNG, `534a4542a1660953…`.
- So the screenshots are pixel-identical across runs, across the two images built from the two
  manifests of this milestone, and across voters.
- The screenshots are part of the guest's memory, so the fingerprint covers them. They cost
  nothing in image size.
- To see them, a copy of `gui.yaml` that prints three PNGs as base64 to the console (not
  committed; 33 KB of console) was built and run twice. It gave fingerprint `eb2d8d8d6510d4c7`
  twice. The decoded files are in `screens/`:

| file | sha256 | what it shows |
|---|---|---|
| `screens/voter0-cats.png` | `d390623351ec3f40…` (the same hash as on the console of every recorded run) | Voter 0 after clicking Cats: Cats at full colour, Dogs dimmed. |
| `screens/voter1-dogs.png` | `b6f25aab779d577a…` | Voter 1 after clicking Dogs. |
| `screens/results-20-votes.png` | `409d8d4451bdd619…` | The results page after the clean run: "20 votes", 50.0% each. It is pale because the page fades in over a CSS transition, and Chromium took the picture at the same point of that transition in both runs. |

## The Mac (arm64)

- **Setup:** the Mac release (`hydo-mac-0.1.0.tar.gz`, `5c02e7f3…`) installed into a scratch
  prefix. The inputs came from `sh fetch-inputs.sh arm64`, the driver from `gui-driver/build.sh
  aarch64` (`7a23deaf…`), and the manifest is `gui-mac.yaml`.
- **Kernel and RAM:** the build uses the second guest kernel (`Image-explore-netns`), because of
  `network: bridge`. Guest RAM is 4096 MiB, because the root is the initramfs in guest RAM.
- **Image:** the initramfs is `a3e7bb62…` (504,557,344 bytes), the data disk is `8670640…`, and
  the build took 11 minutes. It was built only once.
- **Runs:** each schedule was run twice with `hydo run --image … "<schedule>"` from a fresh boot,
  on an M2 Pro that other sessions were also using.

| schedule | verdict | fingerprint | twice | wall time |
|---|---|---|---|---|
| `disk=none \| run` | OK. The screen confirmed all 20 votes and all 20 are counted. The results page shows "20 votes", 50.0% each. | `0155008471df8611` (stops 18,349, ctl `1aadc4006d294a61`) | yes | 275 s, 206 s |
| `disk=cut \| run K=worker@5+10 C=12` | VIOLATION `screen.confirmed_counted_soon`: "the screen confirmed 12 votes, PostgreSQL counts 4 of them; voter4 (its screen showed the vote for a as cast) is not counted" | `91c1e47f1e3e27d8` (stops 27,700, ctl `cdd5892e269126e1`) | yes | 248 s, 205 s |

Every Cats confirmation on the Mac is the PNG `23efcf607a2fdc8e…` and every Dogs one is
`7294a14b4db85389…`. They are identical across the Mac's runs, but they are not the x86_64 bytes.
