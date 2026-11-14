# Testing your own project

This page takes you from an empty repository to your first exploration. Read the quickstart for
your platform first ([Linux](../hydo/dist/linux/QUICKSTART-linux.md),
[Mac](../hydo/dist/mac/QUICKSTART-mac.md)): its Redis example shows one run from start to end.

## The words hydo uses

- Guest: the Linux system that runs inside the virtual machine, with your services in it.
- Engine: the part of hydo that runs the guest. Linux has two engines: hydo's own kernel module
  `hvx.ko` and the KVM engine. The Mac has one.
- Image: what `hydo build` makes from your project's description file, `hydo.yaml`. On Linux it is one disk; on a
  Mac it is a root file system loaded into memory plus a data disk. It holds your services and
  the commands that drive them.
- Workload: the commands that act as your system's client during a run. A workload is split into
  numbered steps, and a step is one part of it, for example one write.
- Invariant: a property that must hold after every run, for example that every acknowledged write
  is still there. A command that you write checks it, or one of hydo's ready-made checks.
- Schedule: one line of text that names the faults of one run, in the form
  `<host tokens> | run <guest tokens>`. Host tokens are faults of the whole machine, such as
  `disk=cut` (the disk keeps only data that was flushed when the power is cut) or `disk=none` (no
  disk faults). Guest tokens place faults before a step, and change settings of your services.
  `hydo guide faults` lists the tokens.
- Verdict: the outcome of a run: `OK`, `VIOLATION` (an invariant failed), or `ERROR` (the run
  could not finish its workload).
- Seed: a number that starts hydo's pseudo-random choices. The same seed gives the same schedules.
- Fingerprint: a hash of the virtual machine's whole final state.

## The steps

1. Run `hydo init` in your repository. It creates `hydo/hydo.yaml`, `hydo/workload/`,
   `hydo/checks/`, and `hydo/regressions/`. It adds `.hydo/` (hydo's build cache),
   `hydo/images/`, and `hydo-out/` to `.gitignore`, and a hydo section to `AGENTS.md`.
2. If your system has a Docker Compose file, import it instead of the skeleton's `hydo.yaml`. The
   second command downloads the images, once:

   ```sh
   hydo compose docker-compose.yaml --out hydo/hydo.yaml --images-dir images
   sh hydo/fetch-images.sh
   ```

   When Docker runs, the first command also builds the image of each service with `build:`, and
   pins it by its digest. `hydo guide workflow` describes what happens without Docker.

   The import writes a numbered-steps `workload.run` and a `TODO` invariant as placeholders.
   Delete both from `hydo/hydo.yaml`, but keep `workload.steps` and `workload.timeout`: the
   commands in `hydo/workload/` are the workload, and the checks in `hydo/checks/` are the
   invariants. hydo's test composer runs those commands in seeded sequences and concurrent
   mixes. Until you delete `workload.run`, the check of step 5 fails with `workload.run/process:
   the test composer runs the workload`. Until you delete the `TODO` invariant, every run ends
   `ERROR invariant todo.acked_operations_survive could not be evaluated`.

3. Replace `hydo init`'s two placeholder files, `hydo/workload/parallel_driver_todo` and
   `hydo/checks/final_todo`. Write your own commands and checks in their place. Until you do,
   every run fails on purpose.
4. Read `hydo guide workflow`. It says what to put in each file.
5. Run `hydo build hydo/hydo.yaml --check` after each change. It checks the folder without
   building.
6. Build and run without faults, twice. Both runs must end `OK` with the same `fp=`:

   ```sh
   hydo build hydo/hydo.yaml
   hydo run "disk=none | run"
   hydo run "disk=none | run"
   ```

7. Explore, then read the findings. `--strategy process` draws faults as a process over the
   run, a series of faults that start and heal at seeded times; `hydo guide faults` lists the
   strategies:

   ```sh
   hydo explore --seeds 1-20 --strategy process
   hydo report
   ```

An AI coding tool can write the folder for you with the onboarding skill. In Claude Code, run
`/plugin marketplace add kakoc/hydo`, then `/plugin install hydo-onboard@hydo`. For Codex, Cursor,
GitHub Copilot, or Gemini, run `hydo skill install --agent codex` (or `cursor`, `copilot`,
`gemini`). Any other tool can read the skill from `hydo guide skill`.


## Where to read more

- `hydo guide manifest` explains every key of `hydo.yaml`.
- `hydo guide sdks` describes the assertion SDKs for Rust, Go, Python, and JavaScript. An
  assertion is a check that your program makes about itself while it runs, and hydo collects
  them.
- The repository has 26 example projects in [hydo/onboard/examples/](../hydo/onboard/examples/).
