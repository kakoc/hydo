hydo @VERSION@: the guide built into this hydo. Its facts (the project format, the commands and
their options, the fault kinds, the limits) are the ones this hydo has.

To onboard a project, follow the hydo-onboard skill (the process; it asks this guide for facts):

  Claude Code     /plugin marketplace add kakoc/hydo
                  /plugin install hydo-onboard@hydo        (updates come as plugin updates)
  Codex, Cursor, GitHub Copilot, Gemini CLI
                  hydo skill install --agent codex|cursor|copilot|gemini [--user]
                  hydo skill update                        (fetches the newest skill release)
  any other tool  tell it: "run `hydo guide skill` and follow it"

Start with `hydo guide workflow`. The topics:

@TOPICS@
