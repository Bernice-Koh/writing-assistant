//! The ruleset in force: the latest published ruleset version, read once and compiled once, with
//! the citation each checker's flags embed. A store with no ruleset version, or no store, gives an
//! empty ruleset with no version, and spelling, grammar and telltale checking run as they do
//! without one. Running the checkers over a document is the analyzer's (#100).
//!
//! A clause's text is the owner's words, so no log line or `Debug` rendering here carries it: a
//! warning names a checker by its id.

use std::collections::HashMap;
use std::fmt;

use super::checker::{Matcher, Rule, Unit};
use super::evaluator::{Breach, Evaluator, RuleOwner, Violation};
use crate::flag::{ClauseCitation, ClauseId, Flag, FlagOrigin, RuleId, RulesetVersion};
use crate::store::{CheckerId, PublishedCard, PublishedRuleset, Store, StoreError, StoredChecker};

/// A ruleset version and its checkers, each compiled once.
#[derive(Debug, Default)]
pub struct LoadedRuleset {
    version: Option<RulesetVersion>,
    /// In ascending order of id.
    checkers: Vec<LoadedChecker>,
}

/// One checker of a loaded ruleset: its rule compiled, and the citation every flag it raises
/// embeds.
pub struct LoadedChecker {
    id: CheckerId,
    evaluator: Evaluator,
    citation: ClauseCitation,
}

impl LoadedRuleset {
    /// No version and no checker, the ruleset in force when the store did not open.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The latest ruleset version in `store`, read through the store thread with each checker
    /// compiled once. Each checker cites its clause's text from the latest Card version, as it
    /// stands at this load. A checker whose rule does not compile, or whose clause the latest Card
    /// version does not hold, is left out with one `warn` line naming its id, and the rest load.
    /// Empty, with no version, when the store holds no ruleset version.
    ///
    /// # Errors
    /// As [`Store::latest_ruleset_version`] and [`Store::latest_card_version`].
    pub async fn load(store: &Store) -> Result<Self, StoreError> {
        let ruleset = store.latest_ruleset_version().await?;
        let card = store.latest_card_version().await?;
        Ok(Self::compiled(ruleset, card.as_ref()))
    }

    /// [`load`](Self::load) for a synchronous caller, such as `wa`, which blocks on the store's
    /// replies.
    ///
    /// # Errors
    /// As [`load`](Self::load).
    ///
    /// # Panics
    /// When called from async code, which awaits [`load`](Self::load) instead.
    pub fn load_blocking(store: &Store) -> Result<Self, StoreError> {
        let ruleset = store.latest_ruleset_version_blocking()?;
        let card = store.latest_card_version_blocking()?;
        Ok(Self::compiled(ruleset, card.as_ref()))
    }

    fn compiled(ruleset: Option<PublishedRuleset>, card: Option<&PublishedCard>) -> Self {
        let Some(PublishedRuleset {
            version, checkers, ..
        }) = ruleset
        else {
            return Self::empty();
        };
        let clause_texts: HashMap<ClauseId, &str> = card
            .into_iter()
            .flat_map(|card| &card.clauses)
            .map(|clause| (clause.clause_id, clause.wording.text.as_str()))
            .collect();
        Self {
            version: Some(version),
            checkers: checkers
                .into_iter()
                .filter_map(|stored| loaded(stored, version, &clause_texts))
                .collect(),
        }
    }

    /// The version every flag raised under this ruleset records, `None` when no version is loaded.
    pub fn version(&self) -> Option<RulesetVersion> {
        self.version
    }

    pub fn checkers(&self) -> &[LoadedChecker] {
        &self.checkers
    }

