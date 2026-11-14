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

## Later

A `cargo install hydo` is planned; until it exists, use the steps above.

## Upgrading

Upgrade the same way you installed (`brew upgrade hydo`, or the newer tarball's `install.sh`),
then run `hydo --version` and `hydo doctor` again. Images built by an older hydo keep working.
