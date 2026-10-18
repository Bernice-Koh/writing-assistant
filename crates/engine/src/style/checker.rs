//! The checker: a Style Card clause made mechanical, as the clause's id and a [`Rule`]. The rule is
//! the part the engine evaluates: a matcher, a unit and a band, declared for the form modes and
//! the registers it runs in. A telltale pattern carries the same rule under its pattern id (#96).
//!
//! A checker is data. The store holds it as its serde JSON and takes its content hash over those
//! bytes (#82), so every part round-trips through serde, and the modes and the registers are
//! ordered sets: the order they were written in reaches neither the store nor the hash. The rule's
//! fields are flattened into the checker's, so a checker serialises as it did before the rule was
//! a type of its own and every stored hash still names its checker. Its provenance is stored beside
//! it and is not a part of it.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::flag::ClauseId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checker {
    /// The clause the checker serves, which every flag it raises cites.
    #[serde(deserialize_with = "clause_id_from_integer")]
    pub clause_id: ClauseId,
    #[serde(flatten)]
    pub rule: Rule,
}

/// The evaluable part of a checker or a telltale pattern, which the evaluator compiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub matcher: Matcher,
    pub unit: Unit,
    pub band: Band,
    /// The form modes of the paragraphs the rule runs in. It runs in no other.
    pub form_modes: BTreeSet<CheckableMode>,
    /// The registers the rule runs in, by the names the settings' register map gives them. Unset,
    /// it runs in every register.
    pub register_scope: Option<BTreeSet<String>>,
}

/// What a checker looks for in a unit's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Matcher {
    /// Each occurrence of any entry, matched case-insensitively.
    PhraseList { entries: Vec<PhraseEntry> },
    /// Each match of a regular expression in the `regex` crate's syntax, compiled from its source
    /// as written.
    Regex { source: String },
    /// Each match LanguageTool reports under this rule id, for a style rule the owner endorsed.
    LanguageToolRule { rule_id: String },
    /// Each word of a variant pair written in the convention the checker does not prefer. The
    /// pairs and the names of their conventions are the variant-pair table's (#94). Unset, the
    /// preferred convention is the one the unit's text uses more often, or on an equal count the
    /// one it uses first.
    SpellingVariant {
        preferred_convention: Option<String>,
    },
    /// The unit's words, so a band over them limits the unit's length.
    WordCount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhraseEntry {
    pub phrase: String,
    /// Whether a single-word entry also matches the longer words that start with it, so one entry
    /// covers its inflections. Set on an entry that is not one word, the checker does not compile.
    pub prefix: bool,
}

/// The text a checker's band is measured over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unit {
    Span,
    Sentence,
    Paragraph,
    Document,
}

/// How many matches a unit may hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Band {
    /// Not one: every match violates it.
    Forbidden,
    /// At least `minimum` and at most `maximum`, an unset bound being no limit.
    Count {
        minimum: Option<u32>,
        maximum: Option<u32>,
    },
    /// At most `maximum_per_hundred_words` for every hundred words of the unit.
    Rate { maximum_per_hundred_words: f64 },
}

/// A form mode a checker can declare. Code is a mode a paragraph has and no checker can declare,
/// since nothing runs on code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckableMode {
    Prose,
    Fragment,
}

/// The form mode of a paragraph, classified from its structure (#54).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormMode {
    Prose,
    Fragment,
    Code,
}

impl FormMode {
    /// The mode a checker declares to run in a paragraph of this one, or `None` for code.
    pub fn checkable(self) -> Option<CheckableMode> {
        match self {
            Self::Prose => Some(CheckableMode::Prose),
            Self::Fragment => Some(CheckableMode::Fragment),
            Self::Code => None,
        }
    }
}

