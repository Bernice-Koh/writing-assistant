//! Compiles a [`Rule`] once into an [`Evaluator`], and evaluates it over one unit. An evaluation
//! is a pure function of the unit's sentences, the form mode and the register: it calls no model
//! and no network, and the same inputs give the same violations in the same order.
//!
//! Every matcher is evaluated here but a LanguageTool rule id, whose matches LanguageTool reports
//! in the grammar stage (#98). The bands are applied in `band`, and the spelling-variant matcher
//! and its table are in `variants`.

use std::fmt;
use std::ops::Range;

use regex::Regex;
use thiserror::Error;

use super::band;
use super::checker::{Checker, FormMode, Matcher, Rule, Unit};
use super::phrase::{PhraseError, PhraseMatcher};
use super::variants::{UnknownConvention, VariantMatcher};
use crate::flag::{ClauseId, Span};
use crate::words::words;

/// What a rule belongs to, which names it wherever the rule itself is not quoted, as in a compile
/// error. #96 adds a telltale pattern's id.
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

    /// A spelling-variant matcher prefers a convention by one of the variant-pair table's names,
    /// and `name` is none of them.
    #[error(
        "{owner} prefers the spelling convention {name:?}, which the variant-pair table does not \
         hold"
    )]
    UnknownConvention { owner: RuleOwner, name: String },
}

impl CompileError {
    pub fn owner(&self) -> &RuleOwner {
        match self {
            Self::Regex { owner, .. }
            | Self::Phrase { owner, .. }
            | Self::PrefixNotOneWord { owner, .. }
            | Self::UnknownConvention { owner, .. } => owner,
        }
    }
}

/// One violation of a rule's band: the span a flag underlines, anchored on the sentence that holds
/// it, and the numbers the flag's message states.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub span: Span,
    pub breach: Breach,
}

/// How a violation breaks its rule's band. The counts are of the whole unit.
#[derive(Debug, Clone, PartialEq)]
pub enum Breach {
    /// A match under the forbidden band, which every match breaks.
    Forbidden,
    /// A match beyond a count band's maximum, in a unit that holds `count` matches.
    AboveMaximum { count: u32, maximum: u32 },
    /// A sentence of a unit that holds `count` matches where the band asks for `minimum`, which is
    /// `shortfall` more. No match lies beyond the limit, so every sentence of the unit carries one.
    BelowMinimum {
        count: u32,
        minimum: u32,
        shortfall: u32,
    },
    /// A match beyond the count a rate band allows for the unit's length, in a unit that holds
    /// `rate_per_hundred_words` matches for every hundred words.
    AboveRate {
        rate_per_hundred_words: f64,
        maximum_per_hundred_words: f64,
    },
}

/// A match in a unit: the index of the sentence of the unit that holds it, and its byte range in
/// that sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) sentence: usize,
    pub(super) range: Range<usize>,
}

/// A rule compiled for evaluation.
#[derive(Debug)]
pub struct Evaluator {
    owner: RuleOwner,
    rule: Rule,
    matcher: CompiledMatcher,
}

#[derive(Debug)]
enum CompiledMatcher {
    Phrases(PhraseMatcher),
    Regex(Regex),
    LanguageToolRule,
    SpellingVariant(VariantMatcher),
    WordCount,
}

impl Evaluator {
    /// Compiles the rule's regex, or one regex per phrase, and resolves its preferred spelling
    /// convention, here and once, so an evaluation compiles nothing. A failure names `owner`.
    pub fn compile(owner: RuleOwner, rule: Rule) -> Result<Self, CompileError> {
        let matcher = match &rule.matcher {
            Matcher::PhraseList { entries } => {
                CompiledMatcher::Phrases(PhraseMatcher::compile(entries).map_err(|error| {
                    match error {
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
                    }
                })?)
            }
            Matcher::Regex { source } => {
                CompiledMatcher::Regex(Regex::new(source).map_err(|error| CompileError::Regex {
                    owner: owner.clone(),
                    source: error,
                })?)
            }
            Matcher::LanguageToolRule { .. } => CompiledMatcher::LanguageToolRule,
            Matcher::SpellingVariant {
                preferred_convention,
            } => CompiledMatcher::SpellingVariant(
                VariantMatcher::compile(preferred_convention.as_deref()).map_err(
                    |UnknownConvention(name)| CompileError::UnknownConvention {
                        owner: owner.clone(),
                        name,
                    },
                )?,
            ),
            Matcher::WordCount => CompiledMatcher::WordCount,
        };
        Ok(Self {
            owner,
            rule,
            matcher,
        })
    }

