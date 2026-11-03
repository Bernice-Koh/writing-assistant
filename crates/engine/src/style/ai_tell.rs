//! The AI-telltale catalogue, hand-encoded at `src-tauri/resources/ai-telltales/patterns.json`,
//! run on the rule engine. Provenance, licensing, the matching notes, and the reasoning behind
//! every narrowed or dropped pattern are recorded in `src-tauri/resources/ai-telltales/NOTICE.md`,
//! not repeated here.
//!
//! A pattern is a [`Rule`] beside its id, description, source citation and raw flag, written in
//! the file in the rule's own serialised form. It serves no Style Card clause, so it is compiled
//! under its pattern id, and its flags keep their own origin, which carries the id. The app
//! compiles the patterns the settings switch on once, at startup, into one [`Catalogue`].
//!
//! The catalogue's bytes and the variant-pair table's are the rule data compiled into the engine,
//! and one content hash over both, taken once, is recorded on every flag beside the ruleset
//! version, so an edit to either file is told from a ruleset change.

use std::sync::OnceLock;

use serde::Deserialize;

use super::checker::{FormMode, Rule, Unit};
use super::evaluator::{Evaluator, ModeSentence, RuleOwner, Violation};
use super::phrase::CurlyMarks;
use super::variants;
use crate::flag::{CatalogueHash, Flag, FlagOrigin};
use crate::hash::sha256_hex;
use crate::store::Config;

// Compiled in rather than read from the resource directory at run time, so the app and `wa`
// always match against the same catalogue, and one fewer file can be missing at startup.
const CATALOGUE_JSON: &str =
    include_str!("../../../../src-tauri/resources/ai-telltales/patterns.json");

/// One pattern as the catalogue file writes it.
#[derive(Debug, Deserialize)]
struct Pattern {
    id: String,
    description: String,
    #[serde(flatten)]
    rule: Rule,
    /// Whether the pattern's phrases match the text as written, curly marks and all, rather than
    /// reading each curly mark as its straight form. Held beside the rule and not in it: a
    /// checker's content hash is taken over its rule's serialised bytes, so a field of the rule
    /// would change the rule id of every stored checker.
    raw: bool,
    source: String,
}

/// The catalogue file, parsed once.
fn patterns() -> &'static [Pattern] {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        serde_json::from_str(CATALOGUE_JSON).expect(
            "src-tauri/resources/ai-telltales/patterns.json is committed and checked by its tests",
        )
    })
}

/// The id of every pattern in the catalogue, in catalogue order, whether switched on or off. The
/// settings file keys its telltale toggles by these ids.
pub fn pattern_ids() -> impl Iterator<Item = &'static str> {
    patterns().iter().map(|pattern| pattern.id.as_str())
}

/// The SHA-256 of the catalogue's bytes followed by the variant-pair table's, as compiled in,
/// computed once.
fn rule_data_hash() -> &'static CatalogueHash {
    static HASH: OnceLock<CatalogueHash> = OnceLock::new();
    HASH.get_or_init(|| {
        let bytes = [CATALOGUE_JSON.as_bytes(), variants::PAIRS_JSON.as_bytes()].concat();
        CatalogueHash::from_hex(&sha256_hex(&bytes))
            .expect("sha256_hex writes 64 lowercase hex digits")
    })
}

/// The catalogue's patterns that the settings switch on, each compiled once.
#[derive(Debug)]
pub struct Catalogue {
    /// In catalogue order.
    patterns: Vec<CompiledPattern>,
}

/// One pattern of the catalogue, compiled.
#[derive(Debug)]
pub(crate) struct CompiledPattern {
    pattern: &'static Pattern,
    evaluator: Evaluator,
}

impl Catalogue {
    /// Compiles each pattern `settings` switches on: a pattern's toggle, or its default where the
    /// toggles omit it. A pattern switched off is not compiled and raises no flag.
    pub fn load(settings: &Config) -> Self {
        Self::compiled(|pattern_id| settings.is_telltale_enabled(pattern_id))
    }

