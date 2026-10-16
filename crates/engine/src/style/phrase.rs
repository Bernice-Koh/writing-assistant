//! The phrase-list matcher. Each entry is compiled once into a regex of its own, built with
//! `regex::escape`, as the telltale catalogue's matcher in `ai_tell` builds one per phrase. One
//! alternation of every entry would find only one of two entries that overlap, as "is a testament
//! to" and "testament to" do, since a regex search yields matches that do not overlap.
//!
//! The match is case-insensitive and runs against the text as written, so every offset it yields
//! indexes that text. Searching a lowercased copy instead yields offsets into the copy, which are
//! wrong for the original wherever lowercasing changes a character's byte length.

use std::ops::Range;

use regex::Regex;

use super::checker::PhraseEntry;

#[derive(Debug)]
pub(super) struct PhraseMatcher {
    /// One per entry, in the list's order.
    entry_regexes: Vec<Regex>,
}

impl PhraseMatcher {
    /// Fails only where an entry's regex exceeds the `regex` crate's size limit, since an escaped
    /// phrase always parses.
    pub(super) fn compile(entries: &[PhraseEntry]) -> Result<Self, regex::Error> {
        // An entry's prefix flag is read from #108 on.
        let entry_regexes = entries
            .iter()
            .map(|entry| Regex::new(&entry_regex_source(&entry.phrase)))
            .collect::<Result<_, _>>()?;
        Ok(Self { entry_regexes })
    }

    /// The byte range of every occurrence of every entry in `text`, ordered by start and then by
    /// end. Two entries that match the same text, as one listed twice in different case does,
    /// yield it once.
    pub(super) fn find(&self, text: &str) -> Vec<Range<usize>> {
        let mut ranges: Vec<Range<usize>> = self
            .entry_regexes
            .iter()
            .flat_map(|regex| regex.find_iter(text).map(|found| found.range()))
            .collect();
        ranges.sort_by_key(|range| (range.start, range.end));
        ranges.dedup();
        ranges
    }
}

/// An edge of the phrase that is a letter or digit must fall on a word boundary: without one,
/// "here is a" matches inside "there is a" and "boasts a" inside "boasts about". An edge that is
/// punctuation or a space carries no such requirement, so a dash is found between two words.
fn entry_regex_source(phrase: &str) -> String {
    let boundary = |edge: Option<char>| {
        if edge.is_some_and(char::is_alphanumeric) {
            r"\b"
        } else {
            ""
        }
    };
    format!(
        "(?i){}{}{}",
        boundary(phrase.chars().next()),
        regex::escape(phrase),
        boundary(phrase.chars().next_back()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(phrases: &[&str], prefix: bool) -> PhraseMatcher {
        let entries: Vec<PhraseEntry> = phrases
            .iter()
            .map(|phrase| PhraseEntry {
                phrase: (*phrase).to_owned(),
                prefix,
            })
            .collect();
        PhraseMatcher::compile(&entries).unwrap()
    }

    /// The telltale catalogue's `em-en-dashes` phrases.
    fn dashes() -> PhraseMatcher {
        matcher(&["\u{2014}", "\u{2013}", " -- "], false)
    }

    /// The telltale catalogue's `copula-avoidance` phrases.
    fn copulas() -> PhraseMatcher {
        matcher(
            &[
                "serves as a",
                "serves as the",
                "stands as a",
                "stands as the",
                "boasts a",
                "boasts an",
            ],
            false,
        )
    }

    /// The telltale catalogue's `collaborative-communication-artifacts` phrases.
    fn artifacts() -> PhraseMatcher {
        matcher(
            &[
                "i hope this helps",
                "of course!",
                "certainly!",
                "would you like me to",
                "let me know if",
                "here is a",
            ],
            false,
        )
    }

    /// The text of each match, sliced from `text` by the range the matcher gave, which panics on a
    /// range that does not fall on character boundaries.
    fn matched<'t>(matcher: &PhraseMatcher, text: &'t str) -> Vec<&'t str> {
        matcher
            .find(text)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn a_character_that_changes_byte_length_when_lowercased_does_not_shift_the_span() {
        // A capital dotted I is two bytes and lowercases to three, and the Kelvin sign is three
        // and lowercases to one, so an offset found in a lowercased copy of either sentence
        // indexes the original one or two bytes off. In the first sentence that lands inside the
        // em dash, which is not a character boundary.
        let text = "In İzmir\u{2014}a port city, trade grew.";
        assert_eq!(matched(&dashes(), text), ["\u{2014}"]);

        let text = "At 300 \u{212A} the alloy boasts a low resistance.";
        assert_eq!(matched(&copulas(), text), ["boasts a"]);
    }

    #[test]
    fn flags_every_occurrence_of_a_phrase_in_one_sentence() {
        let text = "The policy\u{2014}announced without warning\u{2014}affects everyone.";
        let ranges = dashes().find(text);
        assert_eq!(ranges.len(), 2, "{ranges:?}");
        assert_ne!(ranges[0].start, ranges[1].start);
        assert_eq!(matched(&dashes(), text), ["\u{2014}", "\u{2014}"]);
    }

    #[test]
    fn a_phrase_edge_that_is_a_letter_or_digit_matches_only_at_a_word_boundary() {
        // "here is a" sits inside "there is a".
        assert!(matched(&artifacts(), "There is a cat on the mat.").is_empty());
        assert_eq!(
            matched(&artifacts(), "Here is a summary of the meeting."),
            ["Here is a"]
        );

        // "boasts a" sits inside "boasts about".
        assert!(matched(&copulas(), "The author boasts about the prize.").is_empty());
        assert_eq!(
            matched(&copulas(), "The town boasts a harbour."),
            ["boasts a"]
        );
    }

    #[test]
    fn a_phrase_edge_that_is_punctuation_needs_no_word_boundary() {
        // No word boundary lies between "!" and the closing quotation mark, or between two em
        // dashes.
        assert_eq!(
            matched(&artifacts(), "She replied \"Certainly!\" and left."),
            ["Certainly!"]
        );
        assert_eq!(
            matched(&dashes(), "Wait\u{2014}\u{2014}no."),
            ["\u{2014}", "\u{2014}"]
        );
    }

    #[test]
    fn an_entry_with_the_prefix_flag_matches_exactly_as_one_without() {
        // Until #108 gives the flag its meaning, "intricate" does not reach "intricately".
        let text = "An intricate design, intricately drawn.";
        let flagged = matcher(&["intricate"], true);
        let unflagged = matcher(&["intricate"], false);
        assert_eq!(matched(&flagged, text), ["intricate"]);
        assert_eq!(flagged.find(text), unflagged.find(text));
    }

    #[test]
    fn overlapping_entries_each_match() {
        let text = "Its success is a testament to the team.";
        let nested = matcher(&["is a testament to", "testament to"], false);
        assert_eq!(
            matched(&nested, text),
            ["is a testament to", "testament to"]
        );
    }

    #[test]
    fn an_entry_listed_twice_matches_each_occurrence_once() {
        let twice = matcher(&["boasts a", "Boasts A"], false);
        assert_eq!(matched(&twice, "The town boasts a harbour."), ["boasts a"]);
    }
}
