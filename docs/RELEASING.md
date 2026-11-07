# Releasing hydo

The steps of a release, in order. Each release is two tarballs and the two sources archives that
accompany them; nothing is published until step 7, and step 8 points the Homebrew tap at the new
Mac tarball. The history of how each step was first done is in docs/HISTORY.md (Milestones 149,
150, 153–158 and 168).

## 1. Before building

1. Decide the version (`hydo/VERSION`, and `HYDO_VERSION` for the release scripts; AGENTS.md open
   item 40 says what a version change moves). The SDKs carry the same version (docs/PUBLISHING.md,
   "The next version", lists the files and the images a version change moves).
2. On the final commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`
   and `cargo test --locked --release --workspace` pass. Among them:
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
  `HYDO_ASSETS_FROM=~/hydo-layout-assets HYDO_SOURCES_FROM=<cache> sh hydo/dist/linux/build-release.sh <out>`,
  with `~/hydo-rust/env.sh` sourced. Twice, into two output directories.
- **Mac, on this Mac.** Check `df -h /` (two builds and the sources archives need several GB).
  Build from the same archive at one fixed tree path for both builds (the Mach-O `LC_UUID`
  depends on it), from clean target directories, with the assets copied from
  `hydo/dist/mac/assets/` of the main checkout: `HYDO_SOURCES_FROM=<cache> sh hydo/dist/mac/build-release.sh <out>`.
  Twice.
- The two builds of each platform must give the same sha256 for the tarball and for the
  sources archive.

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
- **Mac**: `install.sh --prefix <new> --add-to-path` with stubs for Python, Go, cargo, rustc,
  rustup, Nix, the compilers, `make`, `xcrun` and Docker; `hydo setup`, `hydo doctor`, the
  quickstart with every Mac value of docs/REBASELINE.md, each twice; no stub called.

## 5. Scan the contents

Unpack each tarball and expand every `.tar`, `.gz`, `.xz` and `.cpio.gz` in it; search every
file's bytes for the values of the local list (AGENTS.local.md, whole words, any case), home
paths, remote-shell words, dotted quads and the competitor names. Matches may only be in
third-party files and must be explained. For each sources archive, scan the files hydo writes
(`README.md`, `THIRD-PARTY.md`, `third-party.json`, `hydo/`); the upstream archives are third-party
as they are.

## 6. Collect and record

- Put the four files into `hydo/dist/out/` of the main checkout (the previous ones into
  `hydo/dist/out/before/`) and write `SHA256SUMS` with all four lines.
- Record the values: AGENTS.md (the current tarballs, a milestone line and the index),
  docs/HISTORY.md (a milestone section), the release READMEs (`hydo/dist/*/README.md`).

## 7. Publish (the user decides)

Create the GitHub release for the tag of the final commit and upload all four files and
`SHA256SUMS`. The release notes name the sources archive as the corresponding source of the
third-party software in the tarballs. Keep every sources archive available for as long as its
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
  tarball (today `guest/bzImage` and `guest/initramfs-kafka.cpio.gz` for Linux,
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
