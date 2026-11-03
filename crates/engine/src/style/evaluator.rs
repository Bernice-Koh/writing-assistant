//! Compiles a [`Rule`] once into an [`Evaluator`], and evaluates it over one unit. An evaluation
//! is a pure function of the unit's sentences, each with its paragraph's form mode, and the
//! register: it calls no model and no network, and the same inputs give the same violations in the
//! same order.
//!
//! Every matcher is evaluated here but a LanguageTool rule id, whose matches LanguageTool reports
//! in the grammar stage (#98). The bands are applied in `band`, and the spelling-variant matcher
//! and its table are in `variants`.

use std::fmt;
use std::ops::Range;

use regex::Regex;
use thiserror::Error;

use super::band;
use super::checker::{Band, Checker, FormMode, Matcher, Rule, Unit};
use super::phrase::{CurlyMarks, PhraseError, PhraseMatcher};
use super::variants::{UnknownConvention, VariantMatcher};
use crate::flag::{ClauseId, Span};
use crate::words::words;

/// What a rule belongs to, which names it wherever the rule itself is not quoted, as in a compile
/// error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleOwner {
    /// The checker that serves this Style Card clause.
    Clause(ClauseId),
    /// The telltale catalogue's pattern of this id.
    Pattern(String),
}

impl fmt::Display for RuleOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clause(clause_id) => write!(formatter, "the checker for clause {}", clause_id.0),
            Self::Pattern(pattern_id) => write!(formatter, "the telltale pattern {pattern_id}"),
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

    /// The band holds no count a unit could be measured against, so evaluating it would flag
    /// nonsense.
    #[error("the band of {owner} does not compile: {fault}")]
    MalformedBand { owner: RuleOwner, fault: BandFault },
}

impl CompileError {
    pub fn owner(&self) -> &RuleOwner {
        match self {
            Self::Regex { owner, .. }
            | Self::Phrase { owner, .. }
            | Self::PrefixNotOneWord { owner, .. }
            | Self::UnknownConvention { owner, .. }
            | Self::MalformedBand { owner, .. } => owner,
        }
    }
}

/// How a band is malformed.
#[derive(Debug, Clone, PartialEq)]
pub enum BandFault {
    /// A count band whose minimum exceeds its maximum, which every count breaks.
    MinimumAboveMaximum { minimum: u32, maximum: u32 },
    /// A rate band whose rate is negative, infinite or not a number.
    RateNegativeOrNotFinite { maximum_per_hundred_words: f64 },
}

impl fmt::Display for BandFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MinimumAboveMaximum { minimum, maximum } => write!(
                formatter,
                "its minimum, {minimum}, is above its maximum, {maximum}"
            ),
            Self::RateNegativeOrNotFinite {
                maximum_per_hundred_words,
            } => write!(
                formatter,
                "its rate, {maximum_per_hundred_words} per hundred words, is negative or not \
                 finite"
            ),
        }
    }
}

/// How `band` is malformed, or `None` when it is not.
fn band_fault(band: &Band) -> Option<BandFault> {
    match *band {
        Band::Count {
            minimum: Some(minimum),
            maximum: Some(maximum),
        } if minimum > maximum => Some(BandFault::MinimumAboveMaximum { minimum, maximum }),
        Band::Rate {
            maximum_per_hundred_words,
        } if !maximum_per_hundred_words.is_finite() || maximum_per_hundred_words < 0.0 => {
            Some(BandFault::RateNegativeOrNotFinite {
                maximum_per_hundred_words,
            })
        }
        _ => None,
    }
}

/// One sentence of a unit handed to [`Evaluator::evaluate`], with the form mode of its paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeSentence<'a> {
    pub text: &'a str,
    pub mode: FormMode,
}

/// One violation of a rule's band: the sentence that holds it, the span a flag underlines,
/// anchored on that sentence, and the numbers the flag's message states.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// The index of the sentence in the slice [`Evaluator::evaluate`] was handed, counted over
    /// every sentence of the slice, whether the rule declares its mode or not.
    pub sentence: usize,
    pub span: Span,
    pub breach: Breach,
}

