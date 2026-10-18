//! Which of a unit's matches break a rule's band, and the numbers each flag's message states. A
//! maximum, a rate and the forbidden band flag the matches beyond their limit, so a flag marks what
//! to cut. A minimum has no match beyond its limit and underlines the whole unit instead.

use super::checker::Band;
use super::evaluator::{Breach, Found};
use crate::words::word_count;

/// The fewest words a rate band measures. Below fifty, one match is more than two per hundred
/// words, and the rate says more about the unit's length than about the habit it limits.
const RATE_BAND_MINIMUM_WORDS: usize = 50;

/// Each match or sentence of `unit` that breaks `band`, with its breach. `found` is the unit's
/// matches in document order, and the breaches of each kind keep that order.
pub(super) fn breaches(band: &Band, found: &[Found], unit: &[&str]) -> Vec<(Found, Breach)> {
    match *band {
        Band::Forbidden => beyond(found, 0, &Breach::Forbidden),
        Band::Count { minimum, maximum } => {
            let count = saturating_count(found.len());
            let mut breaches = Vec::new();
            if let Some(maximum) = maximum.filter(|&maximum| count > maximum) {
                breaches.extend(beyond(
                    found,
                    maximum as usize,
                    &Breach::AboveMaximum { count, maximum },
                ));
            }
            if let Some(minimum) = minimum.filter(|&minimum| count < minimum) {
                let breach = Breach::BelowMinimum {
                    count,
                    minimum,
                    shortfall: minimum - count,
                };
                breaches.extend(whole_sentences(unit).map(|sentence| (sentence, breach.clone())));
            }
            breaches
        }
        Band::Rate {
            maximum_per_hundred_words,
        } => {
            let words: usize = unit.iter().map(|sentence| word_count(sentence)).sum();
            if words < RATE_BAND_MINIMUM_WORDS {
                return Vec::new();
            }
            // The most matches a unit of this length holds within the band. A negative rate
            // allows none.
            let allowed = (maximum_per_hundred_words * words as f64 / 100.0).floor() as usize;
            let rate_per_hundred_words = found.len() as f64 * 100.0 / words as f64;
            beyond(
                found,
                allowed,
                &Breach::AboveRate {
                    rate_per_hundred_words,
                    maximum_per_hundred_words,
                },
            )
        }
    }
}

/// The matches after the first `allowed` of `found`, each with `breach`.
fn beyond(found: &[Found], allowed: usize, breach: &Breach) -> Vec<(Found, Breach)> {
    found
        .iter()
        .skip(allowed)
        .map(|found| (found.clone(), breach.clone()))
        .collect()
}

/// All of each sentence of `unit` that holds any text, as a match.
fn whole_sentences<'u>(unit: &'u [&str]) -> impl Iterator<Item = Found> + 'u {
    unit.iter()
        .enumerate()
        .filter(|(_, text)| !text.is_empty())
        .map(|(sentence, text)| Found {
            sentence,
            range: 0..text.len(),
        })
}

