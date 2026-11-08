# Releasing hydo

The steps of a release, in order. Each release is two tarballs, the two sources archives that
accompany them and, since 0.2.0, the Mac programs' signatures asset
`hydo-mac-<version>-signatures.tar.gz` (docs/design/signing.md); nothing is published until
step 7, and step 8 points the Homebrew tap at the new Mac tarball. The onboarding skill is released on its own (the last section). The history of how
each step was first done is in docs/HISTORY.md (Milestones 149, 150, 153–158, 168 and 170).
docs/PUBLISHING.md lists every place hydo is published.

## 1. Before building

1. Decide the version (`hydo/VERSION`, and `HYDO_VERSION` for the release scripts; AGENTS.md open
   item 40 says what a version change moves). The SDKs carry the same version (docs/PUBLISHING.md,
   "The next version", lists the files and the images a version change moves).
2. On the final commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`
   and `cargo test --locked --release --workspace` pass: the full test tier, every test of the
   workspace and not only the per-push tier's (docs/CI.md "Test tiers"); the `ci-full` workflow
   runs it on GitHub's Linux and macOS runners (`gh workflow run ci-full.yml --ref <branch>`).
   Among them:
   - `public_content` (no machine details, no value of the local list, in any tracked file);
   - `hydo-release`'s `third_party` tests: the component lists `hydo/dist/<platform>/third-party.json`
     are complete (every file of `assets.sha256` and `guest-programs.sha256` and every literal
     destination of `build-release.sh` belongs to a component; every component has a license, its
     full license texts and a source, a copyleft one its source archive pinned by sha256; every
     Rust crate compiled into a shipped program has a license and license texts);
   - `hydo-release`'s `sdks` tests: every SDK package has this version, Apache-2.0 and the
     repository, and the Go module `sdk/go` holds the frozen copy's code.
3. `git grep -i` the competitor names AGENTS.md "No other product in hydo's space" names: only
   citations in research notes.
4. The recorded example values are those of the final commit: `hydo-bench rebaseline --check --thorough`
   (every image built twice, every schedule run twice from fresh boots, nothing reused) on
   the bare-metal machine (every `hvx` image of `hydo/onboard/recorded.json`) and on a Mac (every
   `hvm` image) exits 0 (docs/REBASELINE.md "How to re-baseline"). Exit status 1 is a verdict
   change, a run whose two repetitions differ or an image that does not build twice to the same
   bytes: find the cause before the release, because a user who rebuilds an example would see it.
   Exit status 3 means only values moved: re-record with `--write`, review the list's diff and
   commit it (with a raised `hydo_image::EPOCH` when an engine, the controller, the agent pin or a
   guest kernel moved them). If this step is skipped, the release ships examples whose documented
   values may no longer be what they give.

### When a shipped file, a pinned asset or a crate changes

Update the platform's `third-party.json` (format: `hydo/crates/hydo-release/src/lib.rs`):

- a new third-party file: add it to the `files` of its component, or add a component with its
  `name`, `version`, SPDX `license`, `texts` (copy its license files from its source archive into
  `hydo/dist/licenses/<archive name>/`), `sources` (`file`, `sha256`, `url`, `from`) and, when
  Nix built it, `nix` (system, attribute, store paths). A copyleft component built by Nix needs
  its recipe: the attribute in the pinned nixpkgs, or, when another nixpkgs revision built it,
  that tree as a source (`nix-pack:` with the GitHub archive URL) named by `nixpkgs`, with
  `drv` and ideally its derivation closure (`nix-recipe:`) named by `recipe` (the busybox 1.37.0
  entry is the example). Record the revision when a store path is fetched; recovering it later
  took the build machine's fetcher cache (docs/BUILD-PINS.md "The sources archives");
- a new source archive: fetch it into the sources cache with `hydo/dist/fetch-sources.sh` on a
  Linux machine with Nix and the pinned nixpkgs (the bare-metal machine), or by hand for a `url`
  source, and pin its sha256 (a line with `-` as its sha256 fetches the file and prints it);
- a new Rust crate: nothing, unless the test says it has no license field or text (then a
  `rust_overrides` entry);
- `target/release/hydo-release check <linux|mac> --repo .` runs the checks without the tests.

## 2. Build each tarball twice

Each build also writes, after the tarball, `hydo-<platform>-<version>-sources.tar.gz`. Its source
archives come from a sources cache (`HYDO_SOURCES_FROM=DIR`, else `hydo/dist/<platform>/sources/`,
filled once from the machine with the Nix store paths).

- **Linux, on the bare-metal machine.** Create `/var/tmp/hydo-QUIET` first (another agent's
  low-priority work pauses while it exists) and remove it when done. Check `free -m`. Build in
  an own tree made from an archive of the commit's tracked files, with `hydo/COMMIT` holding the
  full commit hash, from clean Rust targets (`target/`, `hydo/crates/hydo-agent/target/`,
  `hydo/crates/hydo-read/target/` removed):
  `HYDO_ASSETS_FROM=<assets> HYDO_SOURCES_FROM=<cache> sh hydo/dist/linux/build-release.sh <out>`,
  with `~/hydo-rust/env.sh` sourced. Twice, into two output directories. The assets directory
  must hold every file of `assets.sha256` and nothing else: since 0.2.0 that includes the KVM
  engine's guest kernel `guest/bzImage-kvm` (`d50fbc67…`, built with a sandboxed Nix,
  docs/BUILD-PINS.md "The KVM engine's guest kernel"). `~/hydo-layout-assets` on the bare-metal
  machine is 0.1.0's set, without the KVM kernel; leave it so, because other trees build 0.1.0
  from it and the script copies the whole directory and counts its files. Milestone 172 used `~/hydo-m172-assets`, hard links to it plus `guest/bzImage-kvm`. The sources cache
  must hold the M158 addendum's files (`~/hydo-m159-sources` on the bare-metal machine;
  `~/hydo-m158-sources` lacks busybox's recipe).
- **Mac, on this Mac.** Check `df -h /` (two builds and the sources archives need several GB).
  Build from the same archive at one fixed tree path for both builds (the Mach-O `LC_UUID`
  depends on it), from clean target directories, with the assets copied from
  `hydo/dist/mac/assets/` of the main checkout: `HYDO_SOURCES_FROM=<cache> sh hydo/dist/mac/build-release.sh <out>`.
  Twice.
- The two builds of each platform must give the same sha256 for the tarball and for the
  sources archive. The Mac tarball is unsigned at this point (the linker's ad-hoc signatures);
  step 2a makes the published one.

## 2a. Sign and notarize the Mac programs (since 0.2.0)

On this Mac, with the Developer ID key in the login keychain (codesign asks once whether it may
use it) and the notarytool credentials stored as the keychain profile `hydo-notary`:

```
sh hydo/dist/mac/sign-release.sh <out>/hydo-mac-<version>.tar.gz <sign out>
```

It signs `bin/hydo`, `bin/hydo-build`, `bin/hydo-fetch-image` and `bin/hydo-hvm` (with the
hypervisor entitlement) with the hardened runtime and a secure timestamp, submits them to Apple's
notary service in one zip and waits for `Accepted` (at most 3 hours, `HYDO_NOTARY_TIMEOUT`; the
answer and the log go to `notary-submit.json` and `notary-log.json`), writes the signatures asset
`<sign out>/hydo-mac-<version>-signatures.tar.gz`, and makes the signed tarball
`<sign out>/signed/hydo-mac-<version>.tar.gz` from the unsigned one with
`attach-signatures.sh`. Then prove it as a user would: attach the asset to the other unsigned
build of step 2 (`sh hydo/dist/mac/attach-signatures.sh <out 2>/hydo-mac-<version>.tar.gz
<asset> <check out>`) and compare the sha256 with the signed tarball's. The signed tarball is the
one every later step uses and step 7 publishes as `hydo-mac-<version>.tar.gz`; the unsigned one is
not published. A stapled ticket is not possible for command-line programs (docs/design/signing.md).
If this step is skipped, the release is unsigned as 0.1.0 was, and `hydo setup` signs the engine
ad hoc on each user's Mac.

## 3. Compare with the previous tarballs

Unpack the new and the previous tarball of each platform and compare every file's sha256. Only
the changes the release intends may differ (for example `SHA256SUMS`, the commit compiled into
`bin/hydo` and `bin/hydo-build`, changed documents); no guest file and no pinned asset may.

## 4. Verify from clean prefixes

- **Linux** (the bare-metal machine, every VM through `~/bin/hvx-run`, at most two at a time):
  `install.sh --no-module` into a new prefix with a PATH that has no Python, Go, Rust, Nix, C
  compiler or `make` (traps that log a call and fail); `hydo doctor`; the quickstart of the
  release README with every value of docs/REBASELINE.md (the newest section's quickstart values),
  each twice, as the user and again as root (the engine in-process); no trap called. In a second
  clean prefix with the machine's compiler on PATH, `hydo module build` must build the loaded
  module's srcversion (build only; never load or unload a module for this).
- **Linux with KVM** (since 0.2.0; the build machine, as its login user, at most two VMs, no
  module touched): `install.sh --no-module` into a new prefix; `hydo doctor --backend kvm`; the
  quickstart with `--backend kvm` (the values of docs/design/kvm-engine.md for the Redis image:
  `bfa03053d51bec8a` and `8ed16d6da60c34c6`), each twice, and the exploration with `hydo repro`
  of it.
- **Mac** (the signed tarball): `install.sh --prefix <new> --add-to-path` with stubs for Python,
  Go, cargo, rustc, rustup, Nix, the compilers, `make`, `xcrun` and Docker; `hydo setup` (it must
  not sign `bin/hydo-hvm` again: its record has no `signed` line), `hydo doctor` (it names the
  Developer ID), the quickstart with every Mac value of docs/REBASELINE.md, each twice; no stub
  called. On the installed programs: `codesign --verify --strict --verbose=2`, `codesign -dvv`
  (the Developer ID, the team, `flags=0x10000(runtime)`, a timestamp), and Gatekeeper's verdict
  `spctl --assess --type open --context context:primary-signature -v` ("accepted, source=Notarized
  Developer ID"; `--type execute` assesses app bundles only).

## 5. Scan the contents

Unpack each tarball and expand every `.tar`, `.gz`, `.xz` and `.cpio.gz` in it; search every
file's bytes for the values of the local list (AGENTS.local.md, whole words, any case), home
paths, remote-shell words, dotted quads and the competitor names. Matches may only be in
third-party files and must be explained. For each sources archive, scan the files hydo writes
(`README.md`, `THIRD-PARTY.md`, `third-party.json`, `hydo/`); the upstream archives are third-party
as they are. Scan the signed Mac tarball (not only the unsigned one) and the signatures asset too.

## 6. Collect and record

- Put the five files (the two tarballs, the Mac one signed, the two sources archives and the
  signatures asset) into `hydo/dist/out/` of the main checkout (the previous ones into
  `hydo/dist/out/before/`) and write `SHA256SUMS` with all five lines.
- Record the values: AGENTS.md (the current tarballs, a milestone line and the index),
  docs/HISTORY.md (a milestone section), the release READMEs (`hydo/dist/*/README.md`).

## 7. Publish (the user decides)

Create the GitHub release for the tag of the final commit and upload all five files and
`SHA256SUMS`. The release notes name the sources archive as the corresponding source of the
third-party software in the tarballs, and say that the Mac tarball is the unsigned build of the tag
with the signatures asset attached (`hydo/dist/mac/attach-signatures.sh`, docs/design/signing.md). Keep every sources archive available for as long as its
tarball is offered (and at least three years after the last download of it, as GPL-2.0 section 3
asks of a written offer).

## 8. Update the Homebrew tap (after the release is published)

`brew install kakoc/tap/hydo` installs the Mac tarball of the release the formula names
(https://github.com/kakoc/homebrew-tap, `Formula/hydo.rb`; Milestone 168). After step 7, in a
clone of the tap repository:

1. `sh hydo/dist/mac/update-homebrew.sh <version> <tap clone>` writes the formula's `url` and
   `sha256` for `hydo-mac-<version>.tar.gz` from the release's `SHA256SUMS` and checks the
   published tarball against it (Homebrew reads the version from the url).
2. `brew style Formula/hydo.rb`, then commit (`hydo <version>`) and push the tap.
3. On a Mac: `brew update`, then `brew upgrade kakoc/tap/hydo` (or `brew install
   kakoc/tap/hydo`); `hydo doctor` must pass every check, the quickstart's Redis example must give
   the Mac values of docs/REBASELINE.md, and `brew test hydo` and `brew audit --strict --online
   kakoc/tap/hydo` must pass.

If this step is skipped, `brew install kakoc/tap/hydo` keeps installing the previous release.

## 9. Add the release to the release index (after publishing)

Every later hydo fetches this release's archive, and the guest kernels inside it, by sha256 from
the release index compiled into hydo (`hydo/dist/releases.txt`, Milestone 162,
docs/design/install-cache.md). On `main`, add the release's lines in one commit, after the release
is published and only then (a line names a file that must exist at
`<release URL>/v<version>/<file>`):

- `archive <version> linux <sha256> hydo-linux-<version>.tar.gz` and the same for `mac`, with the
  sha256 of the published `SHA256SUMS` asset;
- one `file <version> <platform> <sha256> <path>` line per guest kernel and initramfs in each
  tarball (today `guest/bzImage`, `guest/bzImage-kvm` (since 0.2.0) and `guest/initramfs-kafka.cpio.gz` for Linux,
  `hv/hvm/explore/out/Image-explore` and `Image-explore-netns` for the Mac), with the sha256 of
  the tarball's own `SHA256SUMS` line.

Never change a line that is there. `cargo test -p hydo-image` checks the format. The release
tarballs and `build-release.sh` do not change for this: the index is in the program, not in the
release.

## 10. Publish the SDKs (the user decides; with the release)

Publish every SDK with the release's version, in the order and with the commands of
docs/PUBLISHING.md: the four assertion SDKs, `hydo-assert` on crates.io, on PyPI and on npm, and
the tag `sdk/go/v<version>` for the Go module. The Rust test SDK (`hydo-test`,
`hydo-test-macros`, `cargo-hydo`) is not published (`publish = false`) until the user decides
about code-first tests.
An uploaded version can never be replaced on any of them (only yanked, deprecated or retracted),
so run each dry run first.

## Releasing the onboarding skill (separate from hydo releases)

The onboarding skill (`skills/hydo-onboard/`, Milestone 170) is the process of onboarding a
project; hydo's facts are in the guide built into each `hydo` (`hydo/guide/`, `hydo guide`). The
skill therefore has its own version (`metadata.version` in its `SKILL.md`, 1.0.0 first) and its
own releases, and a new skill version needs no new hydo. A change of the skill alone never needs
the steps above; a hydo release ships the skill as it is in the tree (`skill/hydo-onboard/` and
inside `bin/hydo`). Users get a skill release in two ways:

- **Claude Code** reads the marketplace `.claude-plugin/marketplace.json` at the repository root
  (there since hydo 0.2.0's release commit; until then it was staged at
  `hydo/dist/skill/marketplace.json`, so that the skill and a hydo with its guide topics went out
  together, user decision 2026-11-07) (`/plugin marketplace add kakoc/hydo`, then `/plugin install hydo-onboard@hydo`). Its plugin
  entry names the skill folder and the version. Claude Code offers an update when that version
  changes on the default branch (`claude plugin update hydo-onboard@hydo`, the `/plugin` panel,
  or by itself when the user turned on auto-update for the marketplace, which is off by default
  for marketplaces outside Anthropic's), so pushing the bumped version to `main` is the release
  for Claude Code users.
- **Codex, Cursor, GitHub Copilot and Gemini CLI** get the skill with `hydo skill install --agent
  <tool>` (the version carried by their hydo) and later `hydo skill update`, which fetches the
  newest GitHub release tagged `skill-v<version>`, checks its archive against the release's
  `SHA256SUMS` and installs it.

Steps, on a commit of `main` that holds the new skill:

1. Edit the skill. Keep it process only: every key, command, option or limit of hydo belongs in
   `hydo/guide/` (where a new hydo version ships it), and the skill names it as `hydo guide
   <topic>`. If the skill needs a guide topic that only a newer hydo has, raise
   `metadata.hydo-min` to that hydo's version and say in the step what the user should upgrade.
2. Raise `metadata.version` in `skills/hydo-onboard/SKILL.md` and `version` of the plugin entry in
   `.claude-plugin/marketplace.json` to the same new version (a fix: the third number; a new step
   or reference: the second). `cargo test -p hydo-agents --test it skill_check::` refuses different
   versions, an unknown `hydo guide` topic and anything in the folder that Claude Code would load
   as another plugin component; `claude plugin validate .` must pass.
3. Commit and merge to `main`.
4. `sh hydo/dist/skill/build-skill-release.sh` writes `hydo/dist/out/skill/hydo-skill-<version>.tar.gz`
   and `SHA256SUMS` (it packs twice and compares, and refuses uncommitted skill changes).
5. (the user decides) Tag the commit `skill-v<version>`, create a GitHub release for the tag (not
   marked as the latest release, so that `releases/latest` stays a hydo release), and upload both
   files. Never replace the assets of a published skill release: publish a new version instead.
6. Check: `hydo skill update --check --to <a folder with the previous version>` reports the new
   version (exit status 3), and `hydo skill update --to <that folder>` installs it.

If step 5 is skipped, Claude Code users still get the new version from `main` (step 3), but
`hydo skill update` keeps installing the previous release.
