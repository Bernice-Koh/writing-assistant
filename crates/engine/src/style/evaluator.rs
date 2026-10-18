//! Compiles a [`Rule`] once into an [`Evaluator`], and evaluates it over one unit's text. An
//! evaluation is a pure function of the text, the paragraph's form mode and the register: it calls
//! no model and no network, and the same inputs give the same matches in the same order.
//!
//! Evaluated today: the phrase list and the regex under the forbidden band, where every match is a
//! violation. The count and rate bands and the spelling-variant and word-count matchers arrive with
//! #94, and a LanguageTool rule id is evaluated in the grammar stage (#98). Until then each
//! evaluates to no match.

use std::fmt;
use std::ops::Range;

use regex::Regex;
use thiserror::Error;

use super::checker::{Band, Checker, FormMode, Matcher, Rule};
use super::phrase::{PhraseError, PhraseMatcher};
use crate::flag::{ClauseId, Span};

/// What a rule belongs to, which names it wherever the rule itself is not quoted, as in a compile
/// error. A telltale pattern's rule belongs to its pattern id (#96).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleOwner {
    /// The checker that serves this Style Card clause.
    Clause(ClauseId),
}

impl fmt::Display for RuleOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clause(clause_id) => write!(formatter, "the checker for clause {}", clause_id.0),
        }
    }
}

/// Why a rule did not compile. It names the rule by its owner and holds only the rule's own data,
/// never a document's text.
#[derive(Debug, Error)]
pub enum CompileError {
    #[error("the regex of {owner} does not compile: {source}")]
    Regex {
        owner: RuleOwner,
        #[source]
        source: regex::Error,
    },

    #[error("a phrase of {owner} does not compile: {source}")]
    Phrase {
        owner: RuleOwner,
        #[source]
        source: regex::Error,
    },

    /// The prefix flag lets a one-word entry match the longer words that start with it, and
    /// means nothing on an entry of more words or none.
    #[error(
        "the phrase at index {entry_index} of {owner} carries the prefix flag and is not one word"
    )]
    PrefixNotOneWord {
        owner: RuleOwner,
        entry_index: usize,
    },
}

impl CompileError {
    pub fn owner(&self) -> &RuleOwner {
        match self {
            Self::Regex { owner, .. }
            | Self::Phrase { owner, .. }
            | Self::PrefixNotOneWord { owner, .. } => owner,
        }
    }
}

/// A rule compiled for evaluation.
#[derive(Debug)]
pub struct Evaluator {
    owner: RuleOwner,
    rule: Rule,
    /// `None` for a matcher that does not search the text.
    search: Option<TextSearch>,
}

#[derive(Debug)]
enum TextSearch {
    Phrases(PhraseMatcher),
    Regex(Regex),
}

impl Evaluator {
    /// Compiles the rule's regex, or one regex per phrase, here and once, so an evaluation
    /// compiles nothing. A failure names `owner`.
    pub fn compile(owner: RuleOwner, rule: Rule) -> Result<Self, CompileError> {
        let search = match &rule.matcher {
            Matcher::PhraseList { entries } => Some(TextSearch::Phrases(
                PhraseMatcher::compile(entries).map_err(|error| match error {
                    PhraseError::Regex(source) => CompileError::Phrase {
                        owner: owner.clone(),
                        source,
                    },
                    PhraseError::PrefixNotOneWord { entry_index } => {
                        CompileError::PrefixNotOneWord {
                            owner: owner.clone(),
                            entry_index,
                        }
                    }
                })?,
            )),
            Matcher::Regex { source } => {
                Some(TextSearch::Regex(Regex::new(source).map_err(|error| {
                    CompileError::Regex {
                        owner: owner.clone(),
                        source: error,
                    }
                })?))
            }
            Matcher::LanguageToolRule { .. }
            | Matcher::SpellingVariant { .. }
            | Matcher::WordCount => None,
        };
        Ok(Self {
            owner,
            rule,
            search,
        })
    }

    pub fn owner(&self) -> &RuleOwner {
        &self.owner
    }

    pub fn rule(&self) -> &Rule {
        &self.rule
    }

