# The install cache: hydo fetches what a version needs from its release

Status: built in Milestone 162 (crates `hydo-cli` `src/setup.rs`, `src/cache.rs` and `src/fetch.rs`,
`hydo-image` `src/store.rs`, `src/releases.rs` and `kernels::obtain`, `hydo-config`); for after
the first release.

## What it is for

Until Milestone 162 a hydo installation was exactly one unpacked release tarball, and nothing
could be fetched again: a damaged file meant unpacking the tarball again (M154), and an image
built by an older hydo whose guest kernel the running hydo does not ship was refused (M155,
`kernels::obtain` found nothing). This design gives hydo one per-user cache that it fills from
the project's GitHub releases, verified by sha256, so that:

- `hydo setup --get [VERSION]` installs a version into `~/.hydo/versions/<version>` from its one
  release archive and points `~/.hydo/bin/hydo` at it. An upgrade is one command, the old version
  stays where it was, and nothing is downloaded twice. This is the step a `cargo install hydo` or
  a Homebrew formula needs later: they install the `hydo` program only, and `hydo setup --get`
  fetches the rest of the same version.
- `hydo setup` on an unpacked release restores a missing or changed file from that version's
  archive instead of refusing (the plug-in point `obtain_assets` of M154).
- A run of an image whose record names a guest kernel this hydo does not have finds the release
  that shipped that kernel, fetches that release's archive and takes the kernel out of it (the
  plug-in point `kernels::obtain` of M155). An image built by hydo 0.1.0 keeps running under
  every later hydo.

The release format does not change: one archive per platform and version
(`hydo-<platform>-<version>.tar.gz`, top directory `hydo-<platform>-<version>/`, the tree's
`SHA256SUMS` inside it) and the release's own `SHA256SUMS` asset, which lists the archives
(docs/RELEASING.md). No archive is split into programs and assets; a later "smaller downloads"
step can add per-file assets without changing the cache, because the cache is keyed by sha256.

## Where from

`<base>/v<version>/<file>`, with `<base>` = `https://github.com/kakoc/hydo/releases/download`
(`HYDO_RELEASE_URL` names a mirror; the tests use a local server). Two kinds of file are fetched:
an archive (`hydo-linux-0.1.0.tar.gz`) and the release's `SHA256SUMS` asset. Only the archive of
the running platform is fetched (an x86_64 image cannot run on a Mac, and the reverse).

## How it is verified

Nothing is used before its sha256 matches a value hydo already trusts:

1. **An archive** must have the sha256 the **release index compiled into hydo** gives
   (`hydo/dist/releases.txt`, below). Every published release is added to the index after it is
   published, so every hydo knows the archives of all releases before it by sha256, and fetching
   them needs no other trust. The one archive a hydo cannot know is its own version's (the
   archive contains the program, so the program cannot contain the archive's hash). For it,
   hydo fetches the release's `SHA256SUMS` asset over TLS from the release URL and takes the
   archive's line from it; that is exactly the trust a person has who downloads the tarball by
   hand from the release page. When both are known they must agree.
2. **A file taken out of an archive** must have the sha256 it is wanted for: a guest kernel the
   sha256 the image's record names; a file restored into a release tree the line of that tree's
   own `SHA256SUMS` (which came out of the verified archive or the original tarball); a pinned
   asset or guest program also this hydo's compiled pin (M154's `pins`), checked by the
   inspection after every change.
3. **The cache is checked on use**, not trusted: a store entry is hashed again (once per process)
   before it is returned, and a wrong one is deleted and fetched again.

