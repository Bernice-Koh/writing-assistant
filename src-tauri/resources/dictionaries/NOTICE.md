# Dictionary provenance

`en_GB.aff` and `en_GB.dic` are vendored unmodified from
[LibreOffice/dictionaries](https://github.com/LibreOffice/dictionaries), path `en/`, commit
`e7f163feb2beaf526135132d8716e68e19d2716e` (2025-03-31). Maintained by David Bartlett, Andrew
Brown, and Marco A.G.Pinto, built on Kevin Atkinson's original Pspell and Aspell wordlist.

License: LGPL, as stated in `en_GB.aff`'s own file header ("Released under LGPL") and in the
accompanying `README_en_GB.txt` ("covered by his original LGPL licence", "provided under the
LGPL"). Neither states a specific LGPL version. The `en/` folder's own `license.txt` in the same
source repository is the full text of the GPL version 2, not the LGPL; that file is the blanket
license shown by the LibreOffice Extension Manager for the whole multi-dictionary package (`en_US`,
`en_GB`, `en_CA`, `en_AU`, `en_ZA` together) and does not override the LGPL statement in `en_GB`'s
own file header and README, which speak specifically to this dictionary.

These two files are LGPL, not MIT: this project's own source stays MIT, and these files are kept
as separate, unmodified, clearly attributed data, matching the vendoring approach already used for
the LanguageTool JAR and the AI-telltale catalog.

`en_sg_supplement.dic` is not vendored. It is a plain word list written for this project, one word
per line, no affix rules, checked in addition to `en_GB.dic` before a word is flagged as
misspelled.
