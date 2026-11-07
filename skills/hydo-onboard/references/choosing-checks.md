# Choosing the checks

A check is worth writing when it states something the system **promises**: in its documentation,
its API contract, its schema constraints, or what its users rely on. A check derived from what the
code happens to do today finds nothing and breaks on every change.

Start from what the user said must never happen, then go through this list:

1. **Acknowledged writes survive** every fault, including a power cut. The first check of almost
   every project.
2. **Exactly once**: every acknowledged operation is applied once, not zero times and not twice
   (retries, duplicate deliveries, idempotency keys).
3. **Failed means not applied**: an operation the system reported as failed left no effect. (An
   operation whose outcome the client did not learn may have either; do not check it either way.)
4. **Domain rules**: no negative balance, totals conserved, no order without its items, unique
   keys unique.
5. **Monotonic reads**: a version, counter or offset that a client saw never goes back.
6. **Caches agree with the store**: a cache never serves a value the store does not hold
   (allowing for its documented staleness).
7. **Liveness**: after the faults stop, the system answers again within a bound.
8. **Side effects outside**: an outside service (a payment, an e-mail) is called at most once per
   operation, and every acknowledged operation's effect happened.
9. **History**: when clients read and write registers or lists, a recorded history checked for
   linearizability or a consistency level finds anomalies no single read-back shows.

For each candidate, ask: does the project promise it under this fault? A cache that is documented
as lossy on a crash, or an fsync setting that trades durability on purpose, is a configuration
finding to report, not a check to fail. Record such settings as knobs so both sides can be run.

Keep checks cheap and exact: compare sets of ids rather than counts, read the store directly
rather than through the system under test, and say in one line what was violated.

How to write each kind with the installed hydo (the file names, exit codes and the ready-made
store readers and history checkers): `hydo guide checks` and `hydo guide invariant-patterns`.

Then tell the user plainly: the checks need human judgment; ask them to review each one, and say
which ones you are unsure about and why.
