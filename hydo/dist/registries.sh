#!/usr/bin/env bash
# The registry side of a release: what .github/workflows/release.yml and packages.yml run
# (docs/PUBLISHING.md "Publishing from GitHub Actions"). Every command also works by hand from
# a checkout; only `publish-crates` without --dry-run uploads anything, and only with a
# crates.io token in CARGO_REGISTRY_TOKEN (in the workflow, a 30-minute token from crates.io's
# trusted publishing; by hand, `cargo login`'s).
#
#   registries.sh crates
#       The crates of `cargo install hydo` in publishing order (the `cargo publish --registry
#       crates-io -p <crate>` lines of docs/PUBLISHING.md, which the hydo-release `crates` test
#       checks and hydo/dist/crates/rehearse.sh packs), then `hydo-assert` (sdk/rust).
#   registries.sh versions VERSION
#       Fails unless every package a release publishes carries VERSION: hydo/VERSION, the
#       workspace version, every `hydo-*` entry of [workspace.dependencies] (except
#       hydo-agent-proto, which keeps its own version: docs/PUBLISHING.md), sdk/rust, sdk/python
#       (pyproject.toml and __version__) and sdk/js.
#   registries.sh release TAG [WAIT_SECONDS]
#       Fails unless the GitHub release TAG is published (not a draft) with the four tarballs and
#       SHA256SUMS uploaded, and SHA256SUMS names every tarball asset; waits up to WAIT_SECONDS
#       (default 0) for it. Reads GH_TOKEN or GITHUB_TOKEN when set (a higher rate limit).
#   registries.sh crate-published NAME VERSION
#       Exit status 0 when crates.io's index lists NAME VERSION, 1 when not.
#   registries.sh npm-published NAME VERSION
#       Exit status 0 when registry.npmjs.org has NAME VERSION, 1 when not.
#   registries.sh publish-crates [--dry-run] [--deadline SECONDS]
#       Publishes every crate of `crates` whose version (each crate's own: cargo metadata, or
#       sdk/rust/Cargo.toml) crates.io does not have yet, in order, with `cargo publish --locked
#       --no-verify` (the workflow checked every package with `cargo package` before it asked
#       for a token; a published crate is never built again here). A crate already there is
#       skipped, so a rerun after a failure is safe. With --deadline it starts no crate after
#       SECONDS and says so in GITHUB_OUTPUT (`done=false`), so that the workflow can ask for a
#       new token (a trusted-publishing token lives 30 minutes) and run it again.
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../.." && pwd)"
# owner/name: the workflow's repository, else the workspace's `repository`
REPO="${GITHUB_REPOSITORY:-$(sed -n 's|^repository = "https://github.com/\(.*\)"$|\1|p' "$TOP/Cargo.toml" | head -1)}"
UA="hydo-release (https://github.com/$REPO)"

die() { echo "error: $*" >&2; exit 1; }

