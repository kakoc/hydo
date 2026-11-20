# Installing or upgrading hydo

Install only after the user said yes. Say first what you will run, what it changes (a Homebrew
package, or files under `~/.hydo` and a line in the shell startup file), and that the Linux
installer asks for `sudo` to load a kernel module.

## Apple Silicon Mac (macOS 13 or later)

With Homebrew:

```sh
brew install kakoc/tap/hydo        # upgrade later with: brew upgrade hydo
hydo doctor
```

Without Homebrew: download `hydo-mac-<version>.tar.gz` from https://github.com/kakoc/hydo/releases,
then:

```sh
tar xzf hydo-mac-<version>.tar.gz
./hydo-mac-<version>/install.sh --add-to-path   # installs into ~/.hydo
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor
```

## x86_64 Linux

Download `hydo-linux-<version>.tar.gz` from https://github.com/kakoc/hydo/releases, then:

```sh
tar xzf hydo-linux-<version>.tar.gz
./hydo-linux-<version>/install.sh --add-to-path   # installs into ~/.hydo; asks for sudo
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor
```

The installer builds and loads hydo's kernel module, which needs the running kernel's headers, a
C compiler and `make`. The release's `README.md` (its quickstart) explains `--no-module`, running
without `sudo`, and what `hydo doctor` reports. If `hydo doctor` reports a failure, show it to the
user; do not change the machine's kernel modules or groups without asking.

## With cargo (a Mac, or Linux with KVM)

```sh
cargo install hydo --locked        # installs into ~/.cargo/bin
hydo setup                         # downloads the guest kernel and tools
hydo doctor
```

## Upgrading

From the hydo release after 0.4.0 on, one command updates hydo, the way it was installed, and
every copy of this skill. Ask the user first, saying what `hydo update --check` printed:

```sh
hydo update --check     # what is newer; changes nothing (exit status 3: something is newer)
hydo update --yes       # update hydo and the skill
```

From hydo 0.5.0 on, this skill has hydo's version and comes in hydo's release. Its older
versions 1.0.0 to 2.4.0 count as older than any hydo version. hydo 0.4.1 updates only hydo, so
after its `hydo update`, run `hydo update --check` once more and offer the skill's update.

hydo 0.4.0 and older have no `hydo update` (they print the usage text). Upgrade such a hydo once
by hand, the same way it was installed:

- Homebrew: `brew update && brew upgrade kakoc/tap/hydo`.
- cargo: `cargo install hydo --locked`, then `hydo setup`.
- The release archive: download only this platform's `hydo-<mac|linux>-<version>.tar.gz` and
  `SHA256SUMS` from https://github.com/kakoc/hydo/releases (not the `-sources.tar.gz` archives,
  which hold third-party sources and are large), check the archive (`shasum -a 256 -c
  --ignore-missing SHA256SUMS` on a Mac, `sha256sum -c --ignore-missing SHA256SUMS` on Linux),
  unpack it, and run its `install.sh`. On Linux add `--no-module` unless the user uses the
  kernel module.

Then update the skill: `hydo skill update` for its copies, and for Claude Code's plugin `claude
plugin marketplace update hydo` and `claude plugin update hydo@hydo`. Run `hydo --version` and
`hydo doctor` again. Images built by an older hydo keep working; the release notes' "Before you
upgrade" section says when images get new fingerprints and should be built again.
