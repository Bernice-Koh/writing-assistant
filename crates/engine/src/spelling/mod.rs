//! Spelling checks through spellbook, which reads the vendored Hunspell-format dictionary pair:
//! `en_GB` in its `-ise` form, with a Singapore word list on top. The word list is plain words
//! merged into the same dictionary, with no affix file of its own. Provenance and licensing for
//! the vendored dictionary pair are recorded in `src-tauri/resources/dictionaries/NOTICE.md`.

pub mod error;

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
    /// word absent from both `en_GB` and the Singapore supplement, each carrying the corrections
    /// spellbook suggests, up to `MAX_SUGGESTIONS`.
    ///
    /// Each flag anchors on `text` itself, carrying the word's own offset into it, rather than on
    /// the bare misspelled word. [`crate::capture::Capture::span_rect`] and
    /// [`crate::capture::Capture::replace`] both resolve an anchor to its *first* occurrence, so a
    /// word misspelled twice anchored on itself produces two flags addressing the same first
    /// occurrence: one underline drawn twice and the second misspelling left unmarked. Grammar
    /// flags already anchor on their sentence for the same reason (see `languagetool::client`).
    ///
    /// Capitalisation (proper nouns, a capital at the start of a sentence) is left entirely to
    /// `Dictionary::check`'s own casing rules; this function does not reimplement or second-guess
    /// them, and their exact behaviour against real `.aff` `SFX`/`PFX` rules has not been
    /// independently verified against every case.
    pub fn check(&self, text: &str) -> Vec<Flag> {
        let mut flags = Vec::new();
        let mut local_start = 0;
        for word in text.split_word_bounds() {
            let local_length = word.encode_utf16().count();
            if word.chars().any(char::is_alphabetic) && !self.dictionary.check(word) {
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
        Flag {
            id: format!("spelling:{local_start}"),
            origin: FlagOrigin::Spelling,
            span: Span {
                anchor: text.to_string(),
                local_start,
                local_length,
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
