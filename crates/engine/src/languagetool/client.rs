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

/// Bounds each steady-state check, every check after the warm-up, so that a server that stops
/// answering holds a recheck for this long per sentence rather than for [`REQUEST_TIMEOUT`]. A
/// check still unanswered at the bound fails for its sentence, a failure distinct from a check
/// that found nothing.
///
/// Set from a measurement on the development machine against the 6.6 server under the bundled
/// runtime: 1,000 warm checks in picky mode, one sentence per request and in sequence, as the
/// analyzer sends them, of 973 distinct sentences of three words or more, split as the analyzer
/// splits a document, from two public-domain books, a novel and a work of natural history. The
/// sentences ran to 141 words, with a median of 21 and a 95th percentile of 61. The checks took a
/// median of 16.7 ms, a 95th percentile of 32.7 ms, a 99th of 69.5 ms and at most 108.3 ms. The
/// median rose with length, from 13.4 ms up to ten words to 25.6 ms past fifty, and the five
/// slowest checks were of sentences of 30 to 72 words. One second is about nine times the slowest
/// check, a margin for a collection pause in the server's runtime or a loaded machine, at a cost of
/// one second a sentence while the server has stopped answering.
const STEADY_STATE_TIMEOUT: Duration = Duration::from_secs(1);

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

    /// LanguageTool's matches for `text`, one sentence, in a steady-state check bounded by
    /// [`STEADY_STATE_TIMEOUT`]. The routing anchors each match's flag on the match's sentence with
    /// its offset, counted from the start of the text sent, which holds only when the two are one
    /// string.
    ///
    /// # Errors
    /// [`LanguageToolError::CheckTimeout`] when the server does not answer inside the bound, and
    /// [`LanguageToolError::Request`] when the request fails or its response does not parse. Either
    /// fails the check for its sentence.
    pub(super) async fn check(&self, text: &str) -> Result<Vec<Match>, LanguageToolError> {
        self.check_within(text, STEADY_STATE_TIMEOUT).await
    }

    /// [`Self::check`] bounded by [`REQUEST_TIMEOUT`], for the warm-up check, which pays for
    /// LanguageTool's cold first check.
    ///
    /// # Errors
    /// As [`Self::check`].
    pub(super) async fn warm_up_check(&self, text: &str) -> Result<Vec<Match>, LanguageToolError> {
        self.check_within(text, REQUEST_TIMEOUT).await
    }

    async fn check_within(
        &self,
        text: &str,
        bound: Duration,
    ) -> Result<Vec<Match>, LanguageToolError> {
        let failed = |error| check_error(error, bound);
        let response = self
            .check_request(text, bound)
            .send()
            .await
            .map_err(failed)?
            .error_for_status()?;
        let body: CheckResponse = response.json().await.map_err(failed)?;
        Ok(body.matches)
    }

    fn check_request(&self, text: &str, bound: Duration) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}/v2/check", self.base_url))
            .query(&[("language", "en-GB"), ("level", "picky"), ("text", text)])
            .timeout(bound)
    }
}

/// `error`, from a check bounded by `bound`, as a timeout when the bound ran out.
fn check_error(error: reqwest::Error, bound: Duration) -> LanguageToolError {
    if error.is_timeout() {
        LanguageToolError::CheckTimeout {
            timeout_ms: bound.as_millis() as u64,
        }
    } else {
        error.into()
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::fixtures::*;
    use super::*;

    /// The category ids extracted from the 6.6 rule files, with the extraction in its header.
    const EXTRACTED_CATEGORIES: &str = include_str!("en_gb_categories.txt");

    /// An HTTP answer with `status` and `body`, which closes the connection.
    fn answer(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn empty_answer() -> String {
        answer("200 OK", r#"{"matches": []}"#)
    }

    /// A client of a local stub that stands in for LanguageTool. The stub reads each request and
    /// sends `reply` after its delay, or, given no reply, never answers and holds the connection
    /// open until the client drops it.
    fn client_of_stub(reply: Option<(Duration, String)>) -> LanguageToolClient {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let reply = reply.clone();
                thread::spawn(move || {
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|end| end == b"\r\n\r\n") {
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    match reply {
                        Some((delay, answer)) => {
                            thread::sleep(delay);
                            stream.write_all(answer.as_bytes()).ok();
                        }
                        None => while matches!(stream.read(&mut buffer), Ok(read) if read > 0) {},
                    }
                });
            }
        });
        LanguageToolClient::new(port)
    }

    fn is_steady_state_timeout(result: &Result<Vec<Match>, LanguageToolError>) -> bool {
        matches!(
            result,
            Err(LanguageToolError::CheckTimeout { timeout_ms })
                if u128::from(*timeout_ms) == STEADY_STATE_TIMEOUT.as_millis()
        )
    }

    #[test]
    fn a_check_request_carries_picky_mode_beside_the_en_gb_variant_and_its_bound() {
        let request = LanguageToolClient::new(8081)
            .check_request("A sentence.", STEADY_STATE_TIMEOUT)
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
        assert_eq!(request.timeout(), Some(&STEADY_STATE_TIMEOUT));
    }

    #[tokio::test]
    async fn a_check_the_server_never_answers_fails_at_the_steady_state_bound_unlike_an_empty_one()
    {
        let silent = client_of_stub(None);
        let started = std::time::Instant::now();
        let unanswered = silent.check("A sentence.").await;
        let waited = started.elapsed();
        assert!(is_steady_state_timeout(&unanswered), "{unanswered:?}");
        assert!(
            waited >= STEADY_STATE_TIMEOUT && waited < REQUEST_TIMEOUT,
            "{waited:?}"
        );

        let answering = client_of_stub(Some((Duration::ZERO, empty_answer())));
        let found_nothing = answering.check("A sentence.").await;
        assert!(
            found_nothing.as_ref().is_ok_and(Vec::is_empty),
            "{found_nothing:?}"
        );
    }

    #[tokio::test]
    async fn the_warm_up_check_and_the_readiness_poll_keep_their_twenty_second_bound() {
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(20));
        // Past the steady-state bound and far inside the warm-up's.
        let delay = STEADY_STATE_TIMEOUT + Duration::from_millis(500);
        let slow = client_of_stub(Some((delay, empty_answer())));

        let steady_state = slow.check("A sentence.").await;
        assert!(is_steady_state_timeout(&steady_state), "{steady_state:?}");
        let warm_up = slow.warm_up_check("Hello.").await;
        assert!(warm_up.as_ref().is_ok_and(Vec::is_empty), "{warm_up:?}");
        assert!(slow.languages_reachable().await);
    }

    #[tokio::test]
    async fn no_rendering_of_a_failed_check_carries_the_text_checked() {
        let text = "A sentence only its writer has seen.";
        let silent = client_of_stub(None);
        let failing = client_of_stub(Some((
            Duration::ZERO,
            answer("500 Internal Server Error", ""),
        )));
        let failures = [
            silent.check(text).await.unwrap_err(),
            failing.check(text).await.unwrap_err(),
        ];
        assert!(
            matches!(failures[1], LanguageToolError::Request(_)),
            "{failures:?}"
        );
        for failure in &failures {
            for rendering in [failure.to_string(), format!("{failure:?}")] {
                assert!(!rendering.contains("writer"), "{rendering}");
            }
        }
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
