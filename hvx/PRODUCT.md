# hydo as a product: what it offers, what is missing, and what is decided

This page describes hydo from a user's point of view: what a team gets today, the gaps we know of, and what the user decided about each gap (2026-10-24). The engineering history is in `hvx/HISTORY.md`; the current state is in `CLAUDE.md`.

## What a user gets today

A team describes its system in one `hydo.yaml` (`hvx/onboard/MANIFEST.md`): the services, a workload split into steps, and the invariants to check. hydo runs the whole system inside a deterministic virtual machine, so every run can be replayed bit for bit, and injects faults from a seeded schedule:

- process kills, pauses and timed kills; a freeze at an exact packet of a service's traffic;
- network delay, loss, duplication and reordering, one-way faults, and partitions between services;
- power cuts (also at the k-th disk flush), lying disks, I/O errors;
- clock jumps; changes of thread timing (tick jitter, extra preemption points, several virtual CPUs);
- FoundationDB-style fault points inside the code (BUGGIFY).

The explorer (`hvx/explore/explore.py`) generates schedules with several strategies and sweeps, re-runs every failure to prove it reproduces, and shrinks it to a minimal schedule. Assertions are properties of five kinds (always, sometimes, reachable, unreachable, eventually; `hvx/ASSERTIONS.md`). It runs on x86_64 Linux with VT-x (`hvx/dist/`, the Linux release) and on Apple Silicon Macs (`hvx-mac/dist/`).

## Two kinds of projects

**Infrastructure** (message brokers, databases, consensus systems, caches). The properties are generic: acknowledged writes survive, replicas agree, histories are consistent. hydo finds this class well (durability windows, crash consistency, members diverging, as with etcd issue 14685).

**Business software** (orders, payments, workflows, notifications). The bugs are logic bugs under failure and concurrency: a charge taken twice, an order stuck forever, a notification sent after an acknowledgement, a retry that duplicates an effect, services that disagree about one entity. hydo can find these, but today someone has to write the workload and the checks by hand.

## Gaps and decisions

| # | Gap | Decision (2026-10-24) |
|---|---|---|
| 1 | **Test composer.** Users write small commands ("place order", "refund", "webhook arrives"); hydo composes them into random sequences and concurrent mixes, instead of a fixed list of steps. | Not decided yet. |
| 2 | **Interactive debugging of a failing run** (a shell inside the VM at any moment, a merged timeline of all services' logs). | Not needed now: an AI agent does the analysis of a failing run. |
| 3 | **Ready-made checkers:** a history recorder plus linearizability and isolation checkers (Jepsen Knossos/Porcupine/Elle style) for infrastructure; "exactly once / no side effect after X" checkers for business software. | **Approved; in progress** (branch `history-checkers`; the business checkers with item 4). |
| 4 | **Faults at the protocol level and fake outside services:** HTTP 500/429, slow answers, dropped answers, duplicated or reordered webhooks, gRPC deadlines; fakes that record every side effect. | **Approved; in progress** (branch `fakes-protocol-faults`). |
| 5 | **Fast-forwarding virtual time** (run 30 days of business time in minutes). | Not decided yet. |
| 6a | **Assertion SDKs in more languages.** | **Rust approved; in progress** (branch `rust-sdk`). Others later. |
| 6b | **Docker Compose import** (a starting `hydo.yaml` from `docker-compose.yaml`). | **Approved; in progress** (branch `compose-import`). |
| 6c | **CI mode** (below). | Document only; to be discussed later. |
| 7 | **Scale and where it runs** (below). | Document only; not now. |

### CI mode (to be discussed)

What a CI integration needs: run hydo on every merge request or nightly; compare against a recorded baseline; deduplicate findings across runs (one finding per root cause, not per schedule); report only new findings, with the minimal schedule and the command that reproduces it; and check that a fixed bug stays fixed by re-running its saved reproducer on every build. Open questions: how much exploration budget a CI run gets, where images are cached, and how findings are tracked over time.

### Where hydo runs, and scale (not now)

Users are expected to run hydo locally (a developer's Linux machine or Mac) and in CI such as GitHub Actions. That has consequences to keep in mind:

- The Linux backend needs bare metal or a VM with nested VT-x, a kernel module, and root (or the optional udev rule) for the device. Hosted CI runners are usually VMs: nested virtualisation and loading a kernel module may not be allowed, and without a PMU the exact branch clock is not available (the exit-count clock is used instead; fingerprints then differ from PMU-clock runs).
- The Mac backend needs Apple Silicon and no root.
- The search budget is limited by one machine: today tens to a few hundred runs per hour on a 4-core box. Spreading the explorer over several machines would multiply the budget, but is not planned now.

### Known blind spots (to be discussed later)

- **Performance problems:** time inside the VM is virtual, so slowness and throughput regressions are not measured.
- **Long-running leaks** (memory, file descriptors, disk growth): found only if the workload runs long enough.
- **Guests larger than 3 GiB of RAM:** not supported yet (the PCI-hole split is not implemented).
- **AMD processors:** not supported by the Linux backend (VT-x only).
- **AVX-512 and some CPU features:** not offered to the guest (x86-64-v3 is the highest level, opt-in).
- **On the Mac:** a pure memory-flag spin in user space with no VM exits cannot be interrupted deterministically, and windows of plain work with no exits are out of reach of timing changes.
- **Bugs that need several machines' kernels to differ** (different kernel versions, real hardware failures beyond disks and network) are out of scope; all services share one guest kernel.
- **Security problems and user interfaces** are not what hydo tests.
