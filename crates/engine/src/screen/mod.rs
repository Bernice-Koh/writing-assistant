//! The screen: the deterministic layer a rewrite candidate is checked by before it is shown. It runs
//! the checks typed text gets, through the engine functions the analyzer calls, beside the content
//! check against the passage the candidate rewrites. The rewriter calls it in process from Phase 4
//! (#60), and the bench through `wa screen`, so what the bench measures is what ships.
//!
//! A run checks content preservation first, over the two whole texts. It then segments the
//! candidate as the analyzer segments live text, into paragraphs with their form modes and each
//! paragraph into sentences, and runs the analyzer's three passes with the candidate as the
//! document: spelling, the catalogue's patterns and the ruleset's checkers of a span or a sentence
//! unit over each sentence under its paragraph's mode, those of a paragraph unit over each
//! paragraph, and those of a document unit over the whole candidate, where a rule counts over the
//! paragraphs of the modes it declares alone. Spelling accepts a word the dictionary lacks when the
//! passage holds it, as written or with only its first letter's case changed, whichever apostrophe
//! either text writes, so a proper noun carried from the passage is never a misspelling. Each
//! sentence of prose then goes to the grammar source, one at a time, under the register and the
//! ruleset, as the analyzer sends it. Nothing but the content check runs on a paragraph of code.
//! The telltale toggles act through the catalogue the screen is built with, as they do for the
//! analyzer.
//!
//! A grammar check that fails on any sentence, with LanguageTool down, degraded or past its bound,
//! fails the whole run with [`ScreenError::GrammarCheckDidNotRun`], so no screening reads as clean
//! without grammar.
//!
//! Each result is tagged with its source. Content-preservation failures, spelling flags and
//! grammar flags are hard. Style flags, those of the ruleset's checkers and of the LanguageTool
//! rules a checker endorses, and telltale flags are soft. Hard against soft decides what the
//! working card does with a failed candidate and what synthesis records, and not the verdict: the
//! verdict fails on any result, hard or soft, and passes on none. The killing rule is the source of
//! the first result in the fixed order content preservation, spelling, grammar, style, telltales,
//! and a candidate with no result has none. Each list holds its results in that order of source,
//! and a source's flags in document order, each placed in the candidate as the analyzer places a
//! flag in the document: its id scoped by its sentence's index, its occurrence index counted, and
//! the ruleset version and the catalogue hash stamped on it.
//!
//! The screen suppresses nothing. The analyzer's suppression by severity is the overlay's display
//! rule, which keeps one underline where flags overlap; the screen lists every result each source
//! raised, since the verdict fails on any and the lists are the bench's record.
//!
//! A result can quote the candidate's words, through a flag's anchor and message, so no `Debug`
//! rendering here carries a flag's text: a result renders as its source and its flag's id.

pub mod content;

use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;

use crate::analyzer::passes::{self, Sources, UnitFlag};
use crate::analyzer::{form_mode, placed_in_document, sentences};
use crate::flag::{CatalogueHash, Flag, FlagOrigin, RulesetVersion, Severity};
use crate::languagetool::{GrammarSource, LanguageToolError};
use crate::spelling::SpellChecker;
use crate::style::ai_tell::Catalogue;
use crate::style::checker::FormMode;
use crate::style::evaluator::ModeSentence;
use crate::style::ruleset::LoadedRuleset;
use content::ContentRule;

/// The version of the build a screening runs under: the engine crate's, which the app and `wa`
/// both embed, since the screen runs inside the engine.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A source of the screen's results, and the rule a failed candidate is killed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum KillingRule {
    /// Content preservation.
    Content,
    Spelling,
    Grammar,
    Style,
    /// The telltale catalogue.
    Tells,
}

impl KillingRule {
    /// Every source, in the fixed order the killing rule is the first of.
    pub const ORDER: [Self; 5] = [
        Self::Content,
        Self::Spelling,
        Self::Grammar,
        Self::Style,
        Self::Tells,
    ];

    /// The source's place in [`Self::ORDER`].
    fn rank(self) -> usize {
        Self::ORDER
            .iter()
            .position(|rule| *rule == self)
            .expect("ORDER holds every variant")
    }
}