    /// The style flag for `violation` of `checker`, one of this ruleset's checkers. It cites the
    /// checker's clause, records this ruleset's version, and leaves the catalogue hash for the
    /// analyzer to set. Its message states the count and the limit, the shortfall, or the rate,
    /// and is empty for a match under the forbidden band, which breaks the clause itself.
    pub fn flag(&self, violation: Violation, checker: &LoadedChecker) -> Flag {
        Flag {
            // The checker and the span tell two of the checker's flags on one sentence apart, and
            // neither is draft text, which an id never carries.
            id: format!(
                "style:{}:{}:{}",
                checker.id.0, violation.span.local_start, violation.span.local_length
            ),
            origin: FlagOrigin::Style(checker.citation.clone()),
            message: message(&violation.breach, checker.evaluator.rule()),
            span: violation.span,
            suggestions: Vec::new(),
            source_detail: checker.citation.rule_id.0.clone(),
            ruleset_version: self.version,
            catalogue_hash: None,
        }
    }
}

impl LoadedChecker {
    pub fn id(&self) -> CheckerId {
        self.id
    }

    pub fn evaluator(&self) -> &Evaluator {
        &self.evaluator
    }

    pub fn citation(&self) -> &ClauseCitation {
        &self.citation
    }
}

/// Shows the clause text's length in bytes in place of the text, since a `Debug` rendering ends up
/// in log lines and test failure messages.
impl fmt::Debug for LoadedChecker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedChecker")
            .field("id", &self.id)
            .field("evaluator", &self.evaluator)
            .field("clause_id", &self.citation.clause_id)
            .field("rule_id", &self.citation.rule_id)
            .field("clause_text_bytes", &self.citation.clause_text.len())
            .finish()
    }
}

/// `stored` compiled, with its citation, or `None` after one warning when its rule does not
/// compile or `clause_texts`, the latest Card version's, holds no text for its clause.
fn loaded(
    stored: StoredChecker,
    version: RulesetVersion,
    clause_texts: &HashMap<ClauseId, &str>,
) -> Option<LoadedChecker> {
    let StoredChecker {
        id,
        checker,
        content_hash,
        ..
    } = stored;
    let rule_id = rule_id(&checker.rule.matcher, content_hash);
    let evaluator = match Evaluator::compile(RuleOwner::Clause(checker.clause_id), checker.rule) {
        Ok(evaluator) => evaluator,
        Err(error) => {
            log::warn!(
                "left checker {} of ruleset version {} out of the loaded ruleset, since it does \
                 not compile: {error}",
                id.0,
                version.0
            );
            return None;
        }
    };
    let Some(clause_text) = clause_texts.get(&checker.clause_id) else {
        log::warn!(
            "left checker {} of ruleset version {} out of the loaded ruleset, since its clause, \
             {}, is not in the latest Card version",
            id.0,
            version.0,
            checker.clause_id.0
        );
        return None;
    };
    Some(LoadedChecker {
        id,
        evaluator,
        citation: ClauseCitation {
            clause_id: checker.clause_id,
            rule_id,
            clause_text: (*clause_text).to_owned(),
        },
    })
}

/// LanguageTool's own id for a checker whose matcher is a LanguageTool rule id, and the checker's
/// content hash for every other, so a changed parameter is a new rule id.
fn rule_id(matcher: &Matcher, content_hash: String) -> RuleId {
    match matcher {
        Matcher::LanguageToolRule { rule_id } => RuleId(rule_id.clone()),
        _ => RuleId(content_hash),
    }
}