/// Reads back the integer a [`ClauseId`] serialises as. The flag types derive `Serialize` only,
/// since a flag never travels back into the engine.
fn clause_id_from_integer<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<ClauseId, D::Error> {
    i64::deserialize(deserializer).map(ClauseId)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(phrase: &str, prefix: bool) -> PhraseEntry {
        PhraseEntry {
            phrase: phrase.to_owned(),
            prefix,
        }
    }

    fn registers(names: &[&str]) -> Option<BTreeSet<String>> {
        Some(names.iter().map(|name| (*name).to_owned()).collect())
    }

    /// One checker per matcher kind, between them holding every unit, every band and both form
    /// modes, with the register scope set and unset.
    fn a_checker_of_every_kind() -> Vec<Checker> {
        let prose_and_fragment = BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]);
        vec![
            Checker {
                clause_id: ClauseId(1),
                rule: Rule {
                    matcher: Matcher::PhraseList {
                        entries: vec![entry("it is worth noting", false), entry("intricate", true)],
                    },
                    unit: Unit::Paragraph,
                    band: Band::Count {
                        minimum: None,
                        maximum: Some(2),
                    },
                    form_modes: BTreeSet::from([CheckableMode::Prose]),
                    register_scope: None,
                },
            },
            Checker {
                clause_id: ClauseId(2),
                rule: Rule {
                    matcher: Matcher::Regex {
                        source: "\u{2014}".to_owned(),
                    },
                    unit: Unit::Span,
                    band: Band::Forbidden,
                    form_modes: prose_and_fragment.clone(),
                    register_scope: registers(&["default", "email"]),
                },
            },
            Checker {
                clause_id: ClauseId(3),
                rule: Rule {
                    matcher: Matcher::LanguageToolRule {
                        rule_id: "PASSIVE_VOICE".to_owned(),
                    },
                    unit: Unit::Sentence,
                    band: Band::Forbidden,
                    form_modes: BTreeSet::from([CheckableMode::Prose]),
                    register_scope: None,
                },
            },
            Checker {
                clause_id: ClauseId(4),
                rule: Rule {
                    matcher: Matcher::SpellingVariant {
                        preferred_convention: None,
                    },
                    unit: Unit::Document,
                    band: Band::Forbidden,
                    form_modes: prose_and_fragment.clone(),
                    register_scope: None,
                },
            },
            Checker {
                clause_id: ClauseId(5),
                rule: Rule {
                    matcher: Matcher::SpellingVariant {
                        preferred_convention: Some("british".to_owned()),
                    },
                    unit: Unit::Span,
                    band: Band::Forbidden,
                    form_modes: BTreeSet::from([CheckableMode::Fragment]),
                    register_scope: registers(&["email"]),
                },
            },
            Checker {
                clause_id: ClauseId(6),
                rule: Rule {
                    matcher: Matcher::WordCount,
                    unit: Unit::Sentence,
                    band: Band::Count {
                        minimum: Some(3),
                        maximum: Some(30),
                    },
                    form_modes: BTreeSet::from([CheckableMode::Prose]),
                    register_scope: None,
                },
            },
            Checker {
                clause_id: ClauseId(7),
                rule: Rule {
                    matcher: Matcher::PhraseList {
                        entries: vec![entry("perhaps", false)],
                    },
                    unit: Unit::Document,
                    band: Band::Rate {
                        maximum_per_hundred_words: 1.5,
                    },
                    form_modes: prose_and_fragment,
                    register_scope: registers(&[]),
                },
            },
        ]
    }

    #[test]
    fn a_checker_of_every_matcher_kind_round_trips() {
        for checker in a_checker_of_every_kind() {
            let serialised = serde_json::to_string(&checker).unwrap();
            let read_back: Checker = serde_json::from_str(&serialised).unwrap();
            assert_eq!(read_back, checker, "{serialised}");
        }
    }

    #[test]
    fn a_checker_serialises_with_each_part_named() {
        // #95 hashes this serialisation, so a renamed field or variant changes every rule id.
        let checker = a_checker_of_every_kind().remove(0);
        assert_eq!(
            serde_json::to_value(checker).unwrap(),
            json!({
                "clauseId": 1,
                "matcher": {
                    "kind": "phraseList",
                    "entries": [
                        { "phrase": "it is worth noting", "prefix": false },
                        { "phrase": "intricate", "prefix": true },
                    ],
                },
                "unit": "paragraph",
                "band": { "kind": "count", "minimum": null, "maximum": 2 },
                "formModes": ["prose"],
                "registerScope": null,
            })
        );
    }

    #[test]
    fn a_checker_serialises_to_the_bytes_it_did_before_the_rule_was_its_own_type() {
        // The store's content hash is taken over these bytes, so a change to them, the order of
        // the keys included, gives a stored checker a second rule id.
        let checker = a_checker_of_every_kind().remove(0);
        assert_eq!(
            serde_json::to_string(&checker).unwrap(),
            r#"{"clauseId":1,"matcher":{"kind":"phraseList","entries":[{"phrase":"it is worth noting","prefix":false},{"phrase":"intricate","prefix":true}]},"unit":"paragraph","band":{"kind":"count","minimum":null,"maximum":2},"formModes":["prose"],"registerScope":null}"#
        );
    }

    #[test]
    fn modes_and_registers_written_in_another_order_serialise_alike() {
        let written = json!({
            "clauseId": 2,
            "matcher": { "kind": "regex", "source": "\u{2014}" },
            "unit": "span",
            "band": { "kind": "forbidden" },
            "formModes": ["fragment", "prose"],
            "registerScope": ["email", "default"],
        });
        let checker: Checker = serde_json::from_value(written).unwrap();
        let serialised = serde_json::to_value(&checker).unwrap();
        assert_eq!(serialised["formModes"], json!(["prose", "fragment"]));
        assert_eq!(serialised["registerScope"], json!(["default", "email"]));
        assert_eq!(checker, a_checker_of_every_kind().remove(1));
    }

    #[test]
    fn no_checker_can_declare_the_code_mode() {
        let written = json!({
            "clauseId": 2,
            "matcher": { "kind": "regex", "source": "\u{2014}" },
            "unit": "span",
            "band": { "kind": "forbidden" },
            "formModes": ["prose", "code"],
            "registerScope": null,
        });
        assert!(serde_json::from_value::<Checker>(written).is_err());
    }

    #[test]
    fn code_is_the_one_form_mode_with_no_checkable_mode() {
        assert_eq!(FormMode::Prose.checkable(), Some(CheckableMode::Prose));
        assert_eq!(
            FormMode::Fragment.checkable(),
            Some(CheckableMode::Fragment)
        );
        assert_eq!(FormMode::Code.checkable(), None);
    }
}
