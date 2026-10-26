#!/usr/bin/env python3
"""Check the skills in this directory against the Agent Skills specification
(https://agentskills.io/specification) and against the files they copy from the repository.

    python3 skills/check.py [--skills-ref PATH]

1. skills-ref (https://github.com/agentskills/agentskills/tree/main/skills-ref), when found on
   PATH or given with --skills-ref: `skills-ref validate <skill>` must print "Valid skill".
2. The specification's rules, checked here (skills-ref checks only the first group):
   name 1-64 chars, lowercase letters, digits and single hyphens, equal to the directory name;
   description 1-1024 chars; compatibility <= 500 chars; metadata a map of strings; only the
   allowed frontmatter keys; SKILL.md under 500 lines; every relative file it links exists and
   is one level deep (references/x.md, assets/x, scripts/x).
3. Copies stay in sync: assets/example-redis-composed/hydo/ equals
   hvx/onboard/examples/redis-composed/hydo/ (bytes and executable bits), and the tool
   directories .claude/skills/<name> and .agents/skills/<name> are symlinks to skills/<name>.
Exit status 0 = everything holds.
"""
import filecmp, os, re, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.dirname(HERE)
ALLOWED = {"name", "description", "license", "compatibility", "metadata", "allowed-tools"}
bad = []


def frontmatter(text):
    m = re.match(r"^---\n(.*?)\n---\n", text, re.S)
    if not m:
        return None
    fm, key = {}, None
    for line in m.group(1).split("\n"):
        if re.match(r"^\S[^:]*:", line):
            key, _, v = line.partition(":")
            fm[key.strip()] = v.strip() if v.strip() else {}
        elif line.startswith("  ") and isinstance(fm.get(key), dict):
            k, _, v = line.strip().partition(":")
            fm[key][k.strip()] = v.strip().strip('"')
    return fm


def check_skill(d):
    name = os.path.basename(d)
    text = open(os.path.join(d, "SKILL.md")).read()
    fm = frontmatter(text)
    if fm is None:
        bad.append("%s: no YAML frontmatter" % name); return
    for k in fm:
        if k not in ALLOWED:
            bad.append("%s: frontmatter key %r not in the specification" % (name, k))
    n = fm.get("name", "")
    if not (1 <= len(n) <= 64 and re.fullmatch(r"[a-z0-9]+(-[a-z0-9]+)*", n)):
        bad.append("%s: name %r breaks the naming rules" % (name, n))
    if n != name:
        bad.append("%s: name %r is not the directory name" % (name, n))
    desc = fm.get("description", "")
    if not isinstance(desc, str) or not 1 <= len(desc) <= 1024:
        bad.append("%s: description must be 1-1024 characters (%d)" % (name, len(desc) if isinstance(desc, str) else -1))
    if "compatibility" in fm and not (isinstance(fm["compatibility"], str) and 1 <= len(fm["compatibility"]) <= 500):
        bad.append("%s: compatibility must be 1-500 characters" % name)
    if "metadata" in fm and not isinstance(fm["metadata"], dict):
        bad.append("%s: metadata must be a map of strings" % name)
    lines = text.count("\n")
    if lines >= 500:
        bad.append("%s: SKILL.md has %d lines (keep it under 500)" % (name, lines))
    for link in re.findall(r"\]\(([^)#]+)\)", text):
        if re.match(r"[a-z]+:", link):
            continue
        p = os.path.normpath(os.path.join(d, link))
        if not os.path.exists(p):
            bad.append("%s: SKILL.md links %s, which does not exist" % (name, link))
        elif link.count("/") > 1:
            bad.append("%s: SKILL.md links %s, deeper than one level" % (name, link))
    print("rules: %s: name, description (%d chars), compatibility, metadata, %d lines, links" % (name, len(desc), lines))


def skills_ref(d, exe):
    r = subprocess.run([exe, "validate", d], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    print("skills-ref: " + r.stdout.strip())
    if r.returncode != 0:
        bad.append("skills-ref rejected %s" % d)


def same_tree(a, b):
    c = filecmp.dircmp(a, b)
    diff = c.left_only + c.right_only + c.diff_files + c.funny_files
    for f in c.common_files:
        if os.access(os.path.join(a, f), os.X_OK) != os.access(os.path.join(b, f), os.X_OK):
            diff.append(f + " (executable bit)")
    for s in c.common_dirs:
        diff += [s + "/" + x for x in same_tree(os.path.join(a, s), os.path.join(b, s))]
    return diff


def main():
    a = sys.argv[1:]
    exe = a[a.index("--skills-ref") + 1] if "--skills-ref" in a else shutil.which("skills-ref")
    skills = sorted(os.path.join(HERE, x) for x in os.listdir(HERE) if os.path.isfile(os.path.join(HERE, x, "SKILL.md")))
    for d in skills:
        if exe:
            skills_ref(d, exe)
        else:
            print("skills-ref: not found (install it from https://github.com/agentskills/agentskills/tree/main/skills-ref); "
                  "the rules below are checked anyway")
        check_skill(d)
        n = os.path.basename(d)
        for tool in (".claude/skills", ".agents/skills"):
            p = os.path.join(TOP, tool, n)
            if not (os.path.islink(p) and os.path.realpath(p) == os.path.realpath(d)):
                bad.append("%s/%s is not a symlink to skills/%s" % (tool, n, n))
    ex = os.path.join(HERE, "hydo-onboard", "assets", "example-redis-composed", "hydo")
    src = os.path.join(TOP, "hvx", "onboard", "examples", "redis-composed", "hydo")
    d = same_tree(src, ex)
    if d:
        bad.append("assets/example-redis-composed/hydo differs from hvx/onboard/examples/redis-composed/hydo: %s "
                   "(copy it again: rm -r %s && cp -Rp %s %s)" % (", ".join(d), ex, src, os.path.dirname(ex)))
    else:
        print("sync: assets/example-redis-composed/hydo equals hvx/onboard/examples/redis-composed/hydo")
    for b in bad:
        print("FAIL " + b)
    print("skills/check.py: %s" % ("OK" if not bad else "%d problems" % len(bad)))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
