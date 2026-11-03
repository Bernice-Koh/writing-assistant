//! A grammar source with no LanguageTool behind it, which the analyzer's tests and the screen's
//! share.

use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::flag::{Flag, FlagOrigin, RulesetVersion, Span};
use crate::languagetool::{GrammarSource, LanguageToolError};
use crate::style::checker::FormMode;
use crate::style::ruleset::LoadedRuleset;

/// What [`FakeGrammar`] answers a check with.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Answer {
    /// A sentence's captured response routed where it has one, and otherwise a grammar flag on
    /// each occurrence in the sentence of each of its flagged words.
    Flags,
    /// A request that failed.
    RequestFailure,
    /// A check past its bound.
    Timeout,
    /// The answer of a supervisor whose restarts are exhausted.
    Degraded,
}

/// A grammar source with no LanguageTool behind it. It records every sentence it is sent, answers
/// each as its [`Answer`] says, and while held keeps every check waiting until it is released.
pub(crate) struct FakeGrammar {
    flagged_words: Vec<&'static str>,
    /// Each sentence with a response captured from LanguageTool, which a check of that sentence
    /// routes through the routing the supervisor runs, under the ruleset, the mode and the
    /// register the check is given.
    captured_responses: Vec<(&'static str, &'static str)>,
    answer: Mutex<Answer>,
    sent: Mutex<Vec<String>>,
    held: watch::Sender<bool>,
}

impl FakeGrammar {
    /// A source that answers with a grammar flag on each of `flagged_words`, unheld.
    pub(crate) fn flagging(flagged_words: &[&'static str]) -> Arc<Self> {
        Self::new(flagged_words, &[])
    }

    /// A source that answers each sentence of `captured_responses` with its response routed, and
    /// any other sentence with no flag, unheld.
    pub(crate) fn routing(captured_responses: &[(&'static str, &'static str)]) -> Arc<Self> {
        Self::new(&[], captured_responses)
    }

    fn new(
        flagged_words: &[&'static str],
        captured_responses: &[(&'static str, &'static str)],
    ) -> Arc<Self> {
        Arc::new(Self {
            flagged_words: flagged_words.to_vec(),
            captured_responses: captured_responses.to_vec(),
            answer: Mutex::new(Answer::Flags),
            sent: Mutex::new(Vec::new()),
            held: watch::channel(false).0,
        })
    }

    pub(crate) fn answer_with(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    pub(crate) fn hold(&self) {
        self.held.send_replace(true);
    }

    pub(crate) fn release(&self) {
        self.held.send_replace(false);
    }

    /// Every sentence sent so far, in order.
    pub(crate) fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    /// The flags of a check of `sentence` that answers: one grammar flag per occurrence of a
    /// flagged word, numbered and anchored as LanguageTool's routing builds them, recording a
    /// version the analyzer does not hold and no catalogue hash.
    pub(crate) fn grammar_flags(&self, sentence: &str) -> Vec<Flag> {
        let mut found: Vec<(usize, &str)> = self
            .flagged_words
            .iter()
            .flat_map(|word| sentence.match_indices(word))
            .collect();
        found.sort();
        found
            .into_iter()
            .enumerate()
            .map(|(index, (byte_offset, word))| Flag {
                id: format!("grammar:{index}:TEST_RULE"),
                origin: FlagOrigin::Grammar,
                span: Span {
                    anchor: sentence.to_owned(),
                    local_start: sentence[..byte_offset].encode_utf16().count(),
                    local_length: word.encode_utf16().count(),
                    occurrence: 0,
                },
                message: "a grammar flag".to_owned(),
                suggestions: Vec::new(),
                source_detail: "TEST_RULE".to_owned(),
                ruleset_version: Some(RulesetVersion(99)),
                catalogue_hash: None,
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl GrammarSource for FakeGrammar {
    async fn check(
        &self,
        sentence: &str,
        ruleset: &LoadedRuleset,
        mode: FormMode,
        register: &str,
    ) -> Result<Vec<Flag>, LanguageToolError> {
        self.sent.lock().unwrap().push(sentence.to_owned());
        let mut held = self.held.subscribe();
        held.wait_for(|held| !held).await.unwrap();
        let answer = *self.answer.lock().unwrap();
        match answer {
            Answer::Flags => Ok(self
                .captured_responses
                .iter()
                .find(|(captured, _)| *captured == sentence)
                .map_or_else(
                    || self.grammar_flags(sentence),
                    |(_, response)| {
                        crate::languagetool::route_captured_response(
                            response, ruleset, mode, register,
                        )
                    },
                )),
            Answer::RequestFailure => {
                let unbuildable = reqwest::Client::new().get("not a url").build();
                Err(LanguageToolError::from(unbuildable.unwrap_err()))
            }
            Answer::Timeout => Err(LanguageToolError::CheckTimeout { timeout_ms: 1000 }),
            Answer::Degraded => Err(LanguageToolError::Degraded),
        }
    }
}