    fn compiled(is_enabled: impl Fn(&str) -> bool) -> Self {
        let patterns = patterns()
            .iter()
            .filter(|pattern| is_enabled(&pattern.id))
            .map(|pattern| CompiledPattern {
                pattern,
                evaluator: compile(pattern),
            })
            .collect();
        Self { patterns }
    }

    /// The content hash of the rule data compiled into the engine, which every flag the analyzer
    /// publishes records as its [`Flag::catalogue_hash`]. The toggles do not change it: it is
    /// taken over the files' bytes, whichever patterns are switched on.
    pub fn hash(&self) -> &'static CatalogueHash {
        rule_data_hash()
    }

    /// The id of each pattern switched on, in catalogue order.
    pub fn enabled_pattern_ids(&self) -> impl Iterator<Item = &str> {
        self.patterns
            .iter()
            .map(|compiled| compiled.pattern.id.as_str())
    }

    /// One flag for each match in `sentence` of each pattern switched on whose unit is a span or a
    /// sentence, so a phrase that occurs twice is flagged twice, with `mode` the form mode of the
    /// sentence's paragraph and `register` its register. In catalogue order, and within a pattern
    /// in order of start. A pattern of a paragraph or the document unit is evaluated over its unit
    /// by the analyzer's paragraph or document pass.
    ///
    /// Every flag anchors on `sentence` itself and carries the match's own offset into it, the
    /// same anchoring [`crate::spelling`] and [`crate::languagetool`] use. The analyzer counts a
    /// span's occurrence index over the text before the sentence, so two flags anchored on
    /// identical bare matches in one sentence, a single emoji or a dash each, would share an index
    /// and address the same match.
    pub fn flags(&self, sentence: &str, mode: FormMode, register: &str) -> Vec<Flag> {
        let sentence = [ModeSentence {
            text: sentence,
            mode,
        }];
        self.patterns
            .iter()
            .filter(|compiled| compiled.evaluator.rule().unit.is_within_a_sentence())
            .flat_map(|compiled| {
                compiled
                    .evaluator
                    .evaluate(&sentence, register)
                    .into_iter()
                    .map(|violation| compiled.flag(violation))
            })
            .collect()
    }

    /// Each pattern switched on whose unit is `unit`, in catalogue order. The analyzer's paragraph
    /// and document passes evaluate the patterns of their unit through it.
    pub(crate) fn patterns_of_unit(&self, unit: Unit) -> impl Iterator<Item = &CompiledPattern> {
        self.patterns
            .iter()
            .filter(move |compiled| compiled.evaluator.rule().unit == unit)
    }
}

impl CompiledPattern {
    pub(crate) fn evaluator(&self) -> &Evaluator {
        &self.evaluator
    }

    /// The telltale for `violation` of this pattern. It cites no clause and carries no
    /// replacement: the catalogue holds phrasings to cut, not substitutions. The analyzer stamps
    /// the ruleset version and the catalogue hash as it publishes.
    pub(crate) fn flag(&self, violation: Violation) -> Flag {
        let pattern = self.pattern;
        Flag {
            // The pattern and the span tell two of the pattern's flags on one sentence apart, and
            // neither is draft text, which an id never carries.
            id: format!(
                "ai-tell:{}:{}:{}",
                pattern.id, violation.span.local_start, violation.span.local_length
            ),
            origin: FlagOrigin::AiTell {
                pattern_ids: vec![pattern.id.clone()],
            },
            span: violation.span,
            message: pattern.description.clone(),
            suggestions: Vec::new(),
            source_detail: pattern.source.clone(),
            ruleset_version: None,
            catalogue_hash: None,
        }
    }
}

