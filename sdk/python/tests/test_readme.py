"""The README (the PyPI page) is tested like code.

Every ``python`` block of README.md runs as its own program, in a fresh process with this
checkout's ``src`` on the path, and must exit with status 0. The complete program's records
must be exactly the ``text`` block that follows it in the README.

    python3 -m unittest discover -s sdk/python/tests
"""

import os
import re
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
README = os.path.join(ROOT, "README.md")


def blocks():
    """(info string, text, the heading above it) of every fenced block of the README."""
    out, heading, info, lines = [], None, None, []
    with open(README, encoding="utf-8") as f:
        for line in f:
            if info is None and line.startswith("#"):
                heading = line.strip()
            if line.startswith("```"):
                if info is None:
                    info, lines = line[3:].strip(), []
                else:
                    out.append((info, "".join(lines), heading))
                    info = None
            elif info is not None:
                lines.append(line)
    return out


def run(code, env_extra, tmp):
    path = os.path.join(tmp, "example.py")
    with open(path, "w", encoding="utf-8") as f:
        f.write(code)
    env = {k: v for k, v in os.environ.items() if not k.startswith("HYDO_")}
    env.update(PYTHONPATH=os.path.join(ROOT, "src"), PYTHONDONTWRITEBYTECODE="1")
    env["HYDO_BUGGIFY_FILE"] = os.path.join(tmp, "no-such-file")
    env.update(env_extra)
    return subprocess.run([sys.executable, path], cwd=tmp, env=env, capture_output=True, timeout=60)


class Readme(unittest.TestCase):
    def test_every_python_block_runs(self):
        found = [(text, h) for info, text, h in blocks() if info == "python"]
        self.assertGreaterEqual(len(found), 7)
        for text, heading in found:
            with self.subTest(heading=heading), tempfile.TemporaryDirectory() as tmp:
                p = run(text, {"HYDO_ASSERT_OUT": "off", "HYDO_HIST_OUT": "off"}, tmp)
                self.assertEqual(p.returncode, 0, "%s\n%s" % (text, p.stderr.decode()))

    def test_the_complete_program_writes_what_the_readme_shows(self):
        bs = blocks()
        i = next(i for i, b in enumerate(bs) if b[2] == "## A complete program" and b[0] == "python")
        program, (info, want, _) = bs[i][1], bs[i + 1]
        self.assertEqual(info, "text")
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, "console")
            open(out, "w").close()
            p = run(program, {"HYDO_ASSERT_OUT": out}, tmp)
            self.assertEqual(p.returncode, 0, p.stderr.decode())
            with open(out, encoding="utf-8") as f:
                self.assertEqual(f.read(), want)

    def test_links_are_absolute(self):
        # PyPI shows the README outside the repository
        with open(README, encoding="utf-8") as f:
            text = f.read()
        for target in re.findall(r"\]\(([^)]*)\)", text):
            self.assertTrue(target.startswith("https://"), target)


if __name__ == "__main__":
    unittest.main()