    pub fn owner(&self) -> &RuleOwner {
        &self.owner
    }

    pub fn rule(&self) -> &Rule {
        &self.rule
    }

    /// The rule's violations in one unit, handed over as its sentences in order: one sentence for
    /// a span or a sentence unit, the paragraph's for a paragraph unit, and the document's for a
    /// document unit. The caller splits the text into paragraphs on its paragraph marks and each
    /// paragraph into sentences, so no sentence holds a paragraph mark and no violation's span
    /// crosses one. Handed several sentences, a span or a sentence unit's rule reads each as a unit
    /// of its own.
    ///
    /// The band's counts run over the whole unit, and each violation is a [`Span`] anchored on the
    /// sentence that holds its match, with its offset and length in UTF-16 code units, the anchor
    /// contract the flag types hold. They come in document order: by sentence, then by start.
    /// Empty where the rule does not declare `mode`, the form mode of the unit's text, or where
    /// `register` is outside its scope.
    pub fn evaluate<S: AsRef<str>>(
        &self,
        sentences: &[S],
        mode: FormMode,
        register: &str,
    ) -> Vec<Violation> {
        if !self.runs_in(mode, register) {
            return Vec::new();
        }
        let sentences: Vec<&str> = sentences.iter().map(AsRef::as_ref).collect();
        match self.rule.unit {
            Unit::Span | Unit::Sentence => sentences
                .iter()
                .flat_map(|sentence| self.violations_in(std::slice::from_ref(sentence)))
                .collect(),
            Unit::Paragraph | Unit::Document => self.violations_in(&sentences),
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

    fn violations_in(&self, unit: &[&str]) -> Vec<Violation> {
        let Some(found) = self.matcher.find(unit) else {
            return Vec::new();
        };
        let mut breaches = band::breaches(&self.rule.band, &found, unit);
        // A count band whose minimum is above its maximum can break both, and gives the
        // maximum's flags ahead of the minimum's underlines.
        breaches.sort_by_key(|(found, _)| (found.sentence, found.range.start, found.range.end));
        if matches!(self.matcher, CompiledMatcher::WordCount) {
            breaches = joined_per_sentence(breaches);
        }
        breaches
            .into_iter()
            .map(|(found, breach)| Violation {
                span: span_in(unit[found.sentence], found.range),
                breach,
            })
            .collect()
    }
}

impl CompiledMatcher {
    /// Each match in `unit`, in document order: by sentence, then by start, then by end. An empty
    /// match is dropped: it has nothing to underline or cut, and a pattern that can match empty,
    /// such as an empty phrase, matches at every position.
    ///
    /// `None` for a LanguageTool rule id, whose matches LanguageTool reports in the grammar stage
    /// (#98). This engine has no matches of one to count, which is not a count of none, so no band
    /// applies and no minimum underlines the unit.
    fn find(&self, unit: &[&str]) -> Option<Vec<Found>> {
        let found = match self {
            Self::Phrases(phrases) => in_each_sentence(unit, |sentence| phrases.find(sentence)),
            // The text as written, curly marks and all, so a regex can name a curly mark. Only
            // the phrase matcher reads a curly mark as its straight form.
            Self::Regex(regex) => in_each_sentence(unit, |sentence| {
                regex
                    .find_iter(sentence)
                    .map(|found| found.range())
                    .collect()
            }),
            Self::LanguageToolRule => return None,
            Self::SpellingVariant(variants) => variants.find(unit),
            Self::WordCount => in_each_sentence(unit, |sentence| {
                words(sentence)
                    .map(|(start, word)| start..start + word.len())
                    .collect()
            }),
        };
        Some(found)
    }
}

/// The non-empty ranges `find` gives in each sentence of `unit`, as matches in the unit.
fn in_each_sentence(unit: &[&str], find: impl Fn(&str) -> Vec<Range<usize>>) -> Vec<Found> {
    unit.iter()
        .enumerate()
        .flat_map(|(sentence, text)| {
            find(text)
                .into_iter()
                .filter(|range| !range.is_empty())
                .map(move |range| Found { sentence, range })
        })
        .collect()
}

/// The word-count matcher's breaches in one sentence joined into one, from the first word's start
/// to the last word's end, so the words beyond a limit are one underline and not one a word.
/// `breaches` are in document order.
fn joined_per_sentence(breaches: Vec<(Found, Breach)>) -> Vec<(Found, Breach)> {
    let mut joined: Vec<(Found, Breach)> = Vec::new();
    for (found, breach) in breaches {
        match joined.last_mut() {
            Some((last, last_breach))
                if last.sentence == found.sentence && *last_breach == breach =>
            {
                last.range.end = last.range.end.max(found.range.end);
            }
            _ => joined.push((found, breach)),
        }
    }
    joined
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
pub(super) mod tests {
    use std::collections::BTreeSet;

    use unicode_segmentation::UnicodeSegmentation;

    use super::*;
    use crate::style::checker::{Band, CheckableMode, PhraseEntry};

    /// A sentence-unit rule under the forbidden band, declared for prose and fragment in every
    /// register.
    pub(in crate::style) fn rule(matcher: Matcher) -> Rule {
        Rule {
            matcher,
            unit: Unit::Sentence,
            band: Band::Forbidden,
            form_modes: BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]),
            register_scope: None,
        }
    }

    /// [`rule`] over `unit` under `band`.
    pub(in crate::style) fn banded(matcher: Matcher, unit: Unit, band: Band) -> Rule {
        Rule {
            unit,
            band,
            ..rule(matcher)
        }
    }

    pub(in crate::style) fn count(minimum: Option<u32>, maximum: Option<u32>) -> Band {
        Band::Count { minimum, maximum }
    }

    /// The checker for clause `clause` with `rule`.
    fn checker(clause: i64, rule: Rule) -> Checker {
        Checker {
            clause_id: ClauseId(clause),
            rule,
        }
    }

    pub(in crate::style) fn phrases(phrases: &[&str]) -> Matcher {
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

    pub(in crate::style) fn compiled(rule: Rule) -> Evaluator {
        Evaluator::compile(RuleOwner::Clause(ClauseId(1)), rule).unwrap()
    }

    /// The violations of `evaluator` over the unit of `sentences`, in prose under the default
    /// register.
    pub(in crate::style) fn violations(
        evaluator: &Evaluator,
        sentences: &[&str],
    ) -> Vec<Violation> {
        evaluator.evaluate(sentences, FormMode::Prose, "default")
    }

    /// The span of each of `evaluator`'s violations over the one sentence `text`.
    fn in_prose(evaluator: &Evaluator, text: &str) -> Vec<Span> {
        violations(evaluator, &[text])
            .into_iter()
            .map(|violation| violation.span)
            .collect()
    }

    /// The exact text a span addresses, sliced back out of its own anchor.
    pub(in crate::style) fn flagged_text(span: &Span) -> String {
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
    fn a_violation_of_the_forbidden_band_carries_nothing_beyond_its_span() {
        let flagged = violations(
            &compiled(rule(phrases(&["boasts a"]))),
            &["The town boasts a harbour."],
        );
        assert_eq!(
            flagged
                .iter()
                .map(|violation| &violation.breach)
                .collect::<Vec<_>>(),
            [&Breach::Forbidden]
        );
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

        let found = evaluator.matcher.find(&[text]).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(&text[found[0].range.clone()], "boasts a");

        let span = span_in(text, found[0].range.clone());
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
    pub(in crate::style) fn owners(evaluators: &[Evaluator]) -> Vec<RuleOwner> {
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
        let text = ["The town boasts a harbour."];
        let mut prose_only = rule(phrases(&["boasts a"]));
        prose_only.form_modes = BTreeSet::from([CheckableMode::Prose]);
        let prose_only = compiled(prose_only);
        assert_eq!(
            prose_only.evaluate(&text, FormMode::Prose, "default").len(),
            1
        );
        assert!(prose_only
            .evaluate(&text, FormMode::Fragment, "default")
            .is_empty());
    }

    #[test]
    fn no_checker_returns_a_match_in_code() {
        let both = compiled(rule(phrases(&["boasts a"])));
        assert!(both
            .evaluate(&["The town boasts a harbour."], FormMode::Code, "default")
            .is_empty());
    }

    #[test]
    fn a_checker_returns_no_match_under_a_register_outside_its_scope() {
        let text = ["The town boasts a harbour."];
        let mut email_only = rule(phrases(&["boasts a"]));
        email_only.register_scope = Some(BTreeSet::from(["email".to_owned()]));
        let email_only = compiled(email_only);
        assert_eq!(
            email_only.evaluate(&text, FormMode::Prose, "email").len(),
            1
        );
        assert!(email_only
            .evaluate(&text, FormMode::Prose, "default")
            .is_empty());
    }

    #[test]
    fn an_unset_register_scope_runs_in_every_register() {
        let every = compiled(rule(phrases(&["boasts a"])));
        for register in ["default", "email", "chat"] {
            let matches =
                every.evaluate(&["The town boasts a harbour."], FormMode::Prose, register);
            assert_eq!(matches.len(), 1, "{register}");
        }
    }

    #[test]
    fn a_languagetool_rule_id_yields_nothing_in_this_engine() {
        // LanguageTool reports a rule id's matches in the grammar stage (#98). A minimum over the
        // matches this engine does not have would underline every sentence.
        let passive = Matcher::LanguageToolRule {
            rule_id: "PASSIVE_VOICE".to_owned(),
        };
        let sentences = [
            "The harbour was built by the town.",
            "It was opened in the spring.",
        ];
        let bands = [
            Band::Forbidden,
            count(Some(1), None),
            count(None, Some(0)),
            Band::Rate {
                maximum_per_hundred_words: 0.0,
            },
        ];
        for unit in [Unit::Span, Unit::Sentence, Unit::Paragraph, Unit::Document] {
            for band in &bands {
                let evaluator = compiled(banded(passive.clone(), unit, band.clone()));
                assert!(
                    violations(&evaluator, &sentences).is_empty(),
                    "{unit:?} {band:?}"
                );
            }
        }
    }

    #[test]
    fn a_sentence_unit_handed_several_sentences_reads_each_as_a_unit() {
        let at_most_one = compiled(banded(
            phrases(&["perhaps"]),
            Unit::Sentence,
            count(None, Some(1)),
        ));
        let sentences = ["Perhaps it rains.", "Perhaps it snows, perhaps not."];
        let flagged = violations(&at_most_one, &sentences);
        assert_eq!(flagged.len(), 1, "{flagged:#?}");
        assert_eq!(flagged[0].span.anchor, sentences[1]);
        assert_eq!(
            flagged[0].span.local_start,
            sentences[1].rfind("perhaps").unwrap()
        );
        assert_eq!(
            flagged[0].breach,
            Breach::AboveMaximum {
                count: 2,
                maximum: 1
            }
        );
    }

    #[test]
    fn a_document_unit_counts_over_the_whole_document_and_anchors_each_flag_on_its_sentence() {
        let first_paragraph = ["Perhaps it rains.", "The ferry waits at the pier."];
        let second_paragraph = ["Perhaps the tide turns.", "It may clear by noon, perhaps."];
        let document: Vec<&str> = first_paragraph
            .iter()
            .chain(&second_paragraph)
            .copied()
            .collect();
        let at_most_two =
            |unit| compiled(banded(phrases(&["perhaps"]), unit, count(None, Some(2))));

        let flagged = violations(&at_most_two(Unit::Document), &document);
        assert_eq!(flagged.len(), 1, "{flagged:#?}");
        assert_eq!(flagged[0].span.anchor, second_paragraph[1]);
        assert_eq!(flagged_text(&flagged[0].span), "perhaps");
        assert_eq!(
            flagged[0].span.local_start,
            second_paragraph[1].find("perhaps").unwrap()
        );
        assert_eq!(
            flagged[0].breach,
            Breach::AboveMaximum {
                count: 3,
                maximum: 2
            }
        );

        // Neither paragraph alone holds more than two.
        let per_paragraph = at_most_two(Unit::Paragraph);
        assert!(violations(&per_paragraph, &first_paragraph).is_empty());
        assert!(violations(&per_paragraph, &second_paragraph).is_empty());
    }

    /// `text` split as the caller splits a document: into paragraphs on its paragraph marks, the
    /// carriage returns Word writes and the newlines a browser writes, and each paragraph into its
    /// sentences, trimmed.
    fn sentences_of(text: &str) -> Vec<&str> {
        text.split(['\r', '\n'])
            .flat_map(UnicodeSegmentation::unicode_sentences)
            .map(str::trim)
            .filter(|sentence| !sentence.is_empty())
            .collect()
    }

    #[test]
    fn no_span_crosses_a_paragraph_mark() {
        let text = "Perhaps it rains\rand the ferry waits.\r\nPerhaps the tide turns. It may clear\nby noon, perhaps.";
        let document = sentences_of(text);
        assert_eq!(
            document,
            [
                "Perhaps it rains",
                "and the ferry waits.",
                "Perhaps the tide turns.",
                "It may clear",
                "by noon, perhaps.",
            ]
        );
        let rules = [
            // A match of all of each sentence.
            banded(regex(r"(?s).+"), Unit::Document, Band::Forbidden),
            // Every sentence of the document underlined.
            banded(phrases(&["perhaps"]), Unit::Document, count(Some(9), None)),
            // The words beyond the second, which run over four sentences.
            banded(Matcher::WordCount, Unit::Document, count(None, Some(2))),
        ];
        for rule in rules {
            let flagged = violations(&compiled(rule.clone()), &document);
            assert!(!flagged.is_empty(), "{rule:?}");
            for violation in flagged {
                let span = &violation.span;
                assert!(document.contains(&span.anchor.as_str()), "{span:?}");
                assert!(
                    span.local_start + span.local_length <= span.anchor.encode_utf16().count(),
                    "{span:?}"
                );
                assert!(!flagged_text(span).contains(['\r', '\n']), "{span:?}");
            }
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

    #[test]
    fn the_same_rule_over_the_same_document_gives_identical_violations_in_identical_order() {
        // Fifty words, so the rate band measures it.
        let paragraph = [
            "Perhaps the colour of the harbour changed, perhaps not.",
            "The color faded by noon.",
            "Perhaps the ferry waits at the pier until the tide turns.",
        ];
        let document: Vec<&str> = paragraph.iter().chain(&paragraph).copied().collect();
        let rules = || {
            [
                banded(phrases(&["perhaps"]), Unit::Document, count(None, Some(1))),
                banded(phrases(&["perhaps"]), Unit::Paragraph, count(Some(9), None)),
                banded(
                    phrases(&["perhaps"]),
                    Unit::Document,
                    Band::Rate {
                        maximum_per_hundred_words: 4.0,
                    },
                ),
                banded(Matcher::WordCount, Unit::Document, count(None, Some(12))),
                banded(
                    Matcher::SpellingVariant {
                        preferred_convention: None,
                    },
                    Unit::Document,
                    Band::Forbidden,
                ),
            ]
        };
        for (rule, again) in rules().into_iter().zip(rules()) {
            let evaluator = compiled(rule);
            let first = violations(&evaluator, &document);
            assert!(!first.is_empty(), "{:?}", evaluator.rule());
            assert_eq!(violations(&evaluator, &document), first);
            assert_eq!(violations(&compiled(again), &document), first);
        }
    }
}
