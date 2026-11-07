# hydo

hydo runs your software in a deterministic virtual machine and injects faults: process kills,
pauses, network faults, power cuts, clock jumps, disk errors and a disk that loses acknowledged
writes. Every run is deterministic, so each failure it finds comes back as a short schedule
string that replays bit for bit.

It runs on an Apple Silicon Mac (Hypervisor.framework, no root) and on x86_64 Linux (its own
VT-x kernel module on bare metal, or stock KVM).

## Install

```sh
cargo install hydo
hydo setup
```

`cargo install hydo` builds the `hydo` command. The rest of a hydo installation is not Rust
code and is not built by cargo: the guest kernels, the guest tools and programs, and the run
engine as its own program (`hydo-hvm` on a Mac, which must carry the hypervisor entitlement;
`hydo-hvx` and `hydo-kvm` on Linux). `hydo setup` (for a `hydo` installed by cargo the same as
`hydo setup --get`) downloads them once from the GitHub release of the same version, checks every file by sha256, signs the Mac engine on your Mac,
and keeps them in `~/.hydo/versions/<version>` (`$HYDO_HOME` names another place). The installed
`hydo` uses that directory as its installation. Nothing is downloaded again on later runs, and
`hydo setup` on a complete installation does nothing.

Then check the machine:

```sh
hydo doctor
```

`hydo guide`, `hydo init` and `hydo skill install` work right after `cargo install`, before
anything is downloaded: the onboarding guide is built into the program.

## First run

```sh
cp -R ~/.hydo/versions/0.2.0/examples/redis ~/hydo-redis && cd ~/hydo-redis
hydo build hydo.yaml
hydo run "disk=none | run"
hydo run "disk=cut | run C=12 R=everysec"
hydo explore --seeds 1-20 --strategy swarm
hydo report
```

The first run passes; the second shows Redis's documented `everysec` window (an acknowledged
key lost at a power cut), and running it again gives the same fingerprint (`fp=`).

## More

The project's repository has the manuals: the project format (`hydo.yaml`), the faults, the
schedule language, the assertion SDKs and the onboarding guide for AI tools
(`hydo guide`). The tarball releases on the same GitHub page are the other way to install: one
archive per platform with everything in it.

License: Apache-2.0 (this crate and the other hydo crates). The Linux kernel module that a
Linux installation can build is GPL-2.0 and is not part of this crate.