fn compile(pattern: &Pattern) -> Evaluator {
    let curly_marks = if pattern.raw {
        CurlyMarks::AsWritten
    } else {
        CurlyMarks::AsStraight
    };
    let owner = RuleOwner::Pattern(pattern.id.clone());
    Evaluator::compile_reading(owner, pattern.rule.clone(), curly_marks)
        .unwrap_or_else(|error| panic!("{error}, though the catalogue's tests compile it"))
}

#[cfg(test)]
impl Catalogue {
    /// Every pattern of the catalogue compiled, whatever its default.
    pub(crate) fn every_pattern() -> Self {
        Self::compiled(|_| true)
    }

    /// The patterns `json` writes, in the catalogue file's form, each compiled, for a test that
    /// needs patterns the catalogue does not hold. The patterns are leaked, so that they live as
    /// long as the catalogue's own.
    pub(crate) fn of_patterns(json: &str) -> Self {
        let patterns: Vec<Pattern> = serde_json::from_str(json).unwrap();
        let patterns: &'static [Pattern] = Box::leak(patterns.into_boxed_slice());
        Self {
            patterns: patterns
                .iter()
                .map(|pattern| CompiledPattern {
                    pattern,
                    evaluator: compile(pattern),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod stems;

#[cfg(test)]
mod structural;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

    use super::*;
    use crate::style::checker::{Band, CheckableMode, Matcher, PhraseEntry, Unit};

    /// The patterns that count a habit over a paragraph or the document, in catalogue order. Every
    /// other pattern bans a phrasing in a span.
    pub(super) const STRUCTURAL_PATTERNS: [&str; 2] = ["rule-of-three", "hyphenated-word-pairs"];

    /// Every pattern that bans a phrasing in a span, in catalogue order.
    fn phrasing_patterns() -> impl Iterator<Item = &'static Pattern> {
        patterns()
            .iter()
            .filter(|pattern| !STRUCTURAL_PATTERNS.contains(&pattern.id.as_str()))
    }

    pub(super) fn scan(text: &str) -> Vec<Flag> {
        Catalogue::every_pattern().flags(text, FormMode::Prose, "default")
    }

    /// The exact text a flag's span addresses, sliced back out of its own anchor, so a test can
    /// assert on what was flagged without depending on how the span happens to be anchored.
    pub(super) fn flagged_text(flag: &Flag) -> String {
        let anchor: Vec<u16> = flag.span.anchor.encode_utf16().collect();
        let end = flag.span.local_start + flag.span.local_length;
        String::from_utf16_lossy(&anchor[flag.span.local_start..end])
    }

    fn pattern_ids_of(flag: &Flag) -> &[String] {
        match &flag.origin {
            FlagOrigin::AiTell { pattern_ids } => pattern_ids,
            other => panic!("not a telltale: {other:?}"),
        }
    }

    pub(super) fn flags_of<'a>(flags: &'a [Flag], pattern_id: &str) -> Vec<&'a Flag> {
        flags
            .iter()
            .filter(|flag| pattern_ids_of(flag) == [pattern_id])
            .collect()
    }

    /// The settings with `toggles` as the telltale toggles, every pattern they omit taking its
    /// default.
    fn toggled(toggles: &[(&str, bool)]) -> Config {
        Config {
            telltale_toggles: toggles
                .iter()
                .map(|(id, on)| ((*id).to_owned(), *on))
                .collect(),
            ..Config::default()
        }
    }

    pub(super) fn pattern(id: &str) -> &'static Pattern {
        patterns()
            .iter()
            .find(|pattern| pattern.id == id)
            .unwrap_or_else(|| panic!("no pattern {id:?}"))
    }

    pub(super) fn phrase_entries(pattern: &Pattern) -> &[PhraseEntry] {
        match &pattern.rule.matcher {
            Matcher::PhraseList { entries } => entries,
            _ => &[],
        }
    }

