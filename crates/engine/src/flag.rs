//! The shared unit every checking source produces and the analyzer, the Tauri command layer, and
//! the overlay all consume. Three unrelated modules produce a [`Flag`] today: spelling,
//! LanguageTool, and the AI-telltale matcher in `style`. The rule layer's checkers are the fourth,
//! and arrive with #95 and #100. Owning the type inside any one of them would make the others
//! reach sideways across the crate for it, so it lives here instead.
//!
//! Every type a [`Flag`] holds derives `Serialize` (camelCase on the wire, per this repo's
//! TypeScript naming convention) so the app's positioned flag, which carries one, can cross the
//! Tauri IPC boundary to the overlay's own webview unchanged. A `Flag` only ever travels from the
//! engine to the frontend, never back, so it has no `Deserialize`. The id newtypes have one, since
//! a checker read back from the store holds a [`ClauseId`].

/// Addresses a span of text the same way [`crate::capture::Capture::replace`] does: an anchor
/// found in the document plus a local offset and length from that anchor, in UTF-16 code units.
/// `capture::mod`'s documentation for `replace` records why an absolute document offset is not
/// used instead: UI Automation's character counting drifts near auto-numbered list items, while a
/// local offset from a freshly found anchor never crosses the boundary that causes the drift. A
/// `Span` is therefore directly usable both to replace its own text later and to resolve its
/// on-screen position, through the same anchor contract in both cases.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub anchor: String,
    pub local_start: usize,
    pub local_length: usize,
}

/// Which checking source produced a [`Flag`]. Each origin has exactly one [`Severity`], given by
/// [`Self::severity`]: spelling and grammar are hard, style and AI tell are soft. The overlay
/// renders each origin with its own underline.
///
/// Suppression, in `analyzer::dedupe`, reads a flag's severity from [`Self::severity`] alone.
///
/// A style flag's [`ClauseCitation`] is held in the `Style` variant rather than in an optional
/// field on [`Flag`], so a style flag cannot be built without a citation and no other origin can
/// carry one, and code that matches a style flag has its citation with no `Option` to unwrap. On
/// the wire the origin stays a bare string, and the citation sits beside it on the flag as
/// `citation`, present on a style flag only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "origin", content = "citation")]
pub enum FlagOrigin {
    Spelling,
    Grammar,
    /// A match from a checker of the loaded ruleset, citing the Style Card clause it serves.
    Style(ClauseCitation),
    AiTell,
}

impl FlagOrigin {
    pub fn severity(&self) -> Severity {
        match self {
            Self::Spelling | Self::Grammar => Severity::Hard,
            Self::Style(_) | Self::AiTell => Severity::Soft,
        }
    }
}

/// Whether a flag marks an error, which is hard, or a matter of style, which is soft. A method on
/// [`FlagOrigin`] gives it rather than a field on [`Flag`], so no flag can carry a severity its
/// origin does not have. Not serialised: the overlay draws by origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Hard,
    Soft,
}

/// The Style Card clause a style flag cites, embedded when the flag is raised so the hover card
/// needs no lookup and a later Card version cannot change what a stored flag said.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClauseCitation {
    pub clause_id: ClauseId,
    /// The rule id of the checker that raised the flag.
    pub rule_id: RuleId,
    /// The clause's text as it stood when the flag was raised.
    pub clause_text: String,
}

/// A Style Card clause's id, which the store assigns when the clause is first inserted and never
/// gives another clause (#82), so it names one clause across every Card version. An `i64`
/// because SQLite's integers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ClauseId(pub i64);

/// The rule id of the checker that raised a style flag. A string, because a style flag raised
/// through an endorsed LanguageTool rule (#98) cites LanguageTool's own rule id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RuleId(pub String);

/// A published ruleset version's id in the store (#82, #95). An `i64` because SQLite's integers
/// are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RulesetVersion(pub i64);

/// The content hash of the telltale catalogue's compiled bytes, as a lowercase hex digest. The
/// algorithm is chosen where the hash is taken, when the catalogue loads on the rule engine (#96).
/// Lowercase so that two hashes of the same bytes compare equal as strings, in the store and in
/// the bench.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct CatalogueHash(String);

impl CatalogueHash {
    /// `None` unless `digest` is a non-empty, even-length run of lowercase hex digits.
    pub fn from_hex(digest: &str) -> Option<Self> {
        let is_digest = !digest.is_empty()
            && digest.len().is_multiple_of(2)
            && digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        is_digest.then(|| Self(digest.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Refuses what [`CatalogueHash::from_hex`] refuses, so a hash read back is a digest.
impl<'de> serde::Deserialize<'de> for CatalogueHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let digest = String::deserialize(deserializer)?;
        Self::from_hex(&digest).ok_or_else(|| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(&digest),
                &"a non-empty, even-length run of lowercase hex digits",
            )
        })
    }
}

