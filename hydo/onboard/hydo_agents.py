"""The AI-facing commands of both hydo command line tools (hvx/dist/hydo, hvx-mac/dist/hydo):

    hydo guide [TOPIC | --list | --path]
    hydo skill install [--agent claude|codex|cursor|copilot|gemini|all ...] [--to DIR] [--user] [--force]
    hydo skill agents | path
    hydo init [--dir DIR] [--name NAME] [--no-agents-md] [--no-gitignore]

One text for every AI tool: the onboarding skill is an Agent Skills folder
(https://agentskills.io/specification) at skills/hydo-onboard/ in hydo's repository and at
skill/hydo-onboard/ in a release. `hydo guide` prints its SKILL.md, so a user of any tool can
say "run `hydo guide` and follow it"; `hydo skill install` copies the folder where each tool
looks for skills; `hydo init` starts a repository's hydo/ folder from the skill's
assets/hydo-init/ template and adds a short section to the repository's AGENTS.md.

Standard library only (Python 3.9+), no state outside the files it writes.
"""
import os
import re
import shutil
import subprocess
import sys

SKILL = "hydo-onboard"

# Where each tool reads Agent Skills, from the tools' own documentation (read 2026-10-26):
#   Claude Code  .claude/skills/<name>, ~/.claude/skills/<name>   https://code.claude.com/docs/en/skills
#   Codex        .agents/skills (cwd up to the repo root), ~/.agents/skills
#                                                                 https://developers.openai.com/codex/skills
#   Cursor       .agents/skills, .cursor/skills (also .claude/skills); ~/.agents/skills, ~/.cursor/skills
#                                                                 https://cursor.com/docs/context/skills
#   Copilot      .github/skills, .claude/skills, .agents/skills; ~/.copilot/skills, ~/.agents/skills
#                https://docs.github.com/en/copilot/concepts/agents/about-agent-skills
#   Gemini CLI   .gemini/skills or .agents/skills; ~/.gemini/skills or ~/.agents/skills
#                                                                 https://geminicli.com/docs/cli/skills/
# .agents/skills is the one directory Codex, Cursor, Copilot and Gemini CLI all read; Claude Code
# reads only .claude/skills.
AGENTS = {
    "claude": (".claude/skills", "Claude Code"),
    "codex": (".agents/skills", "OpenAI Codex"),
    "cursor": (".agents/skills", "Cursor"),
    "copilot": (".agents/skills", "GitHub Copilot (CLI, cloud agent, VS Code)"),
    "gemini": (".agents/skills", "Gemini CLI"),
}
READERS = {
    ".claude/skills": "Claude Code (Cursor and GitHub Copilot read it too)",
    ".agents/skills": "Codex, Cursor, GitHub Copilot and Gemini CLI",
}
GUIDE_TOPICS = {"manifest": "the hydo.yaml and hydo/ folder schema (MANIFEST.md)"}
BEGIN = "<!-- hydo:begin (written by `hydo init`; edits between these markers are replaced) -->"
END = "<!-- hydo:end -->"
BEGIN_RE = re.compile(r"^<!-- hydo:begin\b.*-->[ \t]*$", re.M)
END_RE = re.compile(r"^<!-- hydo:end -->[ \t]*$", re.M)
SKIP = {"__pycache__", ".DS_Store"}         # and AppleDouble "._*" files (skipped())


def skipped(n):
    return n in SKIP or n.startswith("._")


class Fail(Exception):
    pass


def _read(p, n=-1):
    with open(p, encoding="utf-8") as f:
        return f.read(n)


def skill_dir(root):
    """The skill folder: <release>/skill/hydo-onboard, or skills/hydo-onboard in hydo's repository."""
    for d in (os.path.join(root, "skill", SKILL), os.path.join(root, "skills", SKILL)):
        if os.path.isfile(os.path.join(d, "SKILL.md")):
            return d
    raise Fail("the onboarding skill is missing (looked for skill/%s and skills/%s under %s); reinstall hydo"
               % (SKILL, SKILL, root))


def manifest_md(root):
    p = os.path.join(root, "hvx", "onboard", "MANIFEST.md")
    if not os.path.isfile(p):
        raise Fail("MANIFEST.md is missing (%s); reinstall hydo" % p)
    return p


