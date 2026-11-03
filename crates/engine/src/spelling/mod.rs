//! Spelling checks through spellbook, which reads the vendored Hunspell-format dictionary pair:
//! `en_GB` in its `-ise` form, with a Singapore word list on top. The word list is plain words
//! merged into the same dictionary, with no affix file of its own. Provenance and licensing for
//! the vendored dictionary pair are recorded in `src-tauri/resources/dictionaries/NOTICE.md`.
//!
//! Spelling skips a token shaped like a URL, a path or an identifier, whatever the form mode of
//! the paragraph that holds it, since such a token is not prose. The token is the run of
//! non-whitespace characters that holds the word, and it is skipped when it holds a scheme
//! separator, a slash or a backslash, an at sign, an equals sign or an ampersand, or an
//! underscore, when it holds a letter and a dot between two characters that are each a letter or a
//! digit, or when it has a capital letter directly after a lowercase one. A full stop after the
//! token's last letter, as at the end of a sentence, sits between a letter and nothing, and a
//! number such as `3.5` holds no letter, so neither is skipped. Only the token is skipped: a
//! misspelt word beside it is still flagged.

pub mod error;
mod tokens;

use std::collections::hash_map::DefaultHasher;
use std::hash::BuildHasherDefault;
use std::path::Path;

use spellbook::Dictionary;
use unicode_segmentation::UnicodeSegmentation;

pub use error::SpellingError;

use crate::flag::{Flag, FlagOrigin, Span};

/// Corrections kept for a single misspelling, out of spellbook's full suggestion list: past a
/// handful, later suggestions are rarely the one the owner meant. Spellbook has computed the
/// whole list by the time it is cut, so the cap bounds what a flag carries to the hover card and
/// saves the deterministic stage no time.
const MAX_SUGGESTIONS: usize = 5;

/// The apostrophe Word types in place of the straight one, U+2019.
const CURLY_APOSTROPHE: char = '\u{2019}';

const STRAIGHT_APOSTROPHE: &str = "'";

/// Wraps a loaded `en_GB` dictionary with the Singapore supplement merged in through
/// [`Dictionary::add`], so a supplement word is checked and suggested through the same lookup as
/// any other word, with no second word set to keep in sync.
pub struct SpellChecker {
    dictionary: Dictionary<BuildHasherDefault<DefaultHasher>>,
}

impl SpellChecker {
    /// Loads the `en_GB` `.aff`/`.dic` pair from `aff_path` and `dic_path`, then merges each
    /// non-empty line of `supplement_path` into the same dictionary.
    pub fn load(
        aff_path: &Path,
        dic_path: &Path,
        supplement_path: &Path,
    ) -> Result<Self, SpellingError> {
        let aff = read_to_string(aff_path)?;
        let dic = read_to_string(dic_path)?;
        // Spellbook's suggester keeps its best roots in the order the word table yields them, so
        // the default hasher's random seed makes a suggestion list differ between processes. A
        // hasher with no seed makes the table's order, and with it every suggestion list, a
        // function of the dictionary files alone.
        let mut dictionary = Dictionary::new_with_hasher(&aff, &dic, BuildHasherDefault::default())
            .map_err(|error| SpellingError::ParseDictionary(error.to_string()))?;

        let supplement = read_to_string(supplement_path)?;
        for word in supplement
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            dictionary
                .add(word)
                .map_err(|error| SpellingError::ParseSupplementWord {
                    word: word.to_string(),
                    message: error.to_string(),
                })?;
        }

