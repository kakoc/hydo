# miniflux end to end — results (2026-10-18/19, bare-metal test machine)

The first project onboarded that we did not write: miniflux v2.3.3 (a Go RSS reader) on
PostgreSQL 15.6. The manifest is the one the `hydo-onboard` skill produced in its dry run
(`.claude/skills/hydo-onboard/example-miniflux.yaml`); the only change is `src: miniflux`
(the binary is copied next to the manifest by `build.sh`). Results under
`/var/tmp/m48-miniflux` on the box.

**Build.** `git clone --depth 1 --branch v2.3.3 https://github.com/miniflux/v2.git`, then
`CGO_ENABLED=0 GOTOOLCHAIN=local GOFLAGS=-mod=readonly go build -trimpath -buildvcs=false
-ldflags "-s -w -buildid= -X miniflux.app/v2/internal/version.Version=2.3.3"` with go 1.26.5
(`/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5`): two builds byte-identical,
sha256 `dd8e2456…`. `HYDO_MINIFLUX_BIN=<binary> hydo/onboard/examples/miniflux/build.sh`.

| image | init | sha256 (disk) |
|---|---|---|
| `explore-ob-miniflux-disk.img` | before `8ef674e` (signals the first PID only) | `b3409055…` |
| `explore-ob-miniflux-pt-disk.img` | `8ef674e` (whole process tree) | `19518573…` |

The `-pt` image is byte-identical to the scratch image the init fix was tested on.

## What worked first time, and what did not (this measures the skill)

Worked unchanged: the manifest compiled; PostgreSQL started as root through
`su postgres -s /bin/sh -c "exec …"` (busybox has `su`; the recorded PID is the postmaster);
`dynamic_shared_memory_type=mmap`; the `pg_isready` and `miniflux -healthcheck` readiness
checks; migrations and admin creation; the API workload; the "already exists" retry rule; the
`S` knob (`synchronous_commit`, applied with `ALTER SYSTEM` + reload); every final invariant
evaluated; crash recovery after a cut.

Did not work as intended:
1. **Faults hit only the postmaster** (a bug in `hydo-init.sh`, not in the manifest).
   `P=postgres` left the backends serving miniflux (steps 10–12 took 0.06 s "while paused"),
   and the power-cut freeze left the WAL writer flushing, so `synchronous_commit=off` + cut
   lost nothing. Fixed on main in `8ef674e` (kill, pause, resume and the freeze go to the
   whole process tree). With the fix the pause blocks (steps 10–12 take 9 s, three client
   timeouts) and `S=off` + cut loses an acknowledged category.
2. **The periodic invariant almost never ran.** 30 creates take ~0.3 s of guest time, less
   than `every: 2`; it was evaluated only when a fault slowed the workload. Recorded in the
   skill.
3. **The explorer's generators fault only `front`** (miniflux): no generated postgres faults,
   no `S` knob, and no one-way fault was drawn in 30 seeds. Hand-written schedules cover
   postgres.

## Hand-written schedules (each twice, identical fingerprints)

On the old image (first PID only):

| schedule | verdict | fingerprint |
|---|---|---|
| `run` | OK, acked=30 exactly once | `909117d9438d2f99` |
| `run K=postgres@10+3` | OK (3 creates failed fast, the retry re-applied them) | `58774edb2e7b0ee8` |
| `run K=front@10+3` | OK | `839ee5276586b0f8` |
| `run P=postgres@10+3` | OK — but not a real pause (see 1.) | `a3d333f0b73b7674` |
| `run P=postgres@10~50:4000` | OK — same caveat | `12ceadac472f2323` |
| `run O=postgres@10-15:resp:loss,100%` | OK (5 client timeouts; retry finds "already exists") | `1d4b053678d17a90` |
| `run O=front@10-15:resp:loss,100%` | OK | `7fc1a4e946526c7c` |
| `run O=front@10-15:req:loss,100%` | OK | `19cc5fcdc5f2fa81` |
| `run C=20` | OK, acked=20 survive | `9528d6f8aad27135` |
| `run C=20 S=off` | OK — the freeze missed the WAL writer (see 1.) | `35d19164e2645b63` |

On the `-pt` image (whole process tree):

| schedule | verdict | fingerprint |
|---|---|---|
| `run` | OK, acked=30 exactly once | `4d456ac1c7de578f` |
| `run P=postgres@10+3` | OK (real pause; periodic API check evaluated) | `3eb1ee824394c338` |
| `run P=postgres@10~50:4000` | OK | `167462a5fd57d5c1` |
| `run K=postgres@10+3` | OK | `c5c6c73e2d99cbb2` |
| `run C=20` (`synchronous_commit=on`) | OK, acked=20 survive | `9b029d7c21d34a8b` |
| `run C=20 S=off` | **VIOLATION** `category c4 stored 0 times` — PostgreSQL's documented asynchronous-commit window (expected) | `8f7290a59489aaac` |

(All schedules are `disk=none dseed=1 | …`.)

## Exploration

