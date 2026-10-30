"""Tests of hvx/onboard/hydo_agents.py (hydo guide / skill / init), on the repository's skill:
    python3 -m unittest discover -s hvx/onboard/tests -v"""
import io, os, shutil, subprocess, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import hydo_agents as ha  # noqa: E402

TOP = os.path.abspath(os.path.join(HERE, "..", "..", ".."))   # the repository: skills/hydo-onboard


def run(fn, argv, cwd=None):
    out, old = io.StringIO(), os.getcwd()
    try:
        if cwd:
            os.chdir(cwd)
        rc = fn(argv, TOP, out=out)
    finally:
        os.chdir(old)
    return rc, out.getvalue()


def write(p, text):
    with open(p, "w") as f:
        f.write(text)


def read(p):
    with open(p) as f:
        return f.read()


def tree(d):
    r = {}
    for top, _, files in os.walk(d):
        for f in files:
            p = os.path.join(top, f)
            r[os.path.relpath(p, d)] = ((lambda f: (f.read(), f.close())[0])(open(p, "rb")), os.access(p, os.X_OK))
    return r


class Guide(unittest.TestCase):
    def test_skill_md(self):
        rc, out = run(ha.guide, [])
        self.assertEqual(rc, 0)
        self.assertEqual(out, read(os.path.join(TOP, "skills", "hydo-onboard", "SKILL.md")))

    def test_topics(self):
        rc, out = run(ha.guide, ["--list"])
        for t in ("composer", "checks", "hazards", "manifest", "capabilities"):
            self.assertIn("hydo guide " + t, out)
        rc, out = run(ha.guide, ["hazards"])
        self.assertTrue(out.startswith("# Determinism and environment hazards"))
        rc, out = run(ha.guide, ["manifest"])
        self.assertTrue(out.startswith("# hydo.yaml"))
        rc, out = run(ha.guide, ["assets/example-miniflux.yaml"])
        self.assertIn("miniflux", out)
        with self.assertRaises(ha.Fail):
            run(ha.guide, ["../../AGENTS.md"])
        with self.assertRaises(ha.Fail):
            run(ha.guide, ["nope"])


class Skill(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="hydo-agents-test-")
        subprocess.run(["git", "init", "-q", self.d], check=True)

    def tearDown(self):
        shutil.rmtree(self.d)

    def test_each_agent(self):
        src = tree(os.path.join(TOP, "skills", "hydo-onboard"))
        for agent, sub in (("claude", ".claude/skills"), ("codex", ".agents/skills"), ("cursor", ".agents/skills"),
                           ("copilot", ".agents/skills"), ("gemini", ".agents/skills")):
            rc, out = run(ha.skill, ["install", "--agent", agent], cwd=self.d)
            self.assertEqual(rc, 0)
            self.assertEqual(tree(os.path.join(self.d, sub, "hydo-onboard")), src)
        rc, out = run(ha.skill, ["install", "--agent", "all"], cwd=self.d)
        self.assertEqual(out.count("updated"), 2)
        to = os.path.join(self.d, "custom")
        run(ha.skill, ["install", "--to", to])
        self.assertEqual(tree(os.path.join(to, "hydo-onboard")), src)

    def test_refuses_foreign_and_symlink(self):
        t = os.path.join(self.d, ".claude", "skills", "hydo-onboard")
        os.makedirs(t)
        write(os.path.join(t, "SKILL.md"), "---\nname: other\n---\n")
        with self.assertRaises(ha.Fail):
            run(ha.skill, ["install", "--agent", "claude"], cwd=self.d)
        run(ha.skill, ["install", "--agent", "claude", "--force"], cwd=self.d)
        l = os.path.join(self.d, ".agents", "skills", "hydo-onboard")
        os.makedirs(os.path.dirname(l))
        os.symlink(t, l)
        with self.assertRaises(ha.Fail):
            run(ha.skill, ["install", "--agent", "codex"], cwd=self.d)
        with self.assertRaises(ha.Fail):
            run(ha.skill, ["install", "--agent", "vim"], cwd=self.d)


class Init(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix="hydo-init-test-")
        self.repo = os.path.join(self.d, "My_Shop.v2")
        os.makedirs(self.repo)

    def tearDown(self):
        shutil.rmtree(self.d)

    def test_twice(self):
        mine = "# Our agents\n\nUse tabs.\n"
        write(os.path.join(self.repo, "AGENTS.md"), mine)
        write(os.path.join(self.repo, ".gitignore"), "node_modules/\n/.hydo/")
        rc, out = run(ha.init, ["--dir", self.repo])
        self.assertEqual(rc, 0)
        a = read(os.path.join(self.repo, "AGENTS.md"))
        self.assertTrue(a.startswith(mine))
        self.assertIn("run `hydo guide`", a)
        y = read(os.path.join(self.repo, "hydo", "hydo.yaml"))
        self.assertIn("name: my-shop-v2\n", y)
        self.assertTrue(os.access(os.path.join(self.repo, "hydo", "workload", "parallel_driver_todo"), os.X_OK))
        g = read(os.path.join(self.repo, ".gitignore"))
        self.assertEqual(g.count(".hydo/"), 1)
        self.assertIn("hydo/images/\n", g)
        first = tree(self.repo)
        rc, out = run(ha.init, ["--dir", self.repo])
        self.assertIn("hydo section unchanged", out)
        self.assertEqual(tree(self.repo), first)

    def test_marker_update_and_existing_files(self):
        p = os.path.join(self.repo, "AGENTS.md")
        write(p, "top\n\n%s\nold text\n%s\n\nbottom\n" % (ha.BEGIN, ha.END))
        os.makedirs(os.path.join(self.repo, "hydo", "workload"))
        cmd = os.path.join(self.repo, "hydo", "workload", "parallel_driver_mine")
        write(cmd, "#!/bin/sh\n")
        rc, out = run(ha.init, ["--dir", self.repo])
        a = read(p)
        self.assertTrue(a.startswith("top\n\n" + ha.BEGIN) and a.endswith(ha.END + "\n\nbottom\n"), a)
        self.assertNotIn("old text", a)
        self.assertFalse(os.path.exists(os.path.join(self.repo, "hydo", "workload", "parallel_driver_todo")))
        self.assertTrue(os.path.exists(os.path.join(self.repo, "hydo", "checks", "final_todo")))

    def test_new_agents_md_and_check(self):
        run(ha.init, ["--dir", self.repo])
        self.assertTrue(read(os.path.join(self.repo, "AGENTS.md")).startswith("# Instructions for coding agents\n"))
        r = subprocess.run([sys.executable, os.path.join(TOP, "hvx", "onboard", "hydo-build"),
                            os.path.join(self.repo, "hydo", "hydo.yaml"), "--check"], stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, text=True)
        self.assertEqual(r.returncode, 0, r.stdout)


if __name__ == "__main__":
    unittest.main()
