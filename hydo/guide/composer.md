# The test composer: commands in hydo/workload/

The full reference is the section "The test composer" of `hydo guide manifest`. This page is the
part you need to write commands.

## How a run uses the commands

The schedule token `W=<seed>,p..,q..,r..` decides the composition. For each of `workload.steps`
rounds, the composer starts either one serial driver (1 round in 4 when there are parallel
drivers) or 1..`per_round` parallel drivers drawn with repetition, plus an anytime command in 1
round of 3. A round starts every `round_ms` of guest time. A command keeps running across later
rounds and fault points until it ends, so faults land in the middle of commands. At most
`parallel` commands run at once. After the last round the composer waits `drain_timeout` seconds
for running commands, kills the rest, heals the faults, runs the `eventually_` commands, the
`finally_` commands and the checks, and prints one verdict. A power cut freezes everything; the
recovery boot runs the `eventually_` commands and the checks again.

The explorer draws `W=` for every schedule and shrinks it (one command never run, fewer commands,
no concurrency, fewer entries), so a failure ends as a small composition, written in the
resolved form that names commands by id (see Regressions). A schedule written by hand may
leave `W=` out (seed 0 with the manifest's defaults).

## Command kinds

| file name | runs | faults during it | alongside |
|---|---|---|---|
| `first_<name>` | once each, in name order, during provisioning (inside the snapshot every run starts from) | none | nothing |
| `parallel_driver_<name>` | repeatedly, several at once (also copies of itself) | yes | other parallel drivers, anytime |
| `serial_driver_<name>` | repeatedly, never two at once, only when no other driver runs | yes | anytime only |
| `singleton_driver_<name>` | once, in a run with no other driver (1 run in 4 when there are other drivers) | yes | anytime only |
| `anytime_<name>` | in rounds, at any time | yes | anything |
| `eventually_<name>` | once each after the faults stop (and in the recovery boot after a power cut) | none | nothing |
| `finally_<name>` | at the end, only if every started command ran to completion | none | nothing |
| `helper_<anything>` | never: shared code that commands source or call | | |

Dot files are ignored. A directory is allowed only as `helper_*`. Any other prefix is an error,
so a typo does not silently do nothing.

## Rules for a command file

- Executable (`chmod +x`), with a `#!` line. The whole `hydo/workload/` directory is copied to
  `/hydo/workload/` on the guest's read-only root, so a command sources a helper as
  `. /hydo/workload/helper_lib.sh`.
- Optional header lines in the first 4 KiB: `# hydo: timeout=30` (seconds; else
  `test.command_timeout`).
- Exit status of a driver or anytime command: `0` = the operation succeeded; `100` = the command
  saw a violation (the run fails, through the `compose.commands` invariant and the property
  `compose.<id>.no_violation`); anything else = the operation failed (recorded, not fatal).
  A `first_` command that fails fails provisioning. An `eventually_` or `finally_` command is an
  invariant: 0 holds, anything else is a violation.
- Output goes to `/tmp/hydo/compose/log.<entry>`; `HYDO-ASSERT` lines it prints reach the
  run's report when the command's log is collected. A reaction (`A=`) sees only records written to
  the console, as the SDKs write them: `printf 'HYDO-ASSERT {...}\n' > /dev/console`.

## Environment of a command

| variable | meaning |
|---|---|
| `HYDO_CMD` | the command's id (`parallel_driver_transfer`) |
| `HYDO_CMD_ID` | the entry number, unique in a run: a good operation id |
| `HYDO_ROUND` | the round it started in |
| `HYDO_SEED` | a number from the composition's random stream, for the command's own choices |
| `HYDO_ACKED` | file: append one id per line for every operation the client saw acknowledged (carried across a power cut) |
| `HYDO_OUTCOMES` | file: append `<id> ack\|failed\|unknown` per attempt (the last line per id wins); carried across a power cut only with `guest.outcomes_after_cut: true` (then the recovery boot's file holds the last outcome of every id); without it the file is empty there, so a check that looks ids up in it checks nothing after a power cut |
| `HYDO_FAILED` | file (checks only): ids whose last outcome was `failed` |
| the manifest's `env:` | exported to every command; `shell:` helpers are sourced too |

## A command, end to end

```sh
#!/bin/sh
# hydo: timeout=20
# SET k:<id> v<id> and record what the client saw
. /hydo/workload/helper_lib.sh          # R() { timeout 2 /bin/redis-cli "$@" 2>/dev/null; }
id=$HYDO_CMD_ID
if [ "$(R SET k:$id v$id)" = OK ]; then
  echo $id >> $HYDO_ACKED; echo "$id ack" >> $HYDO_OUTCOMES
else
  echo "$id unknown" >> $HYDO_OUTCOMES; exit 1     # no answer: maybe applied, maybe not
fi
```

More, in a complete folder (print each file with `hydo guide examples/example-redis-composed/hydo/<file>`;
`hydo guide examples/example-redis-composed/README.md` lists them): SETs, transfers between
accounts, an AOF rewrite as a serial driver, a read as an anytime command, seed accounts as a
`first_` command, a saved regression.

## Options in hydo.yaml

```yaml
format: 1                    # the project format (a newer one is refused; hydo guide schema)
hydo_version: "0.3"          # the hydo this plan was written for (hydo build refuses another major.minor)
workload:
  steps: 20                  # rounds; faults are placed at round numbers. No run: here.
  timeout: 300
  retry: /bin/my-retry       # optional: after the faults heal, re-send unknown operations
test:
  parallel: 4                # at most this many commands at once
  per_round: 2               # parallel drivers started per round
  round_ms: 200              # a round every round_ms of guest time
  command_timeout: 60
  drain_timeout: 60
  check_every: 2             # default period of always_ checks
  check_within: 30           # default deadline of eventually_ checks
```

`workload.run` and `workload.process` are errors together with commands: the composer runs the
workload.

## Regressions

`hydo/regressions/<name>.schedule`: comment lines, one of `# expect: ok` (the bug stays fixed,
the default), `# expect: violation` (a known open bug still fails) or `# expect: fp=<16 hex>`, and
one schedule line. `hydo save <explore dir>` writes them from an exploration; `hydo repro
--regressions` replays them from fresh boots and exits 1 if one is not as expected.

A saved regression keeps its composition when you add, remove or rename other commands. Every
command has a stable id, `<kind>_<name>` (the file name in `hydo/workload/`), and `hydo save`
writes the composition in its resolved form: `W=:p2,r200,cparallel_driver_transfer,7.4.0.721476632,...`
lists each command run, by id, with its round, its entry number and its `HYDO_SEED`. A long one
is written as `W=@<key>` plus a `# compose: <resolved form>` line, which `hydo build` puts into
the image. The file also records `# command-sha: <id> <hash>` for each command used. So:

- Do not rename a command that a saved regression uses: a missing id fails that regression
  with a message that names the command (the image still builds). Restore the command, or
  delete the regression and save a new one.
- Changing a used command's file only warns ("changed since the regression was saved"); look
  at whether the regression still tests what it was saved for.
- Old regressions in the seed form (`W=<hex seed>,...`) with a `# commands: <ids>` line replay
  the composition they had with that list. Without that line they compose from the current list,
  and `hydo build` warns.
