# AI-telltale catalog provenance

`patterns.json` is hand-encoded, not vendored verbatim: neither of the two upstream repositories
this catalog traces to ships a machine-readable file. `egc365/signs-of-ai-writing`'s own
`PATTERNS.md` is a flat, unstructured Markdown list of pattern names with no regex, phrase list, or
citation fields, despite that repository's README claiming JSON and YAML output. `egc365/humanizer`
(also referenced as "Blade"), the same author's downstream repackaging, has no structured data file
either: its `SKILL.md` is prose meant for an LLM skill invocation, not a parser.

`SKILL.md` was used as the source for this catalog's entries rather than `PATTERNS.md`, because it
carries the concrete detail `PATTERNS.md` lacks: a "words to watch" phrase list and a before/after
example for each pattern. Both files trace to the same ultimate source, Wikipedia's
[Signs of AI writing](https://en.wikipedia.org/wiki/Wikipedia:Signs_of_AI_writing) page, maintained
by WikiProject AI Cleanup, licensed CC BY-SA. `humanizer`'s own packaging is MIT licensed, per its
`LICENSE` file (copyright Siqi Chen).

One inconsistency worth recording: `PATTERNS.md` and `SKILL.md` do not agree on numbering past
pattern 24. `PATTERNS.md` lists "25. Hyphenated Word Pair Overuse" and "26. Persuasive Authority
Tropes"; `SKILL.md` lists the same two patterns as 26 and 27, with "25. Generic Positive
Conclusions" occupying the slot `PATTERNS.md` leaves for the hyphenation pattern. Every `source`
field in `patterns.json` cites `SKILL.md`'s section numbers, since that is the file the phrase lists
were actually read from.

## Patterns not encoded

Seven of the thirty patterns `SKILL.md` documents are not in `patterns.json`. Each is a structural or
frequency-based habit, not a fixed phrasing, and does not reduce to a phrase or regex match against
one sentence without producing either constant false positives or requiring real parsing this module
does not attempt:

- **11, Elegant Variation**: needs tracking a repeated referent's cycling synonyms across sentences.
- **12, False Ranges**: "from X to Y" is extremely common in ordinary writing; judging whether X and
  Y form a meaningful scale needs semantic judgement, not syntax.
- **13, Passive Voice and Subjectless Fragments**: reliable passive-voice detection needs
  grammatical parsing beyond phrase or regex matching.
- **15, Overuse of Boldface**: a single bolded phrase is not a tell on its own, per the source's own
  detection guidance; this is a frequency pattern, not a single-span match.
- **17, Title Case in Headings**: needs per-word capitalisation counting against a detected heading
  line, a different scan shape than the sentence-level matching every other entry here uses.
- **29, Fragmented Headers**: needs comparing a heading line against the paragraph that follows it,
  not a single-sentence match.
- **30, Diff-Anchored Writing**: `SKILL.md` gives no fixed "words to watch" list for this pattern,
  only a description of narrating a change rather than describing a thing as it stands.

## Matching notes

Each pattern is a rule on the engine's rule layer, written in `patterns.json` in the rule's own
form. A phrasing pattern is a phrase list or a regex, over spans, under the forbidden band, declared
for prose and fragment paragraphs in every register. A structural pattern is measured over a
paragraph or the document, in the modes it declares (see Structural patterns below). Beside its rule
a pattern holds its id, its description, its source citation and its raw flag, and it serves no
Style Card clause. A flag carries the pattern's id in its origin and the pattern's source citation
as its detail.

Phrase matching is case-insensitive and flags every occurrence of a phrase. An edge of a phrase
that is a letter or digit matches only at a word boundary, so `here is a` does not match inside
"there is a", and an apostrophe between two letters is inside a word, so a phrase does not match
beside one. A phrase or regex alternative that `SKILL.md` gives in its `-ize` spelling is accepted
in its `-ise` spelling as well.

Word writes curly apostrophes and quotation marks, and the phrases are written with straight
ones, so a phrase reads each curly mark in the text as its straight form: `you're absolutely right`
matches "you’re absolutely right". A regex reads the text as written, so the one regex that names
an apostrophe, pattern 9's (negative parallelism), names the curly form beside the straight one.

The raw flag turns that mapping off for its pattern, whose phrases then match the text as written.
Pattern 19 (curly quotes) alone carries it: its two phrases are the curly quotation marks
themselves, which the mapping would erase.

A single-word entry carries the prefix flag and is written as its stem, which matches every word
that starts with it and flags the whole word, so `intric` flags "intricately". The stem is the
shortest prefix for which every word that starts with it, among the vendored `en_GB` dictionary's
words with their affixes applied and the Singapore supplement's, is an inflection or a derivative
of the entry's word. A test per stem lists the words it covers and a word the prefix one letter
shorter would wrongly cover:

| Stem | Pattern | Entry it replaces | One letter shorter also covers |
|---|---|---|---|
| `intric` | 7, AI vocabulary | `intricate` and `intricacies` | `intrigue` |
| `pivota` | 7, AI vocabulary | `pivotal` | `pivot`, the word `pivotal` derives from |
| `breatht` | 4, promotional language | `breathtaking` | `breath` |
| `exemplif` | 4, promotional language | `exemplifies` | `exempli`, the Latin of "exempli causa" |

`must-visit` is two words joined by a hyphen and `certainly!` is a word closed by a mark, so
neither is a single-word entry, and both match whole.

Each pattern has a toggle in the settings file, `config.json` under the data root, keyed by its id
under `telltale_toggles`. A pattern the toggles omit takes its default: every pattern is on except
pattern 19, which is off because Word autoformats every straight quotation mark into a curly one.
Pattern 14 (em and en dashes) fires on an en dash in a number range, a second case a toggle serves.

The content hash every flag records beside the ruleset version is the SHA-256 of `patterns.json`'s
bytes followed by those of the variant-pair table,
`src-tauri/resources/spelling-variants/pairs.json`, so an edit to either file is told from a
ruleset change.

A phrase belongs to exactly one pattern, which a test on the catalogue enforces. Two phrases were
first encoded under two patterns each. `boasts a` is kept under pattern 8 (copula avoidance), beside
`boasts an`, and dropped from pattern 4 (promotional language). `you're absolutely right` is kept
under pattern 22 (sycophantic tone) and dropped from pattern 20 (collaborative communication
artifacts).

Several entries were narrowed from `SKILL.md`'s own longer word lists to reduce false positives on
ordinary writing in a tool that flags as the owner types: pattern 7 (AI vocabulary) drops single
very common words such as "actually", "crucial", and "valuable" that would fire on completely
ordinary prose, keeping only the more distinctive words and phrases; pattern 3 (superficial -ing
endings) is matched by regex, requiring a preceding comma, rather than by the bare gerund words
alone, since words like "ensuring" or "fostering" are unremarkable outside the specific
tacked-on-clause construction the pattern describes.

### Structural patterns

A structural pattern counts a habit of frequency over a paragraph or the document, which no single
match shows. Each one's unit, band and modes were decided on 14 October 2026 in #101 and are held
here as catalogue data. Each band is a version-one default, revisited against real documents after
Phase 3. Each structural pattern is on by default, with a toggle of its own.

Pattern 10 (rule of three) is a regex over the paragraph under a count band of at most one,
declared for prose alone. The regex finds a list in one sentence: three or more items separated by
commas, with `and` or `or` before the last and the serial comma optional. Each item between the
first and the last is one to three words, and the match takes the first item's last word and the
last item's first word, so its underline lies within the list and a list of four or more items is
one match. A pair joined by `and` is no match, nor is a list with no conjunction, and a comma must
be followed by a space, so `1,000 and 2,000` is no match. One list of three in a paragraph is
ordinary writing, and the tell is ideas forced into threes again and again, so the band allows one
list and flags each list beyond it. A fragment is left out: a list item or a heading that
enumerates has the structure its form declares, not a habit of prose. The regex cannot tell a list
from an opening word or phrase followed by a pair, as in "In 2020, sales rose and costs fell",
which has a list's shape and matches; under the band it is flagged only as a paragraph's second
match. An item of more than three words between the first and the last breaks the list, so such a
list is missed or read as two.

Pattern 26 (hyphenated word pairs) is a regex over the document under a rate band of at most two
per hundred words, declared for prose and fragment. The regex finds words of letters joined by
single hyphens as one match, `cross-functional` and a chain such as `end-to-end` alike, and the
hyphen-minus, the hyphen and the non-breaking hyphen each join a pair. A number range, a dash
written as two hyphens or as a spaced hyphen, a word with a hyphen at its edge, `pre-` or `-ish`,
and a word holding a digit, `COVID-19`, are no match. A single compound is ordinary English, and
the tell is hyphenation applied across a text, so the band is a rate over the whole document and
not a count: it allows one compound in every fifty words, so a long document keeps its ordinary
compounds, and it flags the matches beyond the count the document's length allows. A rate band
does not measure a unit under fifty words, so a short note raises nothing. The regex matches a
compound in any position and does not tell the attributive position, where the hyphen is
standard, from the predicate position the source names, and it matches a hyphenated name in
running text, such as `llama-server`.
