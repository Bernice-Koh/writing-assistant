//! The rule layer's three passes, which run spelling, the ruleset's checkers and the telltale
//! catalogue's patterns, each over the unit its rule is measured on and under one register. They
//! read their sources through [`Sources`], so the analyzer, over live text, and the screen, over a
//! rewrite candidate, call the same functions and no check is implemented twice.
//!
//! - The sentence pass runs spelling and then the patterns and the checkers of a span or a sentence
//!   unit on one sentence.
//! - The paragraph pass runs the patterns and the checkers of a paragraph unit over a paragraph's
//!   sentences.
//! - The document pass runs those of a document unit over the whole document's sentences.
//!
//! Each sentence goes to the evaluator with its paragraph's form mode, and a rule runs over the
//! sentences whose mode it declares. No rule declares code, and spelling has no notion of mode, so
//! the sentence pass leaves a sentence of code out, and the paragraph pass a paragraph of it. The
//! document pass hands over every sentence, those of code among them, so that a violation's index
//! is its sentence's index in the document, and the evaluator ignores the code.
//!
//! The analyzer keeps its caches and its counts around the passes. The sentence pass's flags are
//! cached with the sentence's other parts. The paragraph pass's are cached under the paragraph's
//! key, so a paragraph whose text is unchanged is served from the cache wherever it has moved, and
//! one that holds a changed sentence has a new key and is evaluated. The document pass, a few
//! counts, runs on every recheck and caches nothing.

use super::form_mode::Paragraph;
use super::sentences::Sentence;
use super::{cache_key, Inner};
use crate::flag::Flag;
use crate::spelling::SpellChecker;
use crate::style::ai_tell::Catalogue;
use crate::style::checker::{FormMode, Unit};
use crate::style::evaluator::ModeSentence;
use crate::style::ruleset::LoadedRuleset;

/// What the passes read: the spelling checker, the telltale catalogue, the ruleset and the
/// register every unit is evaluated under. The analyzer lends its own handles, and the screen its
/// own.
pub(crate) struct Sources<'s> {
    pub(crate) spelling: &'s SpellChecker,
    /// The text whose words spelling accepts beside the dictionary's, by
    /// [`SpellChecker::check_against_source`]: the passage a candidate rewrites, for the screen.
    /// `None` for the analyzer, since typed text rewrites nothing.
    pub(crate) spelling_source: Option<&'s str>,
    pub(crate) catalogue: &'s Catalogue,
    pub(crate) ruleset: &'s LoadedRuleset,
    pub(crate) register: &'s str,
}

/// A flag of the paragraph pass or the document pass, with the index of the sentence it anchors on
/// among the sentences the pass evaluated: within the paragraph for the paragraph pass, and within
/// the document for the document pass.
#[derive(Debug, Clone)]
pub(crate) struct UnitFlag {
    pub(crate) sentence: usize,
    pub(crate) flag: Flag,
}

/// The sentence pass over `sentence`, a sentence of a paragraph of `mode`: spelling's flags, then
/// those of the catalogue's patterns of a span or a sentence unit in catalogue order, then those of
/// the ruleset's checkers of those units in ascending order of checker id. A sentence of code has
/// none.
pub(crate) fn sentence_pass(sources: &Sources<'_>, sentence: &str, mode: FormMode) -> Vec<Flag> {
    if mode == FormMode::Code {
        return Vec::new();
    }
    let mut flags = match sources.spelling_source {
        Some(source) => sources.spelling.check_against_source(sentence, source),
        None => sources.spelling.check(sentence),
    };
    flags.extend(sources.catalogue.flags(sentence, mode, sources.register));
    let unit = [ModeSentence {
        text: sentence,
        mode,
    }];
    let ruleset = sources.ruleset;
    flags.extend(
        ruleset
            .checkers()
            .iter()
            .filter(|checker| checker.evaluator().rule().unit.is_within_a_sentence())
            .flat_map(|checker| {
                checker
                    .evaluator()
                    .evaluate(&unit, sources.register)
                    .into_iter()
                    .map(move |violation| ruleset.flag(violation, checker))
            }),
    );
    flags
}

