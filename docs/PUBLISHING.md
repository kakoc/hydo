# Where hydo is published

Every public channel, what it carries, and how it is updated. Publishing is always the user's
decision: no script in this repository tags, uploads or pushes. The steps are in
docs/RELEASING.md.

| Channel | What | Updated by |
|---|---|---|
| GitHub release `v<version>` of https://github.com/kakoc/hydo | the Linux and Mac tarballs, their sources archives, `SHA256SUMS` | docs/RELEASING.md steps 1–7 |
| the release index `hydo/dist/releases.txt` (compiled into hydo) | each published hydo release's archives and guest kernels by sha256 | docs/RELEASING.md step 9 |
| Homebrew tap https://github.com/kakoc/homebrew-tap (`brew install kakoc/tap/hydo`) | the Mac tarball of one release | docs/RELEASING.md step 8 |
| GitHub release `skill-v<version>` | the onboarding skill: `hydo-skill-<version>.tar.gz`, `SHA256SUMS`; read by `hydo skill update` | docs/RELEASING.md, "Releasing the onboarding skill" |
| Claude Code marketplace `.claude-plugin/marketplace.json` on `main` (`/plugin marketplace add kakoc/hydo`, `/plugin install hydo-onboard@hydo`) | the onboarding skill folder `skills/hydo-onboard/` as the plugin `hydo-onboard`, at the version its entry names | the same section, step 3 |

## The onboarding skill

The skill is published separately from hydo (Milestone 170), so its process can improve between
hydo releases while the facts always come from the user's own hydo (`hydo guide`):

- One folder in the Agent Skills format (https://agentskills.io/specification), the same files
  for every tool. Claude Code takes it as a plugin: the marketplace entry's `source` is the folder
  itself (`./skills/hydo-onboard`), and with no `plugin.json` in it the entry is the plugin's
  manifest and the folder's `SKILL.md` its one skill, so nothing is copied or linked. Codex,
  Cursor, GitHub Copilot and Gemini CLI read the copy that `hydo skill install --agent <tool>`
  writes into `.agents/skills/` (Claude Code without the plugin: `.claude/skills/`).
- A user of another tool gets a new skill version with `hydo skill update`, which runs only when
  the user runs it, takes the newest `skill-v<version>` release from the GitHub API, and accepts
  the archive only with the sha256 that the release's `SHA256SUMS` (fetched over TLS) names. The
  skill itself never downloads anything.
- A published skill release is never changed: a fix is a new version. A skill version states the
  oldest hydo it supports (`metadata.hydo-min`); a skill that needs a newer guide topic raises it.
