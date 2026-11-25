# Installing hydo


Each install gives you the `hydo` command. Then `hydo doctor` checks the machine and the
installation. The quickstarts take you from the install to your first exploration:
[Linux](../hydo/dist/linux/QUICKSTART-linux.md) and [Mac](../hydo/dist/mac/QUICKSTART-mac.md).

## macOS

hydo needs an Apple Silicon Mac with macOS 13 or later. With Homebrew:

```sh
brew install kakoc/tap/hydo
hydo doctor
```

Without Homebrew:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.6.0/hydo-mac-0.6.0.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.6.0/SHA256SUMS
shasum -a 256 -c --ignore-missing SHA256SUMS
tar xzf hydo-mac-0.6.0.tar.gz
./hydo-mac-0.6.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

## Linux

Linux has two engines. An engine is the program that runs hydo's virtual machine, and the guest
is the Linux system inside it, with your services. Use the KVM engine unless you need one of two
things: a power cut at an exact disk flush, or inserted timer ticks. Inserted timer ticks are
extra timer interrupts that hydo puts into the guest at chosen points, to change the order in
which its threads run. Those two need the kernel module on your own Intel machine. Both engines
run several virtual CPUs.

- The KVM engine uses the KVM that comes with Linux. It runs on Intel and AMD CPUs, and on a cloud
  VM with nested virtualization (a VM that can run VMs itself). hydo's release checks run it on
  GitHub's hosted runners, which are such VMs. It has one virtual CPU, or 2 to 8
  with `--vcpus N`. Several vCPUs take turns. One runs until it has made a fixed number of VM
  exits, then the next one runs. A VM exit is a moment when the guest stops and the engine
  handles something for it.
- The kernel module `hvx.ko` needs an Intel CPU with VT-x (Intel's hardware support for virtual
  machines) and sudo. It has the PMU branch clock:
  the CPU's performance-monitoring unit counts the guest's conditional branches, so a timer tick
  lands at an exact instruction.

Download and check the release for both engines:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.6.0/hydo-linux-0.6.0.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.6.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar xzf hydo-linux-0.6.0.tar.gz
```

For the KVM engine, your user must be able to open `/dev/kvm`. Install, then let
`hydo doctor --backend kvm` check it (without `--backend kvm`, `hydo doctor` checks the kernel
module's engine):

```sh
./hydo-linux-0.6.0/install.sh --no-module
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor --backend kvm
```

Only if it says that `/dev/kvm` is not usable, add yourself to the group that owns it, often
`kvm`: `sudo usermod -aG kvm "$USER"`, then log in again.

On a machine without the kernel module (no `/dev/hvx`), `hydo run`, `hydo repro`, and
`hydo explore` use the KVM engine by themselves. On a machine that has both engines, add
`--backend kvm` to these commands, or run `export HYDO_BACKEND=kvm` and add that line to your
shell startup file (`~/.bashrc` or `~/.zshrc`) to keep it.

For the kernel module, the machine also needs a C compiler, `make`, `xz`, `flock`, and the running
kernel's headers. Do not install it while other virtual machines run under KVM: loading `hvx.ko`
unloads the kernel modules `kvm_intel` and `kvm`.

```sh
./hydo-linux-0.6.0/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor
```

## With cargo

This works on a Mac and on Linux. You need Rust 1.90 or later. On Linux, run the doctor with `--backend kvm`:

```sh
cargo install hydo --locked
hydo setup
hydo doctor                  # on Linux: hydo doctor --backend kvm
```

`cargo install` builds only the `hydo` command. `hydo setup` downloads the release for your
platform into `~/.hydo/versions/0.6.0` (`HYDO_HOME` names another folder than `~/.hydo`) and
checks every file by sha256. The examples are in `~/.hydo/versions/0.6.0/examples`.

On Linux this gives the KVM engine, and your user must be able to open `/dev/kvm`, as for the
tarball. For hydo's kernel module `hvx.ko`, use the tarball.


## Upgrading

From 0.4.1 on, one command updates hydo and every copy of its onboarding skill:

```sh
hydo update --check          # what is newer; changes nothing (exit status 3: something is newer)
hydo update --yes            # update hydo the way it was installed, and the skill
```

`hydo update` reads how hydo was installed. It upgrades a Homebrew install with Homebrew and a
cargo install with cargo. For a release archive, it downloads only this platform's archive and
the release's `SHA256SUMS`, checks the sha256, and runs the new `install.sh`. It also updates the
skill in Claude Code's plugin and in every folder where hydo installed it. `hydo guide update`
says what it changes and what it reads.

From 0.5.0 on, the skill has hydo's version and comes in hydo's release, as the asset
`hydo-skill-<version>.tar.gz`. The skill's older versions 1.0.0 to 2.4.0 count as older than
any hydo version. hydo 0.4.1 reads the skill from those old skill releases only, so after `hydo
update` from 0.4.1, run `hydo update` once more with the new hydo to update the skill.

`hydo update` reads the list of releases from GitHub's API. Without a login, GitHub allows 60
requests an hour from one address. When `GH_TOKEN` is set, or `gh auth login` was done, hydo
sends that token to GitHub's API, and the limit does not apply. When the limit is reached, `hydo
update` says so and when it resets, and exits with status 4.

hydo 0.4.0 and older have no `hydo update`. Upgrade such a hydo once by hand, the way you
installed it:

- Homebrew: `brew update && brew upgrade kakoc/tap/hydo`.
- cargo: `cargo install hydo --locked`, then `hydo setup`.
- The release archive: download your platform's archive and `SHA256SUMS`, check the archive,
  unpack it, and run `install.sh`. The preceding sections show these commands for 0.6.0.
  You do not need the large `-sources.tar.gz` archives, which hold the third-party sources.

Then update the skill: run `hydo skill update` for its copies. For Claude Code's plugin, run
`claude plugin marketplace update hydo` and then `claude plugin update hydo@hydo`.

Read the "Before you upgrade" section of the release notes of each release after yours first.
From 0.3.4 or older, images built by 0.4.0 or newer get new fingerprints, so build your images
again after the upgrade. From 0.4.0 to 0.4.1, an image you built keeps its fingerprints. Only an
image with fakes, proxies or generated gRPC drivers gets new bytes when you build it again. From
0.4.1 or older to 0.5.0, an image you built keeps its fingerprints until you build it again. An
image that 0.5.0 builds gets new bytes and new fingerprints, and on hvx its guest's clock follows
the virtual TSC. From 0.5.0 to 0.6.0 the same holds. An image you built keeps its fingerprints,
and an image that 0.6.0 builds gets new bytes and new fingerprints. After a power cut, the recovery
boot of such an image gets the first boot's outcomes, and every command that hydo starts in its
guest has a `PATH`.
