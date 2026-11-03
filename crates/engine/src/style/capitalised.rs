//! The capitalised-words matcher: a sentence of enough words, enough of them capitalised, as one
//! match. A word is the engine's one definition, in `words`, and a capitalised word is one whose
//! first character is an upper-case letter, so a word that starts with a digit counts towards the
//! sentence's length and not towards its capitalised share.

use std::ops::Range;

use crate::words::words;

/// A capitalised-words matcher, compiled.
#[derive(Debug)]
pub(super) struct CapitalisedWordsMatcher {
    minimum_words: usize,
    share: f64,
}

/// A share that is not a number from 0 to 1. Above 1 or not a number, it matches no sentence, and
/// below 0 it matches every sentence of enough words, so either is a matcher that does not measure
/// what it names.
#[derive(Debug, PartialEq)]
pub(super) struct ShareOutOfRange(pub(super) f64);

impl CapitalisedWordsMatcher {
    pub(super) fn compile(minimum_words: u32, share: f64) -> Result<Self, ShareOutOfRange> {
        if !(0.0..=1.0).contains(&share) {
            return Err(ShareOutOfRange(share));
        }
        Ok(Self {
            minimum_words: usize::try_from(minimum_words).unwrap_or(usize::MAX),
            share,
        })
    }

    /// From the first word's start to the last word's end of `sentence`, when it holds at least one
    /// word and at least `minimum_words`, and a share of at least `share` of them is capitalised.
    pub(super) fn find(&self, sentence: &str) -> Option<Range<usize>> {
        let mut found: Option<Range<usize>> = None;
        let (mut count, mut capitalised) = (0_usize, 0_usize);
        for (start, word) in words(sentence) {
            count += 1;
            if word.chars().next().is_some_and(char::is_uppercase) {
                capitalised += 1;
            }
            let end = start + word.len();
            found = Some(found.map_or(start..end, |found| found.start..end));
        }
        // A share taken by division, so a share written as a decimal compares equal to the same
        // fraction of words: 0.7 is not above 7 words in 10.
        let is_capitalised =
            count >= self.minimum_words && capitalised as f64 / count as f64 >= self.share;
        found.filter(|_| is_capitalised)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three words and three quarters, the title-case pattern's thresholds.
    fn title_case() -> CapitalisedWordsMatcher {
        CapitalisedWordsMatcher::compile(3, 0.75).unwrap()
    }

    fn matched<'t>(matcher: &CapitalisedWordsMatcher, sentence: &'t str) -> Option<&'t str> {
        matcher.find(sentence).map(|range| &sentence[range])
    }

    #[test]
    fn a_line_of_enough_words_with_enough_capitalised_is_one_match_from_its_first_word_to_its_last()
    {
        assert_eq!(
            matched(&title_case(), "## Notes On The Design"),
            Some("Notes On The Design")
        );
        assert_eq!(
            matched(
                &title_case(),
                "Strategic Negotiations And Global Partnerships:"
            ),
            Some("Strategic Negotiations And Global Partnerships")
        );
    }

    #[test]
    fn the_minimum_words_matches_a_line_at_it_and_not_one_below_it() {
        assert_eq!(matched(&title_case(), "Next Steps"), None);
        assert_eq!(
            matched(&title_case(), "Next Steps Ahead"),
            Some("Next Steps Ahead")
        );
    }

    #[test]
    fn the_share_matches_a_line_at_it_and_not_one_below_it() {
        // Three of four is three quarters, at the share.
        assert_eq!(
            matched(&title_case(), "Notes On the Design"),
            Some("Notes On the Design")
        );
        // Two of three and five of seven are below it.
        assert_eq!(matched(&title_case(), "Notes on Design"), None);
        assert_eq!(
            matched(&title_case(), "Notes On The Design of the Harbour"),
            None
        );
        // Six of eight is at it again.
        assert_eq!(
            matched(&title_case(), "Notes On The Design Of The old harbour"),
            Some("Notes On The Design Of The old harbour")
        );
    }

    #[test]
    fn a_share_written_as_a_decimal_is_met_by_the_same_fraction_of_words() {
        let seven_tenths = CapitalisedWordsMatcher::compile(1, 0.7).unwrap();
        let sentence = "One Two Three Four Five Six Seven eight nine ten";
        assert_eq!(matched(&seven_tenths, sentence), Some(sentence));
    }

    #[test]
    fn a_word_that_starts_with_a_digit_counts_towards_the_length_and_not_the_share() {
        assert_eq!(matched(&title_case(), "Phase 3 Plan"), None);
        assert_eq!(
            matched(&title_case(), "Phase 3 Plan Notes"),
            Some("Phase 3 Plan Notes")
        );
    }

    #[test]
    fn sentence_case_and_a_line_with_no_word_are_no_match() {
        assert_eq!(matched(&title_case(), "Notes on the design"), None);
        assert_eq!(matched(&title_case(), "## --- ##"), None);
        let any_line = CapitalisedWordsMatcher::compile(0, 0.0).unwrap();
        assert_eq!(matched(&any_line, "## --- ##"), None);
        assert_eq!(matched(&any_line, "notes"), Some("notes"));
    }

    #[test]
    fn a_share_that_is_not_a_number_from_zero_to_one_does_not_compile() {
        for share in [-0.25, 1.5, f64::NAN, f64::INFINITY] {
            let refused = CapitalisedWordsMatcher::compile(3, share).unwrap_err();
            assert!(refused.0.is_nan() || refused.0 == share, "{share}");
        }
        for share in [0.0, 0.75, 1.0] {
            assert!(
                CapitalisedWordsMatcher::compile(3, share).is_ok(),
                "{share}"
            );
        }
    }
}
