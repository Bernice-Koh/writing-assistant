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
