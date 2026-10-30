# What works where

hydo has two backends and two ways to build. Check this table before using a feature; `hydo
build --check` validates the schema only, and the full `hydo build` refuses a feature the
backend lacks. State of hydo 0.1 (2026-10-26).

The `hydo/` folder (the test composer) needs a release that contains
`hvx/onboard/hydo_composer.py`; release tarballs built before 2026-10-24 lack it and fail with
"No module named 'hydo_composer'". `hydo guide`, `hydo skill` and `hydo init` need a release
that contains `hvx/onboard/hydo_agents.py`.

- **Linux release** (`hydo` from `hydo-linux-0.1.0`): x86_64 guests on an Intel VT-x machine.
- **Mac release** (`hydo` from `hydo-mac-0.1.0`): arm64 guests on an Apple Silicon Mac.
- **hydo's repository on x86_64 Linux**: `hvx/onboard/hydo-build` with the helper programs built
  from source (`hvx/onboard/world/build.sh` for `hydo-world`, `hvx/readers/build.sh` for
  `hydo-read`, Go for `hydo-grpc-gen`) and the pinned tools of `hvx/BUILD-PINS.md`.

| feature | Linux release | Mac release | repository (x86_64) |
|---|---|---|---|
| services, `root.files`, data disk, `knobs:`, `provision:` | yes | yes | yes |
| `hydo/` folder: commands, checks `final_`/`always_`/`eventually_`, regressions | yes | yes | yes |
| `eventually` liveness checks | yes | yes | yes |
| Docker/OCI images (`image:`), `hydo compose` | yes | yes (no `addr:`: `hydo compose` passes `--no-addr`) | yes |
| `guest.ram_mib` | 256–3072 | 256–4096 | 256–3072 |
| `guest.clock_after_cut` | yes | yes | yes |
| `guest.cpu: v3` (AVX/AVX2) | yes | no (arm64) | yes |
| `params:` (build variants, `hydo build --set k=v`) | yes | no | yes |
| `addr:` and link faults between services | yes | no | yes |
| `network: bridge` (a network namespace per service) | not shipped yet (needs `hydo-init-netns.sh` and the `llc`/`stp`/`bridge`/`veth` modules in the release) | no (the arm64 guest kernel has no network namespaces) | yes |
| network and one-way faults on a port | yes | yes | yes |
| `faults: [freeze]` (`F=`, freeze sweeps) | yes | no | yes |
| `checks:` / `linearizable_` / `list_append_` history checks | yes | not shipped | yes |
| `buggify: true` | not shipped (the init extension is missing from the release) | yes | yes |
| `hydo-read`, `hydo-check` | not bundled (build `hydo-read` in the repository and set `HYDO_READ_BIN`) | no | yes |
| `fakes:`, `proxies:`, `H=` protocol faults, `hydo-world` checks | not shipped | no | yes |
| `hydo-grpc-gen` generated drivers | not shipped (generate in the repository, then copy the output) | not shipped | yes |

Where a feature is missing, say so in the report and offer the closest substitute: a shell
read-back loop instead of `hydo-check` (invariant-patterns.md), a hand-written driver instead of
a generated one, a port fault instead of a link fault, `--no-addr` (one shared loopback
network) instead of `network: bridge`.
