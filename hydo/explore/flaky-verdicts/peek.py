"""peek.py <lite.json> <id>...: print each task's evaluation script without the patch bodies."""
import json
import sys

from swebench.harness.test_spec.test_spec import make_test_spec

rows = {r["instance_id"]: r for r in json.load(open(sys.argv[1]))}
for iid in sys.argv[2:]:
    print("=====", iid)
    skip = False
    for ln in make_test_spec(rows[iid], namespace="swebench").eval_script.split("\n"):
        if skip:
            skip = ln.strip() != "EOF_114329324912"
            continue
        if "<<'EOF_114329324912'" in ln:
            skip = True
        print(ln[:200])