/// One flagged span: what is wrong with it, what could replace it, which source found it, and the
/// ruleset and catalogue in force when it was raised.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Flag {
    pub id: String,
    #[serde(flatten)]
    pub origin: FlagOrigin,
    pub span: Span,
    pub message: String,
    pub suggestions: Vec<String>,
    pub source_detail: String,
    /// The ruleset version in force when the flag was raised, whatever its origin. `None` until a
    /// ruleset is loaded (#95).
    pub ruleset_version: Option<RulesetVersion>,
    /// The telltale catalogue's content hash when the flag was raised, whatever its origin, so a
    /// catalogue change can be told apart from a ruleset change. `None` until the catalogue loads
    /// on the rule engine (#96).
    pub catalogue_hash: Option<CatalogueHash>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn citation() -> ClauseCitation {
        ClauseCitation {
            clause_id: ClauseId(7),
            rule_id: RuleId("HEDGING_PER_PARAGRAPH".to_owned()),
            clause_text: "Hedge at most once a paragraph.".to_owned(),
        }
    }

    fn flag(origin: FlagOrigin) -> Flag {
        Flag {
            id: "0:test:9".to_owned(),
            origin,
            span: Span {
                anchor: "The team boasts a record.".to_owned(),
                local_start: 9,
                local_length: 8,
            },
            message: "test flag".to_owned(),
            suggestions: vec!["has a".to_owned()],
            source_detail: "test".to_owned(),
            ruleset_version: None,
            catalogue_hash: None,
        }
    }

    #[test]
    fn spelling_and_grammar_are_hard_and_style_and_ai_tell_are_soft() {
        assert_eq!(FlagOrigin::Spelling.severity(), Severity::Hard);
        assert_eq!(FlagOrigin::Grammar.severity(), Severity::Hard);
        assert_eq!(FlagOrigin::Style(citation()).severity(), Severity::Soft);
        assert_eq!(FlagOrigin::AiTell.severity(), Severity::Soft);
    }

    #[test]
    fn each_origin_crosses_the_wire_as_the_overlays_name_for_it() {
        // The four members of the overlay's `FlagOrigin` union in `src/components/overlay.tsx`.
        let origins = [
            FlagOrigin::Spelling,
            FlagOrigin::Grammar,
            FlagOrigin::Style(citation()),
            FlagOrigin::AiTell,
        ];
        let names: Vec<serde_json::Value> = origins
            .into_iter()
            .map(|origin| serde_json::to_value(flag(origin)).unwrap()["origin"].clone())
            .collect();
        assert_eq!(names, ["spelling", "grammar", "style", "aiTell"]);
    }

    #[test]
    fn a_flag_crosses_the_wire_in_the_shape_the_overlay_declares() {
        let wire = serde_json::to_value(flag(FlagOrigin::Spelling)).unwrap();
        assert_eq!(
            wire,
            json!({
                "id": "0:test:9",
                "origin": "spelling",
                "span": {
                    "anchor": "The team boasts a record.",
                    "localStart": 9,
                    "localLength": 8,
                },
                "message": "test flag",
                "suggestions": ["has a"],
                "sourceDetail": "test",
                "rulesetVersion": null,
                "catalogueHash": null,
            })
        );
    }

    #[test]
    fn a_style_flag_carries_its_citation_beside_the_origin() {
        let mut style = flag(FlagOrigin::Style(citation()));
        style.ruleset_version = Some(RulesetVersion(3));
        style.catalogue_hash = CatalogueHash::from_hex("9f86d081");
        let wire = serde_json::to_value(style).unwrap();
        assert_eq!(wire["origin"], "style");
        assert_eq!(
            wire["citation"],
            json!({
                "clauseId": 7,
                "ruleId": "HEDGING_PER_PARAGRAPH",
                "clauseText": "Hedge at most once a paragraph.",
            })
        );
        assert_eq!(wire["rulesetVersion"], 3);
        assert_eq!(wire["catalogueHash"], "9f86d081");
    }

    #[test]
    fn a_catalogue_hash_holds_only_a_lowercase_hex_digest() {
        assert!(CatalogueHash::from_hex("09af").is_some());
        for rejected in ["", "09AF", "09a", "09ag", " 09af"] {
            assert!(CatalogueHash::from_hex(rejected).is_none(), "{rejected:?}");
        }
    }

    #[test]
    fn a_catalogue_hash_reads_back_from_its_digest_and_refuses_anything_else() {
        let hash = CatalogueHash::from_hex("9f86d081").unwrap();
        let read_back: CatalogueHash =
            serde_json::from_value(serde_json::to_value(&hash).unwrap()).unwrap();
        assert_eq!(read_back, hash);
        assert_eq!(read_back.as_str(), "9f86d081");
        for rejected in ["", "09AF", "09a", "not hex", " 09af"] {
            assert!(
                serde_json::from_value::<CatalogueHash>(json!(rejected)).is_err(),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn the_id_newtypes_read_back_from_what_they_serialise_as() {
        let citation = citation();
        let clause_id: ClauseId =
            serde_json::from_value(serde_json::to_value(citation.clause_id).unwrap()).unwrap();
        let rule_id: RuleId =
            serde_json::from_value(serde_json::to_value(&citation.rule_id).unwrap()).unwrap();
        let version: RulesetVersion =
            serde_json::from_value(serde_json::to_value(RulesetVersion(3)).unwrap()).unwrap();
        assert_eq!(clause_id, citation.clause_id);
        assert_eq!(rule_id, citation.rule_id);
        assert_eq!(version, RulesetVersion(3));
    }
}
