//! The phrase-list matcher. Each entry is compiled once into a regex of its own, built with
//! `regex::escape`, as the telltale catalogue's matcher in `ai_tell` builds one per phrase. One
//! alternation of every entry would find only one of two entries that overlap, as "is a testament
//! to" and "testament to" do, since a regex search yields matches that do not overlap.
//!
//! The match is case-insensitive and needs no lowercased copy of the text, which would yield
//! offsets into the copy, wrong for the original wherever lowercasing changes a character's byte
//! length.
//!
//! Word writes curly apostrophes and quotation marks, and the entries are written with straight
//! ones, so the entries are searched in a copy of the text in which U+2018 and U+2019 read as the
//! straight apostrophe and U+201C and U+201D as the straight double quotation mark. Nothing else
//! differs. Each of those marks is one character and one UTF-16 code unit, as its straight form is,
//! and three bytes where its straight form is one, so a byte offset into the copy is wrong for the
//! text as written. A match's byte range is carried back to the text as written through the
//! character positions the two share, and no offset into the copy leaves the matcher.
//!
//! An apostrophe between two letters is inside a word, so an edge of an entry that is a letter
//! does not match beside one: "don" does not match in "don't" or "don’t". The `regex` crate's
//! `\b` finds a word boundary on each side of an apostrophe, and the crate has no lookaround, so
//! each match is checked for the rule after the search. The rule agrees with spelling, which
//! segments words by Unicode's word boundary rules (UAX #29): rules WB6 and WB7 hold a letter, an
//! apostrophe, straight or curly, and a letter in one word.
//!
//! A one-word entry with the prefix flag matches every word that starts with it, and the match
//! covers the whole word, so one entry covers its inflections, as "intricate" covers
//! "intricately". The word runs on over letters and digits, and over an apostrophe that a letter
//! follows, so it takes in a possessive and stops before a closing quotation mark.

use std::borrow::Cow;
use std::ops::Range;

use regex::Regex;

use super::checker::PhraseEntry;

/// The rest of a word after a prefix entry, in the searched copy, where every apostrophe is
/// straight.
const REST_OF_WORD: &str = r"(?:[\p{Alphabetic}\p{N}]|'\p{Alphabetic})*";

#[derive(Debug)]
pub(super) struct PhraseMatcher {
    /// One per entry, in the list's order.
    entry_regexes: Vec<Regex>,
}

/// Why a phrase list did not compile.
#[derive(Debug)]
pub(super) enum PhraseError {
    /// An entry's regex exceeds the `regex` crate's size limit. An escaped phrase always parses.
    Regex(regex::Error),
    /// The entry at `entry_index` in the list carries the prefix flag and is not one word.
    PrefixNotOneWord { entry_index: usize },
}

impl PhraseMatcher {
    pub(super) fn compile(entries: &[PhraseEntry]) -> Result<Self, PhraseError> {
        let entry_regexes = entries
            .iter()
            .enumerate()
            .map(|(entry_index, entry)| {
                if entry.prefix && !is_one_word(&entry.phrase) {
                    return Err(PhraseError::PrefixNotOneWord { entry_index });
                }
                Regex::new(&entry_regex_source(entry)).map_err(PhraseError::Regex)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { entry_regexes })
    }

    /// The byte range of every occurrence of every entry in `text`, ordered by start and then by
    /// end. Two entries that match the same text, as one listed twice in different case does,
    /// yield it once.
    pub(super) fn find(&self, text: &str) -> Vec<Range<usize>> {
        let straight = StraightMarks::of(text);
        let mut ranges: Vec<Range<usize>> = self
            .entry_regexes
            .iter()
            .flat_map(|regex| matches_outside_words(regex, straight.searched()))
            .map(|range| straight.range_as_written(range))
            .collect();
        ranges.sort_by_key(|range| (range.start, range.end));
        ranges.dedup();
        ranges
    }
}

/// The byte range of each match of `regex` in `text` that has neither edge inside a word. After a
/// match with an edge inside a word, the search resumes one character after that match's start,
/// not at its end, so a match that overlaps it is still found.
fn matches_outside_words(regex: &Regex, text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut search_start = 0;
    while let Some(found) = regex.find_at(text, search_start) {
        let range = found.range();
        let outside_words = !has_an_edge_inside_a_word(text, &range);
        let resumes_at_end = outside_words && !range.is_empty();
        let (start, end) = (range.start, range.end);
        if outside_words {
            ranges.push(range);
        }
        search_start = if resumes_at_end {
            end
        } else {
            match text[start..].chars().next() {
                Some(first) => start + first.len_utf8(),
                None => break,
            }
        };
    }
    ranges
}

/// Whether a letter at an edge of `range` in `text` has beside it, outside the range, an
/// apostrophe with a letter on its other side.
fn has_an_edge_inside_a_word(text: &str, range: &Range<usize>) -> bool {
    let is_letter = |character: Option<char>| character.is_some_and(char::is_alphabetic);
    let matched = &text[range.clone()];
    let mut before = text[..range.start].chars().rev();
    let mut after = text[range.end..].chars();
    let start_inside = is_letter(matched.chars().next())
        && before.next() == Some('\'')
        && is_letter(before.next());
    let end_inside = is_letter(matched.chars().next_back())
        && after.next() == Some('\'')
        && is_letter(after.next());
    start_inside || end_inside
}

/// Whether `phrase` is one word, which the prefix flag requires: not empty, and holding no
/// whitespace.
fn is_one_word(phrase: &str) -> bool {
    !phrase.is_empty() && !phrase.contains(char::is_whitespace)
}

/// The text the entries are searched in, which is the text as written with each curly apostrophe
/// and quotation mark read as its straight form, and the way back from it to the text as written.
struct StraightMarks<'t> {
    searched: Cow<'t, str>,
    /// For each character of the searched copy in order, and then for its end, the byte offset in
    /// the copy and the byte offset in the text as written. Empty where the text holds no curly
    /// mark, so the copy is the text itself.
    character_offsets: Vec<(usize, usize)>,
}