`explore.py --image explore-ob-miniflux-tree-disk.img --strategy swarm --new-faults --seeds 1-30
--workers 1` (the scratch image, byte-identical to `-pt`): 30 schedules, 10 failures, 10/10
reproduced, all expected classes — 4 × an invariant could not be evaluated under injected I/O
errors, 4 × postgres could not restart under I/O errors, 2 × a lying disk lost an acknowledged
category (minimal `disk=lying:8 dseed=b3c71c31800079a2 | run N=kafka@10-13:duplicate,13%,loss,10%
C=24 T=26:-86400`, `650b9685508b5067` twice; the ignored `N=kafka` token was not shrunk away
because removing it changes the console and thus which writes the lying disk drops). No
failure with durable settings and an honest disk. Report:
`/var/tmp/m48-miniflux/explore-tree/report.md`.

**Classification:** no miniflux or PostgreSQL bug. The one durable-config-relaxed violation is
the documented `synchronous_commit=off` window; the rest are lying-disk / I/O-error classes;
the first-PID signal gap was a harness bug, now fixed.

## Rebuilt with the netem drain (2026-10-23, M77)

`explore-ob-miniflux-nq-disk.img` (`7b694c9f…`, two builds identical; from `2e0e3fe` with the
pinned tools and the same miniflux binary `dd8e2456…`): the `-pt` image with the M71 init, so a
lifted network fault delivers netem's queued packets. Only `/sbin/init` differs from
`explore-ob-miniflux-pt-pin`. All ten hand-written schedules above (both tables) give the same
verdict as on the old image, each twice with the same fingerprint, for example `run` OK
`092d1ebdf7cfd714`, `C=20 S=off` VIOLATION `category c4 stored 0 times` `b883bfaac88baf61`. Full
table: `docs/REBASELINE.md` section 7.1.

## With the store readers in Rust (2026-10-31, M112)

`hydo-read.yaml` (M88: the PostgreSQL loops replaced by `hydo-check`) with the Rust `hydo-read`
(`hydo/crates/hydo-read`, `64452374…`), built on the bare-metal test machine: image `162da151…`.
The image cannot be the M88 build's bytes with only the binary swapped, because a root without
`root.size_mib` is sized from `du -sm` of the staged tree and that depends on the host's file
system, so the Go binary was built into a new image on the same machine too (`303199af…`). Every
verdict line is the Go image's:

| schedule | verdict | Go M88 (`007c4590…`) | Go, rebuilt | Rust (twice) |
|---|---|---|---|---|
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION `exactly-once violated: acked c4 is in categories 0 times (of 20 in acked); missing 16: c4, c5, c6, c7, c8 (+11 more)` | `7c45986931b60e50` | `71f98926480cdf6b` | `0e257643269720d7` |
| `disk=none dseed=1 \| run` | OK `each of the 30 items of acked occurs once in categories (categories has 31 records)` | `016e4b08a2ac4129` | `31bceb13d979a3e0` | `7681d3ab7dcb5947` |

The eight other M88 schedules once each, all OK with Go's line: `K=postgres@10+3`
`61ea87e5bd7c384c`, `K=front@10+3` `25fb9b8924154693`, `P=postgres@10+3` `185dbb0c176847d8`,
`P=postgres@10~50:4000` `3412555891abf546`, `O=postgres@10-15:resp:loss,100%` `092a37d190a52d46`,
`O=front@10-15:resp:loss,100%` `73f3ead7a5736fbc`, `O=front@10-15:req:loss,100%`
`32876de933e0ca30`, `C=20` `ce35e3444da1d458`. Details: `docs/HISTORY.md` Milestone 112.

## Milestone 136: agent images (the current values)

Since M136 every image is an agent image (the guest agent `hydo-agent` as the first process, the
host controller driving it; docs/HISTORY.md "Milestone 136"), so every fingerprint above this
section belongs to a shell-init image of an earlier milestone. The current values, each schedule
run twice with the same fingerprint (docs/REBASELINE.md section 8, which also says how each image
was made):

**x86_64 (hvx), miniflux**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `bf92f72fb2ee7743` | yes |
| `disk=none dseed=1 \| run K=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `94f7dff586a28ece` | yes |
| `disk=none dseed=1 \| run K=front@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `fe35c945ecc011c3` | yes |
| `disk=none dseed=1 \| run P=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `f2a0a493e00ee1fa` | yes |
| `disk=none dseed=1 \| run P=postgres@10~50:4000` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `17ee484de861e07a` | yes |
| `disk=none dseed=1 \| run O=postgres@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `22222bf0ae764cf7` | yes |
| `disk=none dseed=1 \| run O=front@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `2165a6d333a70bb5` | yes |
| `disk=none dseed=1 \| run O=front@10-15:req:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `632a3ecf4a94866f` | yes |
| `disk=none dseed=1 \| run C=20` | OK invariants=2 held (categories.acked_exactly_once: acked=20 exactly once) | `de5339c8ae51f04a` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] category c18 stored 0 times | `5cd2fcd9fc424b6b` | yes |

**x86_64 (hvx), miniflux-read**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: exactly-once holds: each of the 30 items of acked occu... | `d9945a951b2253fd` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] exactly-once violated: acked c18 is in categories 0 times (of 20 ... | `aa403b06c9e5391d` | yes |
