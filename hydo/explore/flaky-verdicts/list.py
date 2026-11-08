"""list.py <lite.json>: one line per SWE-bench task: id, the test command, the number of tests."""
import json
import sys

from swebench.harness.test_spec.test_spec import make_test_spec

for inst in json.load(open(sys.argv[1])):
    spec = make_test_spec(inst, namespace="swebench")
    s = spec.eval_script.split("\n")
    i = s.index(": '>>>>> Start Test Output'")
    n = len(json.loads(inst["FAIL_TO_PASS"])) + len(json.loads(inst["PASS_TO_PASS"]))
    print(inst["instance_id"], n, "|", s[i + 1][:150])