/// One result of the screen, tagged with its source.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum Finding {
    /// The content check's rules that failed, in the order numbers, capitalised tokens, negations,
    /// length.
    Content {
        failed: Vec<ContentRule>,
    },
    Spelling {
        flag: Flag,
    },
    /// A grammar flag of LanguageTool's.
    Grammar {
        flag: Flag,
    },
    /// A style flag, raised by a checker of the ruleset or through a LanguageTool rule a checker
    /// endorses.
    Style {
        flag: Flag,
    },
    Tells {
        flag: Flag,
    },
}

impl Finding {
    /// The result's source, which is the rule it fails a candidate by.
    pub fn rule(&self) -> KillingRule {
        match self {
            Self::Content { .. } => KillingRule::Content,
            Self::Spelling { .. } => KillingRule::Spelling,
            Self::Grammar { .. } => KillingRule::Grammar,
            Self::Style { .. } => KillingRule::Style,
            Self::Tells { .. } => KillingRule::Tells,
        }
    }

    /// A content-preservation failure is hard, and a flag is as hard as its origin.
    pub fn severity(&self) -> Severity {
        match self {
            Self::Content { .. } => Severity::Hard,
            Self::Spelling { flag }
            | Self::Grammar { flag }
            | Self::Style { flag }
            | Self::Tells { flag } => flag.origin.severity(),
        }
    }

    /// `flag` as a result of the source its origin names.
    fn of_flag(flag: Flag) -> Self {
        match flag.origin {
            FlagOrigin::Spelling => Self::Spelling { flag },
            FlagOrigin::Grammar => Self::Grammar { flag },
            FlagOrigin::Style { .. } => Self::Style { flag },
            FlagOrigin::AiTell { .. } => Self::Tells { flag },
        }
    }
}

/// Renders a flag-shaped result as its source and its flag's id, which carries no draft text.
impl fmt::Debug for Finding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = format!("{:?}", self.rule());
        match self {
            Self::Content { failed } => formatter
                .debug_struct(&source)
                .field("failed", failed)
                .finish(),
            Self::Spelling { flag }
            | Self::Grammar { flag }
            | Self::Style { flag }
            | Self::Tells { flag } => formatter
                .debug_struct(&source)
                .field("flag_id", &flag.id)
                .finish(),
        }
    }
}

/// Whether a candidate cleared the whole screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    Pass,
    Fail,
}

/// One screening of a candidate: its results, its verdict and its killing rule, and what it ran
/// under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Screening {
    /// Content-preservation failures, then spelling's flags, then grammar's.
    pub hard_results: Vec<Finding>,
    /// Style flags, then telltale flags.
    pub soft_results: Vec<Finding>,
    pub verdict: Verdict,
    /// `None` when no source raised a result.
    pub killing_rule: Option<KillingRule>,
    /// The ruleset version the screening ran under, `None` when no version is loaded.
    pub ruleset_version: Option<RulesetVersion>,
    /// The content hash of the rule data compiled into the engine, by [`Catalogue::hash`].
    pub catalogue_hash: CatalogueHash,
    /// The id of each telltale pattern switched on, in catalogue order. The toggles change what
    /// the screen flags and leave the catalogue hash as it is, so a screening is reproducible only
    /// with the set recorded.
    pub enabled_pattern_ids: Vec<String>,
    /// [`APP_VERSION`].
    pub app_version: String,
}

#[derive(Debug, Error)]
pub enum ScreenError {
    /// The grammar source failed the check of a sentence, so the run returns no screening rather
    /// than one without grammar.
    #[error("the grammar check did not run: {0}")]
    GrammarCheckDidNotRun(#[source] LanguageToolError),
}

/// The deterministic checks a candidate passes before it is shown, over the sources the caller
/// builds it with, as the analyzer is built.
pub struct Screen {
    spelling: Arc<SpellChecker>,
    grammar: Arc<dyn GrammarSource>,
    ruleset: Arc<LoadedRuleset>,
    catalogue: Arc<Catalogue>,
}

/// One sentence of a candidate, with the flags its checks raised on it.
struct CandidateSentence<'c> {
    text: &'c str,
    /// The byte offset in the candidate where the trimmed sentence starts.
    start: usize,
    /// The form mode of the sentence's paragraph.
    mode: FormMode,
    flags: Vec<Flag>,
}

