//! The engine's one definition of a word: a segment of Unicode's word boundaries (UAX #29), as
//! `unicode-segmentation` splits text for spelling, that holds a letter or a digit. The rule
//! layer's word count and rate bands count by it, and so does the screen's content check (#102),
//! so a length in words means one thing wherever the engine measures it.

use unicode_segmentation::UnicodeSegmentation;

/// Each word of `text` in order, with its byte offset in `text`.
pub(crate) fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_word_bound_indices()
        .filter(|(_, segment)| segment.chars().any(char::is_alphanumeric))
}

pub(crate) fn word_count(text: &str) -> usize {
    words(text).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_a_segment_of_the_word_boundaries_that_holds_a_letter_or_a_digit() {
        // The curly apostrophe and the decimal point hold their word together, the hyphen and the
        // slash split theirs, and the punctuation, the spaces and the rocket hold no letter or
        // digit.
        let text = "Don\u{2019}t miss the well-known 3.5 km walk, rated 4/5 🚀!";
        let found: Vec<&str> = words(text).map(|(_, word)| word).collect();
        assert_eq!(
            found,
            [
                "Don\u{2019}t",
                "miss",
                "the",
                "well",
                "known",
                "3.5",
                "km",
                "walk",
                "rated",
                "4",
                "5"
            ]
        );
        assert_eq!(word_count(text), 11);
    }

    #[test]
    fn each_word_carries_its_own_offset_into_the_text() {
        let text = "Go, go \u{2014} gone.";
        for (offset, word) in words(text) {
            assert_eq!(&text[offset..offset + word.len()], word);
        }
        assert_eq!(
            words(text).map(|(offset, _)| offset).collect::<Vec<_>>(),
            [0, 4, 11]
        );
    }

    #[test]
    fn text_with_no_letter_or_digit_holds_no_word() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count(" \u{2014} ... !? 🚀"), 0);
    }
}
