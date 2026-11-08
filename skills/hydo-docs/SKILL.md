---
name: hydo-docs
description: Write or edit documentation in the hydo repository to its writing standard (docs/STYLE.md). Use when you write or change any Markdown document, README, design note, guide topic or AGENTS.md text in this repository. Gives the rules in brief, the banned words, how to run the Vale check, and the newcomer test.
license: Apache-2.0
metadata:
  version: "1.1.0"
  audience: "contributors"
---

# Write hydo's documentation

This skill is for contributors to the hydo repository. It is not part of the published plugin.
The full standard is `docs/STYLE.md`. Read it once before you write a new page.

## The rules in brief

- Write each page as one kind: a tutorial, a how-to guide, a reference, or an explanation.
- Use plain words and complete sentences. Keep `a`, `an`, and `the`.
- Make a page shorter by covering fewer topics, not by cutting words out of sentences.
- Put one idea in one paragraph, with at most 6 sentences.
- Define every term when you first use it. Do not use project nicknames.
- Make every claim concrete: a command, its real output, or a number from a recorded run.
- Use the active voice and the simple tenses.
- Use at most 20 words in an instruction and 25 words in a description. Start an instruction
  with a verb.
- Use a vertical list when a sentence has more than three items.
- Do not hedge, do not market, and do not end with a summary.

## The banned list

Do not write these words and forms. Vale stops them:

- Marketing words, such as `seamless`, `robust`, `powerful`, `comprehensive`, `cutting-edge`.
- Generated-prose words, such as `leverage`, `delve`, `landscape`, `crucial`, `vital`.
- Filler, such as `it's worth noting`, `please note`, `in today's`, `furthermore`.
- Hedges, such as `might`, `probably`, `perhaps`, `seems to`.
- Intensifiers, such as `very`, `really`, `simply`, `clearly`.
- The contrast `not just X but Y`, rhetorical questions, emoji, and bold in a sentence.

`docs/STYLE.md` has the full list.

## Run the check

Run Vale in the repository root:

```sh
sh ci/vale.sh
```

The command must print `ci/vale.sh: no findings`. To check another file, give its path as an
argument. Fix each finding in the text. If a rule is wrong for one place, use the narrow exception
that `docs/STYLE.md` describes.

## The release docs review

Before every release, a fresh reader checks `README.md` and both quickstarts
(`hydo/dist/linux/QUICKSTART-linux.md`, `hydo/dist/mac/QUICKSTART-mac.md`) against the release
candidate (`docs/RELEASING.md`, step 6a). Give the reader this prompt, and no other context:

```text
You are a developer who has never seen hydo. Read README.md, then the quickstart for your
platform, as if you were deciding whether to use hydo and then trying it. Use the release
candidate in <path to the unpacked tarballs>. Follow every command as written, in a scratch
directory; do not run a command that needs sudo or loads a kernel module, and say which ones
you skipped. For each place where you stopped, guessed, or found the page and the program
disagree, report: the file and line, what you expected, what happened or what was missing, and
a fix in one sentence. Also report every term used before it is defined, every claim without a
command, an output, or a source, and every sentence that is not needed. Do not report style
preferences. End with the findings ordered by how much each one would block a newcomer.
```

Fix each finding, or defer it with a reason in the release notes.

## Do the self-edit pass and the newcomer test

1. Read the page line by line. Delete each sentence that states no fact, repeats another
   sentence, or uses a word that the page does not define.
2. Run every command on the page. For a command that needs a VM, cite the recorded run.
3. Give the page to a reader with no other context. Fix each place where the reader stops.
