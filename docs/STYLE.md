# Writing standard

This page is the writing standard for every document in this repository. It applies to people
and to coding agents. The skill `skills/hydo-docs/` gives the same rules in brief.

The standard has three sources:

- ASD-STE100 Simplified Technical English (STE) for instructions, how-to steps and reference
  text. STE is the controlled English of aerospace maintenance manuals.
- The Google developer documentation style guide (https://developers.google.com/style) for
  explanation pages, where longer prose is allowed.
- Diátaxis (https://diataxis.fr) for the structure of the documentation.

Vale, a prose linter, checks the rules that a program can check. The section
[How the standard is checked](#how-the-standard-is-checked) says how to run it.

## One page, one kind

Diátaxis sorts documentation into four kinds. Each kind answers one need of the reader. Write
each page as one kind, and link to the other kinds.

| Kind | The reader wants to | Examples in this repository |
|---|---|---|
| Tutorial | learn by doing one complete task | the release's quickstart |
| How-to guide | reach one goal | `docs/RELEASING.md`, `docs/PUBLISHING.md` |
| Reference | look up a fact | `hydo/onboard/MANIFEST.md`, `docs/BUILD-PINS.md`, `docs/REBASELINE.md` |
| Explanation | understand why | `docs/ARCHITECTURE.md`, `docs/design/*.md` |

`README.md` is the entry page. It says what hydo does, shows one real run, and links to the four
kinds. It is short: the install details, the limits, and the words a user needs for their own
project are on their own pages (`docs/INSTALL.md`, `docs/LIMITS.md`, `docs/YOUR-PROJECT.md`).
The entry page may open with up to three questions in a quote, speak to the reader directly, and
use an image. It explains a word where the word first matters, not in a list at the top. Every
claim on it still needs a recorded run.

## Rules for every page

- Use plain words. Write `use`, not `leverage`. Write `check`, not `validate the correctness of`.
- Write complete sentences. Keep the articles `a`, `an`, and `the`. Do not compress a sentence
  into a fragment.
- Make a page shorter by covering fewer topics. Do not make it shorter by cutting words out of
  sentences.
- Put one idea in one paragraph.
- Define every term when you first use it. Name the thing by what it is.
- Do not use project nicknames. A nickname is a name that only the project's authors know. Write
  `the order service` for the gRPC service of the example stack, and `the bare-metal machine`
  for the machine that runs hvx.
- Make every claim concrete. Give the command, the real output, or a number from a real run.
  Name where the run is recorded, for example a section of `docs/REBASELINE.md`.
- Use the active voice. Write `hydo replays the run`, not `the run is replayed`.
- Do not hedge. If a fact is not known, say what was not checked.
- Do not market. Say what hydo does and what it does not do.

## Rules for instructions and reference text (STE)

These rules come from ASD-STE100. They apply to how-to steps, to reference pages, and to the
rule lists in `AGENTS.md`.

- Use at most 20 words in an instruction and at most 25 words in a description.
- Use the simple tenses: past, present, and future.
- Give one instruction in one sentence. Start the instruction with a verb.
- Use at most 6 sentences in one paragraph.
- Use one word for one meaning. Use the same word for the same thing on the whole page.
- Use a vertical list when a sentence has more than three items or steps.
- Do not use idioms, slang, filler, or humor.
- Keep names, code, commands, file paths, and error text exact.

Start a warning with the instruction, then give the risk:

> Do not reload `hvx.ko` while another session runs a VM. The reload stops that run.

## Rules for explanation pages (Google style)

Explanation pages can use longer sentences when the reasoning needs them. The rules for every
page still apply. Vale stops any sentence of more than 30 words, on every page.

- Explain one design decision per section. Give the reason and the evidence.
- Use `you` for the reader. Use `hydo` for the program, not `we`.
- Write headings in sentence case: `How a run is replayed`, not `How A Run Is Replayed`.
- Use the serial comma: `kills, pauses, and power cuts`.

## Banned words and phrases

Vale stops these words. They are marketing words, filler, or habits of generated prose.

- Marketing words: `seamless`, `robust`, `powerful`, `comprehensive`, `cutting-edge`,
  `state-of-the-art`, `best-in-class`, `world-class`, `next-generation`, `game-changer`,
  `effortless`, `blazing fast`, `supercharge`, `revolutionize`, `empower`, `unlock`, `elevate`,
  `streamline`, `unparalleled`, `unprecedented`, `holistic`, `synergy`, `paradigm`, `amazing`,
  `awesome`, `exciting`, `elegant`, `magic`, `beautiful`.
- Words of generated prose: `leverage`, `delve`, `deep dive`, `dive into`, `landscape`,
  `tapestry`, `plethora`, `myriad`, `embark`, `boasts`, `crucial`, `pivotal`, `vital`,
  `ever-evolving`, `harness the power`.
- Filler: `it's worth noting`, `it should be noted`, `it is important to note`, `please note`,
  `needless to say`, `as you can see`, `as mentioned above`, `in today's`, `furthermore`,
  `moreover`, `let's`.
- Empty closing summaries: `in conclusion`, `in summary`, `to summarize`, `to sum up`,
  `all in all`, `at the end of the day`. End a page when its last fact is told.
- Hedges: `perhaps`, `might`, `probably`, `possibly`, `arguably`, `presumably`, `somewhat`,
  `fairly`, `quite`, `rather`, `seems to`, `appears to`, `tends to`.
- Intensifiers and false ease: `very`, `really`, `extremely`, `truly`, `basically`,
  `essentially`, `actually`, `simply`, `easily`, `obviously`, `clearly`, `of course`,
  `significantly`, `highly`.
- The contrast `not just X but Y`. State Y.
- Rhetorical questions in body text. A heading can ask the question that its section answers.
- Emoji.
- Bold words in the middle of a sentence. Use bold only for the lead-in of a list item.
- Stacked adjectives, such as `a fast, simple, reliable tool`. Vale cannot find these, so the
  self-edit pass must find them.

## How the standard is checked

Run Vale on the documents that the standard covers:

```sh
sh ci/vale.sh
```

The script downloads Vale 3.25.1 and two style packages once, and checks the sha256 of each
download. Then it runs Vale with `.vale.ini`. Every warning and every error fails the check. The
workflow `.github/workflows/docs.yml` runs the same script on every push that changes a
document or the Vale files.

The covered documents are the list `DOCS` in `ci/vale.sh`: the top-level documents, both
quickstarts, the package pages, and the topics of `hydo guide`. `sh ci/vale.sh --list` prints
them. The list `EXCEPT` names the few files that Vale skips, each with its reason. A file is an
exception only when a fix would change a code example's text, a file inside a guest image, or
output that a test pins.

Two more checks read the same documents:

- `cargo test -p hydo --test it docs::` checks every relative link and anchor, offline. It also
  checks the facts of `README.md` and the quickstarts: the version they name, the fingerprints
  they cite, and the `hydo` commands and options they show. It runs the cheap commands of the
  README in a temporary directory.
- `sh ci/links-external.sh` checks the http and https links. It needs the network, so CI runs it
  only every night and on demand.

To check other files, give them as arguments:

```sh
sh ci/vale.sh docs/CI.md
```

Vale uses three styles:

- `hydo` (`.vale/styles/hydo/`, tracked): the banned list, hedges, intensifiers, the
  `not just X but Y` contrast, sentences of more than 30 words, questions in body text, emoji,
  bold in a sentence, and the known nicknames.
- `Google` 0.7.1: the Google style guide's checks. Examples are heading case, the serial comma,
  the words `we` and `will`, and Latin abbreviations such as `e.g.`.
- `proselint` 0.3.4: clichés, jargon, corporate speak, and other errors of usage.

When Vale reports a finding, change the text. If the rule is wrong for one place, add a narrow
exception and give the reason in the same comment:

```markdown
<!-- vale Google.Will = NO -->
<!-- The release notes describe a future step; "will" is correct here. -->
The next release will ship `guest/hydo-sql`.
<!-- vale Google.Will = YES -->
```

A product name that a capitalization check rejects goes into
`.vale/styles/config/vocabularies/hydo/accept.txt`. Do not switch a rule off for a whole file.

## The self-edit pass

Read the page line by line before you commit it. Test each sentence:

1. The sentence states a fact, a command, or a number.
2. The page needs the sentence. Delete a sentence that repeats another one.
3. A newcomer knows every word in the sentence, or the page defines the word before it.

Delete a sentence that fails a test. Do not reword it to pass.

## The newcomer test

A page passes the newcomer test when a reader who knows only the page can do what it describes.
Before you merge a new page, give it to a reader who has not seen the project. This reader can be
a person or a fresh agent with no other context. Ask the reader to follow the page and to report
each place where they stopped. Fix each place, then run the test again.

Every command on a page must run as written. If a command needs a VM, cite the recorded run that
gave its output, for example `docs/REBASELINE.md` section 13.4.
