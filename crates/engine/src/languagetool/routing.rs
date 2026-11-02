//! Turns LanguageTool's matches for one sentence into flags, given the loaded ruleset, the
//! sentence's form mode and its register. A match takes the first of these routes that holds:
//!
//! 1. A match in the spelling category is dropped, endorsed or not, since spelling has one source.
//! 2. A match whose rule id is the matcher of a checker of the ruleset is endorsed. It becomes a
//!    style flag citing the first such checker, in ascending order of id, that applies to the mode
//!    and the register, and is dropped when none applies.
//! 3. A match of a rule LanguageTool tags picky is dropped. Picky mode is on for the style rules
//!    the owner can endorse, and its rules in the hard categories, smart quotes among them, would
//!    otherwise surface as grammar flags the default level never raised.
//! 4. A match in a hard category becomes a grammar flag.
//! 5. Every other match, in a soft category or in one the route table does not hold, is dropped.
//!
//! LanguageTool counts a match's offset in UTF-16 code units from the start of the text it was
//! sent, which is one sentence, so each flag anchors on the match's sentence with that offset as
//! its local start.

use super::client::{category_route, CategoryRoute, Match};
use crate::flag::{Flag, FlagOrigin, RulesetVersion, Span};
use crate::style::checker::FormMode;
use crate::style::ruleset::{LoadedChecker, LoadedRuleset};

/// The flags `matches`, LanguageTool's for one sentence of `mode` under `register`, route to
/// through `ruleset`, in the order LanguageTool reported them.
pub(super) fn route(
    matches: Vec<Match>,
    ruleset: &LoadedRuleset,
    mode: FormMode,
    register: &str,
) -> Vec<Flag> {
    matches
        .into_iter()
        .enumerate()
        .filter_map(|(index, found)| routed(index, found, ruleset, mode, register))
        .collect()
}

fn routed(
    index: usize,
    found: Match,
    ruleset: &LoadedRuleset,
    mode: FormMode,
    register: &str,
) -> Option<Flag> {
    let category = category_route(&found.rule.category.id);
    if category == CategoryRoute::Spelling {
        return None;
    }
    let endorsing = ruleset.checkers_of_languagetool_rule(&found.rule.id);
    if !endorsing.is_empty() {
        let checker = endorsing
            .into_iter()
            .find(|checker| checker.applies_to(mode, register))?;
        return Some(style_flag(found, checker, ruleset.version()));
    }
    if category != CategoryRoute::Hard || found.rule.is_picky() {
        return None;
    }
    // Numbered by the match's place in LanguageTool's response, which no other match shares.
    Some(grammar_flag(
        format!("grammar:{index}:{}", found.rule.id),
        found,
    ))
}

/// `found` as a style flag citing `checker`'s clause, with LanguageTool's message and replacements,
/// recording `version`. Its id takes the shape of the rule layer's style flags, in which the checker
/// and the span tell two flags on one sentence apart.
fn style_flag(found: Match, checker: &LoadedChecker, version: Option<RulesetVersion>) -> Flag {
    let id = format!("style:{}:{}:{}", checker.id().0, found.offset, found.length);
    Flag {
        origin: FlagOrigin::Style(checker.citation().clone()),
        source_detail: checker.citation().rule_id.0.clone(),
        ruleset_version: version,
        ..grammar_flag(id, found)
    }
}