    #[test]
    fn a_character_that_changes_byte_length_when_lowercased_does_not_shift_the_span() {
        // A capital dotted I is two bytes and lowercases to three, and the Kelvin sign is three
        // and lowercases to one, so an offset found in a lowercased copy of either sentence
        // indexes the original one or two bytes off. In the first sentence that lands inside the
        // em dash, which is not a character boundary.
        let flags = scan("In İzmir\u{2014}a port city, trade grew.");
        let dashes = flags_of(&flags, "em-en-dashes");
        assert_eq!(dashes.len(), 1, "{flags:#?}");
        assert_eq!(flagged_text(dashes[0]), "\u{2014}");

        let flags = scan("At 300 \u{212A} the alloy boasts a low resistance.");
        let copulas = flags_of(&flags, "copula-avoidance");
        assert_eq!(copulas.len(), 1, "{flags:#?}");
        assert_eq!(flagged_text(copulas[0]), "boasts a");
    }

    #[test]
    fn a_flag_names_its_pattern_in_its_origin_cites_its_source_and_no_clause() {
        let flags = scan("This project's success is a testament to the team's hard work.");
        let significance = flags_of(&flags, "significance-legacy-emphasis");
        assert_eq!(significance.len(), 1, "{flags:#?}");
        let flag = significance[0];
        assert_eq!(
            flag.origin,
            FlagOrigin::AiTell {
                pattern_ids: vec!["significance-legacy-emphasis".to_owned()]
            }
        );
        assert_eq!(
            flag.source_detail,
            "humanizer SKILL.md section 1, Wikipedia:Signs of AI writing"
        );
        assert_eq!(
            flag.message,
            pattern("significance-legacy-emphasis").description
        );
        assert!(flag.suggestions.is_empty());
        assert_eq!(flag.ruleset_version, None);
        assert_eq!(flagged_text(flag), "is a testament to");
        // A telltale's origin is serialised without a citation.
        let wire = serde_json::to_value(flag).unwrap();
        assert_eq!(wire["origin"], "aiTell");
        assert!(wire.get("citation").is_none(), "{wire}");
    }

    #[test]
    fn a_flag_id_names_the_pattern_and_the_span_and_no_draft_text() {
        let sentence = "The town boasts a harbour.";
        let flags = scan(sentence);
        assert_eq!(flags.len(), 1, "{flags:#?}");
        assert_eq!(flags[0].id, "ai-tell:copula-avoidance:9:8");
    }

    #[test]
    fn does_not_flag_a_sentence_with_no_catalogued_pattern() {
        let flags = scan("The cat sat on the warm windowsill and watched the rain.");
        assert!(flags.is_empty(), "expected no flags: {flags:#?}");
    }

    #[test]
    fn flags_an_em_dash() {
        let flags = scan("The policy—announced without warning—affects everyone.");
        assert!(!flags_of(&flags, "em-en-dashes").is_empty(), "{flags:#?}");
    }

    #[test]
    fn flags_every_occurrence_of_a_phrase_in_one_sentence() {
        let flags = scan("The policy\u{2014}announced without warning\u{2014}affects everyone.");
        let dashes = flags_of(&flags, "em-en-dashes");
        assert_eq!(dashes.len(), 2, "{flags:#?}");
        assert_ne!(dashes[0].id, dashes[1].id);
        assert_ne!(dashes[0].span.local_start, dashes[1].span.local_start);
        assert!(dashes.iter().all(|flag| flagged_text(flag) == "\u{2014}"));
    }

    #[test]
    fn a_phrase_edge_that_is_a_letter_or_digit_matches_only_at_a_word_boundary() {
        // "here is a" is catalogued, and sits inside "there is a".
        let flags = scan("There is a cat on the mat.");
        assert!(flags.is_empty(), "expected no flags: {flags:#?}");
        let flags = scan("Here is a summary of the meeting.");
        let artifacts = flags_of(&flags, "collaborative-communication-artifacts");
        assert_eq!(artifacts.len(), 1, "{flags:#?}");
        assert_eq!(flagged_text(artifacts[0]), "Here is a");

        // "boasts a" is catalogued, and sits inside "boasts about".
        let flags = scan("The author boasts about the prize.");
        assert!(flags.is_empty(), "expected no flags: {flags:#?}");
        let flags = scan("The town boasts a harbour.");
        let copulas = flags_of(&flags, "copula-avoidance");
        assert_eq!(copulas.len(), 1, "{flags:#?}");
        assert_eq!(flagged_text(copulas[0]), "boasts a");
    }

