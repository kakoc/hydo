<p align="center">
  <img src="docs/img/hero.svg" width="860" alt="An animated terminal in a repository with the Redis example. hydo onboard finds its hydo/ folder and installs the Claude Code skill, whose command is /hydo:onboard. hydo explore runs Redis 30 times with different faults: 13 runs fail, in 3 different ways, and each failing run fails again the same way when hydo replays it. hydo report lists the 3 problems, each with a label, how many runs show it, and the command that shows it again: Redis does not start again after disk errors, Redis loses confirmed data after a power cut with appendfsync set to no, and Redis loses confirmed data with a faulty disk.">
</p>

<h1 align="center">hydo</h1>

> What if the bug that shows up once in a thousand CI runs showed up every time?<br>
> What if the bugs that nobody wrote a test for turned up on their own?<br>
> What if "never charge a customer twice" were checked while your database crashes?

hydo runs your whole system in one virtual machine: your services, their databases, and a
client that uses them. Then it breaks things. It kills processes, pauses them, cuts the network,
pulls the power, makes the disk fail, and moves the clock. After each run it checks the rules
that your system promises, for example that every confirmed payment is still there.

Nothing inside that machine is left to chance, so a run repeats bit for bit. When a rule breaks,
hydo takes away faults until only the ones the failure needs are left. You get one command that
makes the same failure happen again, tomorrow or on a colleague's laptop.

- **It looks for bugs on its own.** `hydo explore` chooses the faults itself, runs them, and
  checks your rules after each run. It found the etcd and NATS bugs below without being told what
  to look for.
- **Every failure comes back.** hydo writes the faults of each run as one line of text. `hydo run`
  with that line repeats the run, and the failure with it.
- **The rules can be your business rules.** A rule is a command you write, such as a SQL query
  over your tables. Two such queries found the two Blnk bugs below.
- **Your software runs unmodified:** Linux programs, plain files, or Docker images. A Docker
  Compose file imports with one command.

## Install

On a Mac, with Homebrew:

```sh
brew install kakoc/tap/hydo
```

On a Mac or on Linux, with cargo:

```sh
cargo install hydo --locked
hydo setup
```

`hydo setup` downloads the Linux kernel and the tools that the virtual machine boots, and checks
each file. hydo needs an Apple Silicon Mac, or an x86_64 Linux machine where your user can use
KVM, the support for virtual machines that Linux has built in. Then `hydo doctor` checks the
machine.
[docs/INSTALL.md](docs/INSTALL.md) has the release archives and their checksums, and hydo's own
kernel module for Intel machines.

## What hydo found in real projects

Each example folder holds the project's hydo setup and the recorded runs.

- **etcd 3.5.5.** A member that crashes right after it defragments its database comes back on a
  different revision than the other members. Known as
  [etcd#14685](https://github.com/etcd-io/etcd/issues/14685), fixed in 3.5.6
  ([the example](hydo/onboard/examples/etcd/)).
- **NATS 2.10.22.** A power cut, or a few crashes and pauses, delete a whole JetStream stream for
  good. Known as [nats-server#6888](https://github.com/nats-io/nats-server/issues/6888), fixed in
  2.10.23 ([the example](hydo/onboard/examples/nats-jetstream/)).
- **Qdrant.** A flaky test that never failed in 2,604 ordinary runs fails on every run when hydo
  pauses one of its threads at one moment
  ([qdrant#10200](https://github.com/qdrant/qdrant/issues/10200#issuecomment-6803314544),
  [the example](hydo/onboard/examples/qdrant-flaky-10200/)).
- **TigerBeetle.** A TigerBeetle server stops for good when two pending transfers expire in the same
  nanosecond ([tigerbeetle#3960](https://github.com/tigerbeetle/tigerbeetle/issues/3960)). hydo
  makes it happen on every run, on the releases 0.16.29 to 0.17.9
  ([the example](hydo/onboard/examples/tigerbeetle/)).
- **Formance Ledger v2.4.** Two transactions that use a new account at the same moment, in two
  currencies, race, and one fails with `500`. A new bug, reported as
  [ledger#2542](https://github.com/formancehq/ledger/issues/2542)
  ([the example](hydo/onboard/examples/formance-ledger/)).
- **Blnk.** Two new bugs. Payments sent at the same time go past their overdraft limit
  ([blnk#437](https://github.com/blnkfinance/blnk/issues/437)). An all-or-nothing batch of
  transfers keeps its first transfer when a later one fails
  ([blnk#438](https://github.com/blnkfinance/blnk/issues/438),
  [the example](hydo/onboard/examples/blnk/)).
- **Lago.** A subscription cancelled while Redis is down is never invoiced, a known bug
  ([lago-api#6901](https://github.com/getlago/lago-api/issues/6901)). hydo also found a
  subscription invoiced twice for one month, a new bug
  ([lago-api#7164](https://github.com/getlago/lago-api/issues/7164),
  [the example](hydo/onboard/examples/lago/)).

## Your own project

In your repository, run:

```sh
hydo onboard
```

`hydo onboard` looks for a Docker Compose file and starts a `hydo/` folder from it, or an empty
one when there is none. It installs the onboarding skill, the instructions that teach an AI coding
tool to set up hydo, for the tools it finds, such as Claude Code. Then it prints the next step. In Claude Code, that step is `/hydo:onboard`. The skill asks about your system and writes
the client and the rules with you. Then `hydo explore` looks for failures, and `hydo report`
lists each one with the command that replays it.

To install the skill as a Claude Code plugin without `hydo onboard`, type these in Claude Code:

```text
/plugin marketplace add kakoc/hydo
/plugin install hydo@hydo
/hydo:onboard
```

[docs/ONBOARD.md](docs/ONBOARD.md) describes the onboarding, and
[docs/YOUR-PROJECT.md](docs/YOUR-PROJECT.md) shows how to write the `hydo/` folder by hand.

## Learn more

- [The Linux quickstart](hydo/dist/linux/QUICKSTART-linux.md) and
  [the Mac quickstart](hydo/dist/mac/QUICKSTART-mac.md) take you from the install to a first
  exploration of a Redis example.
- `hydo guide`, the manual built into the command. Start with `hydo guide workflow`.
- [The example projects](hydo/onboard/examples/) in this repository.
- [What hydo does not do](docs/LIMITS.md).
- [How hydo is built](docs/ARCHITECTURE.md) and [how to contribute](CONTRIBUTING.md).

The current release is hydo 0.3.2. It is experimental: formats and commands can change between
versions.

## License

Apache-2.0 ([LICENSE](LICENSE)). The kernel module [hv/hvx/hvx.c](hv/hvx/hvx.c) is GPL-2.0-only;
[hv/hvx/LICENSE.md](hv/hvx/LICENSE.md) has the details.