hydo never executes a downloaded file: archives are unpacked by Rust code inside hydo (gzip and
tar, entries outside the archive's top directory refused), the Linux guest store tree is unpacked
by `tar` from a `guest/store.tar` whose sha256 matched (M154's step), and on the Mac
`bin/hydo-hvm` is signed, not run. The program a person runs afterwards (`~/.hydo/bin/hydo`) is a
file of a verified archive.

Transport: HTTPS with rustls and the Mozilla root certificates compiled into hydo (the
`webpki-roots` crate the workspace already uses for registry pulls); redirects only to HTTPS
(GitHub serves assets from another host by redirect). A base URL with `http://` is refused unless
its host is the loopback (tests, a local mirror on the same machine), because the release's
`SHA256SUMS` is trusted only through TLS.

## No silent fallbacks

Every fetch is printed (`hydo: fetching <url> (<size>) for <why>`). A failure is an error that
says what was wanted, from where, why it failed and what to do; hydo does not fall back to a
kernel other than the one the image names, to another version's file, or to an unverified file.
`--offline` (or `HYDO_OFFLINE=1`) never opens a connection: what is not in the cache is refused
with the URL to download by hand and `hydo setup --archive <file>` to put it into the cache.

## The cache

`$HYDO_HOME`, default `~/.hydo`:

```
store/<sha256>/<name>      the content store: release archives and the guest kernels and
                           initramfs files taken out of them, one file per sha256, read-only;
                           <name> is the file's name (messages and reports show it)
releases/<version>/SHA256SUMS
                           a release's SHA256SUMS asset as fetched (the archive sha256 of a
                           version the index does not know, kept for offline use)
downloads/<sha256>.part    a download in progress (resumed with an HTTP Range request)
versions/<version>/        an unpacked, set-up release (what install.sh makes under a prefix)
bin/hydo -> ../versions/<version>/bin/hydo
                           the version `hydo setup --get` installed last
.lock                      held (flock) while anything above changes
```

- Keyed by sha256, so only what is missing is fetched, and two versions share every identical
  file: installing a version also enters its guest kernels into the store (a hard link when
  possible), so a later version finds an earlier version's kernel without the network.
- The archive stays in the store after it is unpacked: a damaged version tree, or a kernel of
  that version, is restored from it offline. Removing `store/` or a version directory is always
  safe; hydo fetches again what it then needs.
- A project's own `.hydo/` output folder (M114) is a different folder; at the home directory the
  two names do not collide (the cache uses only the names above).
- The release's `install.sh` installs into `~/.hydo` by default too (a flat tree: `bin/`,
  `guest/`, `VERSION`, `SHA256SUMS`, ...). The cache's names do not collide with a release tree's
  except `bin/hydo`: when `bin/hydo` is a program (such a flat installation) and not this cache's
  link, `--get` leaves it alone and says to run the new version as
  `versions/<version>/bin/hydo`. The Linux `install.sh` replaces its whole prefix when it
  installs again, the cache inside it included (everything there can be fetched again). Moving a
  flat installation into `versions/` is left for later.

## Offline

An existing complete installation never needs the network: `hydo setup` on a release whose files
match makes no connection (not even to ask whether something is newer), `hydo setup --get V` for
an installed and intact V makes none, and a run whose kernel is in the release, in a directory
searched before (M155) or in the store makes none. With `--offline`, a missing archive is refused
with its URL; `hydo setup --archive FILE` puts a downloaded archive into the store (verified by
the index, or by the release's `SHA256SUMS` in the same directory, which `gh release download`
and a browser download of both files give).

## How a kernel is found by sha256 (the release index)

`kernels::find` looks in the directories it looked in before (M155), then asks the source the
`hydo` command registers at its start (`kernels::set_source`): the store, then the release index
for a release of this platform that shipped a file with that sha256, then that release's archive
(store or download), from which only that file is taken. The index is `hydo/dist/releases.txt`,
compiled into hydo:

```
archive 0.1.0 linux 58aab3b1…  hydo-linux-0.1.0.tar.gz
file    0.1.0 linux 0a9ede35…  guest/bzImage
```

`archive` lines pin a release's archives; `file` lines name the guest kernels and initramfs files
inside them (paths below the archive's top directory). The running hydo's own version is in the
index implicitly, from the kernels it pins (`kernels::KNOWN`), with its archive verified as in
"How it is verified". Why an index in the repository, compiled in, and not one per release:

- no network to find the release: a per-release index would mean listing the releases through
  GitHub's API (rate-limited, a second service to trust) or guessing versions;
- the archive's sha256 is pinned in the program, which is stronger than any file fetched with it;
- it is additions only (a released line never changes, a test checks the format and that the
  current pins are in it), so every later hydo knows every earlier release.

What it cannot know: a release newer than the running hydo (an image built by a newer hydo is
refused with a message naming that hydo, as before), and a kernel built in a development tree.
Adding a release to the index is a step of docs/RELEASING.md (after publishing), so the release
tarball and `build-release.sh` do not change.

## Proxies and failures

- Proxies: the standard variables (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, lower-case too, and
  `NO_PROXY`), read by the HTTP client as for `hydo compose`'s registry pulls. A proxy that
  re-signs TLS with its own certificate authority is not supported (use `--archive`).
- Timeouts: 30 s to connect, 60 s per read; no limit on the whole download.
- Retries: up to four attempts with growing pauses (1, 2, 4 s) after a connection error, a
  broken or short body, or an HTTP 408, 429 or 5xx answer; an interrupted download resumes from
  its `.part` file with a Range request (a server that ignores the range sends the whole file
  again, which is taken from the start). Other HTTP answers (404 and the like) fail at once.
- A complete download with the wrong sha256 is refused and deleted; when it was a resumed one it
  is fetched once more from the start (the kept part may have been the bad half), otherwise not.
- At most 4 GiB per file, so a broken server cannot fill the disk.
- Two hydo processes do not fetch the same file at once (the cache lock); a fetch that stopped
  half-way leaves only its `.part` file.

## Reproducible and safe

- What a run boots is chosen by sha256 only (the image's record), so a kernel from the cache is
  the same bytes as the one the image was built with; fingerprints do not depend on where it came
  from. Nothing in this design changes an image, a fingerprint, a schedule or a recorded value.
- The cache never holds an unverified file under a sha256 name: files are written to a temporary
  name, hashed while written, and renamed into `store/<sha256>/` only when they match.
- A version tree is unpacked into a temporary directory, set up there (M154's steps: the guest
  store tree, the Mac signature, the record) and renamed into `versions/<version>` only when the
  inspection finds every file intact; the `bin/hydo` link is replaced atomically after that.
- `hydo setup --get` installs only this hydo's own version or a version the index knows, so every
  archive it installs is verified by a pinned or a TLS-fetched sha256 as above; a version the
  running hydo does not know is refused (install it with its own hydo or its tarball).

## Commands

```
hydo setup [--check | --verify] [--add-to-path] [--offline]
hydo setup --get [VERSION] [--add-to-path] [--offline]
hydo setup --archive FILE
```

- `hydo setup` (an unpacked release): as in M154; a damaged file is restored from the version's
  archive in the store, or fetched, and each restored file is named. `--check` says what setup
  would do, including "restore <n> files from <archive> (in the cache | fetched from <url>)",
  exit 3; damage it cannot restore (a file of another release, or offline without the archive)
  is exit 1 as before. `--verify` checks the files only, as before (install.sh's first step).
- `--get [VERSION]`: install VERSION (default this hydo's) into `$HYDO_HOME/versions/<VERSION>`,
  enter its kernels into the store and point `$HYDO_HOME/bin/hydo` at it; `--add-to-path` adds
  `$HYDO_HOME/bin` to the shell's PATH.
- `--archive FILE`: put a downloaded release archive into the store (verified as above).

## Left for later

- `cargo install hydo` and a Homebrew formula: a `hydo` program outside a release tree could use
  `$HYDO_HOME/versions/<its version>` as its release root (today it must be run through
  `~/.hydo/bin/hydo`, which is the release's own program).
- Smaller downloads: per-file release assets named by sha256 would let a hydo fetch one kernel
  without the whole archive; the store and the index already key everything by sha256.
- Pruning the cache (`store/` entries and versions no image needs).
- A signature over the release's `SHA256SUMS` would make a hydo's own version as strongly pinned
  as the earlier ones.
