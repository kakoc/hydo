# Testing a GUI application through a real browser (Milestone 166, an experiment)

The question was whether hydo can test a web application through what its screen tells the user,
deterministically, with a real browser inside the guest, and whether it can catch a bug that way.

The answer is yes, on x86_64. Headless Chromium 156 runs inside an hvx guest next to Docker's
voting app:

- Two runs of a schedule from fresh boots give the same fingerprint, and the screenshots are
  pixel-identical.
- The test found a vote that the screen showed as cast and that the system then lost.
- The test also found a results page that stays down after a database restart, which a check
  after the run would have missed.

The example is `hydo/onboard/examples/compose-voting-browser/`, and its `RESULTS.md` has every
schedule with its fingerprint.

## The idea

- **The browser is one more client of the backend.** hydo's faults stay the same: kills, pauses,
  network faults, power cuts and disk faults act on the services as before.
- **The user's clicks are the workload.**
- **The strongest check is that the screen agrees with the backend.** A confirmation shown on
  the screen is the last hop of an acknowledgement. So "acknowledged" means "the page showed it
  as done", and an invariant compares that with what the system holds afterwards.
- **The results page is a screen too.** A second invariant compares what it renders with the
  database.

## How it is built

- **The browser:** `chromedp/headless-shell:156.0.8078.48`, an OCI image pinned by its
  per-platform manifest digest (docs/BUILD-PINS.md, "Headless Chromium").
  - It is an image service like any other: one root under `/oci/browser`, its own network
    namespace with `network: bridge`, and `/etc/hosts` bound in.
  - Its process is `sleep infinity`. The browser itself is started by the driver for each
    workload call and for each check.
- **Why an OCI image and not a Nix store path:** the OCI image fits the existing machinery with
  no new code: `hydo-fetch-image` by digest, then `image:` in the manifest. It is 157 MB, it is
  the same bytes on every fetch, and two image builds from it are byte-identical.
  - The pinned nixpkgs (NixOS 24.05) has Chromium 125 with a closure of several hundred store
    paths (GTK, Mesa, X libraries, systemd).
  - That closure would have had to be checked against the rule that keeps iproute2-6.8.0 and
    iptables-1.8.10 out of the bare-metal machine's `/nix/store`, and nothing would have been
    gained.
- **The driver:** `hydo-gui` (`gui-driver/`), about 870 lines of Rust with no dependencies
  outside the standard library, statically linked for musl (568 KB on x86_64, 500 KB on arm64).
  It speaks the Chrome DevTools Protocol over `--remote-debugging-pipe`.
  - The browser reads NUL-terminated JSON commands on file descriptor 3 and writes answers and
    events on descriptor 4. So there is no TCP port, no WebSocket, no HTTP handshake and no
    client library.
  - The driver carries a 200-line JSON reader.
  - It opens a page, waits for `Page.loadEventFired`, clicks with `Input.dispatchMouseEvent`
    (moved, pressed, released) at the button's center, reads the page with `Runtime.evaluate`,
    and takes `Page.captureScreenshot`.
  - Every wait has a deadline in guest time.
- **Why this driver:** it is the simplest choice that is also deterministic.
  - A test framework (Node with a browser-automation library, or Python with WebDriver) would add
    a language runtime and a dependency tree to the guest image, and a second process between the
    test and the browser.
  - The pipe needs nothing in the image except the browser, and the static driver runs in any
    root.
  - The headless shell's command-line modes (`--dump-dom`, `--screenshot`) cannot click.
- **The CDN:** the vote page loads jQuery from `code.jquery.com` and jquery-cookie from cdnjs, and
  it draws its confirmation with jQuery. The guest has no internet.
  - So a `cdn` service (busybox `httpd`, two files pinned by sha256) answers under those names,
    through `/etc/hosts`.
  - Without it, no voter would ever see a vote confirmed: the page loads, the inline script fails
    on `$`, and the buttons stay as they were.
  - Font Awesome comes from an `https` URL and is not served, because there is no TLS in the
    guest. The check icon's element is on the page, but its glyph is not drawn. The confirmation
    stays visible through the dimmed second button.

## Determinism: what it took

Nothing special. The first clean run on hvx and the second gave the same fingerprint, and so did
every pair since then: 6 schedules, each twice on the final image, and 3 on an earlier one.
Every screenshot of a confirmed vote is the same PNG in every run. Per pair, the engine's own
counters are identical too: stops, typed input, the inherited state hash and the control
transcript hash.

hvx makes the whole machine deterministic, and Chromium is a program on that machine like
PostgreSQL, the JVM or .NET:

- Its many threads and processes run on one vCPU, under timer interrupts that arrive at exact
  instruction counts.
- Its clocks are the virtual TSC.
- Its random numbers come from the guest kernel's generator, which is seeded deterministically.
- Its JIT (V8) compiles the same code at the same moments.
- Software rendering (Skia on the CPU) gives the same pixels for the same instructions.

The flags the driver passes were chosen for a quiet browser, not because a run without them was
seen to differ:

- `--headless --no-sandbox --disable-gpu --disable-dev-shm-usage`;
- no first run, background networking, component updates, extensions, sync or crash reporter;
- `--window-size=640,400 --force-device-scale-factor=1 --lang=en-US`, and the same metrics
  through `Emulation.setDeviceMetricsOverride`;