        Ok(Self { dictionary })
    }

    /// Checks every alphabetic word in `text` outside a skipped token, by the rule on this module,
    /// against the dictionary, returning one [`Flag`] per word absent from both `en_GB` and the
    /// Singapore supplement, each carrying the corrections spellbook suggests, up to
    /// `MAX_SUGGESTIONS`.
    ///
    /// Each flag anchors on `text` itself, carrying the word's own offset into it, rather than on
    /// the bare misspelled word. [`crate::capture::Capture::span_rect`] and
    /// [`crate::capture::Capture::replace`] both resolve an anchor at the occurrence its index
    /// names, and the analyzer counts that index over the text before the sentence, so a word
    /// misspelled twice in one sentence and anchored on itself produces two flags with the same
    /// anchor and index, addressing one occurrence: one underline drawn twice and the second
    /// misspelling left unmarked. Grammar
    /// flags already anchor on their sentence for the same reason (see `languagetool::client`).
    ///
    /// Capitalisation is left to `Dictionary::check`, whose casing rules match Hunspell's on the
    /// vendored dictionary, supplement words included: a word stored in lower case is accepted in
    /// lower case, with an initial capital and in capitals; one stored with an initial capital,
    /// such as `London`, is accepted in that form and in capitals and flagged in lower case; one
    /// stored in capitals, such as `HDB`, is flagged in lower case.
    ///
    /// The curly apostrophe Word types, U+2019, is looked up as the straight one through the affix
    /// file's `ICONV` line, which spellbook applies, so a misspelt contraction is flagged whole,
    /// apostrophe included, at its offset in the text as written. The `OCONV` line gives every
    /// suggestion the curly apostrophe, and a suggestion keeps the form the flagged word was typed
    /// with: the curly one stays only where the word holds a curly apostrophe and no straight one,
    /// and is mapped back to the straight one otherwise, so accepting a suggestion never changes
    /// the apostrophes a text is typed with.
    pub fn check(&self, text: &str) -> Vec<Flag> {
        let skipped_tokens = tokens::skipped_tokens(text);
        let in_skipped_token = |byte_start: usize| {
            skipped_tokens
                .iter()
                .any(|token| token.contains(&byte_start))
        };
        let mut flags = Vec::new();
        let mut local_start = 0;
        for (byte_start, word) in text.split_word_bound_indices() {
            let local_length = word.encode_utf16().count();
            if word.chars().any(char::is_alphabetic)
                && !in_skipped_token(byte_start)
                && !self.dictionary.check(word)
            {
                flags.push(self.flag_for(text, word, local_start, local_length));
            }
            local_start += local_length;
        }
        flags
    }

    /// `local_start` is the id's only discriminator: no two flags in one `text` share an offset.
    /// The word itself stays out of the id, because ids reach log lines and draft text never
    /// does.
    fn flag_for(&self, text: &str, word: &str, local_start: usize, local_length: usize) -> Flag {
        let mut suggestions = Vec::new();
        self.dictionary.suggest(word, &mut suggestions);
        suggestions.truncate(MAX_SUGGESTIONS);
        let typed_curly = word.contains(CURLY_APOSTROPHE) && !word.contains(STRAIGHT_APOSTROPHE);
        if !typed_curly {
            for suggestion in &mut suggestions {
                *suggestion = suggestion.replace(CURLY_APOSTROPHE, STRAIGHT_APOSTROPHE);
            }
        }
        Flag {
            id: format!("spelling:{local_start}"),
            origin: FlagOrigin::Spelling,
            span: Span {
                anchor: text.to_string(),
                local_start,
                local_length,
                occurrence: 0,
            },
            message: format!("\"{word}\" is not in the dictionary"),
            suggestions,
            source_detail: "en_GB plus the Singapore supplement".to_string(),
            ruleset_version: None,
            catalogue_hash: None,
        }
    }
}

