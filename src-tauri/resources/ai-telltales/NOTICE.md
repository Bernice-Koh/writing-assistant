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

Nine of the thirty patterns `SKILL.md` documents are not in `patterns.json`. Each is a structural or
frequency-based habit, not a fixed phrasing, and does not reduce to a phrase or regex match against
one sentence without producing either constant false positives or requiring real parsing this module
does not attempt:

- **10, Rule of Three Overuse**: needs counting parallel items across a sentence, not a fixed phrase.
- **11, Elegant Variation**: needs tracking a repeated referent's cycling synonyms across sentences.
- **12, False Ranges**: "from X to Y" is extremely common in ordinary writing; judging whether X and
  Y form a meaningful scale needs semantic judgement, not syntax.
- **13, Passive Voice and Subjectless Fragments**: reliable passive-voice detection needs
  grammatical parsing beyond phrase or regex matching.
- **15, Overuse of Boldface**: a single bolded phrase is not a tell on its own, per the source's own
  detection guidance; this is a frequency pattern, not a single-span match.
- **17, Title Case in Headings**: needs per-word capitalisation counting against a detected heading
  line, a different scan shape than the sentence-level matching every other entry here uses.
- **26, Hyphenated Word Pair Overuse**: the actual tell is uniform hyphenation regardless of
  grammatical position; a single hyphenated compound like "cross-functional team" is ordinary
  English on its own.
- **29, Fragmented Headers**: needs comparing a heading line against the paragraph that follows it,
  not a single-sentence match.
- **30, Diff-Anchored Writing**: `SKILL.md` gives no fixed "words to watch" list for this pattern,
  only a description of narrating a change rather than describing a thing as it stands.

## Matching notes

Phrase matching in `style::ai_tell` is case-insensitive substring matching. Several entries were
narrowed from `SKILL.md`'s own longer word lists to reduce false positives on ordinary writing in a
tool that flags on every keystroke: pattern 7 (AI vocabulary) drops single very common words such as
"actually", "crucial", and "valuable" that would fire on completely ordinary prose, keeping only the
more distinctive words and phrases; pattern 3 (superficial -ing endings) is matched by regex,
requiring a preceding comma, rather than by the bare gerund words alone, since words like "ensuring"
or "fostering" are unremarkable outside the specific tacked-on-clause construction the pattern
describes.
