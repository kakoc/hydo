# Releasing hydo

The steps of a release, in order. Each release is two tarballs, the two sources archives that
accompany them, since 0.2.0 the Mac programs' signatures asset
`hydo-mac-<version>-signatures.tar.gz` (docs/design/signing.md), and from 0.5.0 on the
onboarding skill's archive `hydo-skill-<version>.tar.gz` (the last section). Nothing is
published until step 7, and step 8 points the Homebrew tap at the new Mac tarball. Creating the
GitHub release in step 7 creates the tag `v<version>`, which starts the release workflow: it
publishes the registry packages (step 10) with trusted publishing, so no registry token or
security key is needed. The history of how each step was first done is in docs/HISTORY.md
(Milestones 149, 150, 153–158, 168, 170 and 332).
docs/PUBLISHING.md lists every place hydo is published.

## 1. Before building

1. Decide the version (`hydo/VERSION`, and `HYDO_VERSION` for the release scripts; AGENTS.md open
   item 40 says what a version change moves). The SDKs carry the same version (docs/PUBLISHING.md,
   "The next version", lists the files and the images a version change moves). So does the
   onboarding skill since Milestone 332: raise its four version fields in the same commit (the
   last section, step 3).
   The numbering rule (the user, 2026-11-15) has two parts:
   - A release raises the middle number: 0.4.0, then 0.5.0.
   - A release of small fixes raises the last number: 0.4.1.

   Plans keep their `hydo_version:` across releases. A plan that says `"0.3"` builds on 0.4.0
   and every newer 0.x, and only an older hydo refuses it (Milestone 262). So a release changes
   no plan's pin: not the examples', not `hydo init`'s template, and not the Rust test SDK's
   `HYDO_VERSION`. A plan moves its pin only when it uses a key of a newer release.
   In the same release, copy each format's current schema (`hydo/schema/<format>-v1.json`) that
   differs from its released copy into `hydo/schema/released/`. The schema tests
   (`hydo-schema-compat`) check every later change as an addition to the released copy, so a
   skipped copy lets a later change remove what this release shipped. 0.2.0 to 0.3.1 skipped it;
   0.3.2 copied `project-v1`, `results-v1` and `ci-v1`; 0.3.3 copied `project-v1`; 0.3.4
   copied `project-v1` and `results-v1`; 0.4.0 copied `ci-v1`, `image-v1`, `project-v1` and
   `results-v1`; 0.4.1 copied `project-v1`; 0.5.0 and 0.6.0 copied `project-v1` and `results-v1`.
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

**The order since 0.3.1.** First build one quick candidate of each platform from the commit that
raises the version: one build each, the Mac one unsigned, with no second build, comparison or
notarization. Start step 6a's readers on these candidates at once. Fix their findings in the
shipped pages and the code in one commit, and only then make the builds below, the signing and
the verification, once, from that commit. Delete the quick candidates afterwards. If the readers
run only on the final files, every finding that changes a shipped page costs a second round of
double builds, signing, notarization and verification.

Each build also writes, after the tarball, `hydo-<platform>-<version>-sources.tar.gz`. Its source
archives come from a sources cache (`HYDO_SOURCES_FROM=DIR`, else `hydo/dist/<platform>/sources/`,
filled once from the machine with the Nix store paths).

- **Linux, on the bare-metal machine.** Create `/var/tmp/hydo-QUIET` first (another agent's
  low-priority work pauses while it exists) and remove it when done. Check `free -m`. Build in
  an own tree made from an archive of the commit's tracked files, with `hydo/COMMIT` holding the
  full commit hash, from clean Rust targets (`target/`, `hydo/crates/hydo-agent/target/`,
  `hydo/crates/hydo-read/target/` and, since 0.3.0, `hydo/crates/hydo-sql/target/` removed):
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
- The guest kernels of each tarball are the ones `assets.sha256` pins, and a test of `docs::`
  checks that this list names each of them. Linux: `guest/bzImage`, `guest/bzImage-kvm`,
  `guest/bzImage-kvm-smp`, `guest/bzImage-kvm-cg`, `guest/bzImage-kvm-smp-cg`, and from 0.4.0
  on `guest/bzImage-kvm-containers` and `guest/bzImage-kvm-smp-containers`. Mac:
  `Image-explore`, `Image-explore-netns`, `Image-explore-cg`, `Image-explore-netns-cg`, and from
  0.4.0 on `Image-explore-containers` (`HYDO_KERNEL_CONTAINERS` for `build-release.sh`). A hydo
  built from a tree after a release can pin a guest file that the release lacks; `hydo setup`
  then names the file and refuses (Milestone 301).