    /// The rule's matches in `text`, one unit's text, from a paragraph of form mode `mode` under
    /// the register `register`. Each is a [`Span`] anchored on `text` with its offset and length
    /// in UTF-16 code units, the anchor contract the flag types hold, and they come in the order
    /// they start in `text`. Empty where the rule does not declare `mode`, or where `register` is
    /// outside its scope.
    pub fn evaluate(&self, text: &str, mode: FormMode, register: &str) -> Vec<Span> {
        if !self.runs_in(mode, register) {
            return Vec::new();
        }
        let Some(search) = &self.search else {
            // A LanguageTool rule id is evaluated in the grammar stage (#98), and the
            // spelling-variant and word-count matchers arrive with #94.
            return Vec::new();
        };
        match self.rule.band {
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
            .is_some_and(|mode| self.rule.form_modes.contains(&mode));
        let in_scope = self
            .rule
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
            // The text as written, curly marks and all, so a regex can name a curly mark. Only
            // the phrase matcher reads a curly mark as its straight form.
            Self::Regex(regex) => regex.find_iter(text).map(|found| found.range()).collect(),
        };
        ranges
            .into_iter()
            .filter(|range| !range.is_empty())
            .collect()
    }
}

/// Compiles the rule of each of `checkers` in order, owned by the clause the checker serves. A
/// checker that does not compile is reported and the rest still compile, so one bad checker costs
/// only its own flags.
pub fn compile_all(
    checkers: impl IntoIterator<Item = Checker>,
) -> (Vec<Evaluator>, Vec<CompileError>) {
    let mut evaluators = Vec::new();
    let mut failures = Vec::new();
    for checker in checkers {
        match Evaluator::compile(RuleOwner::Clause(checker.clause_id), checker.rule) {
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

    /// A sentence-unit rule under the forbidden band, declared for prose and fragment in every
    /// register.
    fn rule(matcher: Matcher) -> Rule {
        Rule {
            matcher,
            unit: Unit::Sentence,
            band: Band::Forbidden,
            form_modes: BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]),
            register_scope: None,
        }
    }

    /// The checker for clause `clause` with `rule`.
    fn checker(clause: i64, rule: Rule) -> Checker {
        Checker {
            clause_id: ClauseId(clause),
            rule,
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

    fn compiled(rule: Rule) -> Evaluator {
        Evaluator::compile(RuleOwner::Clause(ClauseId(1)), rule).unwrap()
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
        let matches = in_prose(&compiled(rule(phrases(&["boasts a"]))), text);
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
        let emojis = compiled(rule(regex(
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
    fn a_curly_apostrophe_ahead_of_a_phrase_does_not_shift_its_span() {
        // The curly apostrophe is three bytes and its straight form one, so a byte range from the
        // searched copy starts two bytes early in the text as written: inside the em dash, which
        // is not a character boundary, and slicing there panics.
        let text = "The town\u{2019}s jewel\u{2014}boasts a harbour.";
        let evaluator = compiled(rule(phrases(&["boasts a"])));

        let ranges = evaluator.search.as_ref().unwrap().find(text);
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert_eq!(&text[ranges[0].clone()], "boasts a");

        let span = span_in(text, ranges[0].clone());
        assert_eq!(flagged_text(&span), "boasts a");
        assert_eq!((span.local_start, span.local_length), (17, 8));
        assert_eq!(in_prose(&evaluator, text), [span]);
    }

    #[test]
    fn a_regex_runs_against_the_text_as_written_and_can_name_a_curly_mark() {
        let curly_apostrophe = compiled(rule(regex(r"\x{2019}")));
        let matches = in_prose(&curly_apostrophe, "It\u{2019}s here, and it's there.");
        assert_eq!(
            matches.iter().map(flagged_text).collect::<Vec<_>>(),
            ["\u{2019}"]
        );
        assert_eq!(matches[0].local_start, 2);
        assert!(in_prose(&curly_apostrophe, "It's here.").is_empty());
    }

    #[test]
    fn a_regex_that_does_not_compile_fails_naming_its_clause() {
        let unclosed = rule(regex("(an unclosed group"));
        let error = Evaluator::compile(RuleOwner::Clause(ClauseId(4)), unclosed).unwrap_err();
        assert!(
            matches!(
                error,
                CompileError::Regex {
                    owner: RuleOwner::Clause(ClauseId(4)),
                    ..
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("clause 4"), "{error}");
    }

    /// The owner of each evaluator in `evaluators`, in order.
    fn owners(evaluators: &[Evaluator]) -> Vec<RuleOwner> {
        evaluators
            .iter()
            .map(|evaluator| evaluator.owner().clone())
            .collect()
    }

    #[test]
    fn compiling_a_set_reports_the_checker_that_fails_and_compiles_the_rest() {
        let first = checker(1, rule(phrases(&["boasts a"])));
        let broken = checker(2, rule(regex("(an unclosed group")));
        let last = checker(3, rule(regex("\u{2014}")));

        let (evaluators, failures) = compile_all([first, broken, last]);
        assert_eq!(
            owners(&evaluators),
            [
                RuleOwner::Clause(ClauseId(1)),
                RuleOwner::Clause(ClauseId(3))
            ]
        );
        let failed: Vec<&RuleOwner> = failures.iter().map(CompileError::owner).collect();
        assert_eq!(failed, [&RuleOwner::Clause(ClauseId(2))]);
    }

    #[test]
    fn a_prefix_flag_on_a_phrase_of_many_words_fails_naming_its_clause_and_the_rest_compile() {
        let first = checker(1, rule(phrases(&["boasts a"])));
        let many_words = checker(
            5,
            rule(Matcher::PhraseList {
                entries: vec![
                    PhraseEntry {
                        phrase: "intricate".to_owned(),
                        prefix: true,
                    },
                    PhraseEntry {
                        phrase: "rich tapestry".to_owned(),
                        prefix: true,
                    },
                ],
            }),
        );
        let last = checker(3, rule(regex("\u{2014}")));

        let (evaluators, failures) = compile_all([first, many_words, last]);
        assert_eq!(
            owners(&evaluators),
            [
                RuleOwner::Clause(ClauseId(1)),
                RuleOwner::Clause(ClauseId(3))
            ]
        );
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            matches!(
                failures[0],
                CompileError::PrefixNotOneWord {
                    owner: RuleOwner::Clause(ClauseId(5)),
                    entry_index: 1,
                }
            ),
            "{}",
            failures[0]
        );
        assert!(
            failures[0].to_string().contains("clause 5"),
            "{}",
            failures[0]
        );
    }

    #[test]
    fn a_checker_returns_no_match_in_a_mode_it_does_not_declare() {
        let text = "The town boasts a harbour.";
        let mut prose_only = rule(phrases(&["boasts a"]));
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
        let both = compiled(rule(phrases(&["boasts a"])));
        assert!(both
            .evaluate("The town boasts a harbour.", FormMode::Code, "default")
            .is_empty());
    }

    #[test]
    fn a_checker_returns_no_match_under_a_register_outside_its_scope() {
        let text = "The town boasts a harbour.";
        let mut email_only = rule(phrases(&["boasts a"]));
        email_only.register_scope = Some(BTreeSet::from(["email".to_owned()]));
        let email_only = compiled(email_only);
        assert_eq!(email_only.evaluate(text, FormMode::Prose, "email").len(), 1);
        assert!(email_only
            .evaluate(text, FormMode::Prose, "default")
            .is_empty());
    }

    #[test]
    fn an_unset_register_scope_runs_in_every_register() {
        let every = compiled(rule(phrases(&["boasts a"])));
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
                in_prose(&compiled(rule(matcher.clone())), text).is_empty(),
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
            let mut counted = rule(phrases(&["perhaps", "boasts a"]));
            counted.band = band.clone();
            assert!(in_prose(&compiled(counted), text).is_empty(), "{band:?}");
        }
    }

    #[test]
    fn an_empty_match_is_not_a_match() {
        let optional = compiled(rule(regex("x*")));
        assert!(in_prose(&optional, "abc").is_empty());
        let matches = in_prose(&optional, "a xx b");
        assert_eq!(matches.iter().map(flagged_text).collect::<Vec<_>>(), ["xx"]);

        let empty_phrase = compiled(rule(phrases(&[""])));
        assert!(in_prose(&empty_phrase, "abc").is_empty());
    }

    #[test]
    fn the_same_checker_over_the_same_text_gives_identical_matches_in_identical_order() {
        let text = "Its success is a testament to the team\u{2014}a testament to its care.";
        let listed = || rule(phrases(&["testament to", "\u{2014}", "is a testament to"]));
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

    #[test]
    fn the_same_checker_over_text_with_curly_marks_gives_identical_matches_in_identical_order() {
        let text = "\u{201C}You\u{2019}re absolutely right,\u{201D} he said. You\u{2019}re absolutely right.";
        let listed = || {
            rule(phrases(&[
                "absolutely right",
                "you're absolutely right",
                "\"",
            ]))
        };
        let evaluator = compiled(listed());
        let first = in_prose(&evaluator, text);
        assert_eq!(
            first.iter().map(flagged_text).collect::<Vec<_>>(),
            [
                "\u{201C}",
                "You\u{2019}re absolutely right",
                "absolutely right",
                "\u{201D}",
                "You\u{2019}re absolutely right",
                "absolutely right",
            ]
        );
        assert_eq!(in_prose(&evaluator, text), first);
        assert_eq!(in_prose(&compiled(listed()), text), first);
    }
}