/// The paragraph pass over `paragraph`, whose sentences are `sentences`: the flags of the
/// catalogue's patterns and the ruleset's checkers of a paragraph unit, as [`unit_pass`] orders
/// them. A paragraph of code has none.
pub(crate) fn paragraph_pass(
    sources: &Sources<'_>,
    paragraph: &Paragraph<'_>,
    sentences: &[Sentence<'_>],
) -> Vec<UnitFlag> {
    if paragraph.mode == FormMode::Code {
        return Vec::new();
    }
    let sentences: Vec<ModeSentence<'_>> = sentences
        .iter()
        .map(|sentence| ModeSentence {
            text: sentence.text,
            mode: paragraph.mode,
        })
        .collect();
    unit_pass(sources, Unit::Paragraph, &sentences)
}

/// The document pass over `sentences`, every sentence of the document with its paragraph's mode:
/// the flags of the catalogue's patterns and the ruleset's checkers of a document unit, as
/// [`unit_pass`] orders them.
pub(crate) fn document_pass(
    sources: &Sources<'_>,
    sentences: &[ModeSentence<'_>],
) -> Vec<UnitFlag> {
    unit_pass(sources, Unit::Document, sentences)
}

/// The flags of the catalogue's patterns and then the ruleset's checkers whose unit is `unit` over
/// `sentences` as one unit: the catalogue's in catalogue order, and the ruleset's in ascending order
/// of checker id.
fn unit_pass(sources: &Sources<'_>, unit: Unit, sentences: &[ModeSentence<'_>]) -> Vec<UnitFlag> {
    let register = sources.register;
    let telltales = sources
        .catalogue
        .patterns_of_unit(unit)
        .flat_map(|pattern| {
            pattern
                .evaluator()
                .evaluate(sentences, register)
                .into_iter()
                .map(move |violation| UnitFlag {
                    sentence: violation.sentence,
                    flag: pattern.flag(violation),
                })
        });
    let ruleset = sources.ruleset;
    let style = ruleset
        .checkers()
        .iter()
        .filter(|checker| checker.evaluator().rule().unit == unit)
        .flat_map(|checker| {
            checker
                .evaluator()
                .evaluate(sentences, register)
                .into_iter()
                .map(move |violation| UnitFlag {
                    sentence: violation.sentence,
                    flag: ruleset.flag(violation, checker),
                })
        });
    telltales.chain(style).collect()
}

/// The analyzer's paragraph pass under `register` for `paragraph`, whose sentences are
/// `sentences`: from the paragraph cache where it holds an entry for the paragraph under the
/// register, and otherwise evaluated by [`paragraph_pass`] and cached. A paragraph of code has no
/// flags and takes no entry.
pub(super) fn cached_paragraph_pass(
    inner: &Inner,
    register: &str,
    paragraph: &Paragraph<'_>,
    sentences: &[Sentence<'_>],
) -> Vec<UnitFlag> {
    if paragraph.mode == FormMode::Code {
        return Vec::new();
    }
    let key = cache_key(
        inner.ruleset.version(),
        register,
        paragraph.mode,
        paragraph.text,
    );
    let cached = inner
        .paragraph_cache
        .lock()
        .expect("a poisoned lock here means another thread already panicked")
        .get(&key)
        .cloned();
    if let Some(flags) = cached {
        return flags;
    }

    #[cfg(test)]
    inner
        .paragraph_checks
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let flags = paragraph_pass(&inner.sources(register), paragraph, sentences);
    inner
        .paragraph_cache
        .lock()
        .expect("a poisoned lock here means another thread already panicked")
        .put(key, flags.clone());
    flags
}

/// The analyzer's document pass under `register` over `sentences`, every sentence of the document
/// with its paragraph's mode, evaluated afresh by [`document_pass`].
pub(super) fn counted_document_pass(
    inner: &Inner,
    register: &str,
    sentences: &[ModeSentence<'_>],
) -> Vec<UnitFlag> {
    #[cfg(test)]
    inner
        .document_checks
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    document_pass(&inner.sources(register), sentences)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::super::tests::{
        cached_parts, default_catalogue, flagged_text, pinned_flags, published_flags, step,
        surface_of, FakeCapture, PINNED_DOCUMENT,
    };
    use super::super::{Analyzer, SentenceParts};
    use super::*;
    use crate::capture::{Capture, Surface};
    use crate::flag::{ClauseId, FlagOrigin, RulesetVersion};
    use crate::languagetool::GrammarSource;
    use crate::screen::{Finding, Screen};
    use crate::store::Config;
    use crate::style::ai_tell::Catalogue;
    use crate::style::checker::{Band, CheckableMode, Checker, Matcher, PhraseEntry, Rule};
    use crate::style::ruleset::LoadedRuleset;
    use crate::test_support::{capture_logs, test_spell_checker, FakeGrammar};

    /// The text of the clause every fixture checker serves, written for these tests.
    const CLAUSE: &str = "Hedge rarely.";

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

    fn phrase(phrase: &str) -> Matcher {
        Matcher::PhraseList {
            entries: vec![PhraseEntry {
                phrase: phrase.to_owned(),
                prefix: false,
            }],
        }
    }

    fn regex(source: &str) -> Matcher {
        Matcher::Regex {
            source: source.to_owned(),
        }
    }

    fn at_most(maximum: u32) -> Band {
        Band::Count {
            minimum: None,
            maximum: Some(maximum),
        }
    }

    /// A ruleset at `version` holding `checkers`, with ids from 1 in the order given.
    fn ruleset_at(version: i64, checkers: Vec<Checker>) -> LoadedRuleset {
        LoadedRuleset::of_checkers(
            RulesetVersion(version),
            checkers
                .into_iter()
                .map(|checker| (checker, CLAUSE))
                .collect(),
        )
    }

    /// An analyzer of `fake` under settings whose default register is `default_register` and
    /// whose register map is empty, so that every surface is checked under it, with `grammar` as
    /// its grammar source, `ruleset`, and `catalogue`.
    fn started(
        fake: &Arc<FakeCapture>,
        grammar: Option<&Arc<FakeGrammar>>,
        ruleset: LoadedRuleset,
        catalogue: Catalogue,
        default_register: &str,
    ) -> Analyzer {
        let settings = Config {
            default_register: default_register.to_owned(),
            ..Config::default()
        };
        started_with_settings(fake, grammar, ruleset, catalogue, settings)
    }

    fn started_with_settings(
        fake: &Arc<FakeCapture>,
        grammar: Option<&Arc<FakeGrammar>>,
        ruleset: LoadedRuleset,
        catalogue: Catalogue,
        settings: Config,
    ) -> Analyzer {
        Analyzer::start(
            Arc::clone(fake) as Arc<dyn Capture>,
            test_spell_checker(),
            grammar.map(|grammar| Arc::clone(grammar) as Arc<dyn GrammarSource>),
            Arc::new(ruleset),
            Arc::new(catalogue),
            Arc::new(settings),
        )
    }

    /// An analyzer of `fake` with `checkers` at ruleset version 1 and the default catalogue,
    /// under the default register.
    fn analyzer(
        fake: &Arc<FakeCapture>,
        grammar: Option<&Arc<FakeGrammar>>,
        checkers: Vec<Checker>,
    ) -> Analyzer {
        started(
            fake,
            grammar,
            ruleset_at(1, checkers),
            default_catalogue(),
            "default",
        )
    }

    /// The id and the flagged text of each style flag of `flags`, in order.
    fn style_flags(flags: &[Flag]) -> Vec<(String, String)> {
        flags
            .iter()
            .filter(|flag| matches!(flag.origin, FlagOrigin::Style { .. }))
            .map(|flag| (flag.id.clone(), flagged_text(flag)))
            .collect()
    }

    /// `pairs` as owned strings, to compare with [`style_flags`].
    fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(id, text)| ((*id).to_owned(), (*text).to_owned()))
            .collect()
    }

    /// The style flag id of checker `checker` on the sentence of index `sentence`, on `found` in
    /// `text`, that sentence's text.
    fn style_id(sentence: usize, checker: i64, text: &str, found: &str) -> String {
        let start = text.find(found).unwrap();
        format!("{sentence}:style:{checker}:{start}:{}", found.len())
    }

    #[tokio::test(start_paused = true)]
    async fn span_and_sentence_unit_flags_are_in_the_first_update_and_the_sentences_cached_part() {
        let sentence = "The ferry left the harbour at noon.";
        let fake = Arc::new(FakeCapture::new(sentence));
        let grammar = FakeGrammar::flagging(&[]);
        grammar.hold();
        let analyzer = analyzer(
            &fake,
            Some(&grammar),
            vec![
                checker(regex("harbour"), Unit::Span, Band::Forbidden),
                // Seven words, and the two beyond the fifth are one flag.
                checker(Matcher::WordCount, Unit::Sentence, at_most(5)),
            ],
        );
        step(6).await;

        let updates = analyzer.updates();
        assert_eq!(updates.len(), 1, "{updates:#?}");
        let expected = [
            (style_id(0, 1, sentence, "harbour"), "harbour".to_owned()),
            (style_id(0, 2, sentence, "at noon"), "at noon".to_owned()),
        ];
        assert_eq!(style_flags(&updates[0]), expected);
        let cached = cached_parts(&analyzer, FormMode::Prose, sentence).unwrap();
        let unscoped: Vec<(String, String)> = expected
            .iter()
            .map(|(id, text)| (id["0:".len()..].to_owned(), text.clone()))
            .collect();
        assert_eq!(style_flags(&cached.deterministic), unscoped);

        // Unchanged, the sentence is served from the cache with its style flags.
        fake.set_text(&format!("{sentence} It rained."));
        step(6).await;
        assert_eq!(analyzer.deterministic_check_count(), 2);
        assert_eq!(style_flags(&published_flags(&analyzer)), expected);
    }

    #[tokio::test(start_paused = true)]
    async fn a_paragraph_unit_checker_evaluates_the_changed_paragraph_and_serves_the_rest() {
        let first = "Perhaps it rains. Perhaps it snows.";
        let third = "Perhaps the wind drops. Perhaps it clears.";
        let document = |second: &str| format!("{first}\n{second}\n{third}");
        let fake = Arc::new(FakeCapture::new(&document(
            "The ferry waits. Perhaps the tide turns.",
        )));
        let at_most_one = checker(phrase("perhaps"), Unit::Paragraph, at_most(1));
        let analyzer = analyzer(&fake, None, vec![at_most_one]);
        step(6).await;
        assert_eq!(analyzer.paragraph_check_count(), 3);
        let first_and_third = owned(&[("1:style:1:0:7", "Perhaps"), ("5:style:1:0:7", "Perhaps")]);
        assert_eq!(style_flags(&published_flags(&analyzer)), first_and_third);

        // One sentence of the second paragraph changes, and the paragraph still holds one hedge.
        fake.set_text(&document("The ferry is late. Perhaps the tide turns."));
        step(6).await;
        assert_eq!(analyzer.paragraph_check_count(), 4);
        assert_eq!(analyzer.deterministic_check_count(), 7);
        assert_eq!(style_flags(&published_flags(&analyzer)), first_and_third);

        // Now it holds two, and only it is evaluated again.
        fake.set_text(&document(
            "Perhaps the ferry is late. Perhaps the tide turns.",
        ));
        step(6).await;
        assert_eq!(analyzer.paragraph_check_count(), 5);
        assert_eq!(
            style_flags(&published_flags(&analyzer)),
            owned(&[
                ("1:style:1:0:7", "Perhaps"),
                ("3:style:1:0:7", "Perhaps"),
                ("5:style:1:0:7", "Perhaps"),
            ])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unchanged_paragraph_that_moved_is_served_from_the_cache() {
        let hedged = "Perhaps it rains. Perhaps it snows.";
        let fake = Arc::new(FakeCapture::new(&format!("{hedged}\nThe ferry waits.")));
        let at_most_one = checker(phrase("perhaps"), Unit::Paragraph, at_most(1));
        let analyzer = analyzer(&fake, None, vec![at_most_one]);
        step(6).await;
        assert_eq!(analyzer.paragraph_check_count(), 2);
        assert_eq!(
            style_flags(&published_flags(&analyzer)),
            owned(&[("1:style:1:0:7", "Perhaps")])
        );

        fake.set_text(&format!("A new opening line.\n{hedged}\nThe ferry waits."));
        step(6).await;
        assert_eq!(analyzer.paragraph_check_count(), 3);
        // Scoped by the sentence's new place in the document.
        assert_eq!(
            style_flags(&published_flags(&analyzer)),
            owned(&[("2:style:1:0:7", "Perhaps")])
        );
        let key = cache_key(Some(RulesetVersion(1)), "default", FormMode::Prose, hedged);
        let cached = analyzer
            .inner
            .paragraph_cache
            .lock()
            .unwrap()
            .peek(&key)
            .cloned()
            .unwrap();
        // Cached with its sentence's index within the paragraph, which did not move.
        assert_eq!(
            cached
                .iter()
                .map(|unit_flag| unit_flag.sentence)
                .collect::<Vec<_>>(),
            [1]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_document_unit_checker_is_evaluated_over_the_whole_document_on_every_recheck() {
        let opening = "Perhaps it rains.\nPerhaps it snows.";
        let fake = Arc::new(FakeCapture::new(&format!("{opening}\nThe ferry waits.")));
        let at_most_one = checker(phrase("perhaps"), Unit::Document, at_most(1));
        let analyzer = analyzer(&fake, None, vec![at_most_one]);
        step(6).await;
        assert_eq!(analyzer.document_check_count(), 1);
        let one_beyond = owned(&[("1:style:1:0:7", "Perhaps")]);
        let flags = published_flags(&analyzer);
        assert_eq!(style_flags(&flags), one_beyond);
        assert!(
            flags
                .iter()
                .any(|flag| flag.message == "2 matches in this document, over the maximum of 1"),
            "{flags:#?}"
        );

        // A change to the last paragraph alone, which holds no hedge.
        fake.set_text(&format!("{opening}\nThe ferry is late."));
        step(6).await;
        assert_eq!(analyzer.document_check_count(), 2);
        assert_eq!(analyzer.deterministic_check_count(), 4);
        assert_eq!(style_flags(&published_flags(&analyzer)), one_beyond);

        // A hedge added there is counted with the two in the cached sentences.
        let last = "The ferry is late, perhaps.";
        fake.set_text(&format!("{opening}\n{last}"));
        step(6).await;
        assert_eq!(analyzer.document_check_count(), 3);
        let flags = published_flags(&analyzer);
        assert_eq!(
            style_flags(&flags),
            [
                ("1:style:1:0:7".to_owned(), "Perhaps".to_owned()),
                (style_id(2, 1, last, "perhaps"), "perhaps".to_owned()),
            ]
        );
        assert!(
            flags
                .iter()
                .filter(|flag| matches!(flag.origin, FlagOrigin::Style { .. }))
                .all(|flag| flag.message == "3 matches in this document, over the maximum of 1"),
            "{flags:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn paragraph_and_document_flags_are_in_the_first_update_on_their_sentence_and_occurrence()
    {
        let repeated = "Perhaps it rains.";
        let fake = Arc::new(FakeCapture::new(&format!(
            "{repeated} The ferry waits.\n{repeated} {repeated}"
        )));
        let grammar = FakeGrammar::flagging(&[]);
        grammar.hold();
        let analyzer = analyzer(
            &fake,
            Some(&grammar),
            vec![
                checker(phrase("perhaps"), Unit::Paragraph, at_most(1)),
                checker(phrase("rains"), Unit::Document, at_most(2)),
            ],
        );
        step(6).await;

        let updates = analyzer.updates();
        assert_eq!(updates.len(), 1, "{updates:#?}");
        let first = &updates[0];
        assert_eq!(
            style_flags(first),
            [
                (style_id(3, 1, repeated, "Perhaps"), "Perhaps".to_owned()),
                (style_id(3, 2, repeated, "rains"), "rains".to_owned()),
            ]
        );
        for flag in first
            .iter()
            .filter(|flag| matches!(flag.origin, FlagOrigin::Style { .. }))
        {
            assert_eq!(flag.span.anchor, repeated, "{flag:#?}");
            // Two earlier sentences hold the same text.
            assert_eq!(flag.span.occurrence, 2, "{flag:#?}");
            assert_eq!(flag.ruleset_version, Some(RulesetVersion(1)));
            assert_eq!(
                flag.catalogue_hash.as_ref(),
                Some(default_catalogue().hash())
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_paragraph_or_document_flag_that_a_hard_flag_overlaps_is_suppressed() {
        let misspelled = "This has a mispelling.";
        let rains = "Perhaps it rains.";
        let fake = Arc::new(FakeCapture::new(&format!("{misspelled}\n{rains}")));
        let grammar = FakeGrammar::flagging(&["rains"]);
        grammar.hold();
        let both = Matcher::PhraseList {
            entries: ["mispelling", "rains"]
                .map(|phrase| PhraseEntry {
                    phrase: phrase.to_owned(),
                    prefix: false,
                })
                .to_vec(),
        };
        let analyzer = analyzer(
            &fake,
            Some(&grammar),
            vec![
                checker(both, Unit::Paragraph, Band::Forbidden),
                checker(regex("mispelling|rains"), Unit::Document, Band::Forbidden),
            ],
        );
        step(6).await;

        // The spelling flag suppresses both passes' flags on the misspelling from the first
        // update, and the paragraph pass did raise its own.
        let first = analyzer.updates()[0].clone();
        assert_eq!(
            style_flags(&first),
            [
                (style_id(1, 1, rains, "rains"), "rains".to_owned()),
                (style_id(1, 2, rains, "rains"), "rains".to_owned()),
            ]
        );
        let key = cache_key(
            Some(RulesetVersion(1)),
            "default",
            FormMode::Prose,
            misspelled,
        );
        let cached = analyzer
            .inner
            .paragraph_cache
            .lock()
            .unwrap()
            .peek(&key)
            .cloned()
            .unwrap();
        assert_eq!(
            cached
                .iter()
                .map(|unit_flag| flagged_text(&unit_flag.flag))
                .collect::<Vec<_>>(),
            ["mispelling"]
        );

        // The grammar flag on "rains" suppresses both passes' flags there from the second.
        grammar.release();
        step(1).await;
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        assert!(style_flags(&updates[1]).is_empty(), "{:#?}", updates[1]);
        let hard: Vec<String> = updates[1].iter().map(flagged_text).collect();
        assert_eq!(hard, ["mispelling", "rains"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_checker_runs_only_in_the_modes_it_declares_and_nothing_is_evaluated_in_code() {
        let text =
            "# Perhaps notes\nPerhaps it rains.\n```\nPerhaps it compiles.\n```\n- Perhaps a \
                    list item";
        let fake = Arc::new(FakeCapture::new(text));
        let mut prose_only = checker(phrase("perhaps"), Unit::Paragraph, Band::Forbidden);
        prose_only.rule.form_modes = BTreeSet::from([CheckableMode::Prose]);
        let analyzer = analyzer(
            &fake,
            None,
            vec![
                prose_only,
                checker(phrase("perhaps"), Unit::Document, Band::Forbidden),
                checker(phrase("perhaps"), Unit::Paragraph, Band::Forbidden),
            ],
        );
        step(6).await;

        // The heading, the paragraph of prose and the list item; none of the three lines of code.
        assert_eq!(analyzer.paragraph_check_count(), 3);
        let flags = published_flags(&analyzer);
        let ids: Vec<(String, String)> = style_flags(&flags);
        // By sentence, and within one the paragraph pass's flags before the document pass's. The
        // first checker, prose alone, flags the paragraph of prose alone. A fence line holds no
        // letter or digit and so no sentence, which makes the code's one sentence the third and
        // the list item's the fourth.
        assert_eq!(
            ids,
            owned(&[
                ("0:style:3:2:7", "Perhaps"),
                ("0:style:2:2:7", "Perhaps"),
                ("1:style:1:0:7", "Perhaps"),
                ("1:style:3:0:7", "Perhaps"),
                ("1:style:2:0:7", "Perhaps"),
                ("3:style:3:2:7", "Perhaps"),
                ("3:style:2:2:7", "Perhaps"),
            ])
        );
        assert!(
            flags
                .iter()
                .all(|flag| !flag.span.anchor.contains("compiles")),
            "{flags:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_checker_with_a_register_scope_is_evaluated_under_the_default_register() {
        let fake = Arc::new(FakeCapture::new("Perhaps it rains."));
        let mut checkers = Vec::new();
        for unit in [Unit::Sentence, Unit::Paragraph, Unit::Document] {
            for register in ["email", "chat"] {
                let mut scoped = checker(phrase("perhaps"), unit, Band::Forbidden);
                scoped.rule.register_scope = Some(BTreeSet::from([register.to_owned()]));
                checkers.push(scoped);
            }
        }
        // The settings' default register, which a surface the register map has no key for is
        // checked under.
        let analyzer = started(
            &fake,
            None,
            ruleset_at(1, checkers),
            default_catalogue(),
            "email",
        );
        step(6).await;

        assert_eq!(
            style_flags(&published_flags(&analyzer)),
            owned(&[
                ("0:style:1:0:7", "Perhaps"),
                ("0:style:3:0:7", "Perhaps"),
                ("0:style:5:0:7", "Perhaps"),
            ])
        );
    }

    /// Three checkers that forbid "perhaps" in `register` alone, with ids 1 to 3 by unit: a
    /// sentence, a paragraph and the document.
    fn scoped_to(register: &str) -> Vec<Checker> {
        [Unit::Sentence, Unit::Paragraph, Unit::Document]
            .map(|unit| {
                let mut scoped = checker(phrase("perhaps"), unit, Band::Forbidden);
                scoped.rule.register_scope = Some(BTreeSet::from([register.to_owned()]));
                scoped
            })
            .to_vec()
    }

    /// The style flags [`scoped_to`]'s three checkers raise in their register on a first sentence
    /// that opens with "Perhaps".
    fn perhaps_flagged() -> Vec<(String, String)> {
        owned(&[
            ("0:style:1:0:7", "Perhaps"),
            ("0:style:2:0:7", "Perhaps"),
            ("0:style:3:0:7", "Perhaps"),
        ])
    }

    /// Settings that map Word, and Edge's pages on `mail.example.com`, to `email`, and Edge's
    /// other pages to `chat`, with `default` as the default register.
    fn surface_settings() -> Config {
        let mut settings = Config::default();
        for (key, register) in [
            ("WINWORD.EXE", "email"),
            ("msedge.exe", "chat"),
            ("msedge.exe mail.example.com", "email"),
        ] {
            settings
                .register_map
                .insert(key.to_owned(), register.to_owned());
        }
        settings
    }

    /// An analyzer of `fake` with [`scoped_to`]`("email")`'s checkers under [`surface_settings`].
    fn email_scoped(fake: &Arc<FakeCapture>) -> Analyzer {
        started_with_settings(
            fake,
            None,
            ruleset_at(1, scoped_to("email")),
            default_catalogue(),
            surface_settings(),
        )
    }

    fn page(url: &str) -> Surface {
        Surface {
            page_url: Some(url.to_owned()),
            document_key: Some(url.to_owned()),
            ..surface_of("msedge.exe")
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_register_follows_the_surface_at_the_next_recheck_and_no_entry_crosses_registers() {
        let fake = Arc::new(FakeCapture::new("Perhaps it rains."));
        let analyzer = email_scoped(&fake);
        step(6).await;
        assert!(style_flags(&published_flags(&analyzer)).is_empty());
        assert_eq!(fake.surface_reads(), 1, "one surface read per recheck");

        // Focus moves to Word, which the map gives `email`, onto a document whose first sentence
        // and paragraph the cache holds under the default register.
        fake.focus(
            Some(surface_of("WINWORD.EXE")),
            "Perhaps it rains.\nThe ferry waits.",
        );
        step(6).await;
        assert_eq!(style_flags(&published_flags(&analyzer)), perhaps_flagged());
        assert_eq!(analyzer.deterministic_check_count(), 3);
        assert_eq!(analyzer.paragraph_check_count(), 3);
        assert_eq!(fake.surface_reads(), 2);

        // And back, served from the default register's entries.
        fake.focus(Some(surface_of("fake.exe")), "Perhaps it rains.");
        step(6).await;
        assert!(style_flags(&published_flags(&analyzer)).is_empty());
        assert_eq!(analyzer.deterministic_check_count(), 3);
        assert_eq!(analyzer.paragraph_check_count(), 3);
        assert_eq!(analyzer.updates().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn one_text_on_two_surfaces_of_different_registers_is_rechecked_under_each() {
        let text = "Perhaps it rains.";
        let fake = Arc::new(FakeCapture::new(text));
        let analyzer = email_scoped(&fake);
        step(6).await;
        assert!(style_flags(&published_flags(&analyzer)).is_empty());

        fake.focus(Some(surface_of("WINWORD.EXE")), text);
        step(6).await;
        assert_eq!(analyzer.updates().len(), 2);
        assert_eq!(style_flags(&published_flags(&analyzer)), perhaps_flagged());
        assert_eq!(analyzer.deterministic_check_count(), 2);

        // A change of formatting alone, on the same surface, rechecks nothing.
        fake.raise_event();
        step(6).await;
        assert_eq!(analyzer.updates().len(), 2);
        assert_eq!(fake.surface_reads(), 3);

        fake.focus(Some(surface_of("fake.exe")), text);
        step(6).await;
        assert_eq!(analyzer.updates().len(), 3);
        assert!(style_flags(&published_flags(&analyzer)).is_empty());
        assert_eq!(analyzer.deterministic_check_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_page_on_a_mapped_host_takes_its_hosts_register_over_its_processs() {
        let fake = Arc::new(FakeCapture::on(
            Some(page("https://mail.example.com/compose?to=team#draft")),
            "Perhaps it rains.",
        ));
        let analyzer = email_scoped(&fake);
        step(6).await;
        assert_eq!(style_flags(&published_flags(&analyzer)), perhaps_flagged());

        // Another host in the same process, and a local file, take the process's register.
        for url in ["https://example.com/notes", "file:///C:/pages/notes.html"] {
            fake.focus(Some(page(url)), "Perhaps it snows.");
            step(6).await;
            assert!(style_flags(&published_flags(&analyzer)).is_empty(), "{url}");
            fake.focus(Some(page("https://mail.example.com/")), "Perhaps it rains.");
            step(6).await;
            assert_eq!(
                style_flags(&published_flags(&analyzer)),
                perhaps_flagged(),
                "{url}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_surface_the_backend_cannot_name_is_checked_under_the_default_register() {
        // The web backend answers the surface as unsupported.
        let fake = Arc::new(FakeCapture::on(None, "Perhaps it rains."));
        let settings = Config {
            default_register: "email".to_owned(),
            ..surface_settings()
        };
        let analyzer = started_with_settings(
            &fake,
            None,
            ruleset_at(1, scoped_to("email")),
            default_catalogue(),
            settings,
        );
        step(6).await;
        assert_eq!(fake.surface_reads(), 1);
        assert_eq!(style_flags(&published_flags(&analyzer)), perhaps_flagged());
    }

    #[test]
    fn the_recheck_log_names_the_register_the_process_and_the_host_and_no_more_of_url_or_text() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let url = "https://mail.example.com/pathpart/compose?to=querypart#fragmentpart";
        let ((), lines) = capture_logs(|| {
            runtime.block_on(async {
                let fake = Arc::new(FakeCapture::on(Some(page(url)), "Perhaps it rains."));
                let _analyzer = email_scoped(&fake);
                step(6).await;
                fake.focus(None, "Perhaps it snows.");
                step(6).await;
            });
        });

        let rechecks: Vec<&String> = lines
            .iter()
            .map(|(_, line)| line)
            .filter(|line| line.starts_with("recheck ") && line.contains("under the register"))
            .collect();
        assert_eq!(rechecks.len(), 2, "{lines:#?}");
        for named in [
            "register \"email\"",
            "\"msedge.exe\"",
            "\"mail.example.com\"",
        ] {
            assert!(rechecks[0].contains(named), "{named} in {}", rechecks[0]);
        }
        for named in ["register \"default\"", "does not support"] {
            assert!(rechecks[1].contains(named), "{named} in {}", rechecks[1]);
        }
        for (_, line) in &lines {
            for hidden in [
                "https",
                "pathpart",
                "compose",
                "querypart",
                "fragmentpart",
                "Perhaps",
                "rains",
                "snows",
            ] {
                assert!(!line.contains(hidden), "{hidden} in {line}");
            }
        }
    }

    /// Copies every entry of `from`'s sentence cache and paragraph cache into `to`'s, under the
    /// same keys.
    fn transplant_caches(from: &Analyzer, to: &Analyzer) {
        let sentences: Vec<(u64, SentenceParts)> = from
            .inner
            .cache
            .lock()
            .unwrap()
            .iter()
            .map(|(key, parts)| (*key, parts.clone()))
            .collect();
        let paragraphs: Vec<(u64, Vec<UnitFlag>)> = from
            .inner
            .paragraph_cache
            .lock()
            .unwrap()
            .iter()
            .map(|(key, flags)| (*key, flags.clone()))
            .collect();
        let mut cache = to.inner.cache.lock().unwrap();
        for (key, parts) in sentences {
            cache.put(key, parts);
        }
        let mut paragraph_cache = to.inner.paragraph_cache.lock().unwrap();
        for (key, flags) in paragraphs {
            paragraph_cache.put(key, flags);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_cache_entry_made_under_one_ruleset_version_is_not_served_under_another() {
        let text = "Perhaps it rains.";
        let first_fake = Arc::new(FakeCapture::new(text));
        let first = started(
            &first_fake,
            None,
            ruleset_at(
                1,
                vec![
                    checker(phrase("rains"), Unit::Span, Band::Forbidden),
                    checker(phrase("perhaps"), Unit::Paragraph, Band::Forbidden),
                ],
            ),
            default_catalogue(),
            "default",
        );
        step(6).await;
        let under_first = owned(&[("0:style:1:11:5", "rains"), ("0:style:2:0:7", "Perhaps")]);
        assert_eq!(style_flags(&published_flags(&first)), under_first);

        // Version 2's analyzer holds every entry version 1's made, and checks the text afresh.
        let second_fake = Arc::new(FakeCapture::new(text));
        let second = started(
            &second_fake,
            None,
            ruleset_at(2, vec![checker(phrase("it"), Unit::Span, Band::Forbidden)]),
            default_catalogue(),
            "default",
        );
        transplant_caches(&first, &second);
        step(6).await;
        assert_eq!(second.deterministic_check_count(), 1);
        assert_eq!(second.paragraph_check_count(), 1);
        assert_eq!(
            style_flags(&published_flags(&second)),
            owned(&[("0:style:1:8:2", "it")])
        );

        // The same entries in an analyzer at version 1 are served, so the version in the key is
        // what keeps them from version 2.
        let control_fake = Arc::new(FakeCapture::new(text));
        let control = started(
            &control_fake,
            None,
            ruleset_at(1, Vec::new()),
            default_catalogue(),
            "default",
        );
        transplant_caches(&first, &control);
        step(6).await;
        assert_eq!(control.deterministic_check_count(), 0);
        assert_eq!(control.paragraph_check_count(), 0);
        assert_eq!(style_flags(&published_flags(&control)), under_first);
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_ruleset_adds_no_flag_and_the_published_flags_are_the_pinned_ones() {
        let fake = Arc::new(FakeCapture::new(PINNED_DOCUMENT));
        let grammar = FakeGrammar::flagging(&[]);
        let analyzer = started(
            &fake,
            Some(&grammar),
            ruleset_at(7, Vec::new()),
            default_catalogue(),
            "default",
        );
        step(6).await;

        // The passes ran: one paragraph, and the document once.
        assert_eq!(analyzer.paragraph_check_count(), 1);
        assert_eq!(analyzer.document_check_count(), 1);
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        assert_eq!(updates[1], pinned_flags());
    }

    /// A pattern of `unit` in the catalogue file's form, matching `phrase` under a maximum of one.
    fn pattern_at_most_one(id: &str, phrase: &str, unit: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "description": "A test pattern",
                "matcher": {{ "kind": "phraseList", "entries": [{{ "phrase": "{phrase}", "prefix": false }}] }},
                "unit": "{unit}",
                "band": {{ "kind": "count", "minimum": null, "maximum": 1 }},
                "formModes": ["prose"],
                "raw": false,
                "source": "a test"
            }}"#
        )
    }

    #[tokio::test(start_paused = true)]
    async fn the_catalogues_paragraph_and_document_patterns_ride_the_same_passes() {
        let catalogue = Catalogue::of_patterns(&format!(
            "[{}, {}]",
            pattern_at_most_one("paragraph-hedges", "perhaps", "paragraph"),
            pattern_at_most_one("document-rains", "rains", "document"),
        ));
        let repeated = "Perhaps it rains.";
        let fake = Arc::new(FakeCapture::new(&format!("{repeated} {repeated}")));
        let analyzer = started(&fake, None, LoadedRuleset::empty(), catalogue, "default");
        step(6).await;

        assert_eq!(analyzer.paragraph_check_count(), 1);
        assert_eq!(analyzer.document_check_count(), 1);
        let flags = published_flags(&analyzer);
        let telltales: Vec<(String, String)> = flags
            .iter()
            .filter(|flag| matches!(flag.origin, FlagOrigin::AiTell { .. }))
            .map(|flag| (flag.id.clone(), flagged_text(flag)))
            .collect();
        assert_eq!(
            telltales,
            owned(&[
                ("1:ai-tell:paragraph-hedges:0:7", "Perhaps"),
                ("1:ai-tell:document-rains:11:5", "rains"),
            ])
        );
        assert!(
            flags.iter().all(|flag| flag.span.occurrence == 1),
            "{flags:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_screen_raises_the_flags_the_analyzer_publishes_through_the_same_passes() {
        // No two flags overlap, so the analyzer's suppression publishes every flag as raised.
        let text =
            "This has a mispelling. Perhaps it rains. Perhaps it hails.\nHe go home. Perhaps it \
                    snows.";
        let checkers = || {
            vec![
                checker(phrase("rains"), Unit::Span, Band::Forbidden),
                checker(phrase("perhaps"), Unit::Paragraph, at_most(1)),
                checker(phrase("perhaps"), Unit::Document, at_most(2)),
            ]
        };
        let catalogue = || {
            Catalogue::of_patterns(&format!(
                "[{}]",
                pattern_at_most_one("document-its", "it", "document")
            ))
        };
        let grammar = FakeGrammar::flagging(&["go"]);
        let fake = Arc::new(FakeCapture::new(text));
        let analyzer = started(
            &fake,
            Some(&grammar),
            ruleset_at(1, checkers()),
            catalogue(),
            "default",
        );
        step(6).await;
        let mut published = published_flags(&analyzer);

        let screen = Screen::new(
            Arc::new(test_spell_checker()),
            Arc::clone(&grammar) as Arc<dyn GrammarSource>,
            Arc::new(ruleset_at(1, checkers())),
            Arc::new(catalogue()),
        );
        // A passage that holds none of the candidate's words.
        let screening = screen.run("", text, "default").await.unwrap();
        let mut screened: Vec<Flag> = screening
            .hard_results
            .iter()
            .chain(&screening.soft_results)
            .filter_map(|result| match result {
                Finding::Content { .. } => None,
                Finding::Spelling { flag }
                | Finding::Grammar { flag }
                | Finding::Style { flag }
                | Finding::Tells { flag } => Some(flag.clone()),
            })
            .collect();

        published.sort_by(|first, second| first.id.cmp(&second.id));
        screened.sort_by(|first, second| first.id.cmp(&second.id));
        assert_eq!(screened, published);
        for origin in ["spelling", "grammar", "style", "aiTell"] {
            assert!(
                published
                    .iter()
                    .any(|flag| { serde_json::to_value(flag).unwrap()["origin"] == origin }),
                "no {origin} flag: {published:#?}"
            );
        }
        let units: Vec<&str> = published
            .iter()
            .filter(|flag| matches!(flag.origin, FlagOrigin::Style { .. }))
            .map(|flag| flag.message.as_str())
            .collect();
        assert_eq!(
            units,
            [
                "",
                "2 matches in this paragraph, over the maximum of 1",
                "3 matches in this document, over the maximum of 2",
            ]
        );
    }
}
