//! Matches text against the hand-encoded AI-telltale catalog vendored at
//! `src-tauri/resources/ai-telltales/patterns.json`. Provenance, licensing, and the reasoning
//! behind every narrowed or dropped pattern are recorded in
//! `src-tauri/resources/ai-telltales/NOTICE.md`, not repeated here.

use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

use crate::flag::{Flag, FlagOrigin, Span};

// Compiled in rather than read from the resource directory at run time, so the app and `wa`
// always match against the same catalogue, and one fewer file can be missing at startup.
const CATALOG_JSON: &str =
    include_str!("../../../../src-tauri/resources/ai-telltales/patterns.json");

#[derive(Debug, Deserialize)]
struct TelltalePattern {
    id: String,
    description: String,
    #[serde(default)]
    match_phrases: Vec<String>,
    match_regex: Option<String>,
    source: String,
}

struct CompiledPattern {
    pattern: TelltalePattern,
    /// The pattern's `match_regex` if it has one, then one regex per entry of `match_phrases`.
    matchers: Vec<Regex>,
}

fn catalog() -> &'static Vec<CompiledPattern> {
    static CATALOG: OnceLock<Vec<CompiledPattern>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let patterns: Vec<TelltalePattern> = serde_json::from_str(CATALOG_JSON).expect(
            "src-tauri/resources/ai-telltales/patterns.json is committed and checked in CI",
        );
        patterns
            .into_iter()
            .map(|pattern| {
                let phrase_sources = pattern
                    .match_phrases
                    .iter()
                    .map(|phrase| phrase_regex_source(phrase));
                let matchers = pattern
                    .match_regex
                    .iter()
                    .cloned()
                    .chain(phrase_sources)
                    .map(|source| {
                        Regex::new(&source).unwrap_or_else(|error| {
                            panic!("invalid regex in ai-tell pattern {:?}: {error}", pattern.id)
                        })
                    })
                    .collect();
                CompiledPattern { pattern, matchers }
            })
            .collect()
    })
}

/// The regex source for one catalogued phrase. The match is case-insensitive and runs against
/// the text as written, so every offset it yields indexes that text. Lowercasing a copy and
/// searching that instead yields offsets into the copy, which are wrong for the original wherever
/// lowercasing changes a character's byte length.
///
/// An edge of the phrase that is a letter or digit must fall on a word boundary: without one,
/// "here is a" matches inside "there is a" and "boasts a" inside "boasts about". An edge that is
/// punctuation or a space carries no such requirement, so a dash is found between two words.
fn phrase_regex_source(phrase: &str) -> String {
    let boundary = |edge: Option<char>| {
        if edge.is_some_and(char::is_alphanumeric) {
            r"\b"
        } else {
            ""
        }
    };
    format!(
        "(?i){}{}{}",
        boundary(phrase.chars().next()),
        regex::escape(phrase),
        boundary(phrase.chars().next_back()),
    )
}

/// The id of every pattern in the compiled catalogue, in catalogue order. The settings file keys
/// its telltale toggles by these ids.
pub fn pattern_ids() -> impl Iterator<Item = &'static str> {
    catalog()
        .iter()
        .map(|compiled| compiled.pattern.id.as_str())
}

