//! Spelling checks against the vendored `en_GB` dictionary plus a Singapore English supplement,
//! per README's Language convention section: `en_GB` in its `-ise` form, with local vocabulary
//! layered on top as a plain word list rather than its own hunspell affix file. Provenance and
//! licensing for the vendored dictionary pair are recorded in
//! `resources/dictionaries/NOTICE.md`.

pub mod error;

use std::path::Path;

use spellbook::Dictionary;
use unicode_segmentation::UnicodeSegmentation;

pub use error::SpellingError;

use crate::flag::{Flag, FlagOrigin, Span};

/// Corrections offered for a single misspelling, short of hunspell's full suggestion list: past
/// a handful, later suggestions are rarely the one the user meant, and every extra one costs
/// Tier 0's latency budget to compute.
const MAX_SUGGESTIONS: usize = 5;

/// Wraps a loaded `en_GB` dictionary with the Singapore supplement merged in through
/// [`Dictionary::add`], so a supplement word is checked and suggested through the same lookup as
/// any other word, with no second word set to keep in sync.
pub struct SpellChecker {
    dictionary: Dictionary,
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
        let mut dictionary = Dictionary::new(&aff, &dic)
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

    /// Checks every alphabetic word in `text` against the dictionary, returning one [`Flag`] per
    /// word absent from both `en_GB` and the Singapore supplement, each carrying whatever
    /// corrections hunspell's suggestion algorithm offers.
    ///
    /// Capitalisation (proper nouns, a capital at the start of a sentence) is left entirely to
    /// `Dictionary::check`'s own casing rules; this function does not reimplement or second-guess
    /// them, and their exact behaviour against real `.aff` `SFX`/`PFX` rules has not been
    /// independently verified against every case.
    pub fn check(&self, text: &str) -> Vec<Flag> {
        text.split_word_bounds()
            .filter(|word| word.chars().any(char::is_alphabetic))
            .filter(|word| !self.dictionary.check(word))
            .enumerate()
            .map(|(index, word)| self.flag_for(index, word))
            .collect()
    }

    fn flag_for(&self, index: usize, word: &str) -> Flag {
        let mut suggestions = Vec::new();
        self.dictionary.suggest(word, &mut suggestions);
        suggestions.truncate(MAX_SUGGESTIONS);
        Flag {
            id: format!("spelling:{index}:{word}"),
            origin: FlagOrigin::Spelling,
            span: Span {
                anchor: word.to_string(),
                local_start: 0,
                local_length: word.encode_utf16().count(),
            },
            message: format!("\"{word}\" is not in the dictionary"),
            suggestions,
            source_detail: "en_GB plus the Singapore supplement".to_string(),
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

    fn resource_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("dictionaries")
            .join(name)
    }

    fn checker() -> SpellChecker {
        SpellChecker::load(
            &resource_path("en_GB.aff"),
            &resource_path("en_GB.dic"),
            &resource_path("en_sg_supplement.txt"),
        )
        .expect("the vendored dictionary pair and supplement are well-formed, checked in CI")
    }

    #[test]
    fn flags_a_word_absent_from_both_dictionaries() {
        let flags = checker().check("This sentnce has a typo.");
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span.anchor, "sentnce");
        assert_eq!(flags[0].origin, FlagOrigin::Spelling);
        assert!(!flags[0].suggestions.is_empty());
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

    #[test]
    fn flags_carry_a_utf16_span_length_for_a_multi_byte_word() {
        // "wördz" is not a real word in en_GB or the supplement; its ö is one UTF-16 code unit,
        // so the flagged span's length should be 5, not the 6 UTF-8 bytes it takes up.
        let flags = checker().check("This is wördz not English.");
        let flag = flags
            .iter()
            .find(|flag| flag.span.anchor == "wördz")
            .expect("wördz is not in either dictionary");
        assert_eq!(flag.span.local_length, 5);
    }
}
