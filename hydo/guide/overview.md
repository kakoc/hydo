hydo @VERSION@: the guide built into this hydo. Its facts (the project format, the commands and
their options, the fault kinds, the limits) are the ones this hydo has.

To onboard a project, run `hydo onboard` in its repository. It starts the hydo/ folder, installs
the onboarding skill for the AI tool it finds, and prints the next step. The skill is the
process; it asks this guide for facts. To get the skill by hand:

  Claude Code     /plugin marketplace add kakoc/hydo
                  /plugin install hydo@hydo                (the command is /hydo:onboard;
                                                            updates come as plugin updates)
                  or: hydo skill install --agent claude    (.claude/skills/hydo, the same plugin)
  Codex, Cursor, GitHub Copilot, Gemini CLI
                  hydo skill install --agent codex|cursor|copilot|gemini [--user]
                  hydo skill update                        (fetches the newest skill release)
  any other tool  tell it: "run `hydo guide skill` and follow it"

Start with `hydo guide workflow`. The topics:

@TOPICS@
