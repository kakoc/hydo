# What works where

hydo runs a guest on one of three engines. Check this table before using a feature:

- `hydo build --check` validates the schema only.
- The full `hydo build` refuses a feature the architecture lacks.
- A run refuses a schedule token its engine cannot apply (`UNSUPPORTED on kvm: host token cf=20
  needs hvx`), and a token that names a service, link, fake, or knob the image does not have.

State of hydo 0.3.3 (2026-11-14).

- **hvx** (the Linux release, `hydo-linux-<version>`): x86_64 guests on an Intel VT-x machine,
  through hydo's kernel module (`hydo module install`). The reference engine: every feature.
- **kvm** (`--backend kvm`, or `HYDO_BACKEND=kvm`; in the Linux release since 0.2.0): the same x86_64 images on
  stock Linux KVM (`/dev/kvm`, no module, no root; also in a VM with nested virtualization). Guest
  time is counted in VM exits, as on the Mac. It needs the KVM build of the image's guest kernel
  (`bzImage-kvm`; `hydo doctor --backend kvm` says whether it is there). Its fingerprints are its
  own: `hydo repro` across engines compares verdicts.
- **hvm** (the Mac release, `hydo-mac-<version>`): arm64 guests on an Apple Silicon Mac, through
  Hypervisor.framework. The arm64 build (`hydo-build --arch arm64`, which the Mac release runs)
  supports every manifest feature except `guest.cpu: v3`. `network: bridge`, `addr:` and `faults:
  [freeze]` build the image for a second Mac guest kernel, `Image-explore-netns`, which the image
  records and hydo boots by itself.
- **hydo's repository on x86_64 Linux**: the `hydo-build` program (`cargo build --release -p
  hydo-build`) with the helper programs built from source (`hydo/crates/hydo-world/build.sh` for
  `hydo-world`, `hydo/crates/hydo-read/build.sh` for `hydo-read`, `hydo/onboard/hydo-grpc-gen` for
  the gRPC generator and its static client) and the pinned tools of `docs/BUILD-PINS.md`.

Every image's first process is the guest agent `hydo-agent`, driven by `hydo` itself; a release
is Rust programs only and needs no Python.

## The project format

| feature | hvx (Linux release) | kvm | hvm (Mac release) |
|---|---|---|---|
| services, `root.files`, data disk, `knobs:`, `provision:` | yes | yes | yes |
| `hydo/` folder: commands, checks `final_`/`always_`/`eventually_`, regressions | yes | yes | yes |
| `eventually` liveness checks | yes | yes | yes |
| Docker/OCI images (`image:`), `hydo compose` | yes | yes | yes (a big image root needs a bigger `guest.ram_mib`: the root is in guest RAM) |
| `guest.ram_mib` | 256–8192 | 256–8192 | 256–8192 |
| `guest.clock_after_cut`, `guest.outcomes_after_cut` | yes | yes | yes |
| `services[].start_after`, `services[].logs`, `root.fix_pyc` | yes (not run on hvx yet) | yes (not run on kvm yet) | yes |
| `guest.cpu: v3` (AVX/AVX2) | yes | yes (on a host with AVX2; `hydo doctor --backend kvm` says) | no (arm64) |
| `guest.cpu_cache` (the CPUID cache leaves; on by default for images built at fingerprint epoch 2) | yes (a module from Milestone 203 on) | yes | ignored (arm64) |
| `params:` (build variants, `hydo build --set k=v`) | yes | yes | yes |
| `addr:` and link faults between services | yes | yes | yes (second guest kernel) |
| `network: bridge` (a network namespace per service) | yes | yes | yes (second guest kernel) |
| network and one-way faults on a port | yes | yes | yes |
| `faults: [freeze]` (`F=`, freeze sweeps) | yes | not run on kvm yet | yes (`F=` and `--freeze-call`; freeze sweeps not yet run on the Mac) |
| `checks:` / `linearizable_` / `list_append_` history checks | yes | yes | yes |
| `buggify: true` | yes | yes | yes |
| `hydo-read`, `hydo-check` | yes (`guest/hydo-read`) | yes | yes (`guest/hydo-read`) |
| `fakes:`, `proxies:`, `H=` protocol faults, `hydo-world` checks | yes | yes | yes |
| `errors:` error logs as findings, `hydo errors`, `hydo/known-errors.yaml` | yes | yes | yes (the arm64 build with `errors:` was not run) |
| `hydo-grpc-gen` generated drivers | yes (`bin/hydo-grpc-gen`, x86_64 client) | yes (the same images) | not shipped (generate with `--arch amd64,arm64` in the repository, copy `hydo-grpc.arm64`) |

## Schedules and exploring

| feature | hvx | kvm | hvm |
|---|---|---|---|
| guest tokens `K= P= X= N= O= C= T= F= H= A= W= B=` and knobs | yes | yes | yes |
| storage faults of one service `G=`, `L=` (services with `storage:`; `L=` needs `size_mib:`) | yes | yes | yes |
| disk faults `disk=cut`, `disk=ioerr:<p>`, `disk=lying:<p>` | yes | yes | yes |
| a power cut at the k-th disk FLUSH (`cf=`), `--more-faults` (it draws `cf=`) | yes | no | no |
| several vCPUs (`--vcpus N`, `--turns`, `--sweep turns`) | yes (turns of guest branches) | yes (turns of VM exits, the SMP guest kernel `bzImage-kvm-smp`) | no |
| tick jitter (`--timing`, `tj=`) | yes (branch-counted ticks) | yes (counted in VM exits) | yes (counted in VM exits) |
| forced preemption (`--preempt`) | yes (`pp=`, `xt=`) | yes (`pct=`, counted in VM exits) | yes (`pct=`, counted in VM exits) |
| pauses of the workload (`P=workload@<at>~<off>:<dur>`, `--pause-workload`) | yes | yes (not run on kvm yet) | yes |
| pauses of threads of the workload (`P=workload/<thread>@<at>~<off>:<dur>`, `--pause-thread`) | yes | no (the guest kernel has no cgroups; refused before any VM) | no (the arm64 guest kernels have no cgroups; refused before any VM) |
| code coverage (Intel Processor Trace) | yes | no | no |
| a counter loop with no VM exits (`while (flag) n++`) | interrupted by the PMU clock | resolved in the rewind mode (`spin=rewind`, automatic) | resolved in the rewind mode (automatic) |

Where a feature is missing, say so in the report and offer the closest substitute:

- a shell read-back loop instead of `hydo-check` (`hydo guide invariant-patterns`);
- a hand-written driver instead of a generated one;
- a port fault instead of a link fault;
- `--strategy process` without `--more-faults` on kvm and the Mac.