    #[test]
    fn flags_an_emoji() {
        let flags = scan("Launch Phase 🚀 begins in Q3.");
        assert!(!flags_of(&flags, "emojis").is_empty(), "{flags:#?}");
    }

    #[test]
    fn two_identical_regex_matches_anchor_on_the_scanned_text_at_their_own_offsets() {
        // Anchored on the bare match, both flags would share the occurrence index the analyzer
        // counts before the sentence, and resolve to the same rocket.
        let text = "Launch 🚀 today, then launch 🚀 again.";
        let flags = scan(text);
        let emojis = flags_of(&flags, "emojis");
        assert_eq!(emojis.len(), 2, "{flags:#?}");
        assert_ne!(emojis[0].id, emojis[1].id);
        assert!(emojis.iter().all(|flag| flag.span.anchor == text));
        assert!(emojis.iter().all(|flag| flagged_text(flag) == "🚀"));
        // In UTF-16 code units: the first rocket is two of them, and the second offset counts
        // both.
        let second = text.rfind('🚀').unwrap();
        assert_eq!(
            emojis[1].span.local_start,
            text[..second].encode_utf16().count()
        );
    }

    #[test]
    fn flags_an_inline_header_list_item_across_the_whole_text() {
        let text = "Update notes:\n- **Performance:** Faster load times.\n- **Security:** Encrypted end to end.";
        let flags = scan(text);
        assert_eq!(
            flags_of(&flags, "inline-header-lists").len(),
            2,
            "expected both bulleted items to match: {flags:#?}"
        );
    }

    #[test]
    fn negative_parallelism_accepts_a_straight_a_curly_or_no_apostrophe() {
        for sentence in [
            "It's not just a tool, it's a partner.",
            "It\u{2019}s not just a tool, it\u{2019}s a partner.",
            "It\u{2019}s not merely a tool, it's a partner.",
            "Its not only a tool, its a partner.",
        ] {
            let flags = scan(sentence);
            let parallels = flags_of(&flags, "negative-parallelism");
            assert_eq!(parallels.len(), 1, "{sentence}: {flags:#?}");
            assert_eq!(
                flagged_text(parallels[0]),
                sentence.trim_end_matches(" a partner."),
                "{sentence}"
            );
        }
        assert!(scan("It\u{2019}s not a tool.").is_empty());
    }

    #[test]
    fn flags_a_superficial_ing_ending_only_after_a_comma() {
        let flagged = scan("The design uses blue and gold, symbolizing the coastline.");
        assert_eq!(
            flags_of(&flagged, "superficial-ing-endings").len(),
            1,
            "{flagged:#?}"
        );

        let not_flagged = scan("The team is symbolizing nothing in particular today.");
        assert!(
            flags_of(&not_flagged, "superficial-ing-endings").is_empty(),
            "a bare gerund with no preceding comma should not match: {not_flagged:#?}"
        );
    }

    #[test]
    fn accepts_the_ise_spelling_wherever_the_catalogue_has_an_ize_form() {
        let flags = scan("The crest is blue and gold, symbolising its enduring ties to the sea.");
        assert_eq!(
            flags_of(&flags, "superficial-ing-endings").len(),
            1,
            "{flags:#?}"
        );
        let significance = flags_of(&flags, "significance-legacy-emphasis");
        assert_eq!(significance.len(), 1, "{flags:#?}");
        assert_eq!(flagged_text(significance[0]), "symbolising its enduring");

        let flags = scan("The report closes on the budget, emphasising the shortfall.");
        assert_eq!(
            flags_of(&flags, "superficial-ing-endings").len(),
            1,
            "{flags:#?}"
        );
    }

