# The official Restate image with a small LD_PRELOAD shim that sets the wall clock back
# (SHIFT_BACK_SECS), plus the example's static service program rsv (the deployment, `rsv serve`).
FROM debian:trixie-slim AS shim
RUN apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev && rm -rf /var/lib/apt/lists/*
COPY shiftback.c /src/shiftback.c
RUN gcc -O2 -shared -fPIC -o /src/shiftback.so /src/shiftback.c

FROM docker.restate.dev/restatedev/restate:1.8.1-hotfix.2@sha256:727dc7f0e91baebe9059f636522deb531cd0bdce669e744b2d7cc6c9850f6660
COPY --from=shim /src/shiftback.so /usr/local/lib/shiftback.so
COPY rsv /usr/local/bin/rsv
