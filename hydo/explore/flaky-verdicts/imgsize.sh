#!/bin/bash
# imgsize.sh <repository>...: the compressed size of each image's linux/amd64 layers on Docker
# Hub (tag latest, anonymous pull token), to choose tasks whose images stay small.
A='application/vnd.docker.distribution.manifest.v2+json,application/vnd.oci.image.manifest.v1+json,application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.list.v2+json'
for r in "$@"; do
  tok=$(curl -s "https://auth.docker.io/token?service=registry.docker.io&scope=repository:$r:pull" |
    sed 's/.*"token":"\([^"]*\)".*/\1/')
  m=$(curl -s -H "Authorization: Bearer $tok" -H "Accept: $A" "https://registry-1.docker.io/v2/$r/manifests/latest")
  d=$(echo "$m" | python3 -c '
import json, sys
m = json.load(sys.stdin)
for x in m.get("manifests", []):
    if x.get("platform", {}).get("architecture") == "amd64":
        print(x["digest"]); break')
  [ -n "$d" ] && m=$(curl -s -H "Authorization: Bearer $tok" -H "Accept: $A" "https://registry-1.docker.io/v2/$r/manifests/$d")
  echo "$m" | python3 -c '
import json, sys
m = json.load(sys.stdin)
print(sys.argv[1], sum(l["size"] for l in m["layers"]) // 1048576, "MiB compressed,", len(m["layers"]), "layers")' "$r"
done
