//! The spelling-variant matcher, and the table of variant pairs it reads, hand-written at
//! `src-tauri/resources/spelling-variants/pairs.json` and compiled in. The table's conventions, and
//! how its pairs were chosen, are recorded in `src-tauri/resources/spelling-variants/NOTICE.md`,
//! not repeated here.
//!
//! A word, by the engine's one definition in `crate::words`, matches a form case-insensitively and
//! whole, with a possessive `'s` read off first. The matcher's matches are the words in a form the
//! preferred convention does not write. With no preference, the preferred convention is the one
//! the unit's words write most often. Each word counts for every convention that writes its form,
//! a tie goes to the convention a word wrote first, and a tie that leaves goes to the convention
//! the table lists first. So a document unit flags each word of the minority convention, on an
//! equal count each word of the convention used second, and in a document of one convention
//! nothing.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::Deserialize;

use super::evaluator::Found;
use crate::words::words;

// Compiled in rather than read from the resource directory at run time, as the telltale catalogue
// is, so the app and `wa` always read the same table.
const PAIRS_JSON: &str =
    include_str!("../../../../src-tauri/resources/spelling-variants/pairs.json");

#[derive(Debug, Deserialize)]
struct PairsFile {
    conventions: Vec<String>,
    /// Each pair as its form under each convention, keyed by the convention's name.
    pairs: Vec<BTreeMap<String, String>>,
}

struct VariantTable {
    /// In the file's order, which breaks a tie that the first use leaves.
    conventions: Vec<String>,
    /// Each form of every pair, lowercase, with the index of each convention that writes it, in
    /// ascending order.
    writers: HashMap<String, Vec<usize>>,
}

impl VariantTable {
    fn compiled() -> &'static Self {
        static TABLE: OnceLock<VariantTable> = OnceLock::new();
        TABLE.get_or_init(|| {
            let file: PairsFile = serde_json::from_str(PAIRS_JSON).expect(
                "src-tauri/resources/spelling-variants/pairs.json is committed and checked by its \
                 tests",
            );
            let mut writers: HashMap<String, Vec<usize>> = HashMap::new();
            for pair in &file.pairs {
                for (index, convention) in file.conventions.iter().enumerate() {
                    let form = pair
                        .get(convention)
                        .expect("every pair names every convention, which the table's tests check");
                    writers.entry(form.to_lowercase()).or_default().push(index);
                }
            }
            Self {
                conventions: file.conventions,
                writers,
            }
        })
    }

    fn convention_index(&self, name: &str) -> Option<usize> {
        self.conventions
            .iter()
            .position(|convention| convention == name)
    }
}

/// A word of a unit written in one of the table's forms.
struct Token {
    found: Found,
    /// The conventions that write the word's form.
    writers: &'static [usize],
}

#[derive(Debug)]
pub(super) struct VariantMatcher {
    /// By index into the table's conventions. `None` prefers the unit's majority.
    preferred: Option<usize>,
}

/// A preferred convention named by a name the table does not hold.
#[derive(Debug)]
pub(super) struct UnknownConvention(pub(super) String);

impl VariantMatcher {
    pub(super) fn compile(preferred_convention: Option<&str>) -> Result<Self, UnknownConvention> {
        let preferred = preferred_convention
            .map(|name| {
                VariantTable::compiled()
                    .convention_index(name)
                    .ok_or_else(|| UnknownConvention(name.to_owned()))
            })
            .transpose()?;
        Ok(Self { preferred })
    }

    /// The words of `unit` in a form the preferred convention does not write, in document order.
    pub(super) fn find(&self, unit: &[&str]) -> Vec<Found> {
        let tokens = tokens(unit);
        let Some(preferred) = self.preferred.or_else(|| majority(&tokens)) else {
            return Vec::new();
        };
        tokens
            .into_iter()
            .filter(|token| !token.writers.contains(&preferred))
            .map(|token| token.found)
            .collect()
    }
}

/// Each word of `unit` in one of the table's forms, in document order.
fn tokens(unit: &[&str]) -> Vec<Token> {
    let table = VariantTable::compiled();
    unit.iter()
        .enumerate()
        .flat_map(|(sentence, text)| {
            words(text).filter_map(move |(start, word)| {
                let stem = without_possessive(word);
                table
                    .writers
                    .get(&stem.to_lowercase())
                    .map(|writers| Token {
                        found: Found {
                            sentence,
                            range: start..start + stem.len(),
                        },
                        writers,
                    })
            })
        })
        .collect()
}

