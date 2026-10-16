//! Compiles a [`Checker`] once into an [`Evaluator`], and evaluates it over one unit's text. An
//! evaluation is a pure function of the text, the paragraph's form mode and the register: it calls
//! no model and no network, and the same inputs give the same matches in the same order.
//!
//! Evaluated today: the phrase list and the regex under the forbidden band, where every match is a
//! violation. The count and rate bands and the spelling-variant and word-count matchers arrive with
//! #94, and a LanguageTool rule id is evaluated in the grammar stage (#98). Until then each
//! evaluates to no match.

use std::ops::Range;

use regex::Regex;
use thiserror::Error;

use super::checker::{Band, Checker, FormMode, Matcher};
use super::phrase::PhraseMatcher;
use crate::flag::{ClauseId, Span};

/// Why a checker did not compile. It names the checker by the clause it serves and holds only the
/// checker's own data, never a document's text.
#[derive(Debug, Error)]
pub enum CompileError {
    #[error(
        "the regex of the checker for clause {clause} does not compile: {source}",
        clause = clause_id.0
    )]
    Regex {
        clause_id: ClauseId,
        #[source]
        source: regex::Error,
    },

    #[error(
        "a phrase of the checker for clause {clause} does not compile: {source}",
        clause = clause_id.0
    )]
    Phrase {
        clause_id: ClauseId,
        #[source]
        source: regex::Error,
    },
}

impl CompileError {
    pub fn clause_id(&self) -> ClauseId {
        match self {
            Self::Regex { clause_id, .. } | Self::Phrase { clause_id, .. } => *clause_id,
        }
    }
}

/// A checker compiled for evaluation.
#[derive(Debug)]
pub struct Evaluator {
    checker: Checker,
    /// `None` for a matcher that does not search the text.
    search: Option<TextSearch>,
}

#[derive(Debug)]
enum TextSearch {
    Phrases(PhraseMatcher),
    Regex(Regex),
}

impl Evaluator {
    /// Compiles the checker's regex, or one regex per phrase, here and once, so an evaluation
    /// compiles nothing.
    pub fn compile(checker: Checker) -> Result<Self, CompileError> {
        let clause_id = checker.clause_id;
        let search = match &checker.matcher {
            Matcher::PhraseList { entries } => Some(TextSearch::Phrases(
                PhraseMatcher::compile(entries)
                    .map_err(|source| CompileError::Phrase { clause_id, source })?,
            )),
            Matcher::Regex { source } => {
                Some(TextSearch::Regex(Regex::new(source).map_err(|error| {
                    CompileError::Regex {
                        clause_id,
                        source: error,
                    }
                })?))
            }
            Matcher::LanguageToolRule { .. }
            | Matcher::SpellingVariant { .. }
            | Matcher::WordCount => None,
        };
        Ok(Self { checker, search })
    }

    pub fn checker(&self) -> &Checker {
        &self.checker
    }

    /// The checker's matches in `text`, one unit's text, from a paragraph of form mode `mode`
    /// under the register `register`. Each is a [`Span`] anchored on `text` with its offset and
    /// length in UTF-16 code units, the anchor contract the flag types hold, and they come in the
    /// order they start in `text`. Empty where the checker does not declare `mode`, or where
    /// `register` is outside its scope.
    pub fn evaluate(&self, text: &str, mode: FormMode, register: &str) -> Vec<Span> {
        if !self.runs_in(mode, register) {
            return Vec::new();
        }
        let Some(search) = &self.search else {
            // A LanguageTool rule id is evaluated in the grammar stage (#98), and the
            // spelling-variant and word-count matchers arrive with #94.
            return Vec::new();
        };
        match self.checker.band {
            // Every match violates the band whatever unit holds it, so the unit changes nothing
            // here: it decides only which text the caller hands over.
            Band::Forbidden => search
                .find(text)
                .into_iter()
                .map(|range| span_in(text, range))
                .collect(),
            // The count and rate bands arrive with #94.
            Band::Count { .. } | Band::Rate { .. } => Vec::new(),
        }
    }

    fn runs_in(&self, mode: FormMode, register: &str) -> bool {
        let declares_mode = mode
            .checkable()
            .is_some_and(|mode| self.checker.form_modes.contains(&mode));
        let in_scope = self
            .checker
            .register_scope
            .as_ref()
            .is_none_or(|scope| scope.contains(register));
        declares_mode && in_scope
    }
}

