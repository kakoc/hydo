#!/bin/bash
# bm.sh '<command>': run one shell command line on the build machine through on_build_machine.
. "$(dirname "$0")/../../../machines.sh"
need_build_machine
on_build_machine "$@"