def topics(sd):
    t = {}
    ref = os.path.join(sd, "references")
    for f in sorted(os.listdir(ref)) if os.path.isdir(ref) else []:
        if f.endswith(".md") and not skipped(f):
            with open(os.path.join(ref, f), encoding="utf-8") as fh:
                first = next((l.strip("# \n") for l in fh if l.strip()), "")
            t[f[:-3]] = first
    return t


# ---------------------------------------------------------------- hydo guide
def guide(argv, root, out=sys.stdout):
    sd = skill_dir(root)
    if not argv:
        out.write(_read(os.path.join(sd, "SKILL.md")))
        return 0
    a = argv[0]
    if a in ("-h", "--help"):
        out.write("usage: hydo guide [TOPIC | FILE | --list | --path]\n"
                  "  no argument: the onboarding skill's SKILL.md (give it to any AI tool: \"run `hydo guide` and follow it\")\n"
                  "  TOPIC: one of `hydo guide --list`;  FILE: a path inside the skill folder, e.g. assets/example-miniflux.yaml\n")
        return 0
    if a == "--path":
        out.write(sd + "\n")
        return 0
    t = topics(sd)
    if a == "--list":
        out.write("%-29s %s\n" % ("hydo guide", "the onboarding skill (SKILL.md)"))
        for k, v in sorted(dict(t, **GUIDE_TOPICS).items()):
            out.write("%-29s %s\n" % ("hydo guide " + k, v))
        out.write("%-29s %s\n" % ("hydo guide <file>", "any file of the skill folder (%s)" % sd))
        return 0
    if a == "manifest":
        out.write(_read(manifest_md(root)))
        return 0
    name = a[:-3] if a.endswith(".md") and "/" not in a else a
    if name in t:
        out.write(_read(os.path.join(sd, "references", name + ".md")))
        return 0
    p = os.path.realpath(os.path.join(sd, a))
    if p.startswith(os.path.realpath(sd) + os.sep) and os.path.isfile(p):
        out.write(_read(p))
        return 0
    raise Fail("no guide topic %r (hydo guide --list)" % a)


# ---------------------------------------------------------------- hydo skill
def repo_root(start):
    try:
        r = subprocess.run(["git", "-C", start, "rev-parse", "--show-toplevel"], stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL, text=True)
        if r.returncode == 0 and r.stdout.strip():
            return r.stdout.strip()
    except OSError:
        pass
    return os.path.abspath(start)


def is_hydo_skill(d):
    try:
        head = _read(os.path.join(d, "SKILL.md"), 2048)
    except OSError:
        return False
    return re.search(r"^name:\s*%s\s*$" % SKILL, head, re.M) is not None


def copy_skill(src, dst, force, out):
    """Copy the skill folder to dst (a directory named hydo-onboard). An earlier copy of this
    skill is replaced; anything else at dst is left alone unless --force."""
    if os.path.islink(dst):
        raise Fail("%s is a symbolic link (to %s); not replacing it. Remove it or install with --to elsewhere"
                   % (dst, os.readlink(dst)))
    if os.path.exists(dst):
        if os.path.realpath(dst) == os.path.realpath(src):
            raise Fail("%s is the skill's own folder" % dst)
        if not is_hydo_skill(dst) and not force:
            raise Fail("%s exists and is not the %s skill; not replacing it (use --force)" % (dst, SKILL))
        shutil.rmtree(dst)
        verb = "updated"
    else:
        verb = "installed"
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    shutil.copytree(src, dst, ignore=lambda d, names: [n for n in names if skipped(n)])
    out.write("hydo: %s %s\n" % (verb, dst))


