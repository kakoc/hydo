# Example: a hydo/ folder with composed commands

A copy of `hvx/onboard/examples/redis-composed/hydo/` from hydo's repository: Redis with an
append-only file, driven by composed commands (SETs, transfers between accounts, an AOF rewrite
as a serial driver, a read as an anytime command, seed accounts as a `first_` command) and three
checks, plus one saved regression. Its `root.files` point at Redis binaries on hydo's build
machine (`~/hydo/build/redis-server`); in a project, point them at the project's own files.

Try: `disk=none | run W=0000002a,p4,q2,r100` (OK with the default atomic transfer; built with
`--set transfer=racy`, two overlapping transfers lose an update) and `disk=cut | run C=12
R=everysec` (Redis's documented fsync window loses acknowledged SETs).
