//! The catalogue's stems, each measured against the vendored dictionary. A single-word entry
//! carries the prefix flag and is written as its stem: the shortest prefix for which every word of
//! the dictionary pair and the Singapore supplement that starts with it is an inflection or a
//! derivative of the entry's word. Each test lists the words its stem covers, names a word the
//! prefix one letter shorter would cover that is not a form, and checks that the catalogue flags
//! every form it lists.

use std::collections::BTreeSet;

use super::patterns;
use super::tests::{flagged_text, flags_of, pattern, phrase_entries, scan};
use crate::style::checker::PhraseEntry;
use crate::test_support::dictionary_words;
use crate::words::words;

/// Every word of the dictionary pair and the supplement that starts with `prefix`, ignoring case,
/// lowercased.
fn covered_by(prefix: &str) -> BTreeSet<String> {
    dictionary_words()
        .iter()
        .map(|word| word.to_lowercase())
        .filter(|word| word.starts_with(prefix))
        .collect()
}

/// Asserts that `stem` is an entry of `pattern_id` with the prefix flag, that it covers `forms`
/// and no other word, and that the prefix one letter shorter also covers `not_a_form`, which is
/// not one of them, so no shorter stem passes. Then that the catalogue flags each of `forms`
/// whole, under `pattern_id`.
fn assert_stem(pattern_id: &str, stem: &str, forms: &[&str], not_a_form: &str) {
    assert!(
        phrase_entries(pattern(pattern_id))
            .iter()
            .any(|entry| entry.phrase == stem && entry.prefix),
        "{stem:?} is not a prefix entry of {pattern_id:?}"
    );
    let listed: BTreeSet<String> = forms.iter().map(|form| (*form).to_owned()).collect();
    assert_eq!(covered_by(stem), listed, "the words {stem:?} covers");

    let shorter = &stem[..stem.len() - 1];
    assert!(
        covered_by(shorter).contains(not_a_form) && !listed.contains(not_a_form),
        "{shorter:?} covers {not_a_form:?}, which is not a form"
    );

    for form in forms {
        let sentence = format!("They called it {form} again.");
        let flags = scan(&sentence);
        let flagged: Vec<String> = flags_of(&flags, pattern_id)
            .into_iter()
            .map(flagged_text)
            .collect();
        assert_eq!(flagged, [*form], "{sentence}: {flags:#?}");
    }
}

#[test]
fn intric_covers_the_forms_of_intricate_and_intricacies_alone() {
    assert_stem(
        "ai-vocabulary-words",
        "intric",
        &[
            "intricacies",
            "intricacy",
            "intricacy's",
            "intricate",
            "intricately",
            "intricateness",
            "intricateness's",
            "intricatenesses",
        ],
        "intrigue",
    );
}

#[test]
fn pivota_covers_the_forms_of_pivotal_alone() {
    // `pivot` is the word `pivotal` derives from, not a form of it.
    assert_stem(
        "ai-vocabulary-words",
        "pivota",
        &["pivotal", "pivotally"],
        "pivot",
    );
}

#[test]
fn breatht_covers_the_forms_of_breathtaking_alone() {
    assert_stem(
        "promotional-language",
        "breatht",
        &["breathtaking", "breathtakingly"],
        "breath",
    );
}

#[test]
fn exemplif_covers_the_forms_of_exemplifies_alone() {
    // `exempli` is the dictionary's entry for the Latin of "exempli causa".
    assert_stem(
        "promotional-language",
        "exemplif",
        &[
            "exemplification",
            "exemplification's",
            "exemplifications",
            "exemplified",
            "exemplifier",
            "exemplifier's",
            "exemplifiers",
            "exemplifies",
            "exemplify",
            "exemplifying",
        ],
        "exempli",
    );
}

/// Whether `phrase` is one word and nothing else, by the engine's one definition of a word.
fn is_a_single_word(phrase: &str) -> bool {
    matches!(words(phrase).collect::<Vec<_>>()[..], [(0, word)] if word == phrase)
}

#[test]
fn every_single_word_entry_carries_the_prefix_flag_and_every_stem_is_measured_above() {
    let entries: Vec<&PhraseEntry> = patterns().iter().flat_map(phrase_entries).collect();
    let single_words: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| is_a_single_word(&entry.phrase))
        .map(|entry| entry.phrase.as_str())
        .collect();
    let stems: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| entry.prefix)
        .map(|entry| entry.phrase.as_str())
        .collect();
    assert_eq!(single_words, stems);
    assert_eq!(
        stems,
        BTreeSet::from(["breatht", "exemplif", "intric", "pivota"])
    );
    // A hyphenated compound is two words, and an entry closed by a mark is more than a word, so
    // both match whole.
    assert!(!is_a_single_word("must-visit"));
    assert!(!is_a_single_word("certainly!"));
}

#[test]
fn a_form_with_a_curly_possessive_is_flagged_whole() {
    let flags = scan("The intricacy\u{2019}s cost was clear.");
    let flagged: Vec<String> = flags_of(&flags, "ai-vocabulary-words")
        .into_iter()
        .map(flagged_text)
        .collect();
    assert_eq!(flagged, ["intricacy\u{2019}s"]);
}