def skill(argv, root, out=sys.stdout):
    a = list(argv)
    if not a or a[0] in ("-h", "--help"):
        out.write(
            "usage: hydo skill install [--agent NAME ...] [--to DIR] [--user] [--force]\n"
            "       hydo skill agents        where each AI tool reads skills\n"
            "       hydo skill path          the skill folder of this installation\n"
            "  --agent claude|codex|cursor|copilot|gemini|all (repeatable, or comma-separated): copy the skill\n"
            "          into that tool's skill directory of this repository (--user: of your home directory)\n"
            "  --to DIR: copy it to DIR/%s instead (any other tool, or a custom location)\n"
            "  An earlier copy of the skill is replaced; another folder of the same name is kept unless --force.\n"
            "  Alternative for many tools at once: the Vercel `skills` CLI, `npx skills add %s`\n"
            % (SKILL, "<this skill folder>"))
        return 0 if a else 2
    sd = skill_dir(root)
    cmd = a.pop(0)
    if cmd == "path":
        out.write(sd + "\n")
        return 0
    if cmd == "agents":
        out.write("agent     project directory          user directory\n")
        for k, (d, who) in AGENTS.items():
            out.write("%-9s %-26s ~/%-22s %s\n" % (k, d + "/" + SKILL, d + "/" + SKILL, who))
        out.write("all       both directories above\n")
        out.write("(Cursor and GitHub Copilot also read .claude/skills; with both installed they may list the skill twice.)\n")
        return 0
    if cmd != "install":
        raise Fail("unknown `hydo skill %s` (hydo skill --help)" % cmd)
    agents, to, user, force = [], None, False, False
    while a:
        x = a.pop(0)
        if x == "--agent" and a:
            agents += [y for y in a.pop(0).split(",") if y]
        elif x.startswith("--agent="):
            agents += [y for y in x.split("=", 1)[1].split(",") if y]
        elif x == "--to" and a:
            to = a.pop(0)
        elif x == "--user":
            user = True
        elif x == "--force":
            force = True
        else:
            raise Fail("unknown argument %r (hydo skill --help)" % x)
    if not agents and to is None:
        raise Fail("say where: --agent claude|codex|cursor|copilot|gemini|all, or --to DIR (hydo skill --help)")
    if "all" in agents:
        agents = list(AGENTS)
    bad = [x for x in agents if x not in AGENTS]
    if bad:
        raise Fail("unknown agent %s (known: %s, all)" % (", ".join(bad), ", ".join(AGENTS)))
    if to is not None and user:
        raise Fail("--user and --to exclude each other")
    targets = []
    if to is not None:
        targets.append(os.path.join(os.path.abspath(os.path.expanduser(to)), SKILL))
    base = os.path.expanduser("~") if user else repo_root(os.getcwd())
    for d in sorted({AGENTS[x][0] for x in agents}):
        targets.append(os.path.join(base, d, SKILL))
    for t in targets:
        copy_skill(sd, t, force, out)
        rel = next((k for k in READERS if t.endswith(os.path.join(k, SKILL))), None)
        if rel:
            out.write("      read by %s\n" % READERS[rel])
    if not user and to is None:
        out.write("hydo: commit the copied folder so that everyone working in the repository gets the skill\n")
    return 0


# ---------------------------------------------------------------- hydo init
def project_name(d):
    n = re.sub(r"[^a-z0-9-]+", "-", os.path.basename(os.path.abspath(d)).lower()).strip("-")
    n = re.sub(r"-+", "-", n)
    if not n or not n[0].isalpha():
        n = "app-" + n if n else "app"
    return n


def agents_block(sd):
    text = _read(os.path.join(sd, "assets", "hydo-init", "agents-section.md")).strip("\n")
    return "%s\n%s\n%s\n" % (BEGIN, text, END)


def update_agents_md(path, block):
    """Insert or replace the hydo section between the markers; never touch other content.
    Returns 'created', 'added', 'updated' or 'unchanged'."""
    if not os.path.exists(path):
        with open(path, "w", encoding="utf-8") as f:
            f.write("# Instructions for coding agents\n\n" + block)
        return "created"
    old = _read(path)
    b, e = BEGIN_RE.search(old), END_RE.search(old)
    if b and e and e.start() > b.start():
        new = old[:b.start()] + block.rstrip("\n") + old[e.end():]
        if not new.endswith("\n"):
            new += "\n"
        if new == old:
            return "unchanged"
        with open(path, "w", encoding="utf-8") as f:
            f.write(new)
        return "updated"
    if b or e:
        raise Fail("%s has only one of the hydo markers; fix it by hand (both or neither)" % path)
    sep = "" if old.endswith("\n\n") else ("\n" if old.endswith("\n") else "\n\n")
    with open(path, "w", encoding="utf-8") as f:
        f.write(old + sep + block)
    return "added"


def update_gitignore(path, lines):
    have = _read(path).splitlines() if os.path.exists(path) else []
    norm = {l.strip().lstrip("/").rstrip("/") for l in have}
    add = [l for l in lines if l.strip("/") not in norm]
    if not add:
        return []
    old = _read(path) if os.path.exists(path) else ""
    with open(path, "a", encoding="utf-8") as f:
        if old and not old.endswith("\n"):
            f.write("\n")
        f.write("# hydo (hydo init): the build cache and fetched images\n" + "".join(l + "\n" for l in add))
    return add