impl Screen {
    /// A screen over `spelling`, `grammar`, `ruleset`, the ruleset in force, and `catalogue`,
    /// compiled from the settings' telltale toggles.
    pub fn new(
        spelling: Arc<SpellChecker>,
        grammar: Arc<dyn GrammarSource>,
        ruleset: Arc<LoadedRuleset>,
        catalogue: Arc<Catalogue>,
    ) -> Self {
        Self {
            spelling,
            grammar,
            ruleset,
            catalogue,
        }
    }

    /// Screens `candidate`, a rewrite of `passage`, under `register`.
    ///
    /// # Errors
    /// [`ScreenError::GrammarCheckDidNotRun`] when the grammar source fails the check of any
    /// sentence of prose.
    pub async fn run(
        &self,
        passage: &str,
        candidate: &str,
        register: &str,
    ) -> Result<Screening, ScreenError> {
        let content = content::check(passage, candidate);
        let mut candidate_sentences = self.deterministic_passes(passage, candidate, register);
        for sentence in candidate_sentences
            .iter_mut()
            .filter(|sentence| sentence.mode == FormMode::Prose)
        {
            let grammar_flags = self
                .grammar
                .check(sentence.text, &self.ruleset, sentence.mode, register)
                .await
                .map_err(ScreenError::GrammarCheckDidNotRun)?;
            sentence.flags.extend(grammar_flags);
        }

        let content_failure = Some(content.failed_rules())
            .filter(|failed| !failed.is_empty())
            .map(|failed| Finding::Content { failed });
        let flag_results =
            candidate_sentences
                .into_iter()
                .enumerate()
                .flat_map(|(index, sentence)| {
                    let text_before_sentence = &candidate[..sentence.start];
                    sentence.flags.into_iter().map(move |flag| {
                        Finding::of_flag(placed_in_document(
                            flag,
                            index,
                            text_before_sentence,
                            &self.ruleset,
                            &self.catalogue,
                        ))
                    })
                });
        let mut results: Vec<Finding> = content_failure.into_iter().chain(flag_results).collect();
        // A stable sort, so each source's flags keep their document order.
        results.sort_by_key(|result| result.rule().rank());
        let killing_rule = results.first().map(Finding::rule);
        let (hard_results, soft_results) = results
            .into_iter()
            .partition(|result| result.severity() == Severity::Hard);
        Ok(Screening {
            hard_results,
            soft_results,
            verdict: if killing_rule.is_some() {
                Verdict::Fail
            } else {
                Verdict::Pass
            },
            killing_rule,
            ruleset_version: self.ruleset.version(),
            catalogue_hash: self.catalogue.hash().clone(),
            enabled_pattern_ids: self
                .catalogue
                .enabled_pattern_ids()
                .map(str::to_owned)
                .collect(),
            app_version: APP_VERSION.to_owned(),
        })
    }

