# Style Guide

Prose conventions for everything committed to this repository: README, CLAUDE.md, docs/,
issue and PR text, and comment prose in code. Code structure and language mechanics are
`docs/CODE_STYLE.md`; this file is about how the words read.

## Spelling

UK/Commonwealth spelling throughout: organise, colour, licence (noun), practise (verb),
travelled. Do not normalise a file to US spelling to match another file that predates this
rule; fix the older file instead.

## Direct prose

Say the thing in the fewest words that state it plainly. No flowery or indirect language, no
throat-clearing before the point, no padding a short fact into a long paragraph.

State things flatly, including under uncertainty. Confidence markers such as "likely",
"probably", "I think", or "seems to" do not appear in committed prose. If something is
genuinely unconfirmed, say so as an explicit open question ("Not yet decided: ...", "Open
question: ...") rather than folding a hedge into a sentence that otherwise reads as settled.

## No em dash, no section sign

Neither character appears anywhere in committed content, comments included. Use a comma, a
colon, a full stop and a new sentence, or a parenthetical instead of an em dash. Use the word
"Section" or a heading link instead of the section sign.

## Comments and documentation are not addressed to a reader

Comment and documentation prose states facts. It is never phrased as an instruction, aside,
or explanation directed at "you" the developer or "you" the user, and it is never written in
a teaching tone that walks a newcomer through background they don't need for the task at
hand. A comment that would only make sense read aloud to someone is in the wrong register;
rewrite it as a plain statement of the why.

This does not apply to CLAUDE.md and other files whose entire purpose is operational
instruction to Claude Code; that is a different genre from documentation prose.

## Dogfooding

Committed prose holds itself to the same standard the product enforces on a user's draft.
Once the AI-tell detector exists, committed docs are checked against it like any other text.
Until then, the check is manual: avoid the patterns catalogued in the vendored
signs-of-ai-writing data (`src-tauri/resources/ai-telltales/`) the same way the product would
flag them in a user's writing. In particular: no throat-clearing openers, no "it's not just X,
it's Y" parallelism, no summary paragraph that restates what was just said, no reflexive
rule-of-three lists, no "leverage", "utilise", "delve into", "robust", or "seamless" reached
for out of habit rather than accuracy.

## Length

No fixed ceiling on a document's length. A document is as long as its subject requires and no
longer; padding a document to make it look thorough is the failure mode to avoid in the other
direction.
