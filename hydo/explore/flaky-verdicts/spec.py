"""spec.py <lite.json> <instance_id> <out dir>: write one task's files from the SWE-bench harness.

The SWE-bench evaluation script (swebench 3.0.17, `make_test_spec`) does, in a container of the
task's evaluation image: activate the conda environment, set some variables, reinstall the
project (`pip install -e .`), reset the test files, apply the test patch, run the test command
between two markers, reset the test files. Here the same steps are split in two:

  setup.sh      run once when the task's image is made, with the network on: the gold patch and
                the test patch are applied first (by prepare.sh), then every line of the
                evaluation script before the test markers except the git bookkeeping
                (status, show, diff, checkout, apply)
  run-tests.sh  run for every verdict, with the network off: activate the environment, `cd
                /testbed`, the script's `export` lines, the markers and the test command

and also writes gold.patch, test.patch and task.json (instance id, repository, version,
FAIL_TO_PASS, PASS_TO_PASS, the evaluation image's name).
"""
import json
import os
import sys

from swebench.harness.test_spec.test_spec import make_test_spec

START = ": '>>>>> Start Test Output'"
END = ": '>>>>> End Test Output'"
HEREDOC = "EOF_114329324912"


def main():
    lite, iid, out = sys.argv[1:4]
    rows = {r["instance_id"]: r for r in json.load(open(lite))}
    inst = rows[iid]
    spec = make_test_spec(inst, namespace="swebench")
    lines = spec.eval_script.split("\n")
    i, j = lines.index(START), lines.index(END)
    pre, test = lines[:i], lines[i + 1 : j]
    setup, exports, skip = [], [], False
    for ln in pre:
        if skip:
            skip = ln.strip() != HEREDOC
            continue
        if f"<<'{HEREDOC}'" in ln:
            skip = True
            continue
        if ln.startswith(("git status", "git show", "git -c", "git checkout ", "git apply")):
            continue
        setup.append(ln)
        if ln.startswith("export "):
            exports.append(ln)
    head = [
        "#!/bin/bash",
        "set -uxo pipefail",
        "source /opt/miniconda3/bin/activate",
        "conda activate testbed",
        "cd /testbed",
    ]
    os.makedirs(out, exist_ok=True)
    w = lambda name, text: open(os.path.join(out, name), "w").write(text)
    w("gold.patch", inst["patch"])
    w("test.patch", inst["test_patch"])
    w("setup.sh", "\n".join(setup) + "\n")
    w("run-tests.sh", "\n".join(head + exports + [START] + test + [END]) + "\n")
    w(
        "task.json",
        json.dumps(
            {
                "instance_id": iid,
                "repo": inst["repo"],
                "version": inst["version"],
                "FAIL_TO_PASS": json.loads(inst["FAIL_TO_PASS"]),
                "PASS_TO_PASS": json.loads(inst["PASS_TO_PASS"]),
                "image": "swebench/sweb.eval.x86_64." + iid.replace("__", "_1776_").lower() + ":latest",
            },
            indent=1,
        ),
    )


main()