/// What `breach` of `rule` adds to the clause its flag cites, which the hover card shows above it.
fn message(breach: &Breach, rule: &Rule) -> String {
    let unit = match rule.unit {
        Unit::Span | Unit::Sentence => "sentence",
        Unit::Paragraph => "paragraph",
        Unit::Document => "document",
    };
    let (one, many) = match rule.matcher {
        Matcher::WordCount => ("word", "words"),
        _ => ("match", "matches"),
    };
    let counted = |count: u32| format!("{count} {}", if count == 1 { one } else { many });
    match *breach {
        Breach::Forbidden => String::new(),
        Breach::AboveMaximum { count, maximum } => {
            format!(
                "{} in this {unit}, over the maximum of {maximum}",
                counted(count)
            )
        }
        Breach::BelowMinimum {
            count,
            minimum,
            shortfall,
        } => format!(
            "{} in this {unit}, {shortfall} short of the minimum of {minimum}",
            counted(count)
        ),
        Breach::AboveRate {
            rate_per_hundred_words,
            maximum_per_hundred_words,
        } => {
            let rate = rate_beside(rate_per_hundred_words, maximum_per_hundred_words);
            format!(
                "{rate} {} per hundred words in this {unit}, over the maximum of \
                 {maximum_per_hundred_words}",
                if rate == 1.0 { one } else { many }
            )
        }
    }
}

