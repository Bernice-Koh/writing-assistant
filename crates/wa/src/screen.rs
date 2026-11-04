//! `wa screen`: the engine's screen over JSONL, the bench's way to its one implementation.
//!
//! Each input line is a JSON object with `id`, any JSON value, and `passage`, `candidate` and
//! `register`, each a string; any other field is ignored. Each line gets one output line, in input
//! order, written and flushed as it completes. A screened line is its id beside the
//! [`Screening`]'s fields. A line that cannot be screened is its id, or `null` where none could be
//! read, beside an error with a class and a message: `invalidInput` for a line that is not JSON, not
//! an object, or lacks a field or holds one of the wrong type, and `grammarCheckDidNotRun` for the
//! screen's [`ScreenError::GrammarCheckDidNotRun`]. The output is UTF-8 with LF endings. Input lines
//! may end in CRLF, and a UTF-8 byte order mark before the first is skipped.
//!
//! No error message quotes a line's text. A line that is not JSON is described by serde_json's
//! syntax error, which names a category and a position, and a line of the wrong shape by the field
//! at fault. Parsing into a [`Value`] raises no error of the kind that quotes a value, as typed
//! deserialisation's "invalid type" does. Nothing here logs, and the input line has no `Debug`.

use std::io::{self, BufRead, Write};
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use wa_engine::flag::RulesetVersion;
use wa_engine::screen::{Screen, ScreenError, Screening};
use wa_engine::store::{config, Store, StoreError};
use wa_engine::style::ai_tell::Catalogue;
use wa_engine::style::ruleset::LoadedRuleset;

const BYTE_ORDER_MARK: &[u8] = b"\xEF\xBB\xBF";

/// A failure that stops a run partway: standard input or standard output failing.
#[derive(Debug, Error)]
pub enum StreamError {
    #[error("could not read standard input: {0}")]
    Read(#[source] io::Error),
    #[error("could not write standard output: {0}")]
    Write(#[source] io::Error),
}

/// The class of an error line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ErrorClass {
    InvalidInput,
    GrammarCheckDidNotRun,
}

/// The line written for a screened input line.
#[derive(Serialize)]
struct ResultLine<'a> {
    id: &'a Value,
    #[serde(flatten)]
    screening: &'a Screening,
}

/// The line written for an input line that could not be screened.
#[derive(Serialize)]
struct ErrorLine<'a> {
    id: &'a Value,
    error: LineError,
}

#[derive(Serialize)]
struct LineError {
    class: ErrorClass,
    message: String,
}

/// An input line's texts. No `Debug`, since the passage and the candidate are draft text.
struct Texts {
    passage: String,
    candidate: String,
    register: String,
}

/// An input line that is not a screenable object: its id where one could be read, and why.
struct Invalid {
    id: Value,
    message: String,
}

/// The ruleset `wa screen` runs under: `version` from `store` when one is named, and otherwise the
/// latest, which is an empty ruleset with no version when the store holds none.
///
/// # Errors
/// [`StoreError::NoRulesetVersion`] for a named version the store does not hold, and otherwise as
/// [`LoadedRuleset::load_blocking`].
pub fn ruleset(
    store: &Store,
    version: Option<RulesetVersion>,
) -> Result<LoadedRuleset, StoreError> {
    match version {
        Some(version) => LoadedRuleset::load_version_blocking(store, version),
        None => LoadedRuleset::load_blocking(store),
    }
}

/// The telltale catalogue compiled from the settings file under `data_root`, so the toggles in
/// force apply and each screening records the patterns they leave on. Where there is no settings
/// file, the defaults apply, and none is written.
///
/// # Errors
/// [`StoreError::Parse`] for a settings file that does not parse, since a bench must know what it
/// ran under, and [`StoreError::Read`] for one that cannot be read.
pub fn catalogue(data_root: &Path) -> Result<Catalogue, StoreError> {
    let settings = config::load_existing(data_root)?.unwrap_or_default();
    Ok(Catalogue::load(&settings))
}

