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
}

fn catalog() -> &'static Vec<CompiledPattern> {
    static CATALOG: OnceLock<Vec<CompiledPattern>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let patterns: Vec<TelltalePattern> = serde_json::from_str(CATALOG_JSON)
            .expect("resources/ai-telltales/patterns.json is committed and checked in CI");
        patterns
            .into_iter()
            .map(|pattern| {
                let regex = pattern.match_regex.as_deref().map(|source| {
                    Regex::new(source).unwrap_or_else(|error| {
                        panic!("invalid regex in ai-tell pattern {:?}: {error}", pattern.id)
                    })
                });
                CompiledPattern { pattern, regex }
            })
            .collect()
    })
}

/// Scans `text` for catalogued AI-tell patterns, returning one [`Flag`] per match.
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
        // Lowercasing both sides for a case-insensitive match assumes case folding does not
        // change a string's byte length, true for the plain ASCII phrases and English sentences
        // this catalog deals in, so the byte offset found below still indexes correctly into the
        // original, not lowercased, `sentence`.
        let lower_sentence = sentence.to_lowercase();
        for (match_index, phrase) in compiled.pattern.match_phrases.iter().enumerate() {
            let Some(byte_start) = lower_sentence.find(&phrase.to_lowercase()) else {
                continue;
            };
            let local_start = sentence[..byte_start].encode_utf16().count();
            let local_length = phrase.encode_utf16().count();
            flags.push(build_flag(
                compiled,
                &format!("{sentence_index}:{match_index}"),
                sentence,
                local_start,
                local_length,
            ));
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
