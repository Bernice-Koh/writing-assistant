# Spelling variant table provenance

`pairs.json` is hand-written for this project. It vendors no upstream list and carries no
third-party licence. Each pair is a word that English spells two ways by convention, with its form
under each convention the table names. The engine compiles the file in with `include_str!`, as it
compiles the telltale catalogue, and the spelling-variant matcher reads it.

## Conventions

The table names three conventions, in this order:

| Name | Spelling | Examples |
|---|---|---|
| `british` | The spelling of the vendored `en_GB` dictionary, with `-ise` | `organise`, `analyse`, `colour`, `centre`, `travelled` |
| `oxford` | `british` with `-ize` in place of `-ise`, and `-yse` kept | `organize`, `analyse`, `colour`, `centre`, `travelled` |
| `american` | `-ize`, `-yze`, `-or`, `-er`, and a single `l` before a suffix | `organize`, `analyze`, `color`, `center`, `traveled` |

A checker's `preferred_convention` is one of these names, and a checker that names any other does
not compile. The order breaks a tie the first use leaves: where two conventions count alike in a
unit and are first used at the same word, the one listed first is the majority.

## Selection

The starter list holds 52 pairs across `-ise` and `-ize`, `-yse` and `-yze`, `-our` and `-or`,
`-re` and `-er`, `-ence` and `-ense`, a doubled and a single `l`, `-ogue` and `-og`, and a few words
of their own, such as `aluminium` and `pyjamas`. Each inflected form is a pair of its own, since a
word matches whole. The full list is Phase 3's, written when the questionnaire's
spelling-convention answer first uses the table (#59).

A pair is listed only where the vendored `en_GB` dictionary holds its `british` form and no other,
so no form a pair gives another convention is also a British spelling, and a match always marks a
change of convention. A test on the table checks every pair against the dictionary. The dictionary holds both forms of
`practise` and `practice`, `licence` and `license`, `programme` and `program`, `metre` and `meter`,
`tyre` and `tire`, `cheque` and `check`, `kerb` and `curb`, `storey` and `story`, `draught` and
`draft`, and `judgement` and `judgment`, so none of them is listed.

## Matching notes

A word matches a form case-insensitively and whole, as Unicode's word boundaries segment it for
spelling. A possessive `'s`, with a straight or a curly apostrophe, is read off before the match,
so `colour's` matches as `colour`. Each form belongs to one pair only, and each pair names every
convention and holds two distinct forms, which tests on the table enforce.

The matcher reads words and not names, so a proper noun spelt in another convention, as Lincoln
Center is, is a match.
