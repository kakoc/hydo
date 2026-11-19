# Update hydo and its onboarding skill

`hydo update` brings hydo and every copy of its onboarding skill to the newest release. The
onboarding skill is the process an AI tool follows to write a project's `hydo/` folder, and it
has releases of its own. `hydo update` uses the network only when you run it.

## See what is newer

```sh
hydo update --check
```

It prints one line for hydo and one line for each copy of the skill, each with the version it
has and the version of the newest release. It changes nothing. The exit status is 0 when
everything is current, and 3 when something is newer.

When hydo itself is older, the plan also prints the "Before you upgrade" section of the release
notes of each newer release. Read it before you go on: it says, for example, when images get new
fingerprints and must be built again after the update.

## Update

```sh
hydo update --yes
```

Without `--yes`, `hydo update` in a terminal prints the plan and asks once. Outside a terminal,
for example in an AI tool's shell, it prints the plan, changes nothing, and exits with status 3.
The AI tool then shows the plan to you and runs `hydo update --yes` after you agree.

`--version V` installs hydo V instead of the newest release.

## How hydo is updated

`hydo update` reads how hydo was installed from the path of its own program:

- **Homebrew.** It runs `brew upgrade kakoc/tap/hydo`, after `brew update` when the formula is
  older than the release. The formula runs `hydo setup` itself.
- **cargo.** It runs `cargo install hydo --locked --version V` into the same folder, then the new
  hydo's `hydo setup`. `hydo setup` downloads the release's guest files into `~/.hydo`.
- **The release archive** (`install.sh`, by default into `~/.hydo`). It downloads only this
  platform's `hydo-<mac|linux>-V.tar.gz` and the release's `SHA256SUMS` over HTTPS. It installs
  the archive only when its sha256 is the one that `SHA256SUMS` lists. It unpacks the archive in
  a temporary folder and runs its `install.sh` with the folder of your installation.
  - On Linux it passes `--no-module` unless the kernel module `hvx` is loaded. When it is loaded,
    `install.sh` builds and loads the new module with `sudo`, and `sudo` asks for your password.
    `hydo update` says so before it starts.
  - When the udev rule of `/dev/hvx` is installed, it passes `--udev` with the rule's group.
- **The install cache** (`hydo setup --get`, `~/.hydo/versions/<version>`). It downloads and
  checks the archive in the same way, then runs the new hydo's `hydo setup --archive` and `hydo
  setup --get V`.
- **A hydo built in a checkout** of hydo's repository. It changes nothing and prints the commands
  that update it.

Then it runs the new hydo's `hydo doctor` (`install.sh` runs it itself). A failed download, or
an archive with another sha256, leaves the installed hydo as it was.

## Where the skill is updated

Every copy is brought to the newest skill release (tag `skill-v<version>`), checked by sha256
in the same way as `hydo skill update` checks it:

- the copies in `.claude/skills/hydo` and `.agents/skills/onboard` of your home and of the
  repository you run `hydo update` in;
- the `skills/hydo` folder of each Claude Code configuration folder. These folders are
  `CLAUDE_CONFIG_DIR`, `~/.claude`, and every other `~/.claude*` folder that holds `plugins/` or
  `settings.json`;
- every copy that hydo wrote before, also with `--to DIR` or in another project.
  `hydo skill install`, `hydo skill update` and `hydo onboard` record each copy in
  `~/.hydo/skill-copies.txt`, except a copy in the temporary directory. A line whose copy is
  gone is dropped.
- Claude Code's plugin `hydo@hydo` from hydo's marketplace, in each configuration folder that
  lists it. With the `claude` command on `PATH`, hydo runs `claude plugin marketplace update hydo`
  and `claude plugin update hydo@hydo --scope <scope>`, with `CLAUDE_CONFIG_DIR` set to that folder.
  For a project install, the command runs in the project's folder.

In a Claude Code configuration folder, hydo reads only the version of the hydo skill and the
entries of the plugin `hydo@hydo`. It does not read or print other settings or plugins.

A copy made by a hydo older than the registry is found only in the folders that the preceding list names.
Run `hydo skill update` once in such a project: it records the project's copies.

A session of an AI tool that runs keeps the skill it loaded. Start a new session after the
update.

## Upgrading from hydo 0.4.0 or older

Those versions have no `hydo update`, so this one upgrade is done by hand, the way hydo was
installed:

- Homebrew: `brew update && brew upgrade kakoc/tap/hydo`.
- cargo: `cargo install hydo --locked`, then `hydo setup`.
- The release archive: download this platform's archive and `SHA256SUMS` from
  https://github.com/kakoc/hydo/releases, check the archive with `shasum -a 256 -c
  --ignore-missing SHA256SUMS` (Linux: `sha256sum -c --ignore-missing SHA256SUMS`), unpack it,
  and run its `install.sh` (on Linux add `--no-module` when you use the KVM engine). Do not
  download the `-sources.tar.gz` archives: they hold the sources of the third-party software,
  not hydo.

Then update the skill: `hydo skill update` for the copies, and `claude plugin marketplace update
hydo` and `claude plugin update hydo@hydo` for Claude Code's plugin. From then on, `hydo update`
does all of it.

## The line in hydo doctor

`hydo doctor` prints one line when a newer hydo release exists, with the command `hydo update`.
It looks at most once a day, waits at most 3 seconds, and keeps the answer in
`~/.hydo/update-check.json`. Offline it prints nothing and does not fail. `HYDO_OFFLINE=1`, or
`HYDO_UPDATE_CHECK=0`, turns the look off.
