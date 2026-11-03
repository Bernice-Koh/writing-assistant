//! The catalogue's structural patterns, which count a habit over a paragraph or the document
//! rather than ban a phrasing in a span. Each is evaluated through its compiled catalogue entry
//! over a unit's sentences, in the form modes the issue that encoded it declares: a unit just
//! inside its band raises nothing, and one just outside raises a flag on the match beyond the
//! limit.

use std::collections::BTreeSet;

use super::tests::{pattern, STRUCTURAL_PATTERNS};
use super::{patterns, Catalogue};
use crate::store::Config;
use crate::style::checker::{Band, CheckableMode, FormMode, Matcher, Unit};
use crate::style::evaluator::tests::flagged_text;
use crate::style::evaluator::{Breach, ModeSentence, Violation};
use crate::words::word_count;

/// The violations of the catalogue's pattern `id`, compiled as the app compiles it, over the unit
/// of `sentences`, each a sentence of a paragraph of `mode`, under the default register.
fn violations(id: &str, mode: FormMode, sentences: &[&str]) -> Vec<Violation> {
    let catalogue = Catalogue::every_pattern();
    let compiled = catalogue
        .patterns
        .iter()
        .find(|compiled| compiled.pattern.id == id)
        .unwrap_or_else(|| panic!("no pattern {id:?}"));
    let sentences: Vec<ModeSentence<'_>> = sentences
        .iter()
        .map(|&text| ModeSentence { text, mode })
        .collect();
    compiled.evaluator.evaluate(&sentences, "default")
}

/// The text each of `violations` underlines, in order.
fn flagged(violations: &[Violation]) -> Vec<String> {
    violations
        .iter()
        .map(|violation| flagged_text(&violation.span))
        .collect()
}

/// The source of the regex `id`'s matcher holds.
fn regex_source(id: &str) -> &'static str {
    match &pattern(id).rule.matcher {
        Matcher::Regex { source } => source,
        other => panic!("{id:?} has the matcher {other:?}"),
    }
}

#[test]
fn the_structural_patterns_are_the_patterns_of_a_paragraph_or_the_document_unit() {
    let of_a_larger_unit: Vec<&str> = patterns()
        .iter()
        .filter(|pattern| !pattern.rule.unit.is_within_a_sentence())
        .map(|pattern| pattern.id.as_str())
        .collect();
    assert_eq!(of_a_larger_unit, STRUCTURAL_PATTERNS);
}

#[test]
fn every_structural_pattern_is_on_by_default_with_a_toggle_of_its_own() {
    let defaults = Config::default();
    let loaded = Catalogue::load(&defaults);
    for id in STRUCTURAL_PATTERNS {
        assert_eq!(defaults.telltale_toggles.get(id), Some(&true), "{id:?}");
        assert!(loaded.enabled_pattern_ids().any(|on| on == id), "{id:?}");
        let switched_off = Config {
            telltale_toggles: [(id.to_owned(), false)].into(),
            ..Config::default()
        };
        let without = Catalogue::load(&switched_off);
        assert!(!without.enabled_pattern_ids().any(|on| on == id), "{id:?}");
        assert_eq!(
            without.enabled_pattern_ids().count(),
            loaded.enabled_pattern_ids().count() - 1,
            "{id:?}"
        );
    }
}

/// A prose paragraph of three sentences with one list of three items, the source's example, in
/// its second sentence.
const ONE_LIST: [&str; 3] = [
    "The conference runs for two days in the old harbour hall.",
    "The event features keynote sessions, panel discussions, and networking opportunities.",
    "Tickets go on sale in March.",
];

/// A prose paragraph of two sentences with a list of three items in each, the source's example.
const TWO_LISTS: [&str; 2] = [
    "The event features keynote sessions, panel discussions, and networking opportunities.",
    "Attendees can expect innovation, inspiration, and industry insights.",
];

#[test]
fn rule_of_three_counts_lists_per_paragraph_of_prose_at_most_one() {
    let rule_of_three = pattern("rule-of-three");
    assert_eq!(rule_of_three.rule.unit, Unit::Paragraph);
    assert_eq!(
        rule_of_three.rule.band,
        Band::Count {
            minimum: None,
            maximum: Some(1)
        }
    );
    assert_eq!(
        rule_of_three.rule.form_modes,
        BTreeSet::from([CheckableMode::Prose])
    );
    assert_eq!(rule_of_three.rule.register_scope, None);
    assert_eq!(
        rule_of_three.source,
        "humanizer SKILL.md section 10, Wikipedia:Signs of AI writing"
    );
}