    #[test]
    fn a_phrase_with_a_straight_apostrophe_matches_the_curly_one_word_writes() {
        for sentence in [
            "You\u{2019}re absolutely right about the budget.",
            "You're absolutely right about the budget.",
        ] {
            let flags = scan(sentence);
            let sycophantic = flags_of(&flags, "sycophantic-tone");
            assert_eq!(sycophantic.len(), 1, "{flags:#?}");
            assert_eq!(
                flagged_text(sycophantic[0]),
                sentence.trim_end_matches(" about the budget.")
            );
        }
    }

    #[test]
    fn every_catalogue_phrase_belongs_to_exactly_one_pattern() {
        let mut owners: HashMap<String, &str> = HashMap::new();
        let mut entries = 0;
        for pattern in patterns() {
            for entry in phrase_entries(pattern) {
                entries += 1;
                let previous = owners.insert(entry.phrase.to_lowercase(), &pattern.id);
                assert!(
                    previous.is_none(),
                    "phrase {:?} is listed under {previous:?} and again under {:?}",
                    entry.phrase,
                    pattern.id
                );
            }
        }
        // Every entry of every phrase list was read, whatever its flags.
        let listed: usize = patterns()
            .iter()
            .map(|pattern| phrase_entries(pattern).len())
            .sum();
        assert_eq!(entries, listed);
        assert!(listed > 100, "{listed}");
        // No prefix entry covers a one-word entry of another pattern, which would flag one word
        // under both.
        for pattern in patterns() {
            for stem in phrase_entries(pattern).iter().filter(|entry| entry.prefix) {
                let stem = stem.phrase.to_lowercase();
                for (phrase, owner) in &owners {
                    assert!(
                        *owner == pattern.id
                            || phrase.contains(char::is_whitespace)
                            || !phrase.starts_with(&stem),
                        "the stem {stem:?} of {:?} covers {phrase:?} of {owner:?}",
                        pattern.id
                    );
                }
            }
        }
    }

    #[test]
    fn a_phrase_once_listed_under_two_patterns_raises_one_flag() {
        let flags = scan("The town boasts a harbour.");
        assert_eq!(flags.len(), 1, "{flags:#?}");
        assert_eq!(flags_of(&flags, "copula-avoidance").len(), 1);

        let flags = scan("You're absolutely right about the budget.");
        assert_eq!(flags.len(), 1, "{flags:#?}");
        assert_eq!(flags_of(&flags, "sycophantic-tone").len(), 1);
    }

    #[test]
    fn every_pattern_id_is_unique() {
        // A settings toggle names its pattern by id, so two patterns sharing one would share a
        // toggle.
        let mut seen = HashSet::new();
        for id in pattern_ids() {
            assert!(seen.insert(id), "pattern id {id:?} repeats");
        }
        assert_eq!(seen.len(), patterns().len());
        assert_eq!(seen.len(), 23);
    }

    #[test]
    fn every_phrasing_pattern_is_a_phrase_list_or_a_regex_on_spans_under_the_forbidden_band() {
        for pattern in phrasing_patterns() {
            let rule = &pattern.rule;
            let signals = match &rule.matcher {
                Matcher::PhraseList { entries } => entries.len(),
                Matcher::Regex { source } => usize::from(!source.is_empty()),
                other => panic!("{:?} has the matcher {other:?}", pattern.id),
            };
            assert!(signals > 0, "{:?} matches nothing", pattern.id);
            assert_eq!(rule.unit, Unit::Span, "{:?}", pattern.id);
            assert_eq!(rule.band, Band::Forbidden, "{:?}", pattern.id);
            assert_eq!(rule.register_scope, None, "{:?}", pattern.id);
        }
    }

