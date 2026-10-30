#!/usr/bin/env python3
"""Cross-check hydo-compose's Python semantics against Python (needs PyYAML):

    cargo build -p hydo-compose --example pycheck
    python3 hydo/crates/hydo-compose/tests/pycheck.py target/debug/examples/pycheck

YAML: repr(yaml.safe_load(text)) of every Compose file in the repository, the edge cases below and
generated documents; shlex: shlex.split and shlex.quote of generated strings. Prints the counts and
every difference; exit status 1 if any.
"""
import glob, json, os, random, shlex, subprocess, sys
import yaml

TOP = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
PROBE = sys.argv[1]

EDGE = [
    "a: yes\nb: No\nc: on\nd: OFF\ne: y\nf: n\n", "a: 0755\nb: 0o17\nc: 0x1F\nd: 0b101\ne: 1_000\nf: -0\ng: +12\n",
    "a: 1:30\nb: 22:22\nc: 8080:80\nd: 1:30:00\ne: 190:20:30.15\n", "a: 1.5\nb: 1.\nc: .5\nd: 1e5\ne: 1.0e+5\nf: .inf\ng: -.Inf\nh: .NaN\ni: 6.8523015e+5\n",
    "a: ~\nb: null\nc: Null\nd:\ne: NULL\nf: nULL\n", "a: 2024-01-02\nb: 2001-12-14t21:59:43.10-05:00\nc: 2001-12-14 21:59:43.10 -5\nd: 2002-12-14\ne: 2001-12-15T02:59:43.1Z\n",
    "a: '7'\nb: \"yes\"\nc: !!str 12\nd: !!int '12'\ne: !!float '1'\nf: ! 12\n", "x: &a {k: 1, m: 2}\ny: &b {k: 3, z: 4}\ns:\n  <<: [*a, *b]\n  m: 9\n",
    "x: &a\n  k: 1\ns:\n  <<: *a\n  k: 2\n  j: 3\n", "a: 1\na: 2\nb: 3\n", "- a\n- [b, c]\n- {d: e}\n", "a: |\n  x\n  y\nb: >\n  p\n  q\n\nc: |-\n  z\n",
    "a: \"\\u00e9\\t\\n\"\nb: 'it''s'\n", "? complex\n: value\n", "a: [1, 2,\n  3]\n", "test: [\n    \"CMD\",\n    # c\n    \"x\",\n  ]\n",
    "a: =\n", "{a: 1, b: [x, y]}", "", "# only a comment\n", "- 1\n- -1\n- 01\n- 09\n- 0.0\n- -0.0\n", "a: 1e\nb: 12e3\nc: 1.2.3\n",
]


def corpus():
    docs = list(EDGE)
    for p in sorted(glob.glob(os.path.join(TOP, "**", "*compose*.y*ml"), recursive=True)):
        if "/target/" not in p:
            docs.append(open(p, encoding="utf-8").read())
    rnd = random.Random(7)
    atoms = ["yes", "no", "on", "0755", "1:30", "1.5", "~", "", "'x'", '"y"', "2024-1-2", "0x1f", "1e5", "-.inf",
             "abc", "<<", "=", "12_3", "+1", "0b11", "3:60", "10:59:59", "1_0.5", "Null", "TRUE", ".NaN"]
    for _ in range(400):
        docs.append("".join("k%d: %s\n" % (i, rnd.choice(atoms)) for i in range(rnd.randint(1, 6))))
    return docs


class _DT:
    """A datetime shown as the Rust probe shows it: datetime.datetime(<str()>). The importer uses
    only str() of a timestamp (repr would appear only inside an error message)."""
    def __init__(self, d):
        self.d = d

    def __repr__(self):
        return "datetime.datetime(%s)" % self.d


def _dt(x):
    import datetime
    if isinstance(x, datetime.datetime):
        return _DT(x)
    if isinstance(x, list):
        return [_dt(v) for v in x]
    if isinstance(x, dict):
        return {k: _dt(v) for k, v in x.items()}
    return x


def py_yaml(t):
    try:
        return repr(_dt(yaml.safe_load(t)))
    except Exception:
        return "ERR"


def py_shlex(t):
    try:
        return repr(shlex.split(t))
    except ValueError as e:
        return "ERR: %s" % e


def probe(mode, items):
    r = subprocess.run([PROBE, mode], input="".join(json.dumps(x) + "\n" for x in items), stdout=subprocess.PIPE,
                       text=True, check=True)
    return [json.loads(l) for l in r.stdout.splitlines()]


def main():
    bad = 0
    docs = corpus()
    for d, got in zip(docs, probe("yaml", docs)):
        want = py_yaml(d)
        if want != got:
            bad += 1
            print("YAML differs:\n%s\n  python: %s\n  rust:   %s" % (d[:400], want[:400], got[:400]))
    rnd = random.Random(11)
    alpha = ["a", "b", " ", "'", '"', "\\", "$", "\t", "\n", "é", "#", ";", "=", "-"]
    strs = ["".join(rnd.choice(alpha) for _ in range(rnd.randint(0, 12))) for _ in range(5000)]
    for s, got in zip(strs, probe("shlex", strs)):
        if py_shlex(s) != got:
            bad += 1
            print("shlex.split differs: %r python %s rust %s" % (s, py_shlex(s), got))
    for s, got in zip(strs, probe("quote", strs)):
        if shlex.quote(s) != got:
            bad += 1
            print("shlex.quote differs: %r" % s)
    print("yaml: %d documents, shlex: %d strings split and quoted, %d differences" % (len(docs), len(strs), bad))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
