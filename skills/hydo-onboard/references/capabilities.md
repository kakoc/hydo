# What works where

hydo has two backends and two ways to build. Check this table before using a feature; `hydo
build --check` validates the schema only, and the full `hydo build` refuses a feature the
backend lacks. State of hydo 0.1 (2026-10-30).

The `hydo/` folder (the test composer) needs a release that contains
`hydo/onboard/hydo_composer.py`; release tarballs built before 2026-10-24 lack it and fail with
"No module named 'hydo_composer'". `hydo guide`, `hydo skill` and `hydo init` need a release
that contains `hydo/onboard/hydo_agents.py`.

- **Linux release** (`hydo` from `hydo-linux-0.1.0`): x86_64 guests on an Intel VT-x machine.
- **Mac release** (`hydo` from `hydo-mac-0.1.0`): arm64 guests on an Apple Silicon Mac.
- The arm64 build (`hydo-build --arch arm64`, which the Mac release runs) supports every manifest
  feature except `guest.cpu: v3` (Milestone 96). `network: bridge`, `addr:` and `faults: [freeze]`
  build the image for a second Mac guest kernel, `Image-explore-netns`, which the image records and
  hydo boots by itself (Milestone 99); "after M99" below means a Mac release that ships that kernel,
  `guest/hydo_fc` and the aarch64 `ip`/`iptables-legacy` files. A Mac release built before M96 lacks
  `params:`, `hydo-world`, `hydo-read` and the host's history checkers; "after M96" below means a
  release rebuilt with them.
- **hydo's repository on x86_64 Linux**: `hydo/onboard/hydo-build` with the helper programs built
  from source (`hydo/onboard/world/build.sh` for `hydo-world`, `hydo/readers/build.sh` for
  `hydo-read`, Go for `hydo-grpc-gen`) and the pinned tools of `docs/BUILD-PINS.md`.

| feature | Linux release | Mac release | repository (x86_64) |
|---|---|---|---|
| services, `root.files`, data disk, `knobs:`, `provision:` | yes | yes | yes |
| `hydo/` folder: commands, checks `final_`/`always_`/`eventually_`, regressions | yes | yes | yes |
| `eventually` liveness checks | yes | yes | yes |
| Docker/OCI images (`image:`), `hydo compose` | yes | yes (before M99 `hydo compose` passed `--no-addr`; after M99 it writes `network: bridge` as on Linux; a big image root needs a bigger `guest.ram_mib`, since the root is in guest RAM) | yes |
| `guest.ram_mib` | 256–3072 | 256–4096 | 256–3072 |
| `guest.clock_after_cut` | yes | yes | yes |
| `guest.cpu: v3` (AVX/AVX2) | yes | no (arm64) | yes |
| `params:` (build variants, `hydo build --set k=v`) | yes | after M96 | yes |
| `addr:` and link faults between services | yes | after M99 (second guest kernel) | yes |
| `network: bridge` (a network namespace per service) | not shipped yet (needs `hydo-init-netns.sh` and the `llc`/`stp`/`bridge`/`veth` modules in the release) | after M99 (second guest kernel `Image-explore-netns`) | yes |
| network and one-way faults on a port | yes | yes | yes |
| `faults: [freeze]` (`F=`, freeze sweeps) | yes | after M99 (`F=` and `--freeze-call`; freeze sweeps not yet run on the Mac) | yes |
| `checks:` / `linearizable_` / `list_append_` history checks | yes | after M96 (ships `hydo/history/`) | yes |
| `buggify: true` | not shipped (the init extension is missing from the release) | yes | yes |
| `hydo-read`, `hydo-check` | not bundled (build `hydo-read` in the repository and set `HYDO_READ_BIN`) | after M96 (ships `guest/hydo-read`; else `HYDO_READ_BIN_ARM64`) | yes |
| `fakes:`, `proxies:`, `H=` protocol faults, `hydo-world` checks | not shipped | after M96 (ships `guest/hydo-world`; else `HYDO_WORLDBIN_ARM64`) | yes |
| `errors:` error logs as findings, `hydo errors`, `hydo/known-errors.yaml` | not shipped yet (needs a release with `hydo/onboard/hydo-init-errlog.sh` and `hydo_errors.py`) | not shipped yet (same; the arm64 build with `errors:` was not run) | yes |
| `hydo-grpc-gen` generated drivers | not shipped (generate in the repository, then copy the output) | not shipped (generate with `--arch amd64,arm64` in the repository, copy `hydo-grpc.arm64`) | yes |

Where a feature is missing, say so in the report and offer the closest substitute: a shell
read-back loop instead of `hydo-check` (invariant-patterns.md), a hand-written driver instead of
a generated one, a port fault instead of a link fault, `--no-addr` (one shared loopback
network) instead of `network: bridge`.