    /// The candidate's sentences, in document order, each with the flags the sentence pass, the
    /// paragraph pass and the document pass anchored on it, in that order.
    fn deterministic_passes<'c>(
        &self,
        passage: &str,
        candidate: &'c str,
        register: &str,
    ) -> Vec<CandidateSentence<'c>> {
        let sources = Sources {
            spelling: &self.spelling,
            spelling_source: Some(passage),
            catalogue: &self.catalogue,
            ruleset: &self.ruleset,
            register,
        };
        let mut candidate_sentences: Vec<CandidateSentence<'c>> = Vec::new();
        for paragraph in form_mode::split(candidate) {
            let first_sentence = candidate_sentences.len();
            let paragraph_sentences = sentences::split(paragraph.text);
            for sentence in &paragraph_sentences {
                candidate_sentences.push(CandidateSentence {
                    text: sentence.text,
                    start: paragraph.start + sentence.start,
                    mode: paragraph.mode,
                    flags: passes::sentence_pass(&sources, sentence.text, paragraph.mode),
                });
            }
            for UnitFlag { sentence, flag } in
                passes::paragraph_pass(&sources, &paragraph, &paragraph_sentences)
            {
                candidate_sentences[first_sentence + sentence]
                    .flags
                    .push(flag);
            }
        }
        let mode_sentences: Vec<ModeSentence<'c>> = candidate_sentences
            .iter()
            .map(|sentence| ModeSentence {
                text: sentence.text,
                mode: sentence.mode,
            })
            .collect();
        for UnitFlag { sentence, flag } in passes::document_pass(&sources, &mode_sentences) {
            candidate_sentences[sentence].flags.push(flag);
        }
        candidate_sentences
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::flag::ClauseId;
    use crate::languagetool::captured_responses::{GOT_GOTTEN_RESPONSE, PASSIVE_VOICE_RESPONSE};
    use crate::store::Config;
    use crate::style::checker::{Band, CheckableMode, Checker, Matcher, PhraseEntry, Rule, Unit};
    use crate::test_support::{test_spell_checker, Answer, FakeGrammar};
    use KillingRule::{Content, Grammar, Spelling, Style, Tells};

    /// The text of the clause every fixture checker serves, written for these tests.
    const CLAUSE: &str = "Hedge rarely.";

    const REGISTER: &str = "default";

    /// A passage's sentence and the candidate's sentence in its place.
    type SentencePair = (&'static str, &'static str);

    /// For each source, in the killing rule's order, the passage's sentence and the candidate's
    /// that raise a result of that source on the candidate, and the pair that raises none, all
    /// written for these tests. Spelling's needs a word the passage lacks, grammar's the word the
    /// fake grammar source flags, style's the phrase the fixture checker forbids, and the
    /// telltale's a phrase of the default catalogue.
    const SENTENCES: [(SentencePair, SentencePair); 5] = [
        (
            ("Gate 9 opens.", "Gate 8 opens."),
            ("Gate 9 opens.", "Gate 9 opens."),
        ),
        (
            ("The error stays.", "The eror stays."),
            ("The error stays.", "The error stays."),
        ),
        (
            ("He go home.", "He go home."),
            ("He went home.", "He went home."),
        ),
        (
            ("Perhaps it rains.", "Perhaps it rains."),
            ("It rains.", "It rains."),
        ),
        (
            (
                "It stands as a testament to the team.",
                "It stands as a testament to the team.",
            ),
            ("It helps the team.", "It helps the team."),
        ),
    ];

    /// The passage and the candidate whose results come from the sources of `sources` alone.
    fn texts_raising(sources: &[KillingRule]) -> (String, String) {
        let (passage, candidate): (Vec<&str>, Vec<&str>) = KillingRule::ORDER
            .iter()
            .zip(SENTENCES)
            .map(|(rule, (raising, clean))| {
                if sources.contains(rule) {
                    raising
                } else {
                    clean
                }
            })
            .unzip();
        (passage.join(" "), candidate.join(" "))
    }

    fn phrase(phrase: &str) -> Matcher {
        Matcher::PhraseList {
            entries: vec![PhraseEntry {
                phrase: phrase.to_owned(),
                prefix: false,
            }],
        }
    }

    /// A checker of `unit` under `band` over `matcher`, declared for prose and fragments in every
    /// register.
    fn checker(matcher: Matcher, unit: Unit, band: Band) -> Checker {
        Checker {
            clause_id: ClauseId(1),
            rule: Rule {
                matcher,
                unit,
                band,
                form_modes: BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]),
                register_scope: None,
            },
        }
    }

    fn at_most(maximum: u32) -> Band {
        Band::Count {
            minimum: None,
            maximum: Some(maximum),
        }
    }

    /// A ruleset at version 3 holding `checkers`, with ids from 1 in the order given.
    fn ruleset(checkers: Vec<Checker>) -> LoadedRuleset {
        LoadedRuleset::of_checkers(
            RulesetVersion(3),
            checkers
                .into_iter()
                .map(|checker| (checker, CLAUSE))
                .collect(),
        )
    }

    /// The catalogue compiled from the toggles the settings file is first written with.
    fn default_catalogue() -> Catalogue {
        Catalogue::load(&Config::default())
    }

    fn screen_of(
        grammar: &Arc<FakeGrammar>,
        ruleset: LoadedRuleset,
        catalogue: Catalogue,
    ) -> Screen {
        Screen::new(
            Arc::new(test_spell_checker()),
            Arc::clone(grammar) as Arc<dyn GrammarSource>,
            Arc::new(ruleset),
            Arc::new(catalogue),
        )
    }

    /// A screen over `grammar` whose ruleset forbids `perhaps` in a span, with the default
    /// catalogue.
    fn screen(grammar: &Arc<FakeGrammar>) -> Screen {
        screen_of(
            grammar,
            ruleset(vec![checker(
                phrase("perhaps"),
                Unit::Span,
                Band::Forbidden,
            )]),
            default_catalogue(),
        )
    }

    /// Screens the passage and the candidate of [`texts_raising`] `sources`, with the grammar
    /// source flagging `go`.
    async fn screening_raising(sources: &[KillingRule]) -> Screening {
        let (passage, candidate) = texts_raising(sources);
        screen(&FakeGrammar::flagging(&["go"]))
            .run(&passage, &candidate, REGISTER)
            .await
            .unwrap()
    }

    /// The source of each result of `results`, each once, in order.
    fn sources_of(results: &[Finding]) -> Vec<KillingRule> {
        let mut sources: Vec<KillingRule> = results.iter().map(Finding::rule).collect();
        sources.dedup();
        sources
    }

    /// The flag of a flag-shaped result.
    fn flag(result: &Finding) -> &Flag {
        match result {
            Finding::Content { .. } => panic!("a content result holds no flag"),
            Finding::Spelling { flag }
            | Finding::Grammar { flag }
            | Finding::Style { flag }
            | Finding::Tells { flag } => flag,
        }
    }

    /// The text a flag-shaped result's span addresses, sliced out of its anchor.
    fn flagged_text(result: &Finding) -> String {
        let flag = flag(result);
        let anchor: Vec<u16> = flag.span.anchor.encode_utf16().collect();
        let end = flag.span.local_start + flag.span.local_length;
        String::from_utf16_lossy(&anchor[flag.span.local_start..end])
    }

    /// Every result of `screening`, hard then soft.
    fn all_results(screening: &Screening) -> impl Iterator<Item = &Finding> {
        screening.hard_results.iter().chain(&screening.soft_results)
    }

    #[tokio::test]
    async fn a_screening_returns_two_lists_the_verdict_the_killing_rule_and_what_it_ran_under() {
        let screening = screening_raising(&KillingRule::ORDER).await;

        assert!(!screening.hard_results.is_empty());
        assert!(!screening.soft_results.is_empty());
        assert_eq!(screening.verdict, Verdict::Fail);
        assert_eq!(screening.killing_rule, Some(Content));
        assert_eq!(screening.ruleset_version, Some(RulesetVersion(3)));
        assert_eq!(&screening.catalogue_hash, default_catalogue().hash());
        let enabled: Vec<String> = default_catalogue()
            .enabled_pattern_ids()
            .map(str::to_owned)
            .collect();
        assert_eq!(screening.enabled_pattern_ids, enabled);
        assert_eq!(screening.app_version, env!("CARGO_PKG_VERSION"));

        let json = serde_json::to_value(&screening).unwrap();
        let keys: BTreeSet<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "appVersion",
                "catalogueHash",
                "enabledPatternIds",
                "hardResults",
                "killingRule",
                "rulesetVersion",
                "softResults",
                "verdict",
            ])
        );
        assert_eq!(
            json["hardResults"][0],
            serde_json::json!({ "source": "content", "failed": ["numbers"] })
        );
        assert_eq!(json["hardResults"][1]["source"], "spelling");
        assert_eq!(json["hardResults"][1]["flag"]["origin"], "spelling");
        assert_eq!(json["killingRule"], "content");
        assert_eq!(json["verdict"], "fail");
    }

    #[tokio::test]
    async fn spelling_grammar_and_content_results_are_hard_and_style_and_telltale_results_are_soft()
    {
        let screening = screening_raising(&KillingRule::ORDER).await;

        assert_eq!(
            sources_of(&screening.hard_results),
            [Content, Spelling, Grammar]
        );
        assert_eq!(sources_of(&screening.soft_results), [Style, Tells]);
        for result in &screening.hard_results {
            assert_eq!(result.severity(), Severity::Hard, "{result:?}");
        }
        for result in &screening.soft_results {
            assert_eq!(result.severity(), Severity::Soft, "{result:?}");
        }
        let flagged: Vec<(KillingRule, String)> = all_results(&screening)
            .filter(|result| !matches!(result, Finding::Content { .. }))
            .map(|result| (result.rule(), flagged_text(result)))
            .collect();
        assert_eq!(flagged[0], (Spelling, "eror".to_owned()));
        assert_eq!(flagged[1], (Grammar, "go".to_owned()));
        assert_eq!(flagged[2], (Style, "Perhaps".to_owned()));
        assert!(
            flagged[3..]
                .iter()
                .all(|(source, text)| *source == Tells && "stands as a testament to".contains(text)),
            "{flagged:?}"
        );
    }

    #[tokio::test]
    async fn a_candidate_with_a_result_fails_and_a_candidate_with_none_passes() {
        let clean = screening_raising(&[]).await;
        assert!(clean.hard_results.is_empty(), "{clean:?}");
        assert!(clean.soft_results.is_empty(), "{clean:?}");
        assert_eq!(clean.verdict, Verdict::Pass);
        assert_eq!(clean.killing_rule, None);

        for source in KillingRule::ORDER {
            let screening = screening_raising(&[source]).await;
            assert_eq!(screening.verdict, Verdict::Fail, "{source:?}");
        }
    }

    #[tokio::test]
    async fn a_candidate_with_soft_results_only_fails() {
        for soft in [vec![Style], vec![Tells], vec![Style, Tells]] {
            let screening = screening_raising(&soft).await;
            assert!(screening.hard_results.is_empty(), "{screening:?}");
            assert_eq!(sources_of(&screening.soft_results), soft);
            assert_eq!(screening.verdict, Verdict::Fail);
        }
    }

    #[tokio::test]
    async fn the_killing_rule_is_the_first_source_with_a_result_in_the_fixed_order() {
        for first in 0..=KillingRule::ORDER.len() {
            let raising = &KillingRule::ORDER[first..];
            let screening = screening_raising(raising).await;
            assert_eq!(
                screening.killing_rule,
                KillingRule::ORDER.get(first).copied(),
                "{raising:?}"
            );
            let mut sources = sources_of(&screening.hard_results);
            sources.extend(sources_of(&screening.soft_results));
            assert_eq!(sources, raising);
        }
    }

    #[tokio::test]
    async fn a_word_the_dictionary_lacks_and_the_passage_holds_raises_no_result() {
        let grammar = FakeGrammar::flagging(&[]);
        let screen = screen_of(&grammar, ruleset(Vec::new()), default_catalogue());
        for (passage, candidate) in [
            (
                "We met Xilvane at the pier.",
                "At the pier, we met Xilvane.",
            ),
            // Only the first letter's case changes, at the start of a sentence.
            (
                "The xilvane rang at the pier.",
                "Xilvane rang out at the pier.",
            ),
        ] {
            let screening = screen.run(passage, candidate, REGISTER).await.unwrap();
            assert!(
                all_results(&screening).all(|result| result.rule() != Spelling),
                "{candidate}: {screening:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_word_the_dictionary_lacks_and_the_passage_does_not_hold_raises_a_hard_result() {
        let grammar = FakeGrammar::flagging(&[]);
        let screen = screen_of(&grammar, ruleset(Vec::new()), default_catalogue());
        let screening = screen
            .run("We met at the pier.", "We met at the peir.", REGISTER)
            .await
            .unwrap();
        assert_eq!(sources_of(&screening.hard_results), [Spelling]);
        assert_eq!(flagged_text(&screening.hard_results[0]), "peir");
        assert_eq!(screening.killing_rule, Some(Spelling));
    }

    #[tokio::test]
    async fn each_sentence_of_prose_goes_to_the_grammar_source_under_the_register_and_the_ruleset()
    {
        let gotten = "I gotten used to it.";
        let passive = "The report was written by the committee.";
        let grammar = FakeGrammar::routing(&[
            (gotten, GOT_GOTTEN_RESPONSE),
            (passive, PASSIVE_VOICE_RESPONSE),
        ]);
        // The passive voice is endorsed in email alone.
        let mut endorsing = checker(
            Matcher::LanguageToolRule {
                rule_id: "PASSIVE_VOICE_SIMPLE".to_owned(),
            },
            Unit::Sentence,
            Band::Forbidden,
        );
        endorsing.rule.register_scope = Some(BTreeSet::from(["email".to_owned()]));
        let screen = screen_of(&grammar, ruleset(vec![endorsing]), default_catalogue());
        let text = format!("{gotten} {passive}");

        let email = screen.run(&text, &text, "email").await.unwrap();
        assert_eq!(sources_of(&email.hard_results), [Grammar]);
        assert_eq!(flag(&email.hard_results[0]).source_detail, "GOT_GOTTEN");
        assert_eq!(flagged_text(&email.hard_results[0]), "gotten");
        assert_eq!(sources_of(&email.soft_results), [Style]);
        assert!(
            matches!(&flag(&email.soft_results[0]).origin, FlagOrigin::Style { citation } if citation.clause_text == CLAUSE),
            "{:#?}",
            email.soft_results
        );

        // Outside its scope the endorsement is absent, and the picky style match is dropped.
        let chat = screen.run(&text, &text, "chat").await.unwrap();
        assert_eq!(sources_of(&chat.hard_results), [Grammar]);
        assert!(chat.soft_results.is_empty(), "{chat:?}");

        assert_eq!(grammar.sent(), [gotten, passive, gotten, passive]);
    }

    #[tokio::test]
    async fn a_failed_check_of_any_sentence_returns_the_error_that_grammar_did_not_run() {
        let (passage, candidate) = texts_raising(&[]);
        for answer in [Answer::RequestFailure, Answer::Timeout, Answer::Degraded] {
            let grammar = FakeGrammar::flagging(&[]);
            grammar.answer_with(answer);
            let error = screen(&grammar)
                .run(&passage, &candidate, REGISTER)
                .await
                .unwrap_err();
            let ScreenError::GrammarCheckDidNotRun(source) = error;
            let expected = match answer {
                Answer::RequestFailure => matches!(source, LanguageToolError::Request(_)),
                Answer::Timeout => matches!(source, LanguageToolError::CheckTimeout { .. }),
                Answer::Degraded => matches!(source, LanguageToolError::Degraded),
                Answer::Flags => unreachable!(),
            };
            assert!(expected, "{answer:?}: {source:?}");
            // The run stopped at the first sentence.
            assert_eq!(grammar.sent().len(), 1);
        }
    }

    #[tokio::test]
    async fn the_candidate_is_segmented_as_live_text_and_nothing_but_the_content_check_runs_on_code(
    ) {
        let candidate = |code: &str| {
            format!("# Perhaps notes\nPerhaps it rains.\n```\n{code}\n```\n- Perhaps a list item")
        };
        let grammar = FakeGrammar::flagging(&["go", "rains"]);
        let screen = screen_of(
            &grammar,
            ruleset(vec![
                checker(phrase("perhaps"), Unit::Document, at_most(1)),
                checker(phrase("perhaps"), Unit::Paragraph, Band::Forbidden),
            ]),
            default_catalogue(),
        );
        let screening = screen
            .run(
                &candidate("Perhaps it compiles, eror go 42."),
                &candidate("Perhaps it compiles, eror go 43."),
                REGISTER,
            )
            .await
            .unwrap();

        // The content check reads the code.
        assert_eq!(
            screening.hard_results[0],
            Finding::Content {
                failed: vec![ContentRule::Numbers]
            }
        );
        // The heading and the list item are fragments, so the prose paragraph alone is sent.
        assert_eq!(grammar.sent(), ["Perhaps it rains."]);
        assert!(
            all_results(&screening)
                .skip(1)
                .all(|result| !flag(result).span.anchor.contains("compiles")),
            "{screening:?}"
        );
        // The document holds four hedges, and the one in code is not counted.
        let document_flags: Vec<&Flag> = screening
            .soft_results
            .iter()
            .map(flag)
            .filter(|flag| flag.message.contains("in this document"))
            .collect();
        assert_eq!(document_flags.len(), 2, "{screening:?}");
        assert!(
            document_flags
                .iter()
                .all(|flag| flag.message == "3 matches in this document, over the maximum of 1"),
            "{document_flags:#?}"
        );
    }

    #[tokio::test]
    async fn the_telltale_toggles_are_honoured_through_the_catalogue_the_screen_is_built_with() {
        let text = "It stands as a testament to the team.";
        let grammar = FakeGrammar::flagging(&[]);
        let on = screen_of(&grammar, ruleset(Vec::new()), default_catalogue())
            .run(text, text, REGISTER)
            .await
            .unwrap();
        let pattern_ids: BTreeSet<String> = on
            .soft_results
            .iter()
            .map(|result| match &flag(result).origin {
                FlagOrigin::AiTell { pattern_ids } => pattern_ids[0].clone(),
                origin => panic!("{origin:?}"),
            })
            .collect();
        assert!(!pattern_ids.is_empty(), "{on:?}");

        let mut settings = Config::default();
        for id in &pattern_ids {
            settings.telltale_toggles.insert(id.clone(), false);
        }
        let off = screen_of(&grammar, ruleset(Vec::new()), Catalogue::load(&settings))
            .run(text, text, REGISTER)
            .await
            .unwrap();
        assert!(off.soft_results.is_empty(), "{off:?}");
        assert_eq!(off.verdict, Verdict::Pass);
        assert!(off
            .enabled_pattern_ids
            .iter()
            .all(|id| !pattern_ids.contains(id)));
        assert_eq!(
            off.enabled_pattern_ids.len() + pattern_ids.len(),
            on.enabled_pattern_ids.len()
        );
        assert_eq!(off.catalogue_hash, on.catalogue_hash);
    }

    #[tokio::test]
    async fn the_screen_lists_every_result_where_the_analyzer_would_suppress_one() {
        // The grammar flag overlaps the telltales, which also overlap each other.
        let text = "It stands as a testament to the team.";
        let grammar = FakeGrammar::flagging(&["a testament"]);
        let screening = screen_of(&grammar, ruleset(Vec::new()), default_catalogue())
            .run(text, text, REGISTER)
            .await
            .unwrap();
        assert_eq!(sources_of(&screening.hard_results), [Grammar]);
        assert!(
            screening.soft_results.len() > 1,
            "{:?}",
            screening.soft_results
        );
        assert!(screening
            .soft_results
            .iter()
            .all(|result| result.rule() == Tells));
    }

    #[tokio::test]
    async fn each_flag_is_placed_in_the_candidate_as_the_analyzer_places_one_in_the_document() {
        let grammar = FakeGrammar::flagging(&[]);
        let screen = screen_of(&grammar, ruleset(Vec::new()), default_catalogue());
        let candidate = "An eror here. An eror here.";
        let screening = screen
            .run("A slip here. A slip here.", candidate, REGISTER)
            .await
            .unwrap();
        let placed: Vec<(&str, usize)> = screening
            .hard_results
            .iter()
            .map(|result| (flag(result).id.as_str(), flag(result).span.occurrence))
            .collect();
        assert_eq!(placed, [("0:spelling:3", 0), ("1:spelling:3", 1)]);
        for result in &screening.hard_results {
            assert_eq!(flag(result).ruleset_version, Some(RulesetVersion(3)));
            assert_eq!(
                flag(result).catalogue_hash.as_ref(),
                Some(default_catalogue().hash())
            );
        }
    }

    #[tokio::test]
    async fn the_same_inputs_screened_twice_give_byte_identical_serialised_results() {
        let (passage, candidate) = texts_raising(&KillingRule::ORDER);
        let grammar = FakeGrammar::flagging(&["go"]);
        let first_screen = screen(&grammar);
        let first = first_screen
            .run(&passage, &candidate, REGISTER)
            .await
            .unwrap();
        let again = first_screen
            .run(&passage, &candidate, REGISTER)
            .await
            .unwrap();
        // A screen built afresh loads its own spell checker and compiles its own catalogue.
        let second = screen(&grammar)
            .run(&passage, &candidate, REGISTER)
            .await
            .unwrap();

        let serialised = serde_json::to_string(&first).unwrap();
        assert_eq!(serde_json::to_string(&again).unwrap(), serialised);
        assert_eq!(serde_json::to_string(&second).unwrap(), serialised);
    }

    #[test]
    fn a_run_is_a_future_a_caller_can_spawn() {
        fn assert_send<T: Send>(_: &T) {}
        let screen = screen(&FakeGrammar::flagging(&[]));
        assert_send(&screen.run("", "", REGISTER));
    }

    #[test]
    fn a_results_debug_rendering_carries_its_source_and_flag_id_and_no_text() {
        let result = Finding::Spelling {
            flag: Flag {
                id: "0:spelling:3".to_owned(),
                origin: FlagOrigin::Spelling,
                span: crate::flag::Span {
                    anchor: "An eror here.".to_owned(),
                    local_start: 3,
                    local_length: 4,
                    occurrence: 0,
                },
                message: "\"eror\" is not in the dictionary".to_owned(),
                suggestions: vec!["error".to_owned()],
                source_detail: "en_GB plus the Singapore supplement".to_owned(),
                ruleset_version: None,
                catalogue_hash: None,
            },
        };
        let rendered = format!("{result:?}");
        assert_eq!(rendered, "Spelling { flag_id: \"0:spelling:3\" }");
    }
}