fn read_to_string(path: &Path) -> Result<String, SpellingError> {
    std::fs::read_to_string(path).map_err(|source| SpellingError::ReadDictionary {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::dictionary_path;

    fn checker() -> SpellChecker {
        SpellChecker::load(
            &dictionary_path("en_GB.aff"),
            &dictionary_path("en_GB.dic"),
            &dictionary_path("en_sg_supplement.txt"),
        )
        .expect("the vendored dictionary pair and supplement are well-formed, checked in CI")
    }

    /// The exact text a flag's span addresses, sliced back out of its own anchor, so a test can
    /// assert on what was flagged without depending on how the span happens to be anchored.
    fn flagged_text(flag: &Flag) -> String {
        let anchor: Vec<u16> = flag.span.anchor.encode_utf16().collect();
        let end = flag.span.local_start + flag.span.local_length;
        String::from_utf16_lossy(&anchor[flag.span.local_start..end])
    }

    /// Two separate loads stand in for two processes: with a seeded hasher each load drew its own
    /// seed, so the two lists matched only by chance whenever roots tied at the suggester's
    /// boundary.
    #[test]
    fn two_checkers_loaded_separately_suggest_the_same_corrections_in_the_same_order() {
        let first = checker();
        let second = checker();
        for word in ["eror", "mispelling", "definately"] {
            let suggestions = |checker: &SpellChecker| {
                let flags = checker.check(word);
                assert_eq!(flags.len(), 1, "{word}: {flags:#?}");
                flags[0].suggestions.clone()
            };
            assert_eq!(suggestions(&first), suggestions(&second), "{word}");
        }
    }

    #[test]
    fn flags_a_word_absent_from_both_dictionaries() {
        let text = "This sentnce has a typo.";
        let flags = checker().check(text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span.anchor, text);
        assert_eq!(flagged_text(&flags[0]), "sentnce");
        assert_eq!(flags[0].origin, FlagOrigin::Spelling);
        assert!(!flags[0].suggestions.is_empty());
    }

    #[test]
    fn a_flag_id_carries_the_offset_and_not_the_misspelled_word() {
        // The overlay logs the id of a flag it cannot position, so a word in the id is a word of
        // the draft in a log line.
        let flags = checker().check("This sentnce has a typo.");
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].id, "spelling:5");
    }

    #[test]
    fn the_same_word_misspelled_twice_addresses_each_occurrence_separately() {
        // Anchoring on the bare word would give both flags the same span and the same id, so the
        // overlay would underline the first occurrence twice and leave the second unmarked.
        let text = "I recieve one and I recieve two.";
        let flags = checker().check(text);
        assert_eq!(flags.len(), 2, "{flags:#?}");
        assert_ne!(flags[0].span.local_start, flags[1].span.local_start);
        assert_ne!(flags[0].id, flags[1].id);
        assert_eq!(flagged_text(&flags[0]), "recieve");
        assert_eq!(flagged_text(&flags[1]), "recieve");
        // Sliced from the anchor, so these offsets genuinely point at each occurrence in turn.
        assert_eq!(flags[1].span.local_start, text.find("recieve two").unwrap());
    }

    #[test]
    fn does_not_flag_a_singapore_supplement_term() {
        let flags = checker().check("Meet me at the kopitiam near the HDB.");
        assert!(flags.is_empty());
    }

    #[test]
    fn does_not_flag_a_sentence_with_no_misspellings() {
        let flags = checker().check("The quick brown fox jumps over the lazy dog.");
        assert!(flags.is_empty());
    }

    /// Whether `word`, checked alone, raises no flag.
    fn accepts(checker: &SpellChecker, word: &str) -> bool {
        checker.check(word).is_empty()
    }

    #[test]
    fn a_word_stored_in_lower_case_is_accepted_in_lower_case_with_a_capital_and_in_capitals() {
        // `colour` has no capitalised entry of its own in `en_GB.dic`, so both capitalised forms
        // rest on the casing rules alone.
        let checker = checker();
        for word in ["colour", "Colour", "COLOUR"] {
            assert!(accepts(&checker, word), "{word} was flagged");
        }
    }

    #[test]
    fn a_word_stored_with_an_initial_capital_is_flagged_in_lower_case_alone() {
        let checker = checker();
        assert!(accepts(&checker, "London"));
        assert!(accepts(&checker, "LONDON"));
        assert!(!accepts(&checker, "london"));
    }

    #[test]
    fn a_supplement_word_stored_in_capitals_is_flagged_in_lower_case() {
        let checker = checker();
        assert!(accepts(&checker, "HDB"));
        assert!(!accepts(&checker, "hdb"));
    }

    #[test]
    fn a_lower_case_supplement_word_is_accepted_with_a_capital_at_the_start_of_a_sentence() {
        let flags = checker().check("Kopitiam coffee is strong.");
        assert!(flags.is_empty(), "{flags:#?}");
    }

    #[test]
    fn word_bounds_keep_a_curly_apostrophe_contraction_as_one_segment() {
        let segments: Vec<&str> = "We don\u{2019}t.".split_word_bounds().collect();
        assert_eq!(segments, ["We", " ", "don\u{2019}t", "."]);
    }

    #[test]
    fn does_not_flag_a_contraction_typed_with_the_curly_apostrophe() {
        // Word replaces a typed apostrophe with U+2019 by default.
        let flags = checker().check("I don\u{2019}t think it\u{2019}s late.");
        assert!(flags.is_empty(), "{flags:#?}");
    }

    #[test]
    fn one_flag_covers_a_misspelt_curly_apostrophe_contraction_as_written() {
        // The curly apostrophe before the misspelling takes three UTF-8 bytes and one UTF-16 code
        // unit, so a byte offset would land two units past the word.
        let text = "It\u{2019}s fine, it dosen\u{2019}t matter.";
        let flags = checker().check(text);
        assert_eq!(flags.len(), 1, "{flags:#?}");
        let prefix = &text[..text.find("dosen").unwrap()];
        assert_eq!(flags[0].span.local_start, prefix.encode_utf16().count());
        assert_eq!(flags[0].span.local_length, 7);
        assert_eq!(flagged_text(&flags[0]), "dosen\u{2019}t");
    }

    #[test]
    fn a_suggestion_keeps_the_apostrophe_the_flagged_word_was_typed_with() {
        // The affix file's `OCONV` line gives every suggestion U+2019, which is mapped back for a
        // word typed with a straight apostrophe or with none.
        let checker = checker();
        for (word, expected) in [
            ("dosen\u{2019}t", "doesn\u{2019}t"),
            ("dosen't", "doesn't"),
            ("didnt", "didn't"),
        ] {
            let flags = checker.check(word);
            assert_eq!(flags.len(), 1, "{flags:#?}");
            assert!(
                flags[0]
                    .suggestions
                    .iter()
                    .any(|suggestion| suggestion == expected),
                "{word}: {:?}",
                flags[0].suggestions
            );
            let other = if expected.contains(CURLY_APOSTROPHE) {
                STRAIGHT_APOSTROPHE
            } else {
                "\u{2019}"
            };
            assert!(
                flags[0]
                    .suggestions
                    .iter()
                    .all(|suggestion| !suggestion.contains(other)),
                "{word}: {:?}",
                flags[0].suggestions
            );
        }
    }

    /// One token of each shape the skip rule names, in the rule's order: a scheme separator, a
    /// slash, a backslash, an at sign, an equals sign, an ampersand, a dot between letters, a dot
    /// between a digit and a letter, a dot between digits in a token that holds a letter, an
    /// underscore, and a capital after a lowercase letter.
    const SKIPPED_TOKENS: [&str; 11] = [
        "https://exampel.com/pth",
        "src/analyzr",
        "src\\analyzr",
        "naem@exampel.com",
        "q=helo",
        "helo&wrld",
        "overlay.tsx",
        "fiel2.txt",
        "vershun1.2",
        "get_current_flags",
        "FlagOrigin",
    ];

    #[test]
    fn each_skipped_token_holds_a_segment_the_dictionary_rejects() {
        // Without one, finding no flag on the token would test the dictionary and not the rule.
        let checker = checker();
        for token in SKIPPED_TOKENS {
            let rejected = token.split_word_bounds().any(|segment| {
                segment.chars().any(char::is_alphabetic) && !checker.dictionary.check(segment)
            });
            assert!(rejected, "{token}");
        }
    }

    #[test]
    fn a_url_path_or_identifier_shaped_token_is_skipped_in_prose_and_in_a_fragment() {
        // The checker has no notion of form mode, so a sentence and a list item reach it alike.
        // `iPhone` is in `en_GB.dic`, so it shows only that the rule does not break a stored word.
        let checker = checker();
        for token in SKIPPED_TOKENS.into_iter().chain(["iPhone"]) {
            for text in [
                format!("Read {token} first, then {token}."),
                format!("- {token}"),
            ] {
                let flags = checker.check(&text);
                assert!(flags.is_empty(), "{text}: {flags:#?}");
            }
        }
    }

    #[test]
    fn a_misspelt_word_beside_a_skipped_token_is_still_flagged() {
        let checker = checker();
        for token in SKIPPED_TOKENS {
            let flags = checker.check(&format!("We recieve {token} daily."));
            assert_eq!(flags.len(), 1, "{token}: {flags:#?}");
            assert_eq!(flagged_text(&flags[0]), "recieve");
        }
    }

    #[test]
    fn a_query_string_the_sentence_splitter_cut_off_its_url_is_skipped() {
        let text = "See https://exampel.com/serch?q=helo&lang=en for the list.";
        let sentences: Vec<&str> = text.unicode_sentences().collect();
        assert_eq!(
            sentences,
            [
                "See https://exampel.com/serch?",
                "q=helo&lang=en for the list."
            ]
        );
        let checker = checker();
        for sentence in sentences {
            let flags = checker.check(sentence);
            assert!(flags.is_empty(), "{sentence}: {flags:#?}");
        }
    }

    #[test]
    fn a_version_or_a_file_name_with_a_digit_before_its_dot_is_skipped() {
        let flags = checker().check("Install v1.2 and open fiel2.txt first.");
        assert!(flags.is_empty(), "{flags:#?}");
    }

    #[test]
    fn a_misspelt_word_that_ends_a_sentence_is_flagged() {
        // A full stop after the last letter is not a dot between letters.
        let flags = checker().check("Tomorrow we recieve.");
        assert_eq!(flags.len(), 1, "{flags:#?}");
        assert_eq!(flagged_text(&flags[0]), "recieve");
    }

    #[test]
    fn flags_carry_a_utf16_span_length_for_a_multi_byte_word() {
        // "wördz" is not a real word in en_GB or the supplement; its ö is one UTF-16 code unit,
        // so the flagged span's length should be 5, not the 6 UTF-8 bytes it takes up.
        let flags = checker().check("This is wördz not English.");
        let flag = flags
            .iter()
            .find(|flag| flagged_text(flag) == "wördz")
            .expect("wördz is not in either dictionary");
        assert_eq!(flag.span.local_length, 5);
    }
}
