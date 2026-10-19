//! The HTTP client against a running LanguageTool subprocess, the shape of its `/v2/check`
//! response, and the route of each category of its rules. The field names and shapes below were
//! verified against a running LanguageTool 6.6 server. Turning matches into flags is `routing`'s.

use std::time::Duration;

use serde::Deserialize;

use super::error::LanguageToolError;

/// Bounds a single request so a stalled subprocess cannot block a caller indefinitely. Sized for
/// the warm-up check in [`super::process::warm_up`], not steady-state traffic: LanguageTool
/// lazily loads its `en-GB` rule set on the first real check rather than at startup, measured
/// directly at 5.5 seconds cold against well under 100 ms once warm, and the warm-up call pays
/// that cost with this same timeout applied. The grammar stage's checks after warm-up run far
/// inside it; this is a last-resort ceiling, not the target latency.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// LanguageTool's own category for rules that flag a misspelling (for example
/// `MORFOLOGIK_RULE_EN_GB`). Dropped by the routing, endorsed or not: [`crate::spelling`]
/// already covers spelling against the vendored `en_GB` dictionary, and surfacing a second,
/// independent opinion on the same word from LanguageTool's own dictionary would produce
/// inconsistent flags on the same span from two different word lists for the same kind of error.
const SPELLING_CATEGORY_ID: &str = "TYPOS";

/// The tag LanguageTool puts on a rule that runs only in picky mode, listed in the rule's `tags`.
const PICKY_TAG: &str = "picky";

/// How a match routes when no checker names its rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CategoryRoute {
    /// An error: the match is a grammar flag, which is hard.
    Hard,
    /// A matter of style: the match surfaces only through a rule the owner endorsed.
    Soft,
    /// Spelling's: the match is dropped.
    Spelling,
}

/// Every category id of LanguageTool 6.6's rules for `en-GB`, with its route. The source is the
/// four XML rule files the `en-GB` variant loads, `grammar.xml` and `style.xml` under
/// `org/languagetool/rules/en/` and the two of the same names under its `en-GB/` folder, and the
/// Java-coded rules the variant loads, which add `MISC` and `TEXT_ANALYSIS`. `en_gb_categories.txt`
/// beside this file lists the ids with the extraction that produced them, and a test holds this
/// table to it.
///
/// #98 decided the hard categories, those of errors, and the soft ones, those of style. Every other
/// category the files hold is soft, so that a category no decision covers surfaces only through an
/// endorsed rule, and a category missing here, which a later release can add, routes as soft too.
/// No 6.6 category appears only in picky mode: each picky rule sits in a category that also holds
/// rules of the default level, the hard categories among them.
const CATEGORY_ROUTES: [(&str, CategoryRoute); 23] = [
    ("AMERICAN_ENGLISH", CategoryRoute::Hard),
    ("APRIL", CategoryRoute::Soft),
    ("BRE_STYLE_OXFORD_SPELLING", CategoryRoute::Soft),
    ("CASING", CategoryRoute::Hard),
    ("COLLOCATIONS", CategoryRoute::Soft),
    ("COMPOUNDING", CategoryRoute::Hard),
    ("CONFUSED_WORDS", CategoryRoute::Hard),
    ("CREATIVE_WRITING", CategoryRoute::Soft),
    ("GRAMMAR", CategoryRoute::Hard),
    ("MISC", CategoryRoute::Hard),
    ("MULTITOKEN_SPELLING", CategoryRoute::Soft),
    ("NONSTANDARD_PHRASES", CategoryRoute::Soft),
    ("PLAIN_ENGLISH", CategoryRoute::Soft),
    ("PUNCTUATION", CategoryRoute::Hard),
    ("REDUNDANCY", CategoryRoute::Soft),
    ("REPETITIONS_STYLE", CategoryRoute::Soft),
    ("SEMANTICS", CategoryRoute::Hard),
    ("STYLE", CategoryRoute::Soft),
    ("TEXT_ANALYSIS", CategoryRoute::Soft),
    ("TON_ACADEMIC", CategoryRoute::Soft),
    ("TYPOGRAPHY", CategoryRoute::Hard),
    (SPELLING_CATEGORY_ID, CategoryRoute::Spelling),
    ("WIKIPEDIA", CategoryRoute::Soft),
];

