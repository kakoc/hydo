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
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.1/hydo-mac-0.3.1.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.1/SHA256SUMS
shasum -a 256 -c --ignore-missing SHA256SUMS
tar xzf hydo-mac-0.3.1.tar.gz
./hydo-mac-0.3.1/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
```

## Linux

Linux has two engines. Use the KVM engine unless you need one of three things: several virtual
CPUs, a power cut at an exact disk flush, or inserted timer ticks. Inserted timer ticks are extra
timer interrupts that hydo puts into the guest at chosen points, to change the order in which its
threads run. Those three need the kernel module on your own Intel machine.

- The KVM engine uses the KVM that comes with Linux. It runs on Intel and AMD CPUs, and on a cloud
  VM with nested virtualization (a VM that can run VMs itself). It has one virtual CPU.
- The kernel module `hvx.ko` needs an Intel CPU with VT-x (Intel's hardware support for virtual
  machines) and sudo. It has the PMU branch clock:
  the CPU's performance-monitoring unit counts the guest's conditional branches, so a timer tick
  lands at an exact instruction.

Download and check the release for both engines:

```sh
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.1/hydo-linux-0.3.1.tar.gz
curl -LO https://github.com/kakoc/hydo/releases/download/v0.3.1/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar xzf hydo-linux-0.3.1.tar.gz
```

For the KVM engine, your user must be able to open `/dev/kvm`. Install, then let
`hydo doctor --backend kvm` check it:

```sh
./hydo-linux-0.3.1/install.sh --no-module
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
./hydo-linux-0.3.1/install.sh
export PATH="$HOME/.hydo/bin:$PATH"
hydo doctor
```

## With cargo, on a Mac

```sh
cargo install hydo --locked
hydo setup
hydo doctor
```

`cargo install` builds only the `hydo` command. `hydo setup` downloads the release for your
platform into `~/.hydo/versions/0.3.1` (`HYDO_HOME` names another folder than `~/.hydo`) and
checks every file by sha256. On Linux, this path was
only compiled, not run; use the tarball there.