/// How a violation breaks its rule's band. The counts are of the unit's sentences whose form mode
/// the rule declares.
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
    /// Checks the rule's band, compiles its regex, or one regex per phrase, and resolves its
    /// preferred spelling convention, here and once, so an evaluation compiles nothing. A phrase
    /// list reads each curly apostrophe and quotation mark as its straight form. A failure names
    /// `owner`.
    pub fn compile(owner: RuleOwner, rule: Rule) -> Result<Self, CompileError> {
        Self::compile_reading(owner, rule, CurlyMarks::AsStraight)
    }

    /// [`compile`](Self::compile), with a phrase list reading curly marks as `curly_marks` says.
    pub(super) fn compile_reading(
        owner: RuleOwner,
        rule: Rule,
        curly_marks: CurlyMarks,
    ) -> Result<Self, CompileError> {
        if let Some(fault) = band_fault(&rule.band) {
            return Err(CompileError::MalformedBand { owner, fault });
        }
        let matcher = match &rule.matcher {
            Matcher::PhraseList { entries } => CompiledMatcher::Phrases(
                PhraseMatcher::compile(entries, curly_marks).map_err(|error| match error {
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
            ),
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

    /// The rule's violations in one unit under `register`, handed over as its sentences in order,
    /// each with the form mode of its paragraph: one sentence for a span or a sentence unit, the
    /// paragraph's for a paragraph unit, and the document's for a document unit. The caller splits
    /// the text into paragraphs on its paragraph marks and each paragraph into sentences, so no
    /// sentence holds a paragraph mark and no violation's span crosses one.
    ///
    /// The rule runs over the sentences whose mode it declares and ignores the rest, which take no
    /// part in a count and raise nothing; it declares no mode of code. Handed several sentences, a
    /// span or a sentence unit's rule reads each declared one as a unit of its own, and a paragraph
    /// or a document unit's rule reads the declared ones together as its unit, so the band's counts
    /// and a rate band's words run over them alone. Empty where `register` is outside the rule's
    /// scope.
    ///
    /// Each violation names its sentence by its index in `sentences` and is a [`Span`] anchored on
    /// that sentence, with its offset and length in UTF-16 code units, the anchor contract the flag
    /// types hold. They come in document order: by sentence, then by start.
    pub fn evaluate(&self, sentences: &[ModeSentence<'_>], register: &str) -> Vec<Violation> {
        let (indices, texts): (Vec<usize>, Vec<&str>) = sentences
            .iter()
            .enumerate()
            .filter(|(_, sentence)| self.runs_in(sentence.mode, register))
            .map(|(index, sentence)| (index, sentence.text))
            .unzip();
        match self.rule.unit {
            Unit::Span | Unit::Sentence => indices
                .iter()
                .zip(&texts)
                .flat_map(|(index, text)| {
                    self.violations_in(std::slice::from_ref(text), std::slice::from_ref(index))
                })
                .collect(),
            Unit::Paragraph | Unit::Document => self.violations_in(&texts, &indices),
        }
    }

    /// Whether the rule runs on a sentence of a paragraph of form mode `mode` under `register`: it
    /// declares the mode, which code never is, and its register scope is unset or holds the
    /// register. Evaluation and LanguageTool's routing both read this one test.
    pub fn runs_in(&self, mode: FormMode, register: &str) -> bool {
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

    /// The violations in the unit of the sentences `unit`, each naming its sentence by the entry
    /// of `indices` at that sentence's place in `unit`.
    fn violations_in(&self, unit: &[&str], indices: &[usize]) -> Vec<Violation> {
        let Some(found) = self.matcher.find(unit) else {
            return Vec::new();
        };
        let mut breaches = band::breaches(&self.rule.band, &found, unit);
        if matches!(self.matcher, CompiledMatcher::WordCount) {
            breaches = joined_per_sentence(breaches);
        }
        breaches
            .into_iter()
            .map(|(found, breach)| Violation {
                sentence: indices[found.sentence],
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
        occurrence: 0,
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

    /// `texts` as the sentences of paragraphs of `mode`.
    pub(in crate::style) fn in_mode<'a>(
        mode: FormMode,
        texts: &[&'a str],
    ) -> Vec<ModeSentence<'a>> {
        texts
            .iter()
            .map(|&text| ModeSentence { text, mode })
            .collect()
    }

    /// The violations of `evaluator` over the unit of `sentences`, in prose under the default
    /// register.
    pub(in crate::style) fn violations(
        evaluator: &Evaluator,
        sentences: &[&str],
    ) -> Vec<Violation> {
        evaluator.evaluate(&in_mode(FormMode::Prose, sentences), "default")
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

    #[test]
    fn a_rule_of_a_telltale_pattern_that_does_not_compile_fails_naming_the_pattern() {
        let owner = RuleOwner::Pattern("negative-parallelism".to_owned());
        let error =
            Evaluator::compile(owner.clone(), rule(regex("(an unclosed group"))).unwrap_err();
        assert_eq!(error.owner(), &owner);
        assert!(
            error
                .to_string()
                .starts_with("the regex of the telltale pattern negative-parallelism does not"),
            "{error}"
        );
    }

    #[test]
    fn a_phrase_list_compiled_to_read_curly_marks_as_written_matches_them_alone() {
        let text = ["\u{201C}Yes,\u{201D} she said, and \"no\" twice."];
        let curly = || rule(phrases(&["\u{201C}", "\u{201D}"]));
        let owner = RuleOwner::Pattern("curly-quotes".to_owned());
        let as_written =
            Evaluator::compile_reading(owner.clone(), curly(), CurlyMarks::AsWritten).unwrap();
        let flagged: Vec<String> = violations(&as_written, &text)
            .iter()
            .map(|violation| flagged_text(&violation.span))
            .collect();
        assert_eq!(flagged, ["\u{201C}", "\u{201D}"]);
        // Read as straight ones, the text holds no curly mark for the entries to match.
        let as_straight = Evaluator::compile(owner, curly()).unwrap();
        assert!(violations(&as_straight, &text).is_empty());
    }

    #[test]
    fn a_count_band_with_its_minimum_above_its_maximum_fails_naming_its_clause() {
        let inverted = banded(
            phrases(&["perhaps"]),
            Unit::Paragraph,
            count(Some(5), Some(1)),
        );
        let error = Evaluator::compile(RuleOwner::Clause(ClauseId(4)), inverted).unwrap_err();
        assert!(
            matches!(
                error,
                CompileError::MalformedBand {
                    owner: RuleOwner::Clause(ClauseId(4)),
                    fault: BandFault::MinimumAboveMaximum {
                        minimum: 5,
                        maximum: 1
                    },
                }
            ),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "the band of the checker for clause 4 does not compile: its minimum, 5, is above its \
             maximum, 1"
        );
        // A minimum equal to the maximum admits one count, and compiles.
        compiled(banded(
            phrases(&["perhaps"]),
            Unit::Paragraph,
            count(Some(1), Some(1)),
        ));
    }

    #[test]
    fn a_rate_band_that_is_negative_or_not_finite_fails_naming_its_clause() {
        for rate in [-0.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let band = Band::Rate {
                maximum_per_hundred_words: rate,
            };
            let rule = banded(phrases(&["perhaps"]), Unit::Document, band);
            let error = Evaluator::compile(RuleOwner::Clause(ClauseId(4)), rule).unwrap_err();
            assert!(
                matches!(
                    error,
                    CompileError::MalformedBand {
                        owner: RuleOwner::Clause(ClauseId(4)),
                        fault: BandFault::RateNegativeOrNotFinite { .. },
                    }
                ),
                "{rate}: {error}"
            );
            assert!(error.to_string().contains("clause 4"), "{error}");
        }
        // A rate of none per hundred words allows no match, and compiles.
        compiled(banded(
            phrases(&["perhaps"]),
            Unit::Document,
            Band::Rate {
                maximum_per_hundred_words: 0.0,
            },
        ));
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
            prose_only
                .evaluate(&in_mode(FormMode::Prose, &text), "default")
                .len(),
            1
        );
        assert!(prose_only
            .evaluate(&in_mode(FormMode::Fragment, &text), "default")
            .is_empty());
    }

    #[test]
    fn no_checker_returns_a_match_in_code() {
        let both = compiled(rule(phrases(&["boasts a"])));
        let text = ["The town boasts a harbour."];
        assert!(both
            .evaluate(&in_mode(FormMode::Code, &text), "default")
            .is_empty());
    }

    #[test]
    fn a_checker_returns_no_match_under_a_register_outside_its_scope() {
        let text = in_mode(FormMode::Prose, &["The town boasts a harbour."]);
        let mut email_only = rule(phrases(&["boasts a"]));
        email_only.register_scope = Some(BTreeSet::from(["email".to_owned()]));
        let email_only = compiled(email_only);
        assert_eq!(email_only.evaluate(&text, "email").len(), 1);
        assert!(email_only.evaluate(&text, "default").is_empty());
    }

    #[test]
    fn an_unset_register_scope_runs_in_every_register() {
        let every = compiled(rule(phrases(&["boasts a"])));
        let text = in_mode(FormMode::Prose, &["The town boasts a harbour."]);
        for register in ["default", "email", "chat"] {
            assert_eq!(every.evaluate(&text, register).len(), 1, "{register}");
        }
    }

    /// The sentences of a document of four paragraphs, each holding the word "perhaps": a heading,
    /// which is a fragment, a paragraph of prose, a line of code, and a second paragraph of prose.
    fn mixed_document() -> Vec<ModeSentence<'static>> {
        vec![
            ModeSentence {
                text: "Perhaps a heading",
                mode: FormMode::Fragment,
            },
            ModeSentence {
                text: "Perhaps it rains.",
                mode: FormMode::Prose,
            },
            ModeSentence {
                text: "let perhaps = rain();",
                mode: FormMode::Code,
            },
            ModeSentence {
                text: "It may clear by noon, perhaps.",
                mode: FormMode::Prose,
            },
        ]
    }

    /// `rule` declared for prose alone.
    fn in_prose_only(mut rule: Rule) -> Rule {
        rule.form_modes = BTreeSet::from([CheckableMode::Prose]);
        rule
    }

    #[test]
    fn a_paragraph_or_document_unit_counts_the_sentences_of_the_modes_it_declares_alone() {
        let document = mixed_document();
        for unit in [Unit::Paragraph, Unit::Document] {
            let at_most_one = compiled(in_prose_only(banded(
                phrases(&["perhaps"]),
                unit,
                count(None, Some(1)),
            )));
            let flagged = at_most_one.evaluate(&document, "default");
            // Two of the four matches are in prose, and the second is beyond the maximum.
            assert_eq!(flagged.len(), 1, "{unit:?}: {flagged:#?}");
            assert_eq!(flagged[0].sentence, 3, "{unit:?}");
            assert_eq!(flagged[0].span.anchor, document[3].text, "{unit:?}");
            assert_eq!(
                flagged[0].breach,
                Breach::AboveMaximum {
                    count: 2,
                    maximum: 1
                },
                "{unit:?}"
            );

            // Declared for fragments too, the heading's match counts and the code's still does
            // not.
            let both = compiled(banded(phrases(&["perhaps"]), unit, count(None, Some(1))));
            let flagged = both.evaluate(&document, "default");
            let sentences: Vec<usize> =
                flagged.iter().map(|violation| violation.sentence).collect();
            assert_eq!(sentences, [1, 3], "{unit:?}: {flagged:#?}");
            assert!(flagged.iter().all(|violation| violation.breach
                == Breach::AboveMaximum {
                    count: 3,
                    maximum: 1
                }));
        }
    }

    #[test]
    fn a_minimum_underlines_the_declared_sentences_alone() {
        let document = mixed_document();
        let at_least_five = compiled(in_prose_only(banded(
            phrases(&["perhaps"]),
            Unit::Document,
            count(Some(5), None),
        )));
        let flagged = at_least_five.evaluate(&document, "default");
        let sentences: Vec<usize> = flagged.iter().map(|violation| violation.sentence).collect();
        assert_eq!(sentences, [1, 3], "{flagged:#?}");
        assert!(flagged.iter().all(|violation| violation.breach
            == Breach::BelowMinimum {
                count: 2,
                minimum: 5,
                shortfall: 3
            }));
    }

    #[test]
    fn a_rate_band_counts_the_words_of_the_declared_sentences_alone() {
        // Ten words of prose and forty-one of fragments: fifty-one in all, and ten declared.
        let prose = "Perhaps the ferry waits at the old pier until noon.";
        let fragment = "One two three four five six seven eight nine ten eleven twelve \
                        thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty \
                        twentyone twentytwo twentythree twentyfour twentyfive twentysix \
                        twentyseven twentyeight twentynine thirty thirtyone thirtytwo \
                        thirtythree thirtyfour thirtyfive thirtysix thirtyseven thirtyeight \
                        thirtynine forty fortyone";
        let document = [
            ModeSentence {
                text: fragment,
                mode: FormMode::Fragment,
            },
            ModeSentence {
                text: prose,
                mode: FormMode::Prose,
            },
        ];
        assert_eq!(
            (
                crate::words::word_count(prose),
                crate::words::word_count(fragment)
            ),
            (10, 41)
        );
        let rate = |rule: Rule| compiled(rule).evaluate(&document, "default");
        let at_most_one_per_hundred = || {
            banded(
                phrases(&["perhaps"]),
                Unit::Document,
                Band::Rate {
                    maximum_per_hundred_words: 1.0,
                },
            )
        };
        // Over the ten words of prose alone, the unit is under fifty words and is not measured.
        assert!(rate(in_prose_only(at_most_one_per_hundred())).is_empty());
        // Over all fifty-one, one match is about two per hundred words, over the limit of one.
        let flagged = rate(at_most_one_per_hundred());
        assert_eq!(flagged.len(), 1, "{flagged:#?}");
        assert_eq!(flagged[0].sentence, 1);
    }

    #[test]
    fn a_span_unit_skips_an_undeclared_sentence_and_counts_it_in_each_violations_index() {
        let document = mixed_document();
        let forbidden = compiled(in_prose_only(banded(
            phrases(&["perhaps"]),
            Unit::Span,
            Band::Forbidden,
        )));
        let flagged = forbidden.evaluate(&document, "default");
        let found: Vec<(usize, &str)> = flagged
            .iter()
            .map(|violation| (violation.sentence, violation.span.anchor.as_str()))
            .collect();
        assert_eq!(
            found,
            [(1, document[1].text), (3, document[3].text)],
            "{flagged:#?}"
        );
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
        assert_eq!(flagged[0].sentence, 1);
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
        assert_eq!(flagged[0].sentence, 3);
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