/// The route of `category_id` in [`CATEGORY_ROUTES`], soft for a category the table does not hold.
pub(super) fn category_route(category_id: &str) -> CategoryRoute {
    CATEGORY_ROUTES
        .iter()
        .find(|(id, _)| *id == category_id)
        .map_or(CategoryRoute::Soft, |(_, route)| *route)
}

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

    /// True if `/v2/languages` answers at all, regardless of its content. Used only by the
    /// readiness poll that follows a spawn, at startup and after a restart, never as a substitute
    /// for [`Self::check`]. Nothing polls a server that has already answered: supervision waits on
    /// the process exiting.
    pub(crate) async fn languages_reachable(&self) -> bool {
        self.http
            .get(format!("{}/v2/languages", self.base_url))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    /// LanguageTool's matches for `text`, one sentence: the routing anchors each match's flag on
    /// the match's sentence with its offset, counted from the start of the text sent, which holds
    /// only when the two are one string.
    ///
    /// # Errors
    /// When the request fails or its response does not parse, which fails the check for its
    /// sentence.
    pub(super) async fn check(&self, text: &str) -> Result<Vec<Match>, LanguageToolError> {
        let response = self.check_request(text).send().await?.error_for_status()?;
        let body: CheckResponse = response.json().await?;
        Ok(body.matches)
    }

    fn check_request(&self, text: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}/v2/check", self.base_url))
            .query(&[("language", "en-GB"), ("level", "picky"), ("text", text)])
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CheckResponse {
    matches: Vec<Match>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Match {
    pub(super) message: String,
    pub(super) replacements: Vec<Replacement>,
    /// In UTF-16 code units, from the start of the text sent.
    pub(super) offset: usize,
    pub(super) length: usize,
    pub(super) sentence: String,
    pub(super) rule: Rule,
}

#[derive(Debug, Deserialize)]
pub(super) struct Replacement {
    pub(super) value: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Rule {
    pub(super) id: String,
    pub(super) category: Category,
    /// Absent from a rule that carries no tag.
    #[serde(default)]
    tags: Vec<String>,
}

impl Rule {
    pub(super) fn is_picky(&self) -> bool {
        self.tags.iter().any(|tag| tag == PICKY_TAG)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Category {
    pub(super) id: String,
}

/// Responses captured from a running LanguageTool 6.6 server, trimmed to the fields this module
/// reads.
#[cfg(test)]
pub(super) mod fixtures {
    use super::{CheckResponse, Match};

    /// "I gotten used to it." against `language=en-GB`.
    pub(in crate::languagetool) const GOT_GOTTEN_RESPONSE: &str = r#"{
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

    /// "My favorite color is red." against `language=en-GB`: two matches, both from LanguageTool's
    /// own spelling rule.
    pub(in crate::languagetool) const AMERICAN_SPELLING_RESPONSE: &str = r#"{
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

    /// "The report was written by the committee." against `language=en-GB` and `level=picky`: one
    /// match, from a picky style rule. At the default level the same request returns no match.
    pub(in crate::languagetool) const PASSIVE_VOICE_RESPONSE: &str = r#"{
        "matches": [
            {
                "message": "For a more expressive style, consider rephrasing the sentence in the active voice.",
                "replacements": [{"value": "the committee wrote the report"}],
                "offset": 0,
                "length": 39,
                "sentence": "The report was written by the committee.",
                "rule": {"id": "PASSIVE_VOICE_SIMPLE", "category": {"id": "STYLE"}, "tags": ["picky"]}
            }
        ]
    }"#;

    /// "She said \"yes\" to the plan." against `language=en-GB` and `level=picky`: one match, from
    /// a picky rule in a hard category. At the default level the same request returns no match.
    pub(in crate::languagetool) const SMART_QUOTES_RESPONSE: &str = r#"{
        "matches": [
            {
                "message": "Consider using typographic quotation marks here.",
                "replacements": [{"value": "“yes”"}],
                "offset": 9,
                "length": 5,
                "sentence": "She said \"yes\" to the plan.",
                "rule": {"id": "EN_QUOTES", "category": {"id": "TYPOGRAPHY"}, "tags": ["picky"]}
            }
        ]
    }"#;

    /// "This is a very unique idea." against `language=en-GB` and `level=picky`: one match, from a
    /// style rule of the default level.
    pub(in crate::languagetool) const VERY_UNIQUE_RESPONSE: &str = r#"{
        "matches": [
            {
                "message": "Consider using “unique”.",
                "replacements": [{"value": "unique"}],
                "offset": 10,
                "length": 11,
                "sentence": "This is a very unique idea.",
                "rule": {"id": "VERY_UNIQUE", "category": {"id": "REDUNDANCY"}}
            }
        ]
    }"#;

    pub(in crate::languagetool) fn matches_in(response: &str) -> Vec<Match> {
        serde_json::from_str::<CheckResponse>(response)
            .expect("a captured response deserialises")
            .matches
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::fixtures::*;
    use super::*;

    /// The category ids extracted from the 6.6 rule files, with the extraction in its header.
    const EXTRACTED_CATEGORIES: &str = include_str!("en_gb_categories.txt");

    #[test]
    fn a_check_request_carries_picky_mode_beside_the_en_gb_variant() {
        let request = LanguageToolClient::new(8081)
            .check_request("A sentence.")
            .build()
            .unwrap();
        let query: Vec<(String, String)> = request.url().query_pairs().into_owned().collect();
        let expected = [
            ("language", "en-GB"),
            ("level", "picky"),
            ("text", "A sentence."),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()));
        assert_eq!(query, expected);
    }

    #[test]
    fn the_route_table_marks_every_category_the_rule_files_hold_and_no_other() {
        let extracted: BTreeSet<&str> = EXTRACTED_CATEGORIES
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let marked: BTreeSet<&str> = CATEGORY_ROUTES.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            marked.len(),
            CATEGORY_ROUTES.len(),
            "a category is marked twice"
        );
        assert_eq!(marked, extracted);
    }

    #[test]
    fn the_decided_lists_route_as_decided_and_every_other_category_as_soft() {
        let hard: BTreeSet<&str> = CATEGORY_ROUTES
            .iter()
            .filter(|(_, route)| *route == CategoryRoute::Hard)
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(
            hard,
            BTreeSet::from([
                "GRAMMAR",
                "PUNCTUATION",
                "CASING",
                "TYPOGRAPHY",
                "CONFUSED_WORDS",
                "COMPOUNDING",
                "SEMANTICS",
                "MISC",
                "AMERICAN_ENGLISH",
            ])
        );
        for soft in [
            "STYLE",
            "REDUNDANCY",
            "PLAIN_ENGLISH",
            "REPETITIONS_STYLE",
            "COLLOCATIONS",
            "WIKIPEDIA",
        ] {
            assert_eq!(category_route(soft), CategoryRoute::Soft, "{soft}");
        }
        assert_eq!(category_route("TYPOS"), CategoryRoute::Spelling);
        assert_eq!(category_route("A_CATEGORY_NOT_IN_6_6"), CategoryRoute::Soft);
    }

    #[test]
    fn a_picky_rule_is_read_from_its_tags() {
        let passive = matches_in(PASSIVE_VOICE_RESPONSE);
        assert_eq!(passive[0].rule.id, "PASSIVE_VOICE_SIMPLE");
        assert!(passive[0].rule.is_picky());
        assert!(!matches_in(GOT_GOTTEN_RESPONSE)[0].rule.is_picky());
    }
}