/// `count` as a band's count. No unit holds more than `u32::MAX` matches; one that did would
/// count as that many.
fn saturating_count(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use crate::style::checker::{Band, Matcher, Unit};
    use crate::style::evaluator::tests::{
        banded, compiled, count, flagged_text, phrases, violations,
    };
    use crate::style::evaluator::{Breach, Evaluator, Violation};
    use crate::words::word_count;

    fn hedges(unit: Unit, band: Band) -> Evaluator {
        compiled(banded(phrases(&["perhaps"]), unit, band))
    }

    /// The sentence each violation anchors on, and its offset there, in order.
    fn anchors(flagged: &[Violation]) -> Vec<(&str, usize)> {
        flagged
            .iter()
            .map(|violation| (violation.span.anchor.as_str(), violation.span.local_start))
            .collect()
    }

    #[test]
    fn a_maximum_flags_the_matches_beyond_it_with_the_count_and_the_limit() {
        let paragraph = [
            "Perhaps it rains, and perhaps it snows.",
            "Perhaps the ferry is late, or perhaps it is early.",
        ];
        let flagged = violations(&hedges(Unit::Paragraph, count(None, Some(2))), &paragraph);
        // The third and the fourth, both in the second sentence, and neither of the first two.
        assert_eq!(
            anchors(&flagged),
            [
                (paragraph[1], 0),
                (paragraph[1], paragraph[1].rfind("perhaps").unwrap())
            ]
        );
        assert!(flagged
            .iter()
            .all(|violation| flagged_text(&violation.span).eq_ignore_ascii_case("perhaps")));
        assert!(flagged.iter().all(|violation| violation.breach
            == Breach::AboveMaximum {
                count: 4,
                maximum: 2
            }));
    }

    #[test]
    fn a_count_at_its_maximum_flags_nothing() {
        let paragraph = ["Perhaps it rains.", "The ferry waits, perhaps."];
        let flagged = violations(&hedges(Unit::Paragraph, count(None, Some(2))), &paragraph);
        assert!(flagged.is_empty(), "{flagged:#?}");
    }

    #[test]
    fn the_matches_beyond_a_maximum_are_flagged_in_document_order_each_on_its_own_sentence() {
        // The second sentence's match starts further into its sentence than the third's, so an
        // order by offset alone would put the third first.
        let paragraph = [
            "Perhaps it rains.",
            "It may snow later tonight, perhaps.",
            "Perhaps not.",
        ];
        let flagged = violations(&hedges(Unit::Paragraph, count(None, Some(1))), &paragraph);
        assert_eq!(
            anchors(&flagged),
            [
                (paragraph[1], paragraph[1].find("perhaps").unwrap()),
                (paragraph[2], 0)
            ]
        );
        assert!(flagged.iter().all(|violation| violation.breach
            == Breach::AboveMaximum {
                count: 3,
                maximum: 1
            }));
    }

    #[test]
    fn a_minimum_underlines_every_sentence_of_the_unit_with_the_shortfall() {
        let paragraph = [
            "The ferry is late.",
            "Perhaps the tide turned.",
            "It will come.",
        ];
        let flagged = violations(&hedges(Unit::Paragraph, count(Some(2), None)), &paragraph);
        assert_eq!(flagged.len(), 3, "{flagged:#?}");
        for (violation, sentence) in flagged.iter().zip(paragraph) {
            assert_eq!(violation.span.anchor, sentence);
            assert_eq!(flagged_text(&violation.span), sentence);
            assert_eq!(
                violation.breach,
                Breach::BelowMinimum {
                    count: 1,
                    minimum: 2,
                    shortfall: 1
                }
            );
        }
    }

    #[test]
    fn a_sentence_over_its_word_count_maximum_flags_the_words_beyond_it_as_one_span() {
        let sentence = "The ferry from the old harbour leaves at nine each morning, crosses the \
                        strait in forty minutes, and docks beside the market where the \
                        fishmongers set out their catch before the town wakes up.";
        assert_eq!(word_count(sentence), 34);
        let at_most_thirty = compiled(banded(
            Matcher::WordCount,
            Unit::Sentence,
            count(None, Some(30)),
        ));

        let flagged = violations(&at_most_thirty, &[sentence]);
        assert_eq!(flagged.len(), 1, "{flagged:#?}");
        assert_eq!(flagged[0].span.anchor, sentence);
        assert_eq!(flagged_text(&flagged[0].span), "the town wakes up");
        assert_eq!(
            flagged[0].breach,
            Breach::AboveMaximum {
                count: 34,
                maximum: 30
            }
        );

        let thirty_words = sentence.replace(" before the town wakes up", " early");
        assert_eq!(word_count(&thirty_words), 30);
        assert!(violations(&at_most_thirty, &[&thirty_words]).is_empty());
    }

    #[test]
    fn the_word_count_matcher_counts_words_by_the_engines_one_definition() {
        // Eleven words by `crate::words`: the curly apostrophe and the decimal point hold their
        // word together, the hyphen and the slash split theirs, and the rocket is no word.
        let sentence = "Don\u{2019}t miss the well-known 3.5 km walk, rated 4/5 🚀!";
        let at_most_ten = compiled(banded(
            Matcher::WordCount,
            Unit::Sentence,
            count(None, Some(10)),
        ));
        let flagged = violations(&at_most_ten, &[sentence]);
        assert_eq!(flagged.len(), 1, "{flagged:#?}");
        assert_eq!(flagged_text(&flagged[0].span), "5");
        assert_eq!(
            flagged[0].breach,
            Breach::AboveMaximum {
                count: 11,
                maximum: 10
            }
        );
    }

    #[test]
    fn a_word_count_over_a_paragraphs_maximum_flags_the_words_beyond_it_once_in_each_sentence() {
        let paragraph = [
            "The ferry leaves at nine.",
            "It crosses the strait.",
            "It docks by ten.",
        ];
        let at_most_seven = compiled(banded(
            Matcher::WordCount,
            Unit::Paragraph,
            count(None, Some(7)),
        ));
        let flagged = violations(&at_most_seven, &paragraph);
        assert_eq!(
            flagged
                .iter()
                .map(|violation| flagged_text(&violation.span))
                .collect::<Vec<_>>(),
            ["the strait", "It docks by ten"]
        );
    }

    /// A ten-word sentence holding one "perhaps", told apart from its neighbours by `day`.
    fn hedged(day: &str) -> String {
        format!("Perhaps the harbour opens at nine on {day} mornings again.")
    }

    /// A ten-word sentence holding no "perhaps".
    const PLAIN: &str = "The ferry leaves the pier when the tide turns again.";

    fn total_words(unit: &[&str]) -> usize {
        unit.iter().map(|sentence| word_count(sentence)).sum()
    }

    #[test]
    fn a_rate_band_over_several_hundred_words_flags_the_matches_beyond_the_count_it_allows() {
        let days = [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        let hedged_sentences: Vec<String> = days.iter().map(|day| hedged(day)).collect();
        // Every fifth sentence of thirty holds a "perhaps".
        let document: Vec<&str> = (0..30)
            .map(|index| {
                if index % 5 == 0 {
                    hedged_sentences[index / 5].as_str()
                } else {
                    PLAIN
                }
            })
            .collect();
        assert_eq!(total_words(&document), 300);

        let flagged = violations(
            &hedges(
                Unit::Document,
                Band::Rate {
                    maximum_per_hundred_words: 1.0,
                },
            ),
            &document,
        );
        // One per hundred words allows three in three hundred, so the fourth to the sixth are
        // flagged.
        assert_eq!(
            anchors(&flagged),
            [
                (hedged_sentences[3].as_str(), 0),
                (hedged_sentences[4].as_str(), 0),
                (hedged_sentences[5].as_str(), 0)
            ]
        );
        assert!(flagged.iter().all(|violation| violation.breach
            == Breach::AboveRate {
                rate_per_hundred_words: 2.0,
                maximum_per_hundred_words: 1.0
            }));
    }

    #[test]
    fn a_rate_band_flags_nothing_in_a_unit_of_forty_nine_words() {
        let (monday, tuesday, wednesday) =
            (hedged("Monday"), hedged("Tuesday"), hedged("Wednesday"));
        let nine_words = "Perhaps the ferry leaves the pier when tide turns.";
        let paragraph = [
            monday.as_str(),
            tuesday.as_str(),
            wednesday.as_str(),
            PLAIN,
            nine_words,
        ];
        assert_eq!(total_words(&paragraph), 49);

        // Four in forty-nine words is over eight per hundred, and goes unmeasured.
        let flagged = violations(
            &hedges(
                Unit::Paragraph,
                Band::Rate {
                    maximum_per_hundred_words: 2.0,
                },
            ),
            &paragraph,
        );
        assert!(flagged.is_empty(), "{flagged:#?}");
    }

    #[test]
    fn a_rate_band_measures_a_unit_of_fifty_words() {
        let (monday, tuesday, wednesday) =
            (hedged("Monday"), hedged("Tuesday"), hedged("Wednesday"));
        let ten_words = "Perhaps the ferry leaves the pier when the tide turns.";
        let paragraph = [
            monday.as_str(),
            tuesday.as_str(),
            wednesday.as_str(),
            PLAIN,
            ten_words,
        ];
        assert_eq!(total_words(&paragraph), 50);

        let flagged = violations(
            &hedges(
                Unit::Paragraph,
                Band::Rate {
                    maximum_per_hundred_words: 2.0,
                },
            ),
            &paragraph,
        );
        // Two per hundred words allows one in fifty, so the second to the fourth are flagged.
        assert_eq!(
            anchors(&flagged),
            [(paragraph[1], 0), (paragraph[2], 0), (paragraph[4], 0)]
        );
        assert!(flagged.iter().all(|violation| violation.breach
            == Breach::AboveRate {
                rate_per_hundred_words: 8.0,
                maximum_per_hundred_words: 2.0
            }));
    }

    #[test]
    fn a_rate_band_within_its_rate_flags_nothing() {
        let (monday, tuesday) = (hedged("Monday"), hedged("Tuesday"));
        let paragraph = [monday.as_str(), PLAIN, PLAIN, PLAIN, tuesday.as_str()];
        assert_eq!(total_words(&paragraph), 50);
        let flagged = violations(
            &hedges(
                Unit::Paragraph,
                Band::Rate {
                    maximum_per_hundred_words: 4.0,
                },
            ),
            &paragraph,
        );
        assert!(flagged.is_empty(), "{flagged:#?}");
    }

    #[test]
    fn a_rate_band_allows_only_the_whole_matches_its_rate_covers() {
        let (monday, tuesday) = (hedged("Monday"), hedged("Tuesday"));
        let paragraph = [monday.as_str(), PLAIN, PLAIN, PLAIN, tuesday.as_str()];
        assert_eq!(total_words(&paragraph), 50);
        let flagged = violations(
            &hedges(
                Unit::Paragraph,
                Band::Rate {
                    maximum_per_hundred_words: 3.0,
                },
            ),
            &paragraph,
        );
        // Three per hundred words is one and a half in fifty, which allows one, so the second of
        // two is flagged.
        assert_eq!(anchors(&flagged), [(paragraph[4], 0)]);
        assert_eq!(
            flagged[0].breach,
            Breach::AboveRate {
                rate_per_hundred_words: 4.0,
                maximum_per_hundred_words: 3.0
            }
        );
    }

    #[test]
    fn a_count_band_with_its_minimum_above_its_maximum_gives_both_breaches_in_document_order() {
        let paragraph = ["Perhaps it rains, perhaps not.", "Perhaps it snows."];
        let flagged = violations(
            &hedges(Unit::Paragraph, count(Some(5), Some(1))),
            &paragraph,
        );
        let above = Breach::AboveMaximum {
            count: 3,
            maximum: 1,
        };
        let below = Breach::BelowMinimum {
            count: 3,
            minimum: 5,
            shortfall: 2,
        };
        assert_eq!(
            flagged
                .iter()
                .map(|violation| (flagged_text(&violation.span), &violation.breach))
                .collect::<Vec<_>>(),
            [
                (paragraph[0].to_owned(), &below),
                ("perhaps".to_owned(), &above),
                ("Perhaps".to_owned(), &above),
                (paragraph[1].to_owned(), &below),
            ]
        );
    }
}