- `TZ=UTC` and a `HOME` under `/tmp/gui`.

The fonts are the image's own DejaVu fonts. `--single-process` was not needed. Which of these
flags determinism actually needs was not tested. The only expectation is that a flag that
starts work on a wall-clock timer would still be deterministic under hvx, because the clock is
virtual.

## What it costs

| | voting app (`../compose-voting`, M155) | with the browser |
|---|---|---|
| image (x86_64, root and data disk) | 2,819,620,864 bytes | 3,356,491,776 bytes (+537 MB: Chromium 363 MiB unpacked, the driver, the CDN files) |
| guest RAM | 2048 MiB | 3072 MiB (Chromium's browser, renderer and utility processes; actual use was not measured) |
| 20 votes, guest time | about 0.25 s (wget) | 5.9 s, or 6.9 s with the live check every 2 s. That is about 0.3 s per vote: navigation, click, answer, reading the page and a screenshot, plus one Chromium start per workload call. |
| one run from a fresh boot, wall time on the bare-metal machine | — | 41–51 s including the boot. A power cut with its recovery boot takes 47–51 s. |
| image build (`hydo build`, x86_64) | — | 165–167 s |

The screenshots live in the guest's `/tmp`, so they are inside the fingerprint and cost nothing in
the image. A PNG of 640×400 is about 13.5 KB. Printing three as base64 to the console (33 KB of
the 64 KiB console) was used once to take them out of the guest (see the example's RESULTS.md).

## What the screen found

- **A vote the screen showed as cast and the system lost:** `K=worker@5+10 C=12`.
  - While the .NET worker is down, the vote app keeps answering and the votes wait in a Redis
    list that has no persistence.
  - Twelve voters saw their vote as cast. The power cut lost seven of them.
  - After recovery, the results page shows 5 votes.
  - The verdict is `VIOLATION screen.confirmed_counted_soon`, twice with the same fingerprint.
- **A results page that stays down after the database restarts:** `K=db@5+5`.
  - The result app and the worker both stop when PostgreSQL restarts. The result app uses one
    `pg` client and never replaces it.
  - The results page is down while the database answers again.
  - The verdict is `VIOLATION results.page_live`, from the check that runs during the workload.
- **A screen that is right:** `N=vote-redis@5-12:loss,100%`. The vote app answers 500 after its
  Redis timeout, and the screen shows the error, not a confirmation. Nothing the screen
  confirmed was lost.

## What did not work, and what surprised

- **The HTTP workload's loss is not the browser's.** `C=12` alone loses every acknowledged vote
  with the wget workload of `../compose-voting`, and it loses nothing with the browser.
  - At a browser's pace (about 0.3 s per vote), the worker's 100 ms poll moves every vote into
    PostgreSQL before the next one is cast.
  - So the window exists only while the worker falls behind.
  - A test with realistic user pacing finds what users would hit. A test that hammers the
    endpoint finds a window that users rarely reach. The two complement each other.
- **hydo's heal hides a crash that a user would see.** With `restart: no`, a stopped service stays
  down only until the heal, which restarts it.
  - A check after the run therefore saw a working results page after `K=db@5+5`.
  - The outage is visible only to a check during the workload (`every:` with
    `always_or_unreachable`, which answers 2 while the database itself is down).
- **Chromium refuses every load in a network namespace that has only `lo`.** This was seen in a
  native test on the bare-metal machine. The error is `net::ERR_INTERNET_DISCONNECTED`, because
  Chromium treats a machine without a non-loopback interface as offline.
  - With `network: bridge` the browser has `eth0`, so it works.
  - With the default `network: loopback`, a browser service would need a dummy interface. This
    was not tried in a guest.
- **The CDN dependency:** see "The CDN" above. A sealed network is a fault in itself for a page
  that loads its scripts from the internet.
- **The driver's bytes depend on the build host,** like `hydo-read`'s (docs/BUILD-PINS.md). The
  image pins the Mac build.
- **The Mac (arm64)** works the same way, at about five times the wall time:
  - `gui-mac.yaml` needs 4096 MiB, because the root (about 1.5 GB with Chromium) is the initramfs in guest RAM. The initramfs is 504 MB compressed and took 11 minutes to build.
  - The clean run gave OK `0155008471df8611` twice, and the cut with a stalled worker gave VIOLATION `91c1e47f1e3e27d8` twice (12 votes shown as cast, 4 counted).
  - Each run took 205–275 s of wall time.
  - The screenshots differ from x86_64's: the same page renders with the same layout, but not with the same bytes.

## Where to take it next

These were not done:

- A results check that tolerates votes arriving while it reads, so that "the results page shows
  what the database holds" could also run during the workload. Today the live check asks only
  whether the page shows a total.
- Screenshots compared with a recorded reference. Today they are only hashed into the console.
- Keeping one browser for the whole run instead of starting one per workload call.
- An explorer campaign with `--strategy process` on this image.
- Text input (`Input.insertText`) and pages that need TLS. TLS would need a test CA in the guest,
  or `--ignore-certificate-errors`.
