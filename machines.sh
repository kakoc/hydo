# machines.sh -- how the repository's scripts reach the two machines (sourced, never run).
#
# hydo's development uses two machines besides the computer the scripts run on:
#   the bare-metal machine   runs the hvx kernel module and its VMs (an x86_64 host with VT-x
#                            and a working PMU);
#   the build machine        builds guest images with Nix (optional since M140: Nix also runs on
#                            the bare-metal machine).
# How to reach them is not in the repository. It lives in one git-ignored file, machines.local.sh
# at the repository root (in a git worktree without its own copy, the main checkout's; or the
# file $HYDO_MACHINES names), which defines, for each machine a script needs:
#   BARE_METAL           the bare-metal machine as the host part of an rsync destination
#                        (rsync -az FILE "$BARE_METAL:dir/")
#   on_bare_metal ARG..  a shell function that runs its arguments there as one shell command line
#                        (standard input passed through, the remote command's exit status returned)
#   BUILD_MACHINE        and
#   on_build_machine     the same for the build machine.
# A script sources this file and calls need_bare_metal or need_build_machine before its first
# remote step; either stops with a message that names the missing file or definition.

if [ -z "${HYDO_MACHINES:-}" ]; then
  # this file's directory: bash knows it; a POSIX sh caller sets HYDO_REPO (the repository root) first
  _hydo_machines_dir="${HYDO_REPO:-.}"
  if [ -n "${BASH_VERSION:-}" ]; then eval '_hydo_machines_dir="$(dirname "${BASH_SOURCE[0]}")"'; fi
  _hydo_machines_dir="$(cd "$_hydo_machines_dir" && pwd)"
  HYDO_MACHINES="$_hydo_machines_dir/machines.local.sh"
  # in a git worktree without its own copy, the main checkout's file
  if [ ! -f "$HYDO_MACHINES" ] &&
     _hydo_machines_git="$(git -C "$_hydo_machines_dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" &&
     [ -f "${_hydo_machines_git%/.git}/machines.local.sh" ]; then
    HYDO_MACHINES="${_hydo_machines_git%/.git}/machines.local.sh"
  fi
fi
if [ -f "$HYDO_MACHINES" ]; then
  # shellcheck disable=SC1090
  . "$HYDO_MACHINES"
fi

hydo_machines_missing() {   # what, variable, function
  echo "error: this script reaches $1 through $HYDO_MACHINES, which" >&2
  if [ ! -f "$HYDO_MACHINES" ]; then
    echo "  does not exist. Create it (it is git-ignored, never commit it) with" >&2
  else
    echo "  does not define both $2 and the function $3. Add" >&2
  fi
  echo "    $2=<the host part of an rsync destination for $1>" >&2
  echo "    $3() { <a remote-shell command for $1> \"\$@\"; }" >&2
  echo "  (machines.sh at the repository root describes the file)." >&2
  exit 2
}

need_bare_metal() {
  { [ -n "${BARE_METAL:-}" ] && command -v on_bare_metal > /dev/null; } ||
    hydo_machines_missing "the bare-metal machine" BARE_METAL on_bare_metal
}

need_build_machine() {
  { [ -n "${BUILD_MACHINE:-}" ] && command -v on_build_machine > /dev/null; } ||
    hydo_machines_missing "the build machine" BUILD_MACHINE on_build_machine
}

# The machine with the pinned Nix store paths that the release and example scripts copy from: the
# build machine, or the bare-metal machine with HYDO_NIX_MACHINE=bare-metal (Nix runs there since
# M140). need_nix_machine sets NIX_MACHINE (an rsync destination host); on_nix_machine runs there.
need_nix_machine() {
  case "${HYDO_NIX_MACHINE:-build}" in
    bare-metal) need_bare_metal; NIX_MACHINE=$BARE_METAL ;;
    build) need_build_machine; NIX_MACHINE=$BUILD_MACHINE ;;
    *) echo "error: HYDO_NIX_MACHINE=$HYDO_NIX_MACHINE: build or bare-metal" >&2; exit 2 ;;
  esac
}
on_nix_machine() {
  if [ "${HYDO_NIX_MACHINE:-build}" = bare-metal ]; then on_bare_metal "$@"; else on_build_machine "$@"; fi
}