#[test]
fn a_prose_paragraph_with_one_list_of_three_raises_nothing() {
    let flagged = violations("rule-of-three", FormMode::Prose, &ONE_LIST);
    assert!(flagged.is_empty(), "{flagged:#?}");
}

#[test]
fn a_prose_paragraph_with_two_lists_of_three_flags_the_second() {
    let found = violations("rule-of-three", FormMode::Prose, &TWO_LISTS);
    // From the last word of the first item to the first word of the last, so the underline lies
    // within the list.
    assert_eq!(flagged(&found), ["innovation, inspiration, and industry"]);
    assert_eq!(found[0].sentence, 1);
    assert_eq!(found[0].span.anchor, TWO_LISTS[1]);
    assert_eq!(
        found[0].breach,
        Breach::AboveMaximum {
            count: 2,
            maximum: 1
        }
    );
}

#[test]
fn a_fragment_with_two_lists_of_three_raises_nothing() {
    let flagged = violations("rule-of-three", FormMode::Fragment, &TWO_LISTS);
    assert!(flagged.is_empty(), "{flagged:#?}");
}

#[test]
fn a_list_of_four_or_more_items_is_one_match() {
    let paragraph = [
        "We packed tents, stoves, maps, rope and a first-aid kit.",
        "The walk passes a mill, a chapel or a ford.",
    ];
    let found = violations("rule-of-three", FormMode::Prose, &paragraph);
    // Read as two lists, the five items would make the count three.
    assert_eq!(flagged(&found), ["mill, a chapel or a"]);
    assert_eq!(
        found[0].breach,
        Breach::AboveMaximum {
            count: 2,
            maximum: 1
        }
    );
}

#[test]
fn a_list_of_three_is_matched_with_or_without_the_serial_comma_and_with_or() {
    let regex = regex::Regex::new(regex_source("rule-of-three")).unwrap();
    for (sentence, list) in [
        (
            "We sell red, green, and blue paint.",
            "red, green, and blue",
        ),
        ("We sell red, green and blue paint.", "red, green and blue"),
        ("Tea, coffee or juice is served.", "Tea, coffee or juice"),
        (
            "The harbour, the old market and the castle are open.",
            "harbour, the old market and the",
        ),
    ] {
        let found: Vec<&str> = regex
            .find_iter(sentence)
            .map(|found| found.as_str())
            .collect();
        assert_eq!(found, [list], "{sentence}");
    }
}

#[test]
fn an_ordinary_pair_or_a_list_without_a_conjunction_is_no_match() {
    let regex = regex::Regex::new(regex_source("rule-of-three")).unwrap();
    for sentence in [
        "The ferry and the bus run late.",
        "It rains, and the wind drops.",
        "Bring tea or coffee.",
        "We sell red, green, blue.",
        "Prices run between 1,000 and 2,000 pounds.",
    ] {
        assert!(regex.find(sentence).is_none(), "{sentence}");
    }
    // An opening phrase before a pair has a list's shape, which the regex cannot tell from one.
    assert_eq!(
        regex
            .find("In 2020, sales rose and costs fell.")
            .map(|found| found.as_str()),
        Some("2020, sales rose and costs")
    );
    // A paragraph of pairs beside one list holds one match.
    let paragraph = [
        "The ferry and the bus run late, and the pier is shut.",
        "We sell red, green and blue paint.",
    ];
    let flagged = violations("rule-of-three", FormMode::Prose, &paragraph);
    assert!(flagged.is_empty(), "{flagged:#?}");
}

/// A sentence of ten words with no hyphenated pair.
const PLAIN: &str = "The ferry left the harbour at noon on a Tuesday.";

/// Five sentences of ten words, each holding one hyphenated pair, whose two halves count as two
/// words.
const PAIRED: [&str; 5] = [
    "The cross-functional team met at noon on a Tuesday.",
    "Their high-quality report reached the board by late Friday.",
    "A data-driven plan was agreed before the early spring.",
    "The long-term budget was set for the coming year.",
    "A real-time feed now shows each ferry and bus.",
];

/// A document of `sentences` sentences of ten words, whose sentences at 0, 4, 8 and so on hold the
/// first `pairs` of [`PAIRED`] and whose others are [`PLAIN`].
fn ten_word_sentences(sentences: usize, pairs: usize) -> Vec<&'static str> {
    (0..sentences)
        .map(|index| match index % 4 {
            0 if index / 4 < pairs => PAIRED[index / 4],
            _ => PLAIN,
        })
        .collect()
}

