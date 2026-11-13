# Error logs as findings

A service often writes an error line long before any check fails. With `errors:` in
`hydo/hydo.yaml`, hydo sorts the error lines of a run into kinds. It compares the kinds with a
reviewed list of expected ones, `hydo/known-errors.yaml`, and reports only new kinds. The full
reference is the section `Error logs as findings` of `hydo guide manifest`.

## Turn it on

```yaml
errors: true                     # every default: error-level lines of every service
services:
  - name: mysql
    ...
    errors:
      files: [/tmp/mysqld.err]   # a log file the service writes itself (MySQL log-error=, a Java app's log file)
  - name: redis
    ...
    errors:
      format: redis              # Redis marks its most severe level with "#", not with ERROR
```

- The service's stdout/stderr is always scanned. Add `files:` for every log file the service
  writes on its own; look in its configuration (`log-error=`, `log4j` appenders, `--log-file`).
- `format: auto` (the default) knows `ERROR`/`FATAL`/`PANIC`/`SEVERE`, `level=error`,
  `"level":"error"`, `[error]`, Go `panic:` and exception lines. Use `format: redis` for Redis,
  `format: none` with `match:` for a format with no level word, `level: warning` (top level or per
  service) to include warnings, `ignore:` for a line that is known noise in every run.
- `normalize:` (a list of regular expressions) blanks out project-specific ids the default
  template rules miss (for example `'req-[a-z0-9]+'`). Numbers, timestamps, uuids, hex numbers,
  client ports, random ids of 8 characters or more, and the values of `trace-id=`,
  `request_id:` and similar keys are blanked already.

## The known list

`hydo/known-errors.yaml` lives in the repository, next to `hydo.yaml`, and is NOT part of the
image: accepting a kind changes no image and no fingerprint. Do not write entries by guessing.
Build, run, and let the user triage what appeared:

```sh
hydo run "disk=none dseed=1 | run"     # prints each kind: NEW or known, its signature, an example line
hydo explore --seeds 1-20 --strategy process
hydo errors list                       # the kinds of the last exploration
hydo errors accept <signature> --reason "why this is expected"              # always expected
hydo errors accept <signature> --reason "..." --during K=mysql              # only while mysql is down
hydo errors accept <signature> --reason "..." --with C                      # only in runs with a power cut
```

Propose a triage for each new kind, but let the user decide:

- An expected kind gets an entry with a reason. Examples are a client's `connection refused`
  while its server is killed, and a startup warning.
- A kind nobody can explain is a finding. Its minimal schedule, in `report.md`, is the
  reproducer.

Prefer `--during <fault>` over an unscoped entry when the line is only expected while a fault is
on, so the same line without that fault still shows up.

`unknown: fail` at the top of the list (or `hydo explore --errors fail`) makes a new kind fail
the run, for CI once the list is reviewed.

## Pitfalls

- Every line written during boot and provisioning appears in every run (`at` = `setup`). Accept
  those first, with a reason, or the report is noise.
- A service that writes its log into the data disk (`/data/...`): only lines written in the
  current boot are reported. Lines of the first boot that survive a power cut are not counted
  twice.
- Templates are heuristics: if the same kind shows up under several signatures that differ only in
  an id, add a `normalize:` pattern (it changes the image, so rebuild) and re-accept.