/// `found` as a grammar flag under `id`, recording no ruleset version: the analyzer records the
/// version in force on every flag it publishes.
fn grammar_flag(id: String, found: Match) -> Flag {
    Flag {
        id,
        origin: FlagOrigin::Grammar,
        span: Span {
            anchor: found.sentence,
            local_start: found.offset,
            local_length: found.length,
            occurrence: 0,
        },
        message: found.message,
        suggestions: found
            .replacements
            .into_iter()
            .map(|replacement| replacement.value)
            .collect(),
        source_detail: found.rule.id,
        ruleset_version: None,
        catalogue_hash: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::client::fixtures::*;
    use super::*;
    use crate::flag::{ClauseCitation, RuleId};
    use crate::store::{CardMember, CheckerAuthor, ClauseWording, Provenance, Store};
    use crate::style::checker::{Band, CheckableMode, Checker, Matcher, Rule, Unit};
    use crate::test_support::TempRoot;

    const ACTIVE_VOICE: &str = "Write in the active voice.";
    const PLAIN_EMAIL: &str = "Keep email plain.";

    /// A checker's rule endorsing LanguageTool's rule `rule_id`, as the questionnaire writes one:
    /// forbidden per sentence, in prose, under every register.
    fn endorsing(rule_id: &str) -> Rule {
        Rule {
            matcher: Matcher::LanguageToolRule {
                rule_id: rule_id.to_owned(),
            },
            unit: Unit::Sentence,
            band: Band::Forbidden,
            form_modes: BTreeSet::from([CheckableMode::Prose]),
            register_scope: None,
        }
    }

    fn in_email_only(mut rule: Rule) -> Rule {
        rule.register_scope = Some(BTreeSet::from(["email".to_owned()]));
        rule
    }

    /// The ruleset a store loads once it holds a Card of one clause per rule, worded as given, and
    /// a published version in which each rule serves its own clause.
    fn ruleset_of(rules: &[(Rule, &str)]) -> LoadedRuleset {
        let root = TempRoot::new();
        let store = Store::open(root.path()).unwrap();
        let members: Vec<CardMember> = rules
            .iter()
            .map(|(_, text)| {
                CardMember::New(ClauseWording {
                    text: (*text).to_owned(),
                    twin: format!("Ignore this: {text}"),
                    register_scope: None,
                    example: None,
                })
            })
            .collect();
        let card = store
            .insert_card_version_blocking(&members, "the test Card")
            .unwrap();
        let card = store.card_version_blocking(card).unwrap();
        let checkers: Vec<(Checker, Provenance)> = rules
            .iter()
            .zip(&card.clauses)
            .map(|((rule, _), clause)| {
                let checker = Checker {
                    clause_id: clause.clause_id,
                    rule: rule.clone(),
                };
                let provenance = Provenance {
                    author: CheckerAuthor::Template,
                    back_translation: None,
                    test_results: Vec::new(),
                };
                (checker, provenance)
            })
            .collect();
        store.publish_ruleset_blocking(&checkers).unwrap();
        LoadedRuleset::load_blocking(&store).unwrap()
    }

    fn prose(response: &str, ruleset: &LoadedRuleset) -> Vec<Flag> {
        route(matches_in(response), ruleset, FormMode::Prose, "default")
    }

    #[test]
    fn a_typos_match_is_dropped_even_when_a_checker_names_its_rule() {
        let naming = ruleset_of(&[(endorsing("MORFOLOGIK_RULE_EN_GB"), ACTIVE_VOICE)]);
        for ruleset in [LoadedRuleset::empty(), naming] {
            let flags = prose(AMERICAN_SPELLING_RESPONSE, &ruleset);
            assert!(flags.is_empty(), "{flags:#?}");
        }
    }

    #[test]
    fn the_picky_match_of_an_endorsed_rule_is_a_style_flag_citing_its_clause() {
        let ruleset = ruleset_of(&[(endorsing("PASSIVE_VOICE_SIMPLE"), ACTIVE_VOICE)]);
        let checker = &ruleset.checkers()[0];

        let flags = prose(PASSIVE_VOICE_RESPONSE, &ruleset);

        assert_eq!(flags.len(), 1, "{flags:#?}");
        let flag = &flags[0];
        assert_eq!(
            flag.origin,
            FlagOrigin::Style(ClauseCitation {
                clause_id: checker.citation().clause_id,
                rule_id: RuleId("PASSIVE_VOICE_SIMPLE".to_owned()),
                clause_text: ACTIVE_VOICE.to_owned(),
            })
        );
        assert_eq!(
            flag.message,
            "For a more expressive style, consider rephrasing the sentence in the active voice."
        );
        assert_eq!(flag.suggestions, ["the committee wrote the report"]);
        assert_eq!(
            flag.span,
            Span {
                anchor: "The report was written by the committee.".to_owned(),
                local_start: 0,
                local_length: 39,
                occurrence: 0,
            }
        );
        assert_eq!(flag.source_detail, "PASSIVE_VOICE_SIMPLE");
        assert!(flag.ruleset_version.is_some());
        assert_eq!(flag.ruleset_version, ruleset.version());
        assert_eq!(flag.catalogue_hash, None);
        assert_eq!(flag.id, format!("style:{}:0:39", checker.id().0));
    }

    #[test]
    fn a_hard_category_match_no_checker_names_is_a_grammar_flag_with_a_utf16_span() {
        let naming_another = ruleset_of(&[(endorsing("PASSIVE_VOICE_SIMPLE"), ACTIVE_VOICE)]);
        for ruleset in [LoadedRuleset::empty(), naming_another] {
            let flags = prose(GOT_GOTTEN_RESPONSE, &ruleset);
            assert_eq!(flags.len(), 1, "{flags:#?}");
            let flag = &flags[0];
            assert_eq!(flag.origin, FlagOrigin::Grammar);
            assert_eq!(flag.id, "grammar:0:GOT_GOTTEN");
            assert_eq!(
                flag.span,
                Span {
                    anchor: "I gotten used to it.".to_owned(),
                    local_start: 2,
                    local_length: 6,
                    occurrence: 0,
                }
            );
            assert_eq!(flag.suggestions, ["got"]);
            assert_eq!(flag.source_detail, "GOT_GOTTEN");
            assert_eq!(flag.ruleset_version, None);
        }
    }

    #[test]
    fn a_soft_or_unknown_category_match_no_checker_names_is_dropped() {
        let naming_another = ruleset_of(&[(endorsing("EN_QUOTES"), ACTIVE_VOICE)]);
        let mut unknown_category = matches_in(GOT_GOTTEN_RESPONSE);
        unknown_category[0].rule.category.id = "A_CATEGORY_NOT_IN_6_6".to_owned();
        let cases = [
            ("a soft category", matches_in(VERY_UNIQUE_RESPONSE)),
            ("a soft picky rule", matches_in(PASSIVE_VOICE_RESPONSE)),
            ("an unknown category", unknown_category),
        ];
        for (case, found) in cases {
            let flags = route(found, &naming_another, FormMode::Prose, "default");
            assert!(flags.is_empty(), "{case}: {flags:#?}");
        }
    }

    #[test]
    fn a_picky_rule_in_a_hard_category_is_dropped_unless_endorsed() {
        let unendorsed = prose(SMART_QUOTES_RESPONSE, &LoadedRuleset::empty());
        assert!(unendorsed.is_empty(), "{unendorsed:#?}");

        let ruleset = ruleset_of(&[(endorsing("EN_QUOTES"), ACTIVE_VOICE)]);
        let endorsed = prose(SMART_QUOTES_RESPONSE, &ruleset);
        assert_eq!(endorsed.len(), 1, "{endorsed:#?}");
        assert_eq!(
            endorsed[0].origin,
            FlagOrigin::Style(ruleset.checkers()[0].citation().clone())
        );
        assert_eq!(endorsed[0].suggestions, ["\u{201c}yes\u{201d}"]);
    }

    #[test]
    fn a_checker_whose_modes_or_scope_exclude_the_sentence_raises_no_flag_for_its_rule() {
        let mut fragments_only = endorsing("PASSIVE_VOICE_SIMPLE");
        fragments_only.form_modes = BTreeSet::from([CheckableMode::Fragment]);
        let cases = [
            (fragments_only, PASSIVE_VOICE_RESPONSE, FormMode::Prose),
            (
                endorsing("PASSIVE_VOICE_SIMPLE"),
                PASSIVE_VOICE_RESPONSE,
                FormMode::Code,
            ),
            (
                in_email_only(endorsing("PASSIVE_VOICE_SIMPLE")),
                PASSIVE_VOICE_RESPONSE,
                FormMode::Prose,
            ),
            // A hard category's match that a checker names routes through the checker alone.
            (
                in_email_only(endorsing("GOT_GOTTEN")),
                GOT_GOTTEN_RESPONSE,
                FormMode::Prose,
            ),
        ];
        for (rule, response, mode) in cases {
            let ruleset = ruleset_of(&[(rule, ACTIVE_VOICE)]);
            let flags = route(matches_in(response), &ruleset, mode, "default");
            assert!(flags.is_empty(), "{mode:?}: {flags:#?}");
        }
    }

    #[test]
    fn of_two_checkers_naming_one_rule_the_first_that_applies_cites_the_match() {
        let ruleset = ruleset_of(&[
            (
                in_email_only(endorsing("PASSIVE_VOICE_SIMPLE")),
                PLAIN_EMAIL,
            ),
            (endorsing("PASSIVE_VOICE_SIMPLE"), ACTIVE_VOICE),
        ]);
        assert_eq!(ruleset.checkers()[0].citation().clause_text, PLAIN_EMAIL);

        for (register, clause_text) in [("email", PLAIN_EMAIL), ("default", ACTIVE_VOICE)] {
            let flags = route(
                matches_in(PASSIVE_VOICE_RESPONSE),
                &ruleset,
                FormMode::Prose,
                register,
            );
            assert_eq!(flags.len(), 1, "{register}: {flags:#?}");
            let FlagOrigin::Style(citation) = &flags[0].origin else {
                panic!("{register}: {flags:#?}");
            };
            assert_eq!(citation.clause_text, clause_text, "{register}");
        }
    }
}