fn words_in(document: &[&str]) -> usize {
    document.iter().map(|sentence| word_count(sentence)).sum()
}

#[test]
fn hyphenated_word_pairs_is_a_rate_over_the_document_in_prose_and_fragments() {
    let pairs = pattern("hyphenated-word-pairs");
    assert_eq!(pairs.rule.unit, Unit::Document);
    assert_eq!(
        pairs.rule.band,
        Band::Rate {
            maximum_per_hundred_words: 2.0
        }
    );
    assert_eq!(
        pairs.rule.form_modes,
        BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment])
    );
    assert_eq!(pairs.rule.register_scope, None);
    assert_eq!(
        pairs.source,
        "humanizer SKILL.md section 26, Wikipedia:Signs of AI writing"
    );
}

#[test]
fn a_document_of_two_hundred_words_with_four_pairs_raises_nothing() {
    let document = ten_word_sentences(20, 4);
    assert_eq!(words_in(&document), 200);
    for mode in [FormMode::Prose, FormMode::Fragment] {
        let flagged = violations("hyphenated-word-pairs", mode, &document);
        assert!(flagged.is_empty(), "{mode:?}: {flagged:#?}");
    }
}

#[test]
fn a_document_of_two_hundred_words_with_five_pairs_flags_the_fifth() {
    let document = ten_word_sentences(20, 5);
    assert_eq!(words_in(&document), 200);
    for mode in [FormMode::Prose, FormMode::Fragment] {
        let found = violations("hyphenated-word-pairs", mode, &document);
        assert_eq!(flagged(&found), ["real-time"], "{mode:?}");
        assert_eq!(found[0].sentence, 16, "{mode:?}");
        assert_eq!(
            found[0].breach,
            Breach::AboveRate {
                rate_per_hundred_words: 2.5,
                maximum_per_hundred_words: 2.0
            },
            "{mode:?}"
        );
    }
    let in_code = violations("hyphenated-word-pairs", FormMode::Code, &document);
    assert!(in_code.is_empty(), "{in_code:#?}");
}

#[test]
fn a_single_compound_in_a_long_document_raises_nothing() {
    let document = ten_word_sentences(20, 1);
    assert_eq!(document[0], PAIRED[0]);
    let flagged = violations("hyphenated-word-pairs", FormMode::Prose, &document);
    assert!(flagged.is_empty(), "{flagged:#?}");
}

#[test]
fn a_document_under_fifty_words_with_three_pairs_raises_nothing() {
    let under_fifty = [PAIRED[0], PAIRED[1], PAIRED[2], PLAIN];
    assert_eq!(words_in(&under_fifty), 40);
    let unmeasured = violations("hyphenated-word-pairs", FormMode::Prose, &under_fifty);
    assert!(unmeasured.is_empty(), "{unmeasured:#?}");

    // At fifty words the rate is measured, and one pair is all fifty words allow.
    let fifty = [PAIRED[0], PAIRED[1], PAIRED[2], PLAIN, PLAIN];
    assert_eq!(words_in(&fifty), 50);
    let found = violations("hyphenated-word-pairs", FormMode::Prose, &fifty);
    assert_eq!(flagged(&found), ["high-quality", "data-driven"]);
}

#[test]
fn words_of_letters_joined_by_single_hyphens_are_one_match() {
    let regex = regex::Regex::new(regex_source("hyphenated-word-pairs")).unwrap();
    for (sentence, pairs) in [
        ("The cross-functional team met.", &["cross-functional"][..]),
        ("It runs an end-to-end test.", &["end-to-end"][..]),
        (
            "Send an e\u{2011}mail and a cross\u{2010}check.",
            &["e\u{2011}mail", "cross\u{2010}check"][..],
        ),
        (
            "The post-war years and the pre- and post-war ones.",
            &["post-war", "post-war"][..],
        ),
    ] {
        let found: Vec<&str> = regex
            .find_iter(sentence)
            .map(|found| found.as_str())
            .collect();
        assert_eq!(found, pairs, "{sentence}");
    }
}

#[test]
fn a_number_range_a_dash_a_digit_or_a_hyphen_at_a_words_edge_is_no_match() {
    let regex = regex::Regex::new(regex_source("hyphenated-word-pairs")).unwrap();
    for sentence in [
        "Read pages 10-12 of the 2020-21 report.",
        "The COVID-19 figures and the x86-64 build.",
        "A dash -- like this, and a dash - like that.",
        "A dash\u{2014}like this.",
        "The -ish suffix, and the pre- prefix.",
    ] {
        assert!(regex.find(sentence).is_none(), "{sentence}");
    }
}