impl<'t> StraightMarks<'t> {
    fn of(text: &'t str) -> Self {
        if text
            .chars()
            .all(|character| straight_form(character) == character)
        {
            return Self {
                searched: Cow::Borrowed(text),
                character_offsets: Vec::new(),
            };
        }
        let mut searched = String::with_capacity(text.len());
        let mut character_offsets = Vec::with_capacity(text.len() + 1);
        for (offset_as_written, character) in text.char_indices() {
            character_offsets.push((searched.len(), offset_as_written));
            searched.push(straight_form(character));
        }
        character_offsets.push((searched.len(), text.len()));
        Self {
            searched: Cow::Owned(searched),
            character_offsets,
        }
    }

    fn searched(&self) -> &str {
        &self.searched
    }

    /// The byte range in the text as written that holds the characters `range` holds in the
    /// searched copy.
    fn range_as_written(&self, range: Range<usize>) -> Range<usize> {
        if self.character_offsets.is_empty() {
            return range;
        }
        self.offset_as_written(range.start)..self.offset_as_written(range.end)
    }

    fn offset_as_written(&self, searched_offset: usize) -> usize {
        let character_position = self
            .character_offsets
            .binary_search_by_key(&searched_offset, |&(in_searched, _)| in_searched)
            .expect(
                "a regex match starts and ends on a character boundary of the text it searched",
            );
        self.character_offsets[character_position].1
    }
}

fn straight_form(character: char) -> char {
    match character {
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        other => other,
    }
}

