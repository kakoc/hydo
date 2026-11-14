# Valkey 9.1.3, the official Debian image, plus libfaketime. libfaketime is a test tool, not a
# change to Valkey: it moves one process's realtime clock (the wall clock, as an NTP step does)
# and leaves its monotonic clock alone. Valkey itself is unchanged.
FROM valkey/valkey:9.1.3@sha256:3add1a336a920ea54770a001ab6feb3da5fceea96a40373a3a2cd469b815f0b7
RUN apt-get update \
 && apt-get install -y --no-install-recommends faketime \
 && ln -s "$(dpkg -L libfaketime | grep '/libfaketime.so.1$')" /usr/local/lib/libfaketime.so.1 \
 && rm -rf /var/lib/apt/lists/*
