# What hydo does not do

These are the limits of hydo 0.3.2. The [README](../README.md) shows what it does.

- A run gives the same fingerprint again when four things are the same:
  - the engine;
  - the image;
  - the CPU model (for the KVM engine on AMD, the EPYC models measured share their
    fingerprints);
  - the fingerprint epoch, a number that hydo raises when a new version changes fingerprints.
    `hydo repro` says when the epoch differs.
- Across machines, the recorded examples gave the same verdicts. The Redis example gave the same
  verdict on Linux and on a Mac, and on the KVM engine on Intel CPUs. The quickstarts record these
  runs (Linux sections 2 and 6, Mac section 2).
- A failure found on Linux does not replay on a Mac. Your services run as x86_64 Linux programs
  on a Linux host, and as arm64 Linux programs on a Mac.
- The kernel module needs an Intel CPU with VT-x, Intel's hardware support for virtual machines.
  The KVM engine runs on Intel and AMD CPUs.
- A guest has at most 8 GiB of RAM, on every engine.
- Time inside the virtual machine is virtual, so hydo does not measure performance.
- Anyone who can open `/dev/hvx` can run guests through the kernel module, which runs with the
  rights of the kernel. Give that access only to people you would give sudo.
- With Secure Boot on, Linux loads only signed modules. Sign `hvx.ko` with a key that you enrolled
  as a machine owner key (MOK), or use the KVM engine.

