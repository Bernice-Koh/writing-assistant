//! The HTTP client against a running LanguageTool subprocess, and the mapping from its `/v2/check`
//! response into this crate's own [`Flag`] representation. Response field names and shapes below
//! were verified directly against a running LanguageTool 6.6 server, not assumed from
//! documentation alone.

use std::time::Duration;

use serde::Deserialize;

use crate::flag::{Flag, FlagOrigin, Span};

use super::error::LanguageToolError;

/// Bounds a single request so a stalled subprocess cannot block a caller indefinitely. Sized for
/// the warm-up check in [`super::process::warm_up`], not steady-state traffic: LanguageTool
/// lazily loads its `en-GB` rule set on the first real check rather than at startup, measured
/// directly at 5.5 seconds cold against well under 100 ms once warm, and the warm-up call pays
/// that cost with this same timeout applied. Every check after warm-up stays far inside Tier
/// 0.5's hundreds-of-milliseconds budget; this is a last-resort ceiling, not the target latency.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// LanguageTool's own category for rules that flag a misspelling (for example
/// `MORFOLOGIK_RULE_EN_GB`). Filtered out of [`matches_to_flags`]'s output: [`crate::spelling`]
/// already covers spelling against the vendored `en_GB` dictionary, and surfacing a second,
/// independent opinion on the same word from LanguageTool's own dictionary would produce
/// inconsistent flags on the same span from two different word lists for the same kind of error.
/// Every other LanguageTool category, including `AMERICAN_ENGLISH` and `STYLE`, is kept: they are
/// not spelling's job.
const SPELLING_CATEGORY_ID: &str = "TYPOS";

pub struct LanguageToolClient {
    http: reqwest::Client,
    base_url: String,
}

impl LanguageToolClient {
    pub fn new(port: u16) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("a plain HTTP client with only a timeout set cannot fail to build"),
            base_url: format!("http://localhost:{port}"),
        }
    }

    /// True if `/v2/languages` answers at all, regardless of its content. Used only for startup
    /// and health-check readiness polling, never as a substitute for [`Self::check`].
    pub(crate) async fn languages_reachable(&self) -> bool {
        self.http
            .get(format!("{}/v2/languages", self.base_url))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    /// Checks `text`, which callers are expected to pass one sentence at a time: LanguageTool's
    /// own `offset` field in each match is counted from the start of whatever was submitted, and
    /// this function anchors every returned [`Flag`]'s span on `match.sentence` with `offset`
    /// used directly as the local start within it. That equivalence only holds when the submitted
    /// text and the matched sentence are the same string, which multi-sentence input would break.
    ///
    /// Returns `Err` only for a genuine request or parse failure; callers that want the
    /// "degrade to spelling and style flags alone" behaviour AC #38(d) asks for should treat any
    /// `Err` from this call the same as a down subprocess, not propagate it as a hard failure.
    pub async fn check(&self, text: &str) -> Result<Vec<Flag>, LanguageToolError> {
        let response = self
            .http
            .get(format!("{}/v2/check", self.base_url))
            .query(&[("language", "en-GB"), ("text", text)])
            .send()
            .await?
            .error_for_status()?;
        let body: CheckResponse = response.json().await?;
        Ok(matches_to_flags(body.matches))
    }
}

#[derive(Debug, Deserialize)]
struct CheckResponse {
    matches: Vec<Match>,
}

#[derive(Debug, Deserialize)]
struct Match {
    message: String,
    replacements: Vec<Replacement>,
    offset: usize,
    length: usize,
    sentence: String,
    rule: Rule,
}

#[derive(Debug, Deserialize)]
struct Replacement {
    value: String,
}

#[derive(Debug, Deserialize)]
struct Rule {
    id: String,
    category: Category,
}

#[derive(Debug, Deserialize)]
struct Category {
    id: String,
}

fn matches_to_flags(matches: Vec<Match>) -> Vec<Flag> {
    matches
        .into_iter()
        .filter(|found| found.rule.category.id != SPELLING_CATEGORY_ID)
        .enumerate()
        .map(|(index, found)| Flag {
            id: format!("grammar:{index}:{}", found.rule.id),
            origin: FlagOrigin::Grammar,
            span: Span {
                anchor: found.sentence,
                local_start: found.offset,
                local_length: found.length,
            },
            message: found.message,
            suggestions: found
                .replacements
                .into_iter()
                .map(|replacement| replacement.value)
                .collect(),
            source_detail: found.rule.id,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a real LanguageTool 6.6 server's response to checking "I gotten used to
    /// it." against `language=en-GB`, trimmed to the fields this module reads.
    const GOT_GOTTEN_RESPONSE: &str = r#"{
        "matches": [
            {
                "message": "“Gotten” is commonly used in American English. For varieties outside of North America, “got” is the preferred variant.",
                "replacements": [{"value": "got"}],
                "offset": 2,
                "length": 6,
                "sentence": "I gotten used to it.",
                "rule": {"id": "GOT_GOTTEN", "category": {"id": "AMERICAN_ENGLISH"}}
            }
        ]
    }"#;

    /// Captured from a real server's response to checking "My favorite color is red." against
    /// `language=en-GB`: two matches, both from LanguageTool's own spelling rule.
    const AMERICAN_SPELLING_RESPONSE: &str = r#"{
        "matches": [
            {
                "message": "Possible spelling mistake. ‘favorite’ is American English.",
                "replacements": [{"value": "favourite"}],
                "offset": 3,
                "length": 8,
                "sentence": "My favorite color is red.",
                "rule": {"id": "MORFOLOGIK_RULE_EN_GB", "category": {"id": "TYPOS"}}
            },
            {
                "message": "Possible spelling mistake. ‘color’ is American English.",
                "replacements": [{"value": "colour"}],
                "offset": 12,
                "length": 5,
                "sentence": "My favorite color is red.",
                "rule": {"id": "MORFOLOGIK_RULE_EN_GB", "category": {"id": "TYPOS"}}
            }
        ]
    }"#;

    #[test]
    fn maps_a_grammar_match_to_a_flag_with_a_utf16_span() {
        let response: CheckResponse = serde_json::from_str(GOT_GOTTEN_RESPONSE).unwrap();
        let flags = matches_to_flags(response.matches);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].origin, FlagOrigin::Grammar);
        assert_eq!(flags[0].span.anchor, "I gotten used to it.");
        assert_eq!(flags[0].span.local_start, 2);
        assert_eq!(flags[0].span.local_length, 6);
        assert_eq!(flags[0].suggestions, vec!["got".to_string()]);
        assert_eq!(flags[0].source_detail, "GOT_GOTTEN");
    }

    #[test]
    fn filters_out_languagetools_own_spelling_matches() {
        let response: CheckResponse = serde_json::from_str(AMERICAN_SPELLING_RESPONSE).unwrap();
        let flags = matches_to_flags(response.matches);
        assert!(
            flags.is_empty(),
            "TYPOS-category matches are spelling's job, not grammar's"
        );
    }

    #[test]
    fn an_empty_matches_list_produces_no_flags() {
        let response: CheckResponse = serde_json::from_str(r#"{"matches": []}"#).unwrap();
        assert!(matches_to_flags(response.matches).is_empty());
    }
}