# `key = "value"` of a [table] in a TOML file (the simple form these manifests use)
toml_get() {
  awk -v t="[$2]" -v k="$3" '
    /^\[/ { in_t = ($0 == t); next }
    in_t && $1 == k && $2 == "=" { v = $3; gsub(/"/, "", v); print v; exit }
  ' "$1"
}

crates() {
  local list
  list=$(sed -n 's/^ *cargo publish --registry crates-io -p \([a-z0-9-]*\)$/\1/p' "$TOP/docs/PUBLISHING.md")
  [ -n "$list" ] || die "no 'cargo publish --registry crates-io -p <crate>' lines in docs/PUBLISHING.md"
  printf '%s\n' "$list" hydo-assert
}

versions() {
  local want="$1" bad=0 got f deps
  deps=$(sed -n 's/^\(hydo-[a-z0-9-]*\) = {.*version = "\([^"]*\)".*/\1 \2/p' "$TOP/Cargo.toml")
  [ -n "$deps" ] || die "no hydo-* entries with a version in Cargo.toml [workspace.dependencies]"
  check() { # what, value
    if [ "$2" = "$want" ]; then echo "ok   $1 = $2"; else echo "FAIL $1 = ${2:-<none>}, expected $want"; bad=1; fi
  }
  check hydo/VERSION "$(tr -d ' \n' < "$TOP/hydo/VERSION")"
  check "Cargo.toml [workspace.package] version" "$(toml_get "$TOP/Cargo.toml" workspace.package version)"
  while read -r name ver; do
    [ "$name" = hydo-agent-proto ] && { echo "--   Cargo.toml $name = $ver (its own version)"; continue; }
    check "Cargo.toml [workspace.dependencies] $name" "$ver"
  done <<< "$deps"
  check "sdk/rust/Cargo.toml version" "$(toml_get "$TOP/sdk/rust/Cargo.toml" package version)"
  check "sdk/python/pyproject.toml version" "$(toml_get "$TOP/sdk/python/pyproject.toml" project version)"
  f="$TOP/sdk/python/src/hydo_assert/__init__.py"
  check "sdk/python __version__" "$(sed -n 's/^__version__ = "\(.*\)"$/\1/p' "$f")"
  got=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "$TOP/sdk/js/package.json")
  check "sdk/js/package.json version" "$got"
  [ "$bad" = 0 ] || die "the versions differ from $want (docs/PUBLISHING.md \"The next version\" lists the files a release bumps together)"
}

release() {
  local tag="$1" wait="${2:-0}"
  python3 -I - "$REPO" "$tag" "$wait" "$UA" <<'EOF'
import json, os, sys, time, urllib.error, urllib.request
repo, tag, wait, ua = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
version = tag[1:] if tag.startswith("v") else tag
need = [f"hydo-linux-{version}.tar.gz", f"hydo-linux-{version}-sources.tar.gz",
        f"hydo-mac-{version}.tar.gz", f"hydo-mac-{version}-sources.tar.gz", "SHA256SUMS"]
optional = [f"hydo-mac-{version}-signatures.tar.gz"]
token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")

def get(url, accept="application/vnd.github+json"):
    h = {"User-Agent": ua, "Accept": accept}
    if token and url.startswith("https://api.github.com/"):
        h["Authorization"] = f"Bearer {token}"
    return urllib.request.urlopen(urllib.request.Request(url, headers=h), timeout=60).read()

def problems():
    try:
        rel = json.loads(get(f"https://api.github.com/repos/{repo}/releases/tags/{tag}"))
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return [f"no published GitHub release for the tag {tag} in {repo}"]
        raise
    if rel.get("draft"):
        return [f"the release {tag} is a draft"]
    assets = {a["name"]: a for a in rel["assets"]}
    out = [f"the release {tag} has no uploaded asset {n}" for n in need
           if assets.get(n, {}).get("state") != "uploaded"]
    if out:
        return out
    sums = get(assets["SHA256SUMS"]["browser_download_url"], "*/*").decode()
    listed = {l.split()[-1].lstrip("*") for l in sums.splitlines() if l.strip()}
    out = [f"SHA256SUMS of {tag} does not name {n}" for n in assets
           if n.endswith(".tar.gz") and n not in listed]
    for n in optional:
        if n not in assets:
            print(f"note: the release {tag} has no {n} (an unsigned Mac release: docs/RELEASING.md step 2a)")
    return out

deadline = time.time() + wait
while True:
    p = problems()
    if not p:
        print(f"ok   the GitHub release {tag} of {repo} is published with its assets")
        sys.exit(0)
    if time.time() >= deadline:
        for l in p:
            print(f"FAIL {l}")
        sys.exit(1)
    print(f"waiting for the release ({p[0]}); {int(deadline - time.time())} s left", flush=True)
    time.sleep(min(30, max(1, deadline - time.time())))
EOF
}

crate_published() {
  local name="$1" ver="$2" path
  case ${#name} in
    1) path="1/$name" ;;
    2) path="2/$name" ;;
    3) path="3/${name:0:1}/$name" ;;
    *) path="${name:0:2}/${name:2:2}/$name" ;;
  esac
  curl -fsS -A "$UA" "https://index.crates.io/$path" 2>/dev/null | grep -qF "\"vers\":\"$ver\""
}