/// Scans `text` for catalogued AI-tell patterns, returning one [`Flag`] per match, so a phrase
/// that occurs twice is flagged twice.
///
/// Every flag anchors on `text` itself and carries the match's own offset into it, the same
/// anchoring [`crate::spelling`] and [`crate::languagetool`] use. An anchor resolves to its first
/// occurrence in the document, so a flag anchored on its bare match, a single emoji or a dash,
/// would address the first identical match in the document instead of its own. The analyzer
/// hands this function one sentence at a time, so that sentence is the anchor.
pub fn scan(text: &str) -> Vec<Flag> {
    let mut flags = Vec::new();
    for compiled in catalog() {
        for (matcher_index, matcher) in compiled.matchers.iter().enumerate() {
            for found in matcher.find_iter(text) {
                // The offset closes the id: it is the one thing that differs between two matches
                // of the same matcher in one `text`.
                let local_start = text[..found.start()].encode_utf16().count();
                flags.push(Flag {
                    id: format!(
                        "ai-tell:{}:{matcher_index}:{local_start}",
                        compiled.pattern.id
                    ),
                    origin: FlagOrigin::AiTell,
                    span: Span {
                        anchor: text.to_string(),
                        local_start,
                        local_length: found.as_str().encode_utf16().count(),
                    },
                    message: compiled.pattern.description.clone(),
                    suggestions: Vec::new(),
                    source_detail: compiled.pattern.source.clone(),
                    ruleset_version: None,
                    catalogue_hash: None,
                });
            }
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;

    /// The exact text a flag's span addresses, sliced back out of its own anchor, so a test can
    /// assert on what was flagged without depending on how the span happens to be anchored.
    fn flagged_text(flag: &Flag) -> String {
        let anchor: Vec<u16> = flag.span.anchor.encode_utf16().collect();
        let end = flag.span.local_start + flag.span.local_length;
        String::from_utf16_lossy(&anchor[flag.span.local_start..end])
    }

    fn flags_of<'a>(flags: &'a [Flag], pattern_id: &str) -> Vec<&'a Flag> {
        let prefix = format!("ai-tell:{pattern_id}:");
        flags
            .iter()
            .filter(|flag| flag.id.starts_with(&prefix))
            .collect()
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
    fn flags_a_catalogued_phrase() {
        let flags = scan("This project's success is a testament to the team's hard work.");
        assert!(
            flags
                .iter()
                .any(|flag| flag.source_detail.contains("section 1")),
            "expected the significance-legacy-emphasis pattern to fire: {flags:#?}"
        );
        let flag = flags
            .iter()
            .find(|flag| flag.id.starts_with("ai-tell:significance-legacy-emphasis"))
            .unwrap();
        assert_eq!(flag.origin, FlagOrigin::AiTell);
    }

    #[test]
    fn does_not_flag_a_sentence_with_no_catalogued_pattern() {
        let flags = scan("The cat sat on the warm windowsill and watched the rain.");
        assert!(flags.is_empty(), "expected no flags: {flags:#?}");
    }

    #[test]
    fn flags_an_em_dash() {
        let flags = scan("The policy—announced without warning—affects everyone.");
        assert!(flags
            .iter()
            .any(|flag| flag.id.starts_with("ai-tell:em-en-dashes")));
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
        assert!(flags
            .iter()
            .any(|flag| flag.id.starts_with("ai-tell:emojis")));
    }

    #[test]
    fn two_identical_regex_matches_anchor_on_the_scanned_text_at_their_own_offsets() {
        // Anchored on the bare match, both flags would resolve to the first rocket in the
        // document, since an anchor resolves to its first occurrence.
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
        let matches: Vec<_> = flags
            .iter()
            .filter(|flag| flag.id.starts_with("ai-tell:inline-header-lists"))
            .collect();
        assert_eq!(
            matches.len(),
            2,
            "expected both bulleted items to match: {flags:#?}"
        );
    }

    #[test]
    fn flags_a_superficial_ing_ending_only_after_a_comma() {
        let flagged = scan("The design uses blue and gold, symbolizing the coastline.");
        assert!(flagged
            .iter()
            .any(|flag| flag.id.starts_with("ai-tell:superficial-ing-endings")));

        let not_flagged = scan("The team is symbolizing nothing in particular today.");
        assert!(
            !not_flagged
                .iter()
                .any(|flag| flag.id.starts_with("ai-tell:superficial-ing-endings")),
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
    fn every_catalogue_phrase_belongs_to_exactly_one_pattern() {
        let mut owners: HashMap<String, &str> = HashMap::new();
        for compiled in catalog() {
            for phrase in &compiled.pattern.match_phrases {
                let previous = owners.insert(phrase.to_lowercase(), &compiled.pattern.id);
                assert!(
                    previous.is_none(),
                    "phrase {phrase:?} is listed under {previous:?} and again under {:?}",
                    compiled.pattern.id
                );
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
        assert_eq!(seen.len(), catalog().len());
    }

    #[test]
    fn every_catalog_entry_has_at_least_one_match_signal() {
        for compiled in catalog() {
            assert!(
                !compiled.matchers.is_empty(),
                "pattern {:?} has neither match_phrases nor match_regex",
                compiled.pattern.id
            );
        }
    }
}