/// `rate`, which exceeds `maximum`, rounded to the fewest decimal places, from one to four, at
/// which it still reads as more than `maximum`, so a message never states a rate equal to its
/// limit.
fn rate_beside(rate: f64, maximum: f64) -> f64 {
    let rounded = |value: f64, places: i32| {
        let scale = 10_f64.powi(places);
        (value * scale).round() / scale
    };
    (1..=4)
        .map(|places| (rounded(rate, places), rounded(maximum, places)))
        .find(|(shown, limit)| shown > limit)
        .map_or(rate, |(shown, _)| shown)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use log::Level;

    use super::*;
    use crate::store::{CardMember, CheckerAuthor, ClauseWording, Provenance};
    use crate::style::checker::{Band, CheckableMode, Checker, FormMode, PhraseEntry};
    use crate::test_support::{capture_logs, TempRoot};

    const HEDGES: (&str, &str) = (
        "Hedge at most once a paragraph.",
        "Hedge several times in every paragraph.",
    );
    const HEDGES_REWORDED: (&str, &str) = (
        "Hedge no more than once in a paragraph.",
        "Hedge in every sentence.",
    );
    const NO_DASHES: (&str, &str) = ("Never write an em dash.", "Write em dashes freely.");

    /// Every clause's words the tests here store, which no log line may carry.
    const CLAUSE_WORDS: [&str; 6] = [
        HEDGES.0,
        HEDGES.1,
        HEDGES_REWORDED.0,
        HEDGES_REWORDED.1,
        NO_DASHES.0,
        NO_DASHES.1,
    ];

    fn wording((text, twin): (&str, &str)) -> ClauseWording {
        ClauseWording {
            text: text.to_owned(),
            twin: twin.to_owned(),
            register_scope: None,
            example: None,
        }
    }

    /// A store holding a Card of the hedging limit and the dash ban, ids 1 and 2, whose second
    /// version rewords the hedging limit.
    fn store_with_card(root: &TempRoot) -> (Store, ClauseId, ClauseId) {
        let store = Store::open(root.path()).unwrap();
        let first = store
            .insert_card_version_blocking(
                &[
                    CardMember::New(wording(HEDGES)),
                    CardMember::New(wording(NO_DASHES)),
                ],
                "the first Card",
            )
            .unwrap();
        let first = store.card_version_blocking(first).unwrap();
        let (hedges, dashes) = (first.clauses[0].clause_id, first.clauses[1].clause_id);
        store
            .insert_card_version_blocking(
                &[
                    CardMember::Reworded {
                        clause_id: hedges,
                        wording: wording(HEDGES_REWORDED),
                    },
                    CardMember::Stored(first.clauses[1].text_id),
                ],
                "the hedging limit reworded",
            )
            .unwrap();
        (store, hedges, dashes)
    }

    fn rule(matcher: Matcher, unit: Unit, band: Band) -> Rule {
        Rule {
            matcher,
            unit,
            band,
            form_modes: BTreeSet::from([CheckableMode::Prose]),
            register_scope: None,
        }
    }

    fn hedging_limit(clause_id: ClauseId, band: Band) -> Checker {
        Checker {
            clause_id,
            rule: rule(
                Matcher::PhraseList {
                    entries: vec![PhraseEntry {
                        phrase: "perhaps".to_owned(),
                        prefix: false,
                    }],
                },
                Unit::Paragraph,
                band,
            ),
        }
    }

    fn at_most(maximum: u32) -> Band {
        Band::Count {
            minimum: None,
            maximum: Some(maximum),
        }
    }

    fn dash_ban(clause_id: ClauseId) -> Checker {
        Checker {
            clause_id,
            rule: rule(
                Matcher::Regex {
                    source: "\u{2014}".to_owned(),
                },
                Unit::Span,
                Band::Forbidden,
            ),
        }
    }

    fn passive_voice(clause_id: ClauseId) -> Checker {
        Checker {
            clause_id,
            rule: rule(
                Matcher::LanguageToolRule {
                    rule_id: "PASSIVE_VOICE".to_owned(),
                },
                Unit::Sentence,
                Band::Forbidden,
            ),
        }
    }

    fn templated() -> Provenance {
        Provenance {
            author: CheckerAuthor::Template,
            back_translation: None,
            test_results: Vec::new(),
        }
    }

    /// Publishes `checkers` and returns the version as the store holds it.
    fn publish(store: &Store, checkers: &[Checker]) -> PublishedRuleset {
        let with_provenance: Vec<(Checker, Provenance)> = checkers
            .iter()
            .map(|checker| (checker.clone(), templated()))
            .collect();
        let published = store.publish_ruleset_blocking(&with_provenance).unwrap();
        store.ruleset_version_blocking(published.version).unwrap()
    }

    fn warnings(lines: &[(Level, String)]) -> Vec<&str> {
        lines
            .iter()
            .filter(|(level, _)| *level <= Level::Warn)
            .map(|(_, line)| line.as_str())
            .collect()
    }

    fn assert_no_clause_words(lines: &[&str]) {
        for (index, line) in lines.iter().enumerate() {
            for words in CLAUSE_WORDS {
                assert!(
                    !line.contains(words),
                    "line {index} carries a clause's words"
                );
            }
        }
    }

    #[test]
    fn loading_compiles_each_checker_once_and_cites_the_latest_cards_text() {
        let root = TempRoot::new();
        let (store, hedges, dashes) = store_with_card(&root);
        let stored = publish(
            &store,
            &[
                hedging_limit(hedges, at_most(1)),
                dash_ban(dashes),
                passive_voice(dashes),
            ],
        );

        let (ruleset, lines) = capture_logs(|| LoadedRuleset::load_blocking(&store));

        let ruleset = ruleset.unwrap();
        assert!(warnings(&lines).is_empty(), "{lines:?}");
        assert_eq!(ruleset.version(), Some(stored.version));
        assert_eq!(ruleset.checkers().len(), stored.checkers.len());
        let expected_rule_ids = [
            stored.checkers[0].content_hash.clone(),
            stored.checkers[1].content_hash.clone(),
            "PASSIVE_VOICE".to_owned(),
        ];
        let expected_texts = [HEDGES_REWORDED.0, NO_DASHES.0, NO_DASHES.0];
        let pairs = ruleset.checkers().iter().zip(&stored.checkers);
        for (index, (loaded, published)) in pairs.enumerate() {
            assert_eq!(loaded.id(), published.id, "checker {index}");
            assert_eq!(
                loaded.evaluator().rule(),
                &published.checker.rule,
                "{index}"
            );
            assert_eq!(
                loaded.evaluator().owner(),
                &RuleOwner::Clause(published.checker.clause_id),
                "{index}"
            );
            assert_eq!(
                loaded.citation(),
                &ClauseCitation {
                    clause_id: published.checker.clause_id,
                    rule_id: RuleId(expected_rule_ids[index].clone()),
                    clause_text: expected_texts[index].to_owned(),
                },
                "checker {index}"
            );
        }
    }

    #[test]
    fn a_checker_that_does_not_compile_is_left_out_with_one_warning_naming_it_and_no_clause() {
        let root = TempRoot::new();
        let (store, hedges, dashes) = store_with_card(&root);
        let malformed = hedging_limit(
            hedges,
            Band::Count {
                minimum: Some(5),
                maximum: Some(1),
            },
        );
        let stored = publish(
            &store,
            &[
                hedging_limit(hedges, at_most(1)),
                malformed,
                dash_ban(dashes),
            ],
        );

        let (ruleset, lines) = capture_logs(|| LoadedRuleset::load_blocking(&store));

        let ruleset = ruleset.unwrap();
        let warnings = warnings(&lines);
        assert_no_clause_words(&warnings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains(&format!("checker {} ", stored.checkers[1].id.0)),
            "{}",
            warnings[0]
        );
        assert!(warnings[0].contains("does not compile"), "{}", warnings[0]);
        let loaded: Vec<CheckerId> = ruleset.checkers().iter().map(LoadedChecker::id).collect();
        assert_eq!(loaded, [stored.checkers[0].id, stored.checkers[2].id]);
        assert_eq!(ruleset.version(), Some(stored.version));
    }

    #[test]
    fn a_checker_whose_clause_has_left_the_card_is_left_out_with_one_warning_naming_it() {
        let root = TempRoot::new();
        let (store, hedges, dashes) = store_with_card(&root);
        let stored = publish(
            &store,
            &[hedging_limit(hedges, at_most(1)), dash_ban(dashes)],
        );
        let card = store.latest_card_version_blocking().unwrap().unwrap();
        store
            .insert_card_version_blocking(
                &[CardMember::Stored(card.clauses[0].text_id)],
                "the dash ban dropped",
            )
            .unwrap();

        let (ruleset, lines) = capture_logs(|| LoadedRuleset::load_blocking(&store));

        let ruleset = ruleset.unwrap();
        let warnings = warnings(&lines);
        assert_no_clause_words(&warnings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains(&format!("checker {} ", stored.checkers[1].id.0)),
            "{}",
            warnings[0]
        );
        let loaded: Vec<CheckerId> = ruleset.checkers().iter().map(LoadedChecker::id).collect();
        assert_eq!(loaded, [stored.checkers[0].id]);
    }

    #[test]
    fn a_store_with_no_ruleset_version_or_no_store_gives_an_empty_ruleset_with_no_version() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let carded_root = TempRoot::new();
        let (carded, _, _) = store_with_card(&carded_root);

        for ruleset in [
            LoadedRuleset::load_blocking(&store).unwrap(),
            LoadedRuleset::load_blocking(&carded).unwrap(),
            LoadedRuleset::empty(),
        ] {
            assert_eq!(ruleset.version(), None);
            assert!(ruleset.checkers().is_empty());
        }
    }

    #[test]
    fn a_violation_becomes_a_style_flag_citing_its_clause_with_the_version_and_its_numbers() {
        let root = TempRoot::new();
        let (store, hedges, _) = store_with_card(&root);
        let stored = publish(&store, &[hedging_limit(hedges, at_most(1))]);
        let ruleset = LoadedRuleset::load_blocking(&store).unwrap();
        let checker = &ruleset.checkers()[0];
        let paragraph = ["Perhaps it rains, and perhaps it snows.", "Perhaps not."];

        let violations = checker
            .evaluator()
            .evaluate(&paragraph, FormMode::Prose, "default");
        let flags: Vec<Flag> = violations
            .iter()
            .map(|violation| ruleset.flag(violation.clone(), checker))
            .collect();

        assert_eq!(flags.len(), 2, "{flags:#?}");
        let content_hash = &stored.checkers[0].content_hash;
        for (flag, violation) in flags.iter().zip(&violations) {
            assert_eq!(
                flag.origin,
                FlagOrigin::Style(ClauseCitation {
                    clause_id: hedges,
                    rule_id: RuleId(content_hash.clone()),
                    clause_text: HEDGES_REWORDED.0.to_owned(),
                })
            );
            assert_eq!(flag.span, violation.span);
            assert_eq!(
                flag.message,
                "3 matches in this paragraph, over the maximum of 1"
            );
            assert_eq!(flag.ruleset_version, Some(stored.version));
            assert_eq!(flag.catalogue_hash, None);
            assert_eq!(&flag.source_detail, content_hash);
            assert!(flag.suggestions.is_empty());
        }
        assert_ne!(flags[0].id, flags[1].id);
        assert_eq!(
            flags[1].id,
            format!("style:{}:0:7", stored.checkers[0].id.0)
        );
    }

    #[test]
    fn each_breach_states_its_numbers_and_the_forbidden_band_adds_nothing_to_the_clause() {
        let phrases = |unit, band| rule(Matcher::PhraseList { entries: vec![] }, unit, band);
        let cases = [
            (phrases(Unit::Span, Band::Forbidden), Breach::Forbidden, ""),
            (
                rule(Matcher::WordCount, Unit::Sentence, at_most(30)),
                Breach::AboveMaximum {
                    count: 34,
                    maximum: 30,
                },
                "34 words in this sentence, over the maximum of 30",
            ),
            (
                phrases(Unit::Sentence, at_most(0)),
                Breach::AboveMaximum {
                    count: 1,
                    maximum: 0,
                },
                "1 match in this sentence, over the maximum of 0",
            ),
            (
                phrases(
                    Unit::Paragraph,
                    Band::Count {
                        minimum: Some(2),
                        maximum: None,
                    },
                ),
                Breach::BelowMinimum {
                    count: 1,
                    minimum: 2,
                    shortfall: 1,
                },
                "1 match in this paragraph, 1 short of the minimum of 2",
            ),
            (
                phrases(
                    Unit::Document,
                    Band::Rate {
                        maximum_per_hundred_words: 1.5,
                    },
                ),
                Breach::AboveRate {
                    rate_per_hundred_words: 8.0 / 3.0,
                    maximum_per_hundred_words: 1.5,
                },
                "2.7 matches per hundred words in this document, over the maximum of 1.5",
            ),
            (
                phrases(
                    Unit::Document,
                    Band::Rate {
                        maximum_per_hundred_words: 2.0,
                    },
                ),
                // Twenty-one in 1,049 words, which one decimal place would show as the limit.
                Breach::AboveRate {
                    rate_per_hundred_words: 2100.0 / 1049.0,
                    maximum_per_hundred_words: 2.0,
                },
                "2.002 matches per hundred words in this document, over the maximum of 2",
            ),
            (
                rule(
                    Matcher::WordCount,
                    Unit::Paragraph,
                    Band::Rate {
                        maximum_per_hundred_words: 0.5,
                    },
                ),
                Breach::AboveRate {
                    rate_per_hundred_words: 1.0,
                    maximum_per_hundred_words: 0.5,
                },
                "1 word per hundred words in this paragraph, over the maximum of 0.5",
            ),
        ];
        for (broken, breach, expected) in cases {
            assert_eq!(message(&breach, &broken), expected, "{breach:?}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_awaited_load_behaves_as_the_blocking_one() {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let card = store
            .insert_card_version(&[CardMember::New(wording(HEDGES))], "the first Card")
            .await
            .unwrap();
        let hedges = store.card_version(card).await.unwrap().clauses[0].clause_id;
        let published = store
            .publish_ruleset(&[(hedging_limit(hedges, at_most(1)), templated())])
            .await
            .unwrap();

        let ruleset = LoadedRuleset::load(&store).await.unwrap();

        assert_eq!(ruleset.version(), Some(published.version));
        assert_eq!(ruleset.checkers().len(), 1);
        assert_eq!(ruleset.checkers()[0].citation().clause_text, HEDGES.0);
    }
}