/// Screens each line of `input` through `screen` and writes its result line or error line to
/// `output`, each flushed before the next line is read. Returns how many error lines it wrote.
///
/// # Errors
/// [`StreamError`] when `input` cannot be read or `output` cannot be written, which ends the run at
/// that line.
pub async fn screen_lines(
    screen: &Screen,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<usize, StreamError> {
    let mut errored = 0;
    let mut buffer = Vec::new();
    let mut first = true;
    loop {
        buffer.clear();
        if input
            .read_until(b'\n', &mut buffer)
            .map_err(StreamError::Read)?
            == 0
        {
            return Ok(errored);
        }
        let mut line = buffer.as_slice();
        if first {
            line = line.strip_prefix(BYTE_ORDER_MARK).unwrap_or(line);
            first = false;
        }
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);

        let written = match parse(line) {
            Ok((id, texts)) => match screen
                .run(&texts.passage, &texts.candidate, &texts.register)
                .await
            {
                Ok(screening) => serialised(&ResultLine {
                    id: &id,
                    screening: &screening,
                }),
                Err(ScreenError::GrammarCheckDidNotRun(source)) => {
                    errored += 1;
                    error_line(&id, ErrorClass::GrammarCheckDidNotRun, source.to_string())
                }
            },
            Err(Invalid { id, message }) => {
                errored += 1;
                error_line(&id, ErrorClass::InvalidInput, message)
            }
        };
        output
            .write_all(&written)
            .and_then(|()| output.flush())
            .map_err(StreamError::Write)?;
    }
}

/// The id and the texts of `line`, or why it cannot be screened.
fn parse(line: &[u8]) -> Result<(Value, Texts), Invalid> {
    let invalid = |id, message| Invalid { id, message };
    let mut fields = match serde_json::from_slice(line) {
        Ok(Value::Object(fields)) => fields,
        Ok(_) => {
            return Err(invalid(
                Value::Null,
                "the line is not a JSON object".to_owned(),
            ))
        }
        Err(error) => {
            return Err(invalid(
                Value::Null,
                format!("the line is not JSON: {error}"),
            ))
        }
    };
    let Some(id) = fields.remove("id") else {
        return Err(invalid(Value::Null, lacks("id")));
    };
    let texts = text_field(&mut fields, "passage").and_then(|passage| {
        Ok(Texts {
            passage,
            candidate: text_field(&mut fields, "candidate")?,
            register: text_field(&mut fields, "register")?,
        })
    });
    match texts {
        Ok(texts) => Ok((id, texts)),
        Err(message) => Err(invalid(id, message)),
    }
}

/// The string `fields` holds under `name`, or a message naming the field, and never its value.
fn text_field(fields: &mut Map<String, Value>, name: &str) -> Result<String, String> {
    match fields.remove(name) {
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(format!("the field {name:?} is not a string")),
        None => Err(lacks(name)),
    }
}

fn lacks(name: &str) -> String {
    format!("the line lacks the field {name:?}")
}

fn error_line(id: &Value, class: ErrorClass, message: String) -> Vec<u8> {
    serialised(&ErrorLine {
        id,
        error: LineError { class, message },
    })
}