impl TextSearch {
    /// The byte range of each match in `text`, in the order they start. An empty match is dropped:
    /// it has nothing to underline or cut, and a pattern that can match empty, such as an empty
    /// phrase, matches at every position.
    fn find(&self, text: &str) -> Vec<Range<usize>> {
        let ranges = match self {
            Self::Phrases(phrases) => phrases.find(text),
            Self::Regex(regex) => regex.find_iter(text).map(|found| found.range()).collect(),
        };
        ranges
            .into_iter()
            .filter(|range| !range.is_empty())
            .collect()
    }
}

/// Compiles each of `checkers` in order. A checker that does not compile is reported and the rest
/// still compile, so one bad checker costs only its own flags.
pub fn compile_all(
    checkers: impl IntoIterator<Item = Checker>,
) -> (Vec<Evaluator>, Vec<CompileError>) {
    let mut evaluators = Vec::new();
    let mut failures = Vec::new();
    for checker in checkers {
        match Evaluator::compile(checker) {
            Ok(evaluator) => evaluators.push(evaluator),
            Err(error) => failures.push(error),
        }
    }
    (evaluators, failures)
}

/// The byte range `range` of `text`, as a span anchored on `text`.
fn span_in(text: &str, range: Range<usize>) -> Span {
    Span {
        anchor: text.to_owned(),
        local_start: text[..range.start].encode_utf16().count(),
        local_length: text[range].encode_utf16().count(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::style::checker::{CheckableMode, PhraseEntry, Unit};

    /// A sentence-unit checker for clause 1 under the forbidden band, declared for prose and
    /// fragment in every register.
    fn checker(matcher: Matcher) -> Checker {
        Checker {
            clause_id: ClauseId(1),
            matcher,
            unit: Unit::Sentence,
            band: Band::Forbidden,
            form_modes: BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]),
            register_scope: None,
        }
    }

    fn phrases(phrases: &[&str]) -> Matcher {
        Matcher::PhraseList {
            entries: phrases
                .iter()
                .map(|phrase| PhraseEntry {
                    phrase: (*phrase).to_owned(),
                    prefix: false,
                })
                .collect(),
        }
    }

    fn regex(source: &str) -> Matcher {
        Matcher::Regex {
            source: source.to_owned(),
        }
    }

    fn compiled(checker: Checker) -> Evaluator {
        Evaluator::compile(checker).unwrap()
    }

    fn in_prose(evaluator: &Evaluator, text: &str) -> Vec<Span> {
        evaluator.evaluate(text, FormMode::Prose, "default")
    }

    /// The exact text a span addresses, sliced back out of its own anchor.
    fn flagged_text(span: &Span) -> String {
        let anchor: Vec<u16> = span.anchor.encode_utf16().collect();
        String::from_utf16_lossy(&anchor[span.local_start..span.local_start + span.local_length])
    }

    #[test]
    fn a_forbidden_phrase_held_twice_in_a_sentence_matches_twice_on_the_sentence() {
        let text = "🚀 The town boasts a harbour and boasts a castle.";
        let matches = in_prose(&compiled(checker(phrases(&["boasts a"]))), text);
        assert_eq!(matches.len(), 2, "{matches:#?}");
        assert!(matches.iter().all(|span| span.anchor == text));
        assert!(matches.iter().all(|span| flagged_text(span) == "boasts a"));
        // In UTF-16 code units, where the rocket ahead of both is two units and four bytes.
        assert_eq!([matches[0].local_start, matches[1].local_start], [12, 33]);
        assert!(matches.iter().all(|span| span.local_length == 8));
    }

    #[test]
    fn every_regex_match_anchors_on_the_text_at_its_own_offset() {
        let text = "Launch 🚀 today, then launch 🚀 again.";
        let emojis = compiled(checker(regex(
            r"[\x{1F300}-\x{1FAFF}\x{2600}-\x{27BF}\x{1F1E6}-\x{1F1FF}]",
        )));
        let matches = in_prose(&emojis, text);
        assert_eq!(matches.len(), 2, "{matches:#?}");
        assert!(matches.iter().all(|span| span.anchor == text));
        assert!(matches.iter().all(|span| flagged_text(span) == "🚀"));
        // The second offset counts both of the first rocket's two UTF-16 units.
        assert_eq!([matches[0].local_start, matches[1].local_start], [7, 29]);
    }

    #[test]
    fn a_regex_that_does_not_compile_fails_naming_its_clause() {
        let mut unclosed = checker(regex("(an unclosed group"));
        unclosed.clause_id = ClauseId(4);
        let error = Evaluator::compile(unclosed).unwrap_err();
        assert!(
            matches!(
                error,
                CompileError::Regex {
                    clause_id: ClauseId(4),
                    ..
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("clause 4"), "{error}");
    }

    #[test]
    fn compiling_a_set_reports_the_checker_that_fails_and_compiles_the_rest() {
        let mut first = checker(phrases(&["boasts a"]));
        first.clause_id = ClauseId(1);
        let mut broken = checker(regex("(an unclosed group"));
        broken.clause_id = ClauseId(2);
        let mut last = checker(regex("\u{2014}"));
        last.clause_id = ClauseId(3);

        let (evaluators, failures) = compile_all([first, broken, last]);
        let compiled: Vec<ClauseId> = evaluators
            .iter()
            .map(|evaluator| evaluator.checker().clause_id)
            .collect();
        assert_eq!(compiled, [ClauseId(1), ClauseId(3)]);
        let failed: Vec<ClauseId> = failures.iter().map(CompileError::clause_id).collect();
        assert_eq!(failed, [ClauseId(2)]);
    }

    #[test]
    fn a_checker_returns_no_match_in_a_mode_it_does_not_declare() {
        let text = "The town boasts a harbour.";
        let mut prose_only = checker(phrases(&["boasts a"]));
        prose_only.form_modes = BTreeSet::from([CheckableMode::Prose]);
        let prose_only = compiled(prose_only);
        assert_eq!(
            prose_only.evaluate(text, FormMode::Prose, "default").len(),
            1
        );
        assert!(prose_only
            .evaluate(text, FormMode::Fragment, "default")
            .is_empty());
    }

    #[test]
    fn no_checker_returns_a_match_in_code() {
        let both = compiled(checker(phrases(&["boasts a"])));
        assert!(both
            .evaluate("The town boasts a harbour.", FormMode::Code, "default")
            .is_empty());
    }

    #[test]
    fn a_checker_returns_no_match_under_a_register_outside_its_scope() {
        let text = "The town boasts a harbour.";
        let mut email_only = checker(phrases(&["boasts a"]));
        email_only.register_scope = Some(BTreeSet::from(["email".to_owned()]));
        let email_only = compiled(email_only);
        assert_eq!(email_only.evaluate(text, FormMode::Prose, "email").len(), 1);
        assert!(email_only
            .evaluate(text, FormMode::Prose, "default")
            .is_empty());
    }

    #[test]
    fn an_unset_register_scope_runs_in_every_register() {
        let every = compiled(checker(phrases(&["boasts a"])));
        for register in ["default", "email", "chat"] {
            let matches = every.evaluate("The town boasts a harbour.", FormMode::Prose, register);
            assert_eq!(matches.len(), 1, "{register}");
        }
    }

    #[test]
    fn the_matchers_and_bands_evaluated_elsewhere_return_no_match() {
        // #94 and #98 give these their evaluation, and this test changes with them.
        let text = "Perhaps the town boasts a harbour.";
        let elsewhere = [
            Matcher::LanguageToolRule {
                rule_id: "PASSIVE_VOICE".to_owned(),
            },
            Matcher::SpellingVariant {
                preferred_convention: None,
            },
            Matcher::WordCount,
        ];
        for matcher in elsewhere {
            assert!(
                in_prose(&compiled(checker(matcher.clone())), text).is_empty(),
                "{matcher:?}"
            );
        }
        let counting = [
            Band::Count {
                minimum: None,
                maximum: Some(0),
            },
            Band::Rate {
                maximum_per_hundred_words: 0.0,
            },
        ];
        for band in counting {
            let mut counted = checker(phrases(&["perhaps", "boasts a"]));
            counted.band = band.clone();
            assert!(in_prose(&compiled(counted), text).is_empty(), "{band:?}");
        }
    }

    #[test]
    fn an_empty_match_is_not_a_match() {
        let optional = compiled(checker(regex("x*")));
        assert!(in_prose(&optional, "abc").is_empty());
        let matches = in_prose(&optional, "a xx b");
        assert_eq!(matches.iter().map(flagged_text).collect::<Vec<_>>(), ["xx"]);

        let empty_phrase = compiled(checker(phrases(&[""])));
        assert!(in_prose(&empty_phrase, "abc").is_empty());
    }

    #[test]
    fn the_same_checker_over_the_same_text_gives_identical_matches_in_identical_order() {
        let text = "Its success is a testament to the team\u{2014}a testament to its care.";
        let listed = || checker(phrases(&["testament to", "\u{2014}", "is a testament to"]));
        let evaluator = compiled(listed());
        let first = in_prose(&evaluator, text);
        assert_eq!(
            first.iter().map(flagged_text).collect::<Vec<_>>(),
            [
                "is a testament to",
                "testament to",
                "\u{2014}",
                "testament to"
            ]
        );
        assert_eq!(in_prose(&evaluator, text), first);
        assert_eq!(in_prose(&compiled(listed()), text), first);
    }
}