/// The convention `tokens` write most often, or of those that tie, the one a token wrote first, or
/// of those that tie again, the one the table lists first. `None` where no token is written.
fn majority(tokens: &[Token]) -> Option<usize> {
    let conventions = VariantTable::compiled().conventions.len();
    let mut counts = vec![0_usize; conventions];
    let mut first_uses = vec![usize::MAX; conventions];
    for (position, token) in tokens.iter().enumerate() {
        for &convention in token.writers {
            counts[convention] += 1;
            first_uses[convention] = first_uses[convention].min(position);
        }
    }
    (0..conventions)
        .filter(|&convention| counts[convention] > 0)
        .max_by_key(|&convention| {
            (
                counts[convention],
                Reverse(first_uses[convention]),
                Reverse(convention),
            )
        })
}

/// `word` without a possessive `'s`, with a straight or a curly apostrophe, so that `colour's`
/// reads as `colour`.
fn without_possessive(word: &str) -> &str {
    let mut characters = word.char_indices().rev();
    match (characters.next(), characters.next()) {
        (Some((_, 's' | 'S')), Some((apostrophe, '\'' | '\u{2019}'))) => &word[..apostrophe],
        _ => word,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use spellbook::Dictionary;

    use super::*;
    use crate::flag::ClauseId;
    use crate::style::checker::{Band, Checker, Matcher, Unit};
    use crate::style::evaluator::tests::{banded, compiled, flagged_text, owners, violations};
    use crate::style::evaluator::{compile_all, CompileError, Evaluator, RuleOwner};
    use crate::test_support::dictionary_path;

    fn pairs_file() -> PairsFile {
        serde_json::from_str(PAIRS_JSON).unwrap()
    }

    fn preferring(convention: Option<&str>, unit: Unit) -> Evaluator {
        compiled(banded(
            Matcher::SpellingVariant {
                preferred_convention: convention.map(str::to_owned),
            },
            unit,
            Band::Forbidden,
        ))
    }

    /// The text of each of `evaluator`'s violations over the unit of `sentences`, in order.
    fn flagged_words(evaluator: &Evaluator, sentences: &[&str]) -> Vec<String> {
        violations(evaluator, sentences)
            .iter()
            .map(|violation| flagged_text(&violation.span))
            .collect()
    }

    #[test]
    fn the_table_names_three_conventions_and_each_pair_writes_two_forms_under_them() {
        let file = pairs_file();
        assert_eq!(file.conventions, ["british", "oxford", "american"]);
        // The notice states the count.
        assert_eq!(file.pairs.len(), 52);
        let mut names = file.conventions.clone();
        names.sort();
        for pair in &file.pairs {
            assert_eq!(pair.keys().cloned().collect::<Vec<_>>(), names, "{pair:?}");
            let forms: HashSet<&String> = pair.values().collect();
            assert_eq!(forms.len(), 2, "{pair:?}");
            assert!(
                forms.iter().all(|form| **form == form.to_lowercase()),
                "{pair:?}"
            );
        }
    }

    #[test]
    fn each_form_belongs_to_one_pair() {
        let file = pairs_file();
        let mut pair_of: HashMap<&str, usize> = HashMap::new();
        for (index, pair) in file.pairs.iter().enumerate() {
            for form in pair.values().collect::<HashSet<_>>() {
                let previous = pair_of.insert(form, index);
                assert!(
                    previous.is_none(),
                    "{form:?} is in pair {previous:?} and pair {index}"
                );
            }
        }
    }

    #[test]
    fn the_matcher_reads_the_table_compiled_into_the_engine() {
        let table = VariantTable::compiled();
        assert_eq!(table.conventions, ["british", "oxford", "american"]);
        assert_eq!(table.writers["colour"], [0, 1]);
        assert_eq!(table.writers["color"], [2]);
        assert_eq!(table.writers["organise"], [0]);
        assert_eq!(table.writers["organize"], [1, 2]);
        let forms: HashSet<String> = pairs_file()
            .pairs
            .into_iter()
            .flat_map(BTreeMap::into_values)
            .collect();
        assert_eq!(table.writers.len(), forms.len());
    }

    #[test]
    fn the_en_gb_dictionary_holds_each_pairs_british_form_and_no_other() {
        // The pair alone, without the Singapore supplement, and through spellbook's own check,
        // which computes no suggestions for a word it does not hold.
        let read = |name| std::fs::read_to_string(dictionary_path(name)).unwrap();
        let en_gb = Dictionary::new(&read("en_GB.aff"), &read("en_GB.dic")).unwrap();
        for pair in pairs_file().pairs {
            for (convention, form) in &pair {
                assert_eq!(
                    en_gb.check(form),
                    *form == pair["british"],
                    "the {convention} form {form:?}"
                );
            }
        }
    }

    #[test]
    fn with_a_preferred_convention_the_matches_are_the_words_in_a_form_it_does_not_write() {
        let british = ["The organised colour of the centre\u{2019}s theatre."];
        let american = ["The ORGANIZED Color of the center's theater."];
        let span = |convention| preferring(Some(convention), Unit::Span);

        assert!(flagged_words(&span("british"), &british).is_empty());
        assert_eq!(
            flagged_words(&span("british"), &american),
            ["ORGANIZED", "Color", "center", "theater"]
        );
        assert_eq!(flagged_words(&span("oxford"), &british), ["organised"]);
        assert_eq!(
            flagged_words(&span("oxford"), &american),
            ["Color", "center", "theater"]
        );
        assert_eq!(
            flagged_words(&span("american"), &british),
            ["organised", "colour", "centre", "theatre"]
        );
        assert!(flagged_words(&span("american"), &american).is_empty());
    }

    #[test]
    fn a_preferred_convention_the_table_does_not_hold_fails_to_compile_naming_its_clause() {
        let checker = |clause, name: &str| Checker {
            clause_id: ClauseId(clause),
            rule: banded(
                Matcher::SpellingVariant {
                    preferred_convention: Some(name.to_owned()),
                },
                Unit::Span,
                Band::Forbidden,
            ),
        };
        let (evaluators, failures) = compile_all([
            checker(1, "british"),
            checker(2, "canadian"),
            checker(3, "oxford"),
        ]);
        assert_eq!(
            owners(&evaluators),
            [
                RuleOwner::Clause(ClauseId(1)),
                RuleOwner::Clause(ClauseId(3))
            ]
        );
        assert!(
            matches!(
                &failures[..],
                [CompileError::UnknownConvention {
                    owner: RuleOwner::Clause(ClauseId(2)),
                    name,
                }] if name == "canadian"
            ),
            "{failures:?}"
        );
        assert!(
            failures[0].to_string().contains("clause 2"),
            "{}",
            failures[0]
        );
    }

    #[test]
    fn a_document_in_both_conventions_flags_each_word_of_the_less_frequent_one() {
        let document = [
            "The colour of the centre held.",
            "Its flavour changed.",
            "The color faded by noon.",
        ];
        let flagged = violations(&preferring(None, Unit::Document), &document);
        assert_eq!(flagged.len(), 1, "{flagged:#?}");
        assert_eq!(flagged[0].span.anchor, document[2]);
        assert_eq!(flagged_text(&flagged[0].span), "color");

        // Oxford writes both -ize and -our, so it is the majority of a document in both.
        let document = [
            "We organize the catalogue.",
            "We realize the colour is right.",
            "We organise the rest.",
        ];
        assert_eq!(
            flagged_words(&preferring(None, Unit::Document), &document),
            ["organise"]
        );
    }

    #[test]
    fn a_document_in_both_conventions_equally_often_flags_each_word_of_the_one_used_second() {
        let american_first = [
            "The color of the center held.",
            "The colour of the centre faded.",
        ];
        assert_eq!(
            flagged_words(&preferring(None, Unit::Document), &american_first),
            ["colour", "centre"]
        );
        let british_first = [american_first[1], american_first[0]];
        assert_eq!(
            flagged_words(&preferring(None, Unit::Document), &british_first),
            ["color", "center"]
        );
    }

    #[test]
    fn a_document_in_one_convention_flags_nothing() {
        let document = preferring(None, Unit::Document);
        for one_convention in [
            &["The colour of the centre held.", "Its flavour changed."][..],
            &["The color of the center held.", "Its flavor changed."],
            &[
                "We organize the catalogue.",
                "We realize the colour is right.",
            ],
            &["The ferry waits at the pier."],
        ] {
            assert!(
                violations(&document, one_convention).is_empty(),
                "{one_convention:?}"
            );
        }
    }

    #[test]
    fn with_no_preference_the_majority_is_taken_within_the_unit() {
        let sentence = preferring(None, Unit::Sentence);
        let sentences = [
            "The colour of the centre, and the color.",
            "The color of the center.",
        ];
        let flagged = violations(&sentence, &sentences);
        assert_eq!(flagged.len(), 1, "{flagged:#?}");
        assert_eq!(flagged[0].span.anchor, sentences[0]);
        assert_eq!(flagged_text(&flagged[0].span), "color");
    }
}