def init(argv, root, out=sys.stdout):
    a = list(argv)
    d, name, do_agents, do_ignore = os.getcwd(), None, True, True
    while a:
        x = a.pop(0)
        if x in ("-h", "--help"):
            out.write("usage: hydo init [--dir DIR] [--name NAME] [--no-agents-md] [--no-gitignore]\n"
                      "  creates DIR/hydo/ (hydo.yaml skeleton, workload/, checks/, regressions/) where missing,\n"
                      "  adds .hydo/ and hydo/images/ to .gitignore, and a hydo section to AGENTS.md (between markers).\n"
                      "  Existing files are never overwritten; running it again changes nothing.\n")
            return 0
        if x == "--dir" and a:
            d = a.pop(0)
        elif x == "--name" and a:
            name = a.pop(0)
        elif x == "--no-agents-md":
            do_agents = False
        elif x == "--no-gitignore":
            do_ignore = False
        else:
            raise Fail("unknown argument %r (hydo init --help)" % x)
    d = os.path.abspath(d)
    if not os.path.isdir(d):
        raise Fail("no such directory: %s" % d)
    name = name or project_name(d)
    if not re.fullmatch(r"[a-z][a-z0-9-]*", name):
        raise Fail("--name: lowercase letters, digits and '-', starting with a letter (got %r)" % name)
    sd = skill_dir(root)
    tpl = os.path.join(sd, "assets", "hydo-init", "hydo")
    made, kept = [], []
    for top, dirs, files in os.walk(tpl):
        dirs.sort()
        rel = os.path.relpath(top, tpl)
        dst_dir = os.path.normpath(os.path.join(d, "hydo", rel))
        os.makedirs(dst_dir, exist_ok=True)
        for f in sorted(files):
            if skipped(f):
                continue
            src, dst = os.path.join(top, f), os.path.join(dst_dir, f)
            relp = os.path.relpath(dst, d)
            if os.path.lexists(dst):
                kept.append(relp)
                continue
            if rel in ("workload", "checks") and os.path.isdir(dst_dir) and \
                    any(not n.startswith(".") and not n.startswith("helper_") for n in os.listdir(dst_dir)):
                kept.append(relp + " (not added: the folder already has files)")
                continue
            text = _read(src)
            with open(dst, "w", encoding="utf-8") as fh:
                fh.write(text.replace("@NAME@", name))
            shutil.copymode(src, dst)
            made.append(relp)
    for p in made:
        out.write("hydo: created %s\n" % p)
    for p in kept:
        out.write("hydo: kept    %s\n" % p)
    if do_ignore:
        for l in update_gitignore(os.path.join(d, ".gitignore"), [".hydo/", "hydo/images/"]):
            out.write("hydo: added %s to .gitignore\n" % l)
    if do_agents:
        r = update_agents_md(os.path.join(d, "AGENTS.md"), agents_block(sd))
        out.write("hydo: AGENTS.md: hydo section %s\n" % r)
        for c in ("CLAUDE.md", os.path.join(".claude", "CLAUDE.md")):
            p = os.path.join(d, c)
            if os.path.isfile(p) and "@AGENTS.md" not in _read(p):
                out.write("hydo: note: Claude Code reads %s instead of AGENTS.md; add the line `@AGENTS.md` to it "
                          "(https://code.claude.com/docs/en/memory#agents-md)\n" % c)
    compose = [f for f in ("docker-compose.yaml", "docker-compose.yml", "compose.yaml", "compose.yml")
               if os.path.isfile(os.path.join(d, f))]
    out.write("hydo: next: give your AI tool this instruction: \"run `hydo guide` and follow it\"\n")
    if compose:
        out.write("hydo:       this repository has %s: hydo compose %s --out hydo/hydo.yaml --images-dir images\n"
                  % (compose[0], compose[0]))
    out.write("hydo:       check the folder with: hydo build hydo/hydo.yaml --check\n")
    return 0


def run(cmd, argv, root):
    """Entry point for the CLIs: returns an exit status, prints `hydo: <message>` on a failure."""
    try:
        rc = {"guide": guide, "skill": skill, "init": init}[cmd](argv, root)
        sys.stdout.flush()
        return rc
    except Fail as e:
        print("hydo: %s" % e, file=sys.stderr)
        return 1
    except BrokenPipeError:            # `hydo guide | head`: the reader stopped early
        os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        return 0