- The two builds of each platform must give the same sha256 for the tarball and for the
  sources archive. The Mac tarball is unsigned at this point (the linker's ad-hoc signatures);
  step 2a makes the published one.
- From 0.5.0 on, each build also writes the onboarding skill's archive
  `hydo-skill-<version>.tar.gz` into its output folder (`hydo/dist/skill/build-skill-release.sh`,
  which refuses a skill whose version is not `hydo/VERSION`). All four builds must give it the
  same sha256.

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

- Put the six files (the two tarballs, the Mac one signed, the two sources archives, the
  signatures asset and, from 0.5.0 on, the skill's archive `hydo-skill-<version>.tar.gz`) into
  `hydo/dist/out/` of the main checkout (the previous ones into `hydo/dist/out/before/`). Write
  `SHA256SUMS` with all six lines. `hydo skill update` and `hydo update` find the skill's
  sha256 in this file.
- Record the values: AGENTS.md (the current tarballs, a milestone line and the index),
  docs/HISTORY.md (a milestone section), the release READMEs (`hydo/dist/*/README.md`).

## 6a. Docs review (since 0.3.0)

A fresh reader checks the pages a user reads first against the release candidate, before the tag.
Since 0.3.1 the readers get the quick candidates of step 2, so that their findings are fixed
before the final builds:

1. Make sure that `sh ci/vale.sh` prints no findings, and that
   `cargo test -p hydo --test it docs::` passes. The test fails when `README.md` or a quickstart
   names another version, cites a fingerprint that is not recorded, or shows a command or
   option that `hydo` does not have.
2. Start a reader with no other context: a coding agent with the prompt in
   `skills/hydo-docs/SKILL.md`, section `The release docs review`. Give it the release
   candidate's tarballs, `README.md`, and both quickstarts.
3. Fix each finding, or defer it with a reason. Write every deferred finding and its reason into
   the release notes.

Do not tag the release until every finding is fixed or deferred. If this step is skipped, a page
that does not match the release ships with it.

## 7. Publish (the user decides)

Create the GitHub release for the tag of the final commit and upload all six files and
`SHA256SUMS`, in one command:

```sh
gh release create v<version> --target <full commit hash> --title "hydo <version>" \
  --notes-file <notes> hydo/dist/out/hydo-linux-<version>.tar.gz \
  hydo/dist/out/hydo-linux-<version>-sources.tar.gz hydo/dist/out/hydo-mac-<version>.tar.gz \
  hydo/dist/out/hydo-mac-<version>-sources.tar.gz hydo/dist/out/hydo-mac-<version>-signatures.tar.gz \
  hydo/dist/out/hydo-skill-<version>.tar.gz hydo/dist/out/SHA256SUMS
```

From 0.5.0 on, the release workflow's `check` job also requires the skill's archive
(`hydo/dist/registries.sh release`).

`gh` uploads the files to a draft and publishes it when all are there; publishing creates the
tag `v<version>` on that commit, and the new tag starts the release workflow (step 10). This is
why the release comes first and the tag with it: the workflow publishes the crates of
`cargo install hydo`, which are useless before the release's archive can be downloaded, so its
`check` job refuses to publish anything until the release of the tag is published with its
tarballs and `SHA256SUMS`. Pushing the tag first also works (`git tag v<version> <commit>`,
`git push origin v<version>`, then `gh release create v<version> --verify-tag …` with the same
files): the `check` job waits up to 45 minutes for the release; if it takes longer, re-run the
workflow once the release is up. The release notes name the sources archive as the corresponding source of the
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
  tarball (today `guest/bzImage`, `guest/bzImage-kvm` (since 0.2.0), `guest/bzImage-kvm-smp`
  (since 0.3.2), `guest/bzImage-kvm-cg` and `guest/bzImage-kvm-smp-cg` (from the first release
  with Milestone 234), `guest/bzImage-kvm-containers` and `guest/bzImage-kvm-smp-containers`
  (from the first release with Milestone 256) and `guest/initramfs-kafka.cpio.gz` for Linux,
  `hv/hvm/explore/out/Image-explore`, `Image-explore-netns`, from the first release with
  Milestone 234 `Image-explore-cg` and `Image-explore-netns-cg`, and from the first release
  with Milestone 256 `Image-explore-containers` for the Mac), with the sha256 of the tarball's
  own `SHA256SUMS` line. The release that first ships the kernels with cgroups also sets
  `HYDO_KERNEL_CG` and `HYDO_KERNEL_NETNS_CG` for the Mac's `build-release.sh`, and the release
  that first ships the kernel for containers sets `HYDO_KERNEL_CONTAINERS`.

Never change a line that is there. `cargo test -p hydo-image` checks the format. The release
tarballs and `build-release.sh` do not change for this: the index is in the program, not in the
release.

## 10. Publish the packages (the release workflow; starts with step 7)

The tag of step 7 starts `.github/workflows/release.yml` (docs/PUBLISHING.md "Publishing from
GitHub Actions"; Milestone 186), which publishes with trusted publishing, without a token or a
security key: the 20 crates of `cargo install hydo` and `hydo-assert` on crates.io,
`hydo-assert` on PyPI and on npm, and the tag `sdk/go/v<version>` for the Go module. The Rust
test SDK (`hydo-test`, `hydo-test-macros`, `cargo-hydo`) is not published (`publish = false`)
until the user decides about code-first tests. Steps 8 and 9 do not wait for it.

1. Before step 7 (once per release, optional but cheap): a dry run on the release commit's
   branch, `gh workflow run release.yml --ref main -f dry_run=true`: the version check, every
   package packed and checked, `cargo install hydo` rehearsed on Linux and macOS, and the list of
   what the real run would publish. It warns (does not fail) while the release does not exist.
   npm only: if the trusted-publisher entry of `hydo-assert` was never used, it expires two days
   after it was added (docs/PUBLISHING.md, the one-time setup), so add it again first.
2. After step 7: watch the run (`gh run list --workflow release.yml`, then `gh run watch <id>`;
   about 30 to 45 minutes, most of it the `packages` job). With a required reviewer on the
   environment `release`, approve the deployment on the run's page.
3. If a job fails, fix the cause without moving the tag (a registry setting, a trusted-publisher
   entry, a network error) and re-run the failed jobs: every job skips what its registry already
   has. A failure in the packages themselves cannot happen at this point when the `packages`
   workflow passed on the release commit; if it does, the version cannot be published as it is
   (crates.io, PyPI and npm never take a version twice): fix it on `main` and release the next
   patch version.
4. Check from outside (docs/PUBLISHING.md, "Check from outside" and step 5 of the SDKs' commands).

If the workflow cannot run (GitHub Actions down, a registry's trusted publishing unavailable),
publish by hand with the commands of docs/PUBLISHING.md, in the same order; that needs each
registry's token or login, and npm asks for the second factor at every publish. An uploaded
version can never be replaced on any registry (only yanked, deprecated or retracted).

## The onboarding skill (ships with each hydo release)

The onboarding skill `onboard` (Milestone 170; named `onboard` since Milestone 229) is the
process of onboarding a project; hydo's facts are in the guide built into each `hydo`
(`hydo/guide/`, `hydo guide`). It lives in the Claude Code plugin folder `plugin/`:
`plugin/.claude-plugin/plugin.json` names the plugin `hydo`, and `plugin/skills/onboard/` is the
skill in the Agent Skills format. In Claude Code its command is `/hydo:onboard`.

Since Milestone 332 the skill has hydo's version and no releases of its own. Four version
fields equal `hydo/VERSION`: `metadata.version` and `metadata.hydo-min` in `SKILL.md`, the
`version` of `plugin/.claude-plugin/plugin.json`, and the `version` of the plugin entry in
`.claude-plugin/marketplace.json`. A skill change ships with the next hydo release, and a small
skill fix ships with a patch release such as 0.5.1. Users get the skill in three ways:

- **Inside hydo.** Each tarball carries the plugin folder (`plugin/`, and a copy inside
  `bin/hydo`). `hydo skill install --agent <tool>` and `hydo onboard` copy it.
- **From hydo's GitHub release.** From 0.5.0 on, the release `v<version>` has the asset
  `hydo-skill-<version>.tar.gz` (the plugin folder as `hydo/`), with its line in the release's
  `SHA256SUMS`. `hydo skill update` and `hydo update` read the release list once, take the
  newest `v<x>.<y>.<z>` release, check the archive by that line, and install it. Codex, Cursor,
  GitHub Copilot and Gemini CLI get updates this way.
- **From Claude Code's marketplace.** Claude Code reads `.claude-plugin/marketplace.json` on
  `main` (`/plugin marketplace add kakoc/hydo`, then `/plugin install hydo@hydo`). It installs a
  new copy when the plugin's version string differs from the installed one, so a lower version
  counts too. The update comes with `claude plugin update hydo@hydo`, the `/plugin` panel,
  `hydo update`, or auto-update, which is off by default for this marketplace.

The skill's old releases `skill-v1.0.0` to `skill-v2.4.0` had a version line of their own.
`hydo skill update` and `hydo update` treat every installed version from 1.0.0 to 2.4.0 as older
than any hydo version (`hydo_agents::release::skill_lt`), and the skill's step 0 asks `hydo
update --check`. So a copy of 2.4.0 is updated to 0.5.0. hydo's own version must never fall
from 1.0.0 to 2.4.0, and `skill_check::` refuses a `hydo/VERSION` in that range.

Steps of a skill change:

1. Edit the skill. Keep it process only: every key, command, option or limit of hydo belongs in
   `hydo/guide/`, and the skill names it as `hydo guide <topic>`.
2. Merge the change to `main`. Its version stays hydo's current one until the next release.
3. In the commit that raises `hydo/VERSION` (step 1 of the release), raise the four version
   fields to the same version. `cargo test -p hydo-agents --test it skill_check::` refuses a
   skill whose version differs from `hydo/VERSION`, an unknown `hydo guide` topic, and anything
   in the plugin folder that Claude Code would load as another plugin component. `sh
   hydo/dist/registries.sh versions <version>` checks the four fields too. `claude plugin
   validate .` and `claude plugin validate plugin` must pass.
4. Bring the commit that raises the version to `main` together with step 7. Claude Code users
   get the new skill as soon as `main` has it, and its step 0 then asks for the hydo of its
   version, which must be published by then.
5. The builds of step 2 write `hydo-skill-<version>.tar.gz`, and steps 6 and 7 publish it.
6. After step 7, check the update. `hydo skill update --check --to <a folder with the previous
   version>` exits with status 3, and `hydo skill update --to <that folder>` installs the new
   version.

If a release lacks the skill's archive, `hydo skill update` names the missing file and changes
nothing. Claude Code users still get the skill from `main`.

### Once, after 0.5.0 is published

These steps finish the move from the skill's own releases (Milestone 332). The lead does them;
they delete public releases, which the user allowed on 2026-11-20.

1. Write a "Before you upgrade" line into 0.5.0's release notes: "`hydo update` of 0.4.1
   updates hydo only. Run `hydo update` once more with 0.5.0 to bring every copy of the skill to
   0.5.0." hydo 0.4.1 reads the skill from `skill-v` releases only, and its plan shows this line.
2. Delete the old skill releases and their tags, `skill-v1.0.0` to `skill-v2.4.0` (`gh release
   list` names them): `gh release delete skill-v<version> --cleanup-tag --yes` for each. After
   this, hydo 0.4.1 finds no skill release and leaves every copy as it is.
3. Check from a copy of skill 2.4.0: `hydo update --check` of 0.5.0 names it with `2.4.0 ->
   0.5.0`, and `hydo update --yes` installs 0.5.0.

If Claude Code does not offer 0.5.0 to an install of 2.4.0, the user runs `claude plugin
marketplace update hydo`, then `claude plugin update hydo@hydo`. If that still says that the
plugin is at the latest version, the user runs `claude plugin uninstall hydo@hydo`, then `claude
plugin install hydo@hydo`.