/// An edge of the phrase that is a letter or digit must fall on a word boundary: without one,
/// "here is a" matches inside "there is a" and "boasts a" inside "boasts about". An edge that is
/// punctuation or a space carries no such requirement, so a dash is found between two words.
///
/// A prefix entry's end carries no boundary: the match runs on to the end of the word instead.
fn entry_regex_source(entry: &PhraseEntry) -> String {
    let phrase = &entry.phrase;
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
        if entry.prefix {
            REST_OF_WORD
        } else {
            boundary(phrase.chars().next_back())
        },
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
    fn an_entry_with_the_prefix_flag_matches_each_longer_word_that_starts_with_it() {
        let text = "An intricate design, intricately drawn, for its Intricateness.";
        assert_eq!(
            matched(&matcher(&["intricate"], true), text),
            ["intricate", "intricately", "Intricateness"]
        );
    }

    #[test]
    fn an_entry_without_the_prefix_flag_matches_only_the_word_itself() {
        let text = "An intricate design, intricately drawn, for its intricateness.";
        assert_eq!(
            matched(&matcher(&["intricate"], false), text),
            ["intricate"]
        );
    }

    #[test]
    fn a_prefix_entry_does_not_match_inside_a_word() {
        // The start of the entry keeps its word boundary.
        let text = "Nothing unintricate here.";
        assert!(matched(&matcher(&["intricate"], true), text).is_empty());
    }

    #[test]
    fn a_prefix_match_covers_the_whole_word_and_a_possessive() {
        let flagged = matcher(&["intricate"], true);
        assert_eq!(
            matched(&flagged, "The intricate\u{2019}s charm."),
            ["intricate\u{2019}s"]
        );
        assert_eq!(
            matched(&flagged, "The intricately's charm."),
            ["intricately's"]
        );
        // An apostrophe no letter follows closes a quotation and ends the word before it.
        assert_eq!(
            matched(&flagged, "\u{2018}How intricately\u{2019} he said."),
            ["intricately"]
        );
    }

    #[test]
    fn a_prefix_entry_that_is_not_one_word_does_not_compile() {
        let entries = [
            PhraseEntry {
                phrase: "intricate".to_owned(),
                prefix: true,
            },
            PhraseEntry {
                phrase: "rich tapestry".to_owned(),
                prefix: true,
            },
        ];
        assert!(matches!(
            PhraseMatcher::compile(&entries),
            Err(PhraseError::PrefixNotOneWord { entry_index: 1 })
        ));
        let empty = [PhraseEntry {
            phrase: String::new(),
            prefix: true,
        }];
        assert!(matches!(
            PhraseMatcher::compile(&empty),
            Err(PhraseError::PrefixNotOneWord { entry_index: 0 })
        ));
        // Without the flag, a phrase of many words compiles as it always has.
        assert_eq!(
            matched(&matcher(&["rich tapestry"], false), "A rich tapestry."),
            ["rich tapestry"]
        );
    }

    #[test]
    fn an_entry_does_not_match_before_a_straight_apostrophe_inside_a_word() {
        let entry = matcher(&["don"], false);
        assert!(matched(&entry, "I don't know.").is_empty());
        // An apostrophe that closes a quotation is not inside a word.
        assert_eq!(matched(&entry, "Call him 'don' now."), ["don"]);
    }

    #[test]
    fn an_entry_does_not_match_before_a_curly_apostrophe_inside_a_word() {
        let entry = matcher(&["don"], false);
        assert!(matched(&entry, "I don\u{2019}t know.").is_empty());
        assert_eq!(
            matched(&entry, "Call him \u{2018}don\u{2019} now."),
            ["don"]
        );
    }

    #[test]
    fn an_entry_with_a_straight_apostrophe_matches_the_curly_one_inside_a_word() {
        assert_eq!(
            matched(&matcher(&["don't"], false), "I don\u{2019}t know."),
            ["don\u{2019}t"]
        );
    }

    #[test]
    fn an_entry_does_not_match_after_an_apostrophe_inside_a_word() {
        // "re absolutely right" ends a word the catalogue's "you're absolutely right" starts.
        let entry = matcher(&["re absolutely right"], false);
        assert!(matched(&entry, "You\u{2019}re absolutely right.").is_empty());
        assert!(matched(&entry, "You're absolutely right.").is_empty());
    }

    #[test]
    fn a_match_with_an_edge_inside_a_word_does_not_hide_one_that_overlaps_it() {
        // The first "a a" starts after "x'", inside a word; the one a character on does not.
        assert_eq!(matched(&matcher(&["a a"], false), "x'a a a"), ["a a"]);
        let ranges = matcher(&["a a"], false).find("x'a a a");
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert_eq!(ranges[0], 4..7);
    }

    #[test]
    fn spelling_segments_an_apostrophe_between_letters_into_the_word() {
        use unicode_segmentation::UnicodeSegmentation;
        for word in ["don't", "don\u{2019}t", "don\u{2018}t"] {
            assert_eq!(word.split_word_bounds().collect::<Vec<_>>(), [word]);
        }
    }

    #[test]
    fn a_curly_double_quotation_mark_matches_a_straight_one_in_an_entry() {
        let quoted = matcher(&["say \"yes\""], false);
        assert_eq!(
            matched(&quoted, "They say \u{201C}yes\u{201D} too often."),
            ["say \u{201C}yes\u{201D}"]
        );
    }

    #[test]
    fn a_catalogue_phrase_with_a_straight_apostrophe_matches_the_curly_one_word_writes() {
        let opener = matcher(&["you're absolutely right"], false);
        assert_eq!(
            matched(&opener, "You\u{2019}re absolutely right, of course."),
            ["You\u{2019}re absolutely right"]
        );
        assert_eq!(
            matched(&opener, "You're absolutely right, of course."),
            ["You're absolutely right"]
        );
    }

    #[test]
    fn an_opening_curly_mark_matches_its_straight_form_too() {
        let opening = matcher(&["'tis", "\"so"], false);
        assert_eq!(
            matched(&opening, "\u{2018}Tis \u{201C}so\u{201D}."),
            ["\u{2018}Tis", "\u{201C}so"]
        );
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
