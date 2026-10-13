//! Matches text against the hand-encoded AI-telltale catalog vendored at
//! `resources/ai-telltales/patterns.json`. Provenance, licensing, and the reasoning behind every
//! narrowed or dropped pattern are recorded in `resources/ai-telltales/NOTICE.md`, not repeated
//! here.

use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;
use unicode_segmentation::UnicodeSegmentation;

use crate::flag::{Flag, FlagOrigin, Span};

const CATALOG_JSON: &str = include_str!("../../resources/ai-telltales/patterns.json");

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
    regex: Option<Regex>,
    /// One per entry of `match_phrases`, in the same order.
    phrases: Vec<Regex>,
}

fn catalog() -> &'static Vec<CompiledPattern> {
    static CATALOG: OnceLock<Vec<CompiledPattern>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let patterns: Vec<TelltalePattern> = serde_json::from_str(CATALOG_JSON)
            .expect("resources/ai-telltales/patterns.json is committed and checked in CI");
        patterns
            .into_iter()
            .map(|pattern| {
                let compile = |source: &str| {
                    Regex::new(source).unwrap_or_else(|error| {
                        panic!("invalid regex in ai-tell pattern {:?}: {error}", pattern.id)
                    })
                };
                let regex = pattern.match_regex.as_deref().map(compile);
                let phrases = pattern
                    .match_phrases
                    .iter()
                    .map(|phrase| compile(&phrase_regex_source(phrase)))
                    .collect();
                CompiledPattern {
                    pattern,
                    regex,
                    phrases,
                }
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

/// Scans `text` for catalogued AI-tell patterns, returning one [`Flag`] per match, so a phrase
/// that occurs twice in a sentence is flagged twice.
///
/// A phrase-based pattern is checked sentence by sentence, so its flags anchor on the sentence
/// they were found in, the same anchoring [`crate::spelling`] and [`crate::languagetool`] use. A
/// regex-based pattern runs against the whole of `text` directly instead: some, such as the
/// inline-header-list pattern, use multiline anchors that only mean something against the full
/// text, not one sentence at a time, so its flags anchor on the matched substring itself rather
/// than a containing sentence.
pub fn scan(text: &str) -> Vec<Flag> {
    catalog()
        .iter()
        .flat_map(|compiled| match &compiled.regex {
            Some(regex) => scan_with_regex(compiled, regex, text),
            None => scan_with_phrases(compiled, text),
        })
        .collect()
}

fn scan_with_phrases(compiled: &CompiledPattern, text: &str) -> Vec<Flag> {
    let mut flags = Vec::new();
    for (sentence_index, sentence) in text.unicode_sentences().enumerate() {
        for (phrase_index, phrase) in compiled.phrases.iter().enumerate() {
            for found in phrase.find_iter(sentence) {
                // The offset closes the id: it is the one thing that differs between two
                // occurrences of the same phrase in one sentence.
                let local_start = sentence[..found.start()].encode_utf16().count();
                let local_length = found.as_str().encode_utf16().count();
                flags.push(build_flag(
                    compiled,
                    &format!("{sentence_index}:{phrase_index}:{local_start}"),
                    sentence,
                    local_start,
                    local_length,
                ));
            }
        }
    }
    flags
}

fn scan_with_regex(compiled: &CompiledPattern, regex: &Regex, text: &str) -> Vec<Flag> {
    regex
        .find_iter(text)
        .enumerate()
        .map(|(match_index, found)| {
            build_flag(
                compiled,
                &match_index.to_string(),
                found.as_str(),
                0,
                found.as_str().encode_utf16().count(),
            )
        })
        .collect()
}

fn build_flag(
    compiled: &CompiledPattern,
    id_suffix: &str,
    anchor: &str,
    local_start: usize,
    local_length: usize,
) -> Flag {
    Flag {
        id: format!("ai-tell:{}:{id_suffix}", compiled.pattern.id),
        origin: FlagOrigin::AiTell,
        span: Span {
            anchor: anchor.to_string(),
            local_start,
            local_length,
        },
        message: compiled.pattern.description.clone(),
        suggestions: Vec::new(),
        source_detail: compiled.pattern.source.clone(),
    }
}

#[cfg(test)]
mod tests {
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
    fn every_catalog_entry_has_at_least_one_match_signal() {
        for compiled in catalog() {
            assert!(
                compiled.regex.is_some() || !compiled.pattern.match_phrases.is_empty(),
                "pattern {:?} has neither match_phrases nor match_regex",
                compiled.pattern.id
            );
        }
    }
}
