#!/bin/sh
# retry <command> [args...]: runs a build command up to 10 times, until it succeeds.
# Under QEMU's emulation of x86_64 on a Mac, the compiler ends with a segmentation fault at a
# random source file now and then. make then starts again with the files it has not built yet;
# a compiler that crashed wrote no object file. A crash inside one of CMake's checks is not
# retried: the check reports a missing feature (RESULTS.md, "Building the same bytes").
for try in 1 2 3 4 5 6 7 8 9 10; do
  "$@" && exit 0
  echo "retry: '$*' failed (try $try of 10)" >&2
done
exit 1