    #[test]
    fn every_phrasing_pattern_declares_prose_and_fragment() {
        let both = BTreeSet::from([CheckableMode::Prose, CheckableMode::Fragment]);
        for pattern in phrasing_patterns() {
            assert_eq!(pattern.rule.form_modes, both, "{:?}", pattern.id);
        }
        let catalogue = Catalogue::every_pattern();
        let text = "The town boasts a harbour.";
        for mode in [FormMode::Prose, FormMode::Fragment] {
            assert_eq!(catalogue.flags(text, mode, "default").len(), 1, "{mode:?}");
        }
        assert!(catalogue.flags(text, FormMode::Code, "default").is_empty());
    }

    /// A pattern of `unit` banning the phrase "perhaps", in the catalogue file's form.
    fn hedging_pattern(id: &str, unit: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "description": "Hedges",
                "matcher": {{ "kind": "phraseList", "entries": [{{ "phrase": "perhaps", "prefix": false }}] }},
                "unit": "{unit}",
                "band": {{ "kind": "forbidden" }},
                "formModes": ["prose"],
                "raw": false,
                "source": "a test"
            }}"#
        )
    }

    #[test]
    fn a_sentence_is_evaluated_by_the_span_and_sentence_patterns_and_the_rest_by_their_unit() {
        let units = ["span", "sentence", "paragraph", "document"];
        let json = format!(
            "[{}]",
            units
                .map(|unit| hedging_pattern(&format!("{unit}-hedges"), unit))
                .join(",")
        );
        let catalogue = Catalogue::of_patterns(&json);

        let flags = catalogue.flags("Perhaps it rains.", FormMode::Prose, "default");
        let flagged: Vec<String> = flags
            .iter()
            .map(|flag| pattern_ids_of(flag).join(", "))
            .collect();
        assert_eq!(flagged, ["span-hedges", "sentence-hedges"]);
        for unit in [Unit::Paragraph, Unit::Document] {
            let of_unit: Vec<&str> = catalogue
                .patterns_of_unit(unit)
                .map(|compiled| compiled.pattern.id.as_str())
                .collect();
            assert_eq!(of_unit, [format!("{unit:?}-hedges").to_lowercase()]);
        }
    }

    #[test]
    fn every_pattern_compiles_under_its_id() {
        let catalogue = Catalogue::every_pattern();
        assert!(catalogue.enabled_pattern_ids().eq(pattern_ids()));
        for compiled in &catalogue.patterns {
            assert_eq!(
                compiled.evaluator.owner(),
                &RuleOwner::Pattern(compiled.pattern.id.clone())
            );
            assert_eq!(compiled.evaluator.rule(), &compiled.pattern.rule);
        }
    }

    /// A file of the vendored resources, read from disk as the repository holds it.
    fn resource(path: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../src-tauri/resources")
                .join(path),
        )
        .unwrap()
    }

    #[test]
    fn the_hash_is_the_sha256_of_the_catalogue_followed_by_the_variant_table() {
        let catalogue = resource("ai-telltales/patterns.json");
        let pairs = resource("spelling-variants/pairs.json");
        let hash = Catalogue::load(&Config::default()).hash();
        assert_eq!(
            hash.as_str(),
            sha256_hex(&[catalogue.as_slice(), &pairs].concat())
        );
        assert_ne!(
            hash.as_str(),
            sha256_hex(&[pairs.as_slice(), &catalogue].concat())
        );
        assert_ne!(hash.as_str(), sha256_hex(&catalogue));
    }

    #[test]
    fn the_hash_is_computed_once_and_is_the_same_whichever_patterns_are_on() {
        let every = Catalogue::every_pattern();
        let none = Catalogue::compiled(|_| false);
        assert!(std::ptr::eq(every.hash(), none.hash()));
        assert_eq!(none.enabled_pattern_ids().count(), 0);
    }

    #[test]
    fn curly_quotes_alone_carries_the_raw_flag() {
        let raw: Vec<&str> = patterns()
            .iter()
            .filter(|pattern| pattern.raw)
            .map(|pattern| pattern.id.as_str())
            .collect();
        assert_eq!(raw, ["curly-quotes"]);
    }

    #[test]
    fn curly_quotes_switched_on_flags_each_curly_mark_and_no_straight_one() {
        let catalogue = Catalogue::load(&toggled(&[("curly-quotes", true)]));
        let text = "\u{201C}Yes,\u{201D} she said, and \"no\" twice.";
        let flags = catalogue.flags(text, FormMode::Prose, "default");
        let quotes = flags_of(&flags, "curly-quotes");
        assert_eq!(
            quotes
                .iter()
                .map(|flag| flagged_text(flag))
                .collect::<Vec<_>>(),
            ["\u{201C}", "\u{201D}"],
            "{flags:#?}"
        );
        assert_eq!(
            quotes
                .iter()
                .map(|flag| flag.span.local_start)
                .collect::<Vec<_>>(),
            [0, 5]
        );
        let straight = catalogue.flags("She said \"no\" twice.", FormMode::Prose, "default");
        assert!(
            flags_of(&straight, "curly-quotes").is_empty(),
            "{straight:#?}"
        );
    }

    #[test]
    fn a_pattern_switched_off_raises_no_flag_and_every_other_is_unaffected() {
        let text = "The town boasts a harbour\u{2014}and a castle \u{1F680}.";
        let every = Catalogue::every_pattern().flags(text, FormMode::Prose, "default");
        assert!(!flags_of(&every, "em-en-dashes").is_empty(), "{every:#?}");

        let without_dashes =
            Catalogue::load(&toggled(&[("em-en-dashes", false), ("curly-quotes", true)]));
        let flags = without_dashes.flags(text, FormMode::Prose, "default");
        assert!(flags_of(&flags, "em-en-dashes").is_empty(), "{flags:#?}");
        let others: Vec<&Flag> = every
            .iter()
            .filter(|flag| pattern_ids_of(flag) != ["em-en-dashes"])
            .collect();
        assert_eq!(flags.iter().collect::<Vec<_>>(), others);
        assert!(!without_dashes
            .enabled_pattern_ids()
            .any(|id| id == "em-en-dashes"));
    }

    #[test]
    fn a_pattern_absent_from_the_toggles_takes_its_default() {
        let defaults = Catalogue::load(&toggled(&[]));
        let expected: Vec<&str> = pattern_ids().filter(|id| *id != "curly-quotes").collect();
        assert_eq!(defaults.enabled_pattern_ids().collect::<Vec<_>>(), expected);
        let flags = defaults.flags(
            "\u{201C}The town boasts a harbour.\u{201D}",
            FormMode::Prose,
            "default",
        );
        assert!(flags_of(&flags, "curly-quotes").is_empty(), "{flags:#?}");
        assert_eq!(flags_of(&flags, "copula-avoidance").len(), 1, "{flags:#?}");

        // The defaults the settings file is written with switch on the same patterns.
        let written = Catalogue::load(&Config::default());
        assert!(written.enabled_pattern_ids().eq(expected));
    }

    #[test]
    fn the_flags_of_a_sentence_come_in_catalogue_order_and_by_start_within_a_pattern() {
        let text = "It stands as a testament\u{2014}a pivotal moment\u{2014}to the team.";
        let flags = scan(text);
        let order: BTreeMap<&str, usize> = pattern_ids()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();
        let positions: Vec<(usize, usize)> = flags
            .iter()
            .map(|flag| {
                (
                    order[pattern_ids_of(flag)[0].as_str()],
                    flag.span.local_start,
                )
            })
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "{flags:#?}");
        assert!(flags.len() >= 4, "{flags:#?}");
    }
}
