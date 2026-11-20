# Inventory: how the project runs

Read these, in roughly this order, and note what each says about processes, ports, data and
configuration:

1. `docker-compose*.yml`, `compose*.yaml`, `.devcontainer/`: the services, their images, ports,
   volumes, health checks and restart policies. A Compose file is the best starting point.
2. `Dockerfile*`: the program, `CMD`/`ENTRYPOINT`, `EXPOSE`, `HEALTHCHECK`.
3. `Procfile`, systemd units, Helm charts, Kustomize overlays, the platform's environment files,
   and other deployment manifests: the replica count of each service, the size of each cluster,
   and each feature that production turns on (caches, replication of results). The folder copies
   this shape (SKILL.md step 3.3).
4. The build files (`Makefile`, `package.json`, `go.mod`, `Cargo.toml`, `pom.xml`): how each
   program is built, and whether it can be built as a static binary.
5. The README's "run" or "self-host" section and the configuration parser: which settings exist,
   and which of them are durability settings (fsync, commit and replication modes) or timeouts.
6. Database migrations and schemas: keys, unique constraints and foreign keys that checks can use.
7. `.proto` files and API specifications: the operations clients perform.
8. Existing integration or end-to-end tests: candidates for workload commands and checks.

Write a short inventory for the user: for each process, its program, its port, its data
directory, what it depends on, its durability settings, its replica count in production, and the
file where you found each fact. Add each cluster's size and each enabled feature. Mark what you
guessed.

Also list what the system needs from outside (other companies' APIs, e-mail, object storage, DNS,
license servers, telemetry): each becomes a fake, a configuration change, or a blocker to report.