npm_published() {
  curl -fsS -o /dev/null -A "$UA" "https://registry.npmjs.org/$1/$2" 2>/dev/null || return 1
}

# "name version" of each crate of `crates`
crate_versions() {
  local meta list c
  meta=$(cd "$TOP" && cargo metadata --locked --no-deps --format-version 1)
  list=$(crates)
  for c in $list; do
    if [ "$c" = hydo-assert ]; then
      echo "$c $(toml_get "$TOP/sdk/rust/Cargo.toml" package version)"
    else
      printf '%s' "$meta" | python3 -I -c '
import json, sys
name = sys.argv[1]
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == name:
        print(name, p["version"]); break
else:
    sys.exit(f"error: {name} is not a workspace package")' "$c"
    fi
  done
}

publish_crates() {
  local dry=0 deadline=0 start=$SECONDS name ver attempt log status list
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) dry=1 ;;
      --deadline) deadline="$2"; shift ;;
      *) die "publish-crates: unknown argument $1" ;;
    esac
    shift
  done
  [ "$dry" = 1 ] || [ -n "${CARGO_REGISTRY_TOKEN:-}" ] || die "publish-crates: no CARGO_REGISTRY_TOKEN"
  list=$(crate_versions)
  log=$(mktemp)
  while read -r name ver; do
    if crate_published "$name" "$ver"; then
      echo "== $name $ver: already on crates.io, skipped"
      continue
    fi
    if [ "$dry" = 1 ]; then
      echo "== $name $ver: would be published"
      continue
    fi
    if [ "$deadline" != 0 ] && [ $((SECONDS - start)) -ge "$deadline" ]; then
      echo "== the deadline ($deadline s) has passed: $name and the crates after it wait for a new token"
      [ -n "${GITHUB_OUTPUT:-}" ] && echo "done=false" >> "$GITHUB_OUTPUT"
      rm -f "$log"
      return 0
    fi
    for attempt in 1 2 3; do
      echo "== $name $ver: cargo publish (attempt $attempt)"
      status=0
      if [ "$name" = hydo-assert ]; then
        # its own workspace without a lock file (a library with no required dependency)
        ( cd "$TOP/sdk/rust" && cargo publish --no-verify --registry crates-io ) 2>&1 | tee "$log" || status=$?
      else
        ( cd "$TOP" && cargo publish --locked --no-verify --registry crates-io -p "$name" ) 2>&1 | tee "$log" || status=$?
      fi
      [ "$status" = 0 ] && break
      if grep -q "already exists" "$log"; then
        echo "== $name $ver: crates.io already has it"
        status=0
        break
      fi
      [ "$attempt" = 3 ] || { echo "== $name: failed (exit status $status), again in 60 s"; sleep 60; }
    done
    [ "$status" = 0 ] || { rm -f "$log"; die "cargo publish of $name $ver failed three times"; }
  done <<< "$list"
  rm -f "$log"
  [ -n "${GITHUB_OUTPUT:-}" ] && echo "done=true" >> "$GITHUB_OUTPUT"
  return 0
}

cmd="${1:-}"
[ $# -gt 0 ] && shift
case "$cmd" in
  crates) crates ;;
  versions) [ $# = 1 ] || die "usage: registries.sh versions VERSION"; versions "$1" ;;
  release) [ $# -ge 1 ] || die "usage: registries.sh release TAG [WAIT_SECONDS]"; release "$@" ;;
  crate-published) [ $# = 2 ] || die "usage: registries.sh crate-published NAME VERSION"; crate_published "$@" ;;
  npm-published) [ $# = 2 ] || die "usage: registries.sh npm-published NAME VERSION"; npm_published "$@" ;;
  crate-versions) crate_versions ;;
  publish-crates) publish_crates "$@" ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