/// `line` as one line of JSON ending in LF.
fn serialised(line: &impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(line).expect(
        "a line is maps with string keys, sequences, strings, numbers, booleans and nulls, none of \
         which fails to serialise",
    );
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;
    use wa_engine::flag::{Flag, FlagOrigin, Span};
    use wa_engine::languagetool::{GrammarSource, LanguageToolError};
    use wa_engine::screen::APP_VERSION;
    use wa_engine::spelling::{self, SpellChecker};
    use wa_engine::store::{CardMember, CheckerAuthor, ClauseWording, Config, Provenance};
    use wa_engine::style::checker::{
        Band, CheckableMode, Checker, FormMode, Matcher, PhraseEntry, Rule, Unit,
    };

    use super::*;
    use crate::test_support::TempDir;

    /// A grammar source with no LanguageTool behind it: a grammar flag on each occurrence of each
    /// flagged word, and a check that times out for a sentence holding the stalling word.
    struct FakeGrammar {
        flagged: &'static [&'static str],
        stalls_on: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl GrammarSource for FakeGrammar {
        async fn check(
            &self,
            sentence: &str,
            _: &LoadedRuleset,
            _: FormMode,
            _: &str,
        ) -> Result<Vec<Flag>, LanguageToolError> {
            if self.stalls_on.is_some_and(|word| sentence.contains(word)) {
                return Err(LanguageToolError::CheckTimeout { timeout_ms: 1000 });
            }
            let found = self
                .flagged
                .iter()
                .flat_map(|word| sentence.match_indices(word));
            Ok(found
                .enumerate()
                .map(|(index, (start, word))| Flag {
                    id: format!("grammar:{index}:TEST_RULE"),
                    origin: FlagOrigin::Grammar,
                    span: Span {
                        anchor: sentence.to_owned(),
                        local_start: sentence[..start].encode_utf16().count(),
                        local_length: word.encode_utf16().count(),
                        occurrence: 0,
                    },
                    message: "a grammar flag".to_owned(),
                    suggestions: Vec::new(),
                    source_detail: "TEST_RULE".to_owned(),
                    ruleset_version: None,
                    catalogue_hash: None,
                })
                .collect())
        }
    }

    /// The fake that flags `go` and stalls on `stalls`.
    const GRAMMAR: FakeGrammar = FakeGrammar {
        flagged: &["go"],
        stalls_on: Some("stalls"),
    };

    /// The keys of a result line: the id and the screening's eight fields.
    const RESULT_KEYS: [&str; 9] = [
        "appVersion",
        "catalogueHash",
        "enabledPatternIds",
        "hardResults",
        "id",
        "killingRule",
        "rulesetVersion",
        "softResults",
        "verdict",
    ];

    fn spell_checker() -> SpellChecker {
        let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/resources");
        let paths = spelling::default_paths(&resources);
        SpellChecker::load(&paths.aff, &paths.dic, &paths.supplement).unwrap()
    }

    fn screen_of(ruleset: LoadedRuleset, catalogue: Catalogue) -> Screen {
        Screen::new(
            Arc::new(spell_checker()),
            Arc::new(GRAMMAR),
            Arc::new(ruleset),
            Arc::new(catalogue),
        )
    }

    /// A screen over the fake, with no ruleset and the default telltale toggles.
    fn screen() -> Screen {
        screen_of(LoadedRuleset::empty(), Catalogue::load(&Config::default()))
    }

    /// One input line of `id`, with `text` as both the passage and the candidate.
    fn unchanged(id: Value, text: &str) -> String {
        json!({ "id": id, "passage": text, "candidate": text, "register": "default" }).to_string()
    }

    /// The bytes `screen` writes for `input`, and how many lines errored.
    async fn screened_bytes(screen: &Screen, input: &[u8]) -> (Vec<u8>, usize) {
        let mut output = Vec::new();
        let errored = screen_lines(screen, input, &mut output).await.unwrap();
        (output, errored)
    }

    /// The lines `screen` writes for `input`, each parsed, and how many errored.
    async fn screened(screen: &Screen, input: &str) -> (Vec<Value>, usize) {
        let (output, errored) = screened_bytes(screen, input.as_bytes()).await;
        let lines = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (lines, errored)
    }

    fn keys(line: &Value) -> BTreeSet<&str> {
        line.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// `line` without its id.
    fn without_id(line: &Value) -> Value {
        let mut line = line.clone();
        line.as_object_mut().unwrap().remove("id");
        line
    }

    #[tokio::test]
    async fn each_line_gets_its_id_beside_the_screenings_fields_in_input_order() {
        let screen = screen();
        let lines = [
            (
                json!(1),
                "We met at the pier.",
                "We met at the peir.",
                "default",
            ),
            (
                json!("second"),
                "It rains at noon.",
                "It rains at noon.",
                "default",
            ),
            (
                json!({ "batch": 3, "item": [1, 2] }),
                "He go home.",
                "He go home.",
                "email",
            ),
        ];
        let input: String = lines
            .iter()
            .map(|(id, passage, candidate, register)| {
                json!({
                    "id": id,
                    "passage": passage,
                    "candidate": candidate,
                    "register": register,
                    "note": "a field the contract ignores",
                })
                .to_string()
                    + "\n"
            })
            .collect();

        let (output, errored) = screened(&screen, &input).await;

        assert_eq!(errored, 0);
        assert_eq!(output.len(), lines.len());
        for (line, (id, passage, candidate, register)) in output.iter().zip(&lines) {
            assert_eq!(keys(line), BTreeSet::from(RESULT_KEYS));
            assert_eq!(&line["id"], id);
            let screening = screen.run(passage, candidate, register).await.unwrap();
            assert_eq!(without_id(line), serde_json::to_value(&screening).unwrap());
            assert_eq!(line["appVersion"], APP_VERSION);
            assert_eq!(line["rulesetVersion"], Value::Null);
        }
        assert_eq!(output[0]["verdict"], "fail");
        assert_eq!(output[0]["killingRule"], "spelling");
        assert_eq!(output[1]["verdict"], "pass");
        assert_eq!(output[1]["killingRule"], Value::Null);
        assert_eq!(output[2]["killingRule"], "grammar");
    }

    #[tokio::test]
    async fn a_line_of_the_wrong_shape_gets_an_invalid_input_line_and_the_rest_are_screened() {
        // Every line holds this word, which no error line may quote.
        const WORD: &str = "Zanzibar";
        let text = format!("{WORD} is warm.");
        let input = [
            unchanged(json!(1), &text),
            format!("{WORD} is not JSON"),
            String::new(),
            json!([WORD, "an array"]).to_string(),
            json!({ "id": 7, "passage": text, "candidate": text }).to_string(),
            json!({ "id": "typed", "passage": text, "candidate": 5, "register": WORD }).to_string(),
            json!({ "passage": text, "candidate": text, "register": "default" }).to_string(),
            unchanged(json!(2), &text),
        ]
        .join("\n");

        let (output, errored) = screened(&screen(), &input).await;

        assert_eq!(output.len(), 8);
        assert_eq!(errored, 6);
        let expected = [
            (json!(1), None),
            (
                Value::Null,
                Some("the line is not JSON: expected value at line 1 column 1"),
            ),
            (
                Value::Null,
                Some("the line is not JSON: EOF while parsing a value at line 1 column 0"),
            ),
            (Value::Null, Some("the line is not a JSON object")),
            (json!(7), Some("the line lacks the field \"register\"")),
            (
                json!("typed"),
                Some("the field \"candidate\" is not a string"),
            ),
            (Value::Null, Some("the line lacks the field \"id\"")),
            (json!(2), None),
        ];
        for (index, (line, (id, message))) in output.iter().zip(expected).enumerate() {
            assert_eq!(line["id"], id, "line {index}");
            match message {
                None => assert_eq!(keys(line), BTreeSet::from(RESULT_KEYS), "line {index}"),
                Some(message) => {
                    assert_eq!(
                        line,
                        &json!({
                            "id": id,
                            "error": { "class": "invalidInput", "message": message },
                        }),
                        "line {index}"
                    );
                    assert!(!line.to_string().contains(WORD), "line {index}");
                }
            }
        }
    }

    #[tokio::test]
    async fn no_syntax_error_message_quotes_the_line() {
        const WORD: &str = "Zanzibar";
        let malformed = [
            format!(r#"{{"id": 1, "passage": "{WORD}\q"}}"#),
            format!(r#"{{"id": 1, "passage": "{WORD}"}} {WORD}"#),
            format!(r#"{{"id": 1, "passage" "{WORD}"}}"#),
            format!(r#"{{"id": 1, "passage": "{WORD}"#),
            format!("{{\"id\": 1, \"passage\": \"{WORD}\u{1}\"}}"),
            format!(r#"{{"id": 1e999, "passage": "{WORD}"}}"#),
            format!(r#"{{"id": {WORD}}}"#),
        ];

        let (output, errored) = screened(&screen(), &malformed.join("\n")).await;

        assert_eq!(errored, malformed.len());
        for (line, input) in output.iter().zip(&malformed) {
            assert_eq!(line["id"], Value::Null, "{input}");
            assert_eq!(line["error"]["class"], "invalidInput", "{input}");
            let message = line["error"]["message"].as_str().unwrap();
            assert!(message.starts_with("the line is not JSON: "), "{message}");
            assert!(!message.contains(WORD), "{message}");
        }
    }

    #[tokio::test]
    async fn a_grammar_check_that_did_not_run_gives_an_error_line_of_its_class_and_no_result() {
        let input = [
            unchanged(json!("before"), "It rains at noon."),
            unchanged(json!("stalled"), "The server stalls here."),
            unchanged(json!("after"), "It rains at noon."),
        ]
        .join("\n");

        let (output, errored) = screened(&screen(), &input).await;

        assert_eq!(errored, 1);
        assert_eq!(
            output[1],
            json!({
                "id": "stalled",
                "error": {
                    "class": "grammarCheckDidNotRun",
                    "message": LanguageToolError::CheckTimeout { timeout_ms: 1000 }.to_string(),
                },
            })
        );
        for line in [&output[0], &output[2]] {
            assert_eq!(line["verdict"], "pass", "{line}");
        }
    }

    /// A checker forbidding `phrase` in a span, serving `clause_id`, in prose in every register.
    fn forbidding(clause_id: wa_engine::flag::ClauseId, phrase: &str) -> (Checker, Provenance) {
        let checker = Checker {
            clause_id,
            rule: Rule {
                matcher: Matcher::PhraseList {
                    entries: vec![PhraseEntry {
                        phrase: phrase.to_owned(),
                        prefix: false,
                    }],
                },
                unit: Unit::Span,
                band: Band::Forbidden,
                form_modes: BTreeSet::from([CheckableMode::Prose]),
                register_scope: None,
            },
        };
        let provenance = Provenance {
            author: CheckerAuthor::Template,
            back_translation: None,
            test_results: Vec::new(),
        };
        (checker, provenance)
    }

    /// Runs `future` to completion on a runtime of its own, for a test that first calls the
    /// store's blocking API, which panics inside a runtime.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn a_named_ruleset_version_and_the_latest_are_each_screened_under_and_recorded() {
        let root = TempDir::new();
        let store = Store::open(root.path()).unwrap();
        let card = store
            .insert_card_version_blocking(
                &[CardMember::New(ClauseWording {
                    text: "Hedge rarely.".to_owned(),
                    twin: "Hedge freely.".to_owned(),
                    register_scope: None,
                    example: None,
                })],
                "a Card written for the check",
            )
            .unwrap();
        let clause = store.card_version_blocking(card).unwrap().clauses[0].clause_id;
        let first = store
            .publish_ruleset_blocking(&[forbidding(clause, "perhaps")])
            .unwrap()
            .version;
        let second = store
            .publish_ruleset_blocking(&[forbidding(clause, "maybe")])
            .unwrap()
            .version;
        drop(store);
        let store = Store::open_existing(root.path()).unwrap();
        let input = unchanged(json!("hedged"), "Perhaps it rains.");

        let mut versions = Vec::new();
        for (named, expected, style_results) in [(Some(first), first, 1), (None, second, 0)] {
            let screen = screen_of(
                ruleset(&store, named).unwrap(),
                Catalogue::load(&Config::default()),
            );
            let (output, _) = block_on(screened(&screen, &input));
            assert_eq!(output[0]["rulesetVersion"], expected.0, "{named:?}");
            let style = output[0]["softResults"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|result| result["source"] == "style")
                .count();
            assert_eq!(style, style_results, "{named:?}");
            versions.push(output[0]["rulesetVersion"].clone());
        }
        assert_ne!(versions[0], versions[1]);

        let absent = RulesetVersion(second.0 + 1);
        assert!(matches!(
            ruleset(&store, Some(absent)),
            Err(StoreError::NoRulesetVersion { version }) if version == absent
        ));
    }

    #[test]
    fn a_store_with_no_ruleset_version_screens_under_none_and_records_null() {
        let root = TempDir::new();
        drop(Store::open(root.path()).unwrap());
        let store = Store::open_existing(root.path()).unwrap();
        let screen = screen_of(
            ruleset(&store, None).unwrap(),
            Catalogue::load(&Config::default()),
        );

        let (output, _) = block_on(screened(&screen, &unchanged(json!(1), "It rains at noon.")));

        assert_eq!(output[0]["rulesetVersion"], Value::Null);
    }

    /// The telltale pattern ids of `line`'s soft results.
    fn telltale_ids(line: &Value) -> BTreeSet<String> {
        line["softResults"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|result| result["source"] == "tells")
            .flat_map(|result| result["flag"]["patternIds"].as_array().unwrap().clone())
            .map(|id| id.as_str().unwrap().to_owned())
            .collect()
    }

    fn enabled_ids(line: &Value) -> BTreeSet<String> {
        line["enabledPatternIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn the_toggles_of_the_settings_file_under_the_data_root_are_in_force_and_recorded() {
        let input = unchanged(json!(1), "It stands as a testament to the team.");
        let root = TempDir::new();
        std::fs::create_dir_all(root.path()).unwrap();

        // No settings file: the defaults, and none written.
        let defaults = catalogue(root.path()).unwrap();
        assert!(!root.path().join("config.json").exists());
        let (on, _) = screened(&screen_of(LoadedRuleset::empty(), defaults), &input).await;
        let fired = telltale_ids(&on[0]);
        assert!(!fired.is_empty(), "{}", on[0]);

        let toggles: Map<String, Value> =
            fired.iter().map(|id| (id.clone(), json!(false))).collect();
        std::fs::write(
            root.path().join("config.json"),
            json!({ "telltale_toggles": toggles }).to_string(),
        )
        .unwrap();
        let (off, _) = screened(
            &screen_of(LoadedRuleset::empty(), catalogue(root.path()).unwrap()),
            &input,
        )
        .await;

        assert!(telltale_ids(&off[0]).is_empty(), "{}", off[0]);
        assert_eq!(off[0]["verdict"], "pass");
        let enabled_on = enabled_ids(&on[0]);
        let enabled_off = enabled_ids(&off[0]);
        assert!(enabled_off.is_disjoint(&fired));
        assert_eq!(
            enabled_on
                .difference(&enabled_off)
                .cloned()
                .collect::<BTreeSet<_>>(),
            fired
        );
        assert_eq!(off[0]["catalogueHash"], on[0]["catalogueHash"]);

        std::fs::write(root.path().join("config.json"), "{ \"telltale_toggles\": ").unwrap();
        assert!(matches!(
            catalogue(root.path()),
            Err(StoreError::Parse { .. })
        ));
    }

    #[tokio::test]
    async fn the_same_lines_screened_twice_under_one_screen_are_byte_identical() {
        let screen = screen();
        let input = [
            r#"{"id": 1, "passage": "Gate 9 opens. The eror stays.", "candidate": "Gate 8 opens. The eror stays. He go home.", "register": "default"}"#,
            r#"{"id": 2, "passage": "Perhaps it rains.", "candidate": "It stands as a testament to the team.", "register": "email"}"#,
            r#"{"id": 3, "passage": "It rains.", "candidate": "The server stalls here.", "register": "default"}"#,
            "not JSON",
        ]
        .join("\n");

        let (first, first_errored) = screened_bytes(&screen, input.as_bytes()).await;
        let (second, second_errored) = screened_bytes(&screen, input.as_bytes()).await;

        assert_eq!(first, second);
        assert_eq!((first_errored, second_errored), (2, 2));
    }

    #[tokio::test]
    async fn the_output_is_utf8_with_lf_endings_whatever_the_input_ends_with() {
        let mut input = BYTE_ORDER_MARK.to_vec();
        input.extend_from_slice(unchanged(json!("café"), "Ms Tan’s café is naïve.").as_bytes());
        input.extend_from_slice(b"\r\n");
        input.extend_from_slice(unchanged(json!(2), "It rains at noon.").as_bytes());
        input.extend_from_slice(b"\r\n{\"id\": 3, \"passage\": \"caf\xE9\"}\n");
        input.extend_from_slice(unchanged(json!(4), "It rains at noon.").as_bytes());

        let (output, errored) = screened_bytes(&screen(), &input).await;

        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains('\r'));
        assert!(text.ends_with('\n'));
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["id"], "café");
        assert!(text.contains("\"id\":\"café\""), "{text}");
        assert_eq!(lines[1]["verdict"], "pass");
        assert_eq!(lines[2]["error"]["class"], "invalidInput");
        assert_eq!(lines[3]["id"], 4);
        assert_eq!(errored, 1);
    }
}
