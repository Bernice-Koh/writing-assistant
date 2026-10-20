//! The analyzer, between capture and the overlay. It polls the document's text, waits for the
//! typing pause, splits the text into sentences, and checks the changed ones against spelling,
//! the telltale catalogue, and LanguageTool. Suppression by severity is applied to each sentence's
//! flags, the result is cached per sentence, and every recheck publishes the whole document's
//! flags on the watch channel the overlay subscribes to, each span's occurrence index counted from
//! its sentence's position as it publishes. Checking keys to the pause because a flag on a
//! half-typed word is noise.
//!
//! `sentences` covers the split and the diff, `dedupe` suppression. Both are private:
//! [`Analyzer`] is the only public surface.

mod dedupe;
mod sentences;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use lru::LruCache;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::capture::Capture;
use crate::flag::Flag;
use crate::languagetool::LanguageToolSupervisor;
use crate::spelling::SpellChecker;
use crate::style::ai_tell;
use crate::style::checker::FormMode;
use crate::style::ruleset::LoadedRuleset;

/// How often the debounce loop reads the capture backend's current text to look for a change.
/// The 200 ms poll stands until the capture backend's text-changed event triggers the recheck.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How long the document must sit unchanged before a recheck fires, so a burst of keystrokes
/// produces one recheck after typing pauses rather than one per keystroke, per AC #40(a).
const QUIET_THRESHOLD: Duration = Duration::from_millis(500);

/// How many sentences' worth of flags the LRU cache holds at once. Sized for a long document
/// (a few thousand words is on the order of a few hundred sentences) without growing unbounded
/// for the life of a long writing session.
const CACHE_CAPACITY: usize = 1000;

/// Runs the debounce, diff, check, merge, and cache pipeline against a [`Capture`] backend's
/// live text, keeping the latest merged and deduplicated [`Flag`] list for the whole document.
pub struct Analyzer {
    inner: Arc<Inner>,
}

struct Inner {
    capture: Arc<dyn Capture>,
    spelling: SpellChecker,
    languagetool: Option<LanguageToolSupervisor>,
    ruleset: Arc<LoadedRuleset>,
    register: String,
    flags: RwLock<Vec<Flag>>,
    /// Notifies a change to `flags` without carrying the flags themselves through the lock above:
    /// a subscriber (the overlay's own event-emitting task) reads the latest value straight from
    /// this channel via `borrow`, so there is exactly one source of truth, not two that could
    /// drift apart. Kept separate from `flags` rather than folding it into a single
    /// `watch`-backed field because `current_flags` is a plain, lock-based read with no
    /// subscription semantics, a simpler contract for a caller that only wants the value once.
    flags_tx: watch::Sender<Vec<Flag>>,
    cache: Mutex<LruCache<u64, Vec<Flag>>>,
    previous_sentences: Mutex<Vec<String>>,
    /// Counts calls into [`check_sentence`], the work a cache hit skips. Test-only, and held per
    /// instance rather than in a global, so tests running in parallel never share a count.
    #[cfg(test)]
    check_sentence_calls: std::sync::atomic::AtomicUsize,
}

impl Analyzer {
    /// Starts polling `capture` in the background. `languagetool` is `None` when the subprocess
    /// could not be started at all; a `Some` that later degrades fails every check, which
    /// `check_sentence` treats as no grammar flags this time. `ruleset` is the ruleset loaded at
    /// startup: every flag the analyzer publishes carries its version whatever its origin, `None`
    /// when no version is loaded, and LanguageTool's matches route through its checkers.
    /// `register` is the register every sentence is checked under, the settings' default register
    /// until capture supplies the surface's (#58).
    pub fn start(
        capture: Arc<dyn Capture>,
        spelling: SpellChecker,
        languagetool: Option<LanguageToolSupervisor>,
        ruleset: Arc<LoadedRuleset>,
        register: String,
    ) -> Self {
        let (flags_tx, _) = watch::channel(Vec::new());
        let inner = Arc::new(Inner {
            capture,
            spelling,
            languagetool,
            ruleset,
            register,
            flags: RwLock::new(Vec::new()),
            flags_tx,
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(CACHE_CAPACITY)
                    .expect("CACHE_CAPACITY is a nonzero constant declared just above"),
            )),
            previous_sentences: Mutex::new(Vec::new()),
            #[cfg(test)]
            check_sentence_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        tokio::spawn(debounce_loop(Arc::clone(&inner)));
        Self { inner }
    }

    /// The most recently computed, merged, and deduplicated flag set for the whole document.
    /// Empty until the first recheck completes.
    pub fn current_flags(&self) -> Vec<Flag> {
        self.inner
            .flags
            .read()
            .expect("a poisoned lock here means another thread already panicked; propagating that panic is correct, not recovering silently")
            .clone()
    }

    /// Notified with the complete, merged, and deduplicated flag set every time a recheck
    /// finishes, so a subscriber (the overlay's own flag-positioning task) reacts the moment new
    /// flags are ready instead of polling [`Self::current_flags`] on its own schedule. Delivers
    /// only the latest value, not every intermediate one: a subscriber that is mid-reaction to
    /// one recheck when another completes sees the newer flags next, never a queue of stale ones
    /// to catch up on, which is the behaviour this receiver needs since only the current flag set
    /// is ever meaningful to render.
    pub fn subscribe(&self) -> watch::Receiver<Vec<Flag>> {
        self.inner.flags_tx.subscribe()
    }

    /// How many times [`check_sentence`] actually ran real work, as opposed to a cache hit.
    #[cfg(test)]
    fn check_sentence_call_count(&self) -> usize {
        self.inner
            .check_sentence_calls
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

async fn debounce_loop(inner: Arc<Inner>) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    let mut last_seen_hash: Option<u64> = None;
    let mut quiet_since: Option<Instant> = None;
    let mut already_rechecked_this_quiet_period = false;

    loop {
        interval.tick().await;
        let text = match inner.capture.current_text().await {
            Ok(text) => text,
            Err(error) => {
                log::debug!("no document text to analyze: {error}");
                continue;
            }
        };

        let hash = hash_text(&text);
        if Some(hash) != last_seen_hash {
            last_seen_hash = Some(hash);
            quiet_since = Some(Instant::now());
            already_rechecked_this_quiet_period = false;
            continue;
        }

        let Some(since) = quiet_since else { continue };
        if already_rechecked_this_quiet_period || since.elapsed() < QUIET_THRESHOLD {
            continue;
        }
        already_rechecked_this_quiet_period = true;
        recheck(&inner, &text).await;
    }
}

async fn recheck(inner: &Inner, text: &str) {
    let current_sentences = sentences::split(text);
    let current_texts: Vec<String> = current_sentences
        .iter()
        .map(|sentence| sentence.text.clone())
        .collect();
    let changed = {
        let mut previous = inner
            .previous_sentences
            .lock()
            .expect("a poisoned lock here means another thread already panicked");
        let changed = sentences::changed_indices(&previous, &current_texts);
        *previous = current_texts;
        changed
    };

    let mut document_flags = Vec::new();
    for (index, sentence) in current_sentences.iter().enumerate() {
        let key = hash_text(&sentence.text);
        let cached = if changed.contains(&index) {
            None
        } else {
            inner
                .cache
                .lock()
                .expect("a poisoned lock here means another thread already panicked")
                .get(&key)
                .cloned()
        };
        let sentence_flags = match cached {
            Some(flags) => flags,
            // Either the diff marked this sentence changed, or it did not but the cache had no
            // entry for it (first run, or an entry aged out under the LRU's capacity): either
            // way, a fresh check is the correct fallback, not silently showing no flags.
            None => {
                let flags = check_sentence(inner, &sentence.text).await;
                inner
                    .cache
                    .lock()
                    .expect("a poisoned lock here means another thread already panicked")
                    .put(key, flags.clone());
                flags
            }
        };
        let text_before_sentence = &text[..sentence.start];
        document_flags.extend(sentence_flags.into_iter().map(|flag| Flag {
            ruleset_version: inner.ruleset.version(),
            ..with_document_scope(index, with_occurrence(text_before_sentence, flag))
        }));
    }

    *inner
        .flags
        .write()
        .expect("a poisoned lock here means another thread already panicked") =
        document_flags.clone();
    // A subscriber-free channel (the overlay's flag-tracking task not running yet, or the engine
    // running headless in a test) has no receivers left; `send` reporting that is expected, not an error
    // worth logging.
    let _ = inner.flags_tx.send(document_flags);
}

/// Prefixes `flag`'s id with the position in the document of the sentence it was found in. Every
/// checking source numbers its flags within the single sentence it was handed, so two sentences
/// whose flags number the same way arrive with the same id: a misspelled first word in two
/// sentences produces `spelling:0` twice. The overlay keys both its rendered underlines
/// and its hover lookup by id, so a duplicate makes one flag's card open for the other.
///
/// The sentence's index, rather than the hash the cache is keyed by, because the index is the
/// only part of a sentence's identity that separates two textually identical sentences. It shifts
/// when a sentence is inserted earlier in the document, which re-keys the flags after it; that
/// costs a remount of those underlines on the next emit, which is already a whole-set emit.
fn with_document_scope(sentence_index: usize, flag: Flag) -> Flag {
    Flag {
        id: format!("{sentence_index}:{}", flag.id),
        ..flag
    }
}

/// Sets `flag`'s occurrence index from the document text before the sentence it was found in, by
/// the rule on [`crate::flag::Span::occurrence`]: `matches` scans left to right and does not
/// overlap, as the native backend's search does. Counted here, as each recheck publishes, and not
/// cached with the flag, so a cached sentence that an identical one now precedes gets its new
/// index.
fn with_occurrence(text_before_sentence: &str, mut flag: Flag) -> Flag {
    flag.span.occurrence = text_before_sentence
        .matches(flag.span.anchor.as_str())
        .count();
    flag
}

async fn check_sentence(inner: &Inner, sentence: &str) -> Vec<Flag> {
    #[cfg(test)]
    inner
        .check_sentence_calls
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let mut flags = inner.spelling.check(sentence);
    flags.extend(ai_tell::scan(sentence));
    if let Some(languagetool) = &inner.languagetool {
        // Every sentence is prose until form mode is classified per paragraph (#54).
        let checked = languagetool
            .check(sentence, &inner.ruleset, FormMode::Prose, &inner.register)
            .await;
        match checked {
            Ok(grammar_flags) => flags.extend(grammar_flags),
            Err(error) => log::debug!(
                "the grammar check of a sentence of {} bytes failed, so it has no grammar flags \
                 this time: {error}",
                sentence.len()
            ),
        }
    }
    dedupe::apply(sentence, flags)
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    use crate::capture::{CaptureError, CursorRect};
    use crate::flag::{FlagOrigin, RulesetVersion, Span};
    use crate::test_support::dictionary_path;

    struct FakeCapture {
        text: StdMutex<String>,
    }

    impl FakeCapture {
        fn new(text: &str) -> Self {
            Self {
                text: StdMutex::new(text.to_string()),
            }
        }

        fn set_text(&self, text: &str) {
            *self.text.lock().unwrap() = text.to_string();
        }
    }

    #[async_trait::async_trait]
    impl Capture for FakeCapture {
        async fn current_text(&self) -> Result<String, CaptureError> {
            Ok(self.text.lock().unwrap().clone())
        }

        async fn cursor_rect(&self) -> Result<CursorRect, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        async fn replace(
            &self,
            _anchor: &str,
            _local_start: usize,
            _local_length: usize,
            _replacement: &str,
        ) -> Result<(), CaptureError> {
            Ok(())
        }

        async fn document_view_rect(&self) -> Result<CursorRect, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        async fn span_rect(
            &self,
            _anchor: &str,
            _local_start: usize,
            _local_length: usize,
        ) -> Result<Vec<CursorRect>, CaptureError> {
            Err(CaptureError::Unsupported)
        }
    }

    fn test_spell_checker() -> SpellChecker {
        SpellChecker::load(
            &dictionary_path("en_GB.aff"),
            &dictionary_path("en_GB.dic"),
            &dictionary_path("en_sg_supplement.txt"),
        )
        .expect("the vendored dictionary pair and supplement are well-formed, checked in CI")
    }

    /// Advances the paused clock in `POLL_INTERVAL` steps rather than one combined-duration
    /// jump. A single large `tokio::time::advance` call was found not to reliably let a spawned
    /// task observe every intermediate `tokio::time::interval` tick within it, and the debounce
    /// loop relies on seeing each one. Stepping is also how the loop's 200 ms poll runs outside a
    /// test, one tick at a time.
    async fn step(steps: u32) {
        for _ in 0..steps {
            tokio::time::advance(POLL_INTERVAL).await;
        }
    }

    /// The exact text a flag's span addresses, sliced back out of its own anchor, so a test can
    /// assert on what was flagged without depending on how the span happens to be anchored.
    fn flagged_text(flag: &Flag) -> String {
        let anchor: Vec<u16> = flag.span.anchor.encode_utf16().collect();
        let end = flag.span.local_start + flag.span.local_length;
        String::from_utf16_lossy(&anchor[flag.span.local_start..end])
    }

    #[tokio::test(start_paused = true)]
    async fn debounces_rapid_changes_into_one_recheck_after_the_quiet_threshold() {
        let fake = Arc::new(FakeCapture::new("This has a mispelling."));
        let capture: Arc<dyn Capture> = Arc::clone(&fake) as Arc<dyn Capture>;
        let analyzer = Analyzer::start(
            capture,
            test_spell_checker(),
            None,
            Arc::new(LoadedRuleset::empty()),
            "default".to_owned(),
        );

        // Let the loop see the initial text and start its quiet timer; no recheck yet.
        step(1).await;
        assert!(
            analyzer.current_flags().is_empty(),
            "no recheck should fire before the quiet threshold elapses"
        );

        // An edit shortly after resets the timer instead of letting the original one fire.
        step(1).await;
        fake.set_text("This has a mispelling, still.");
        step(1).await;
        assert!(
            analyzer.current_flags().is_empty(),
            "the edit should have reset the quiet timer"
        );

        // Now let it sit unchanged long enough (comfortably past QUIET_THRESHOLD) for the reset
        // timer to elapse.
        step(6).await;
        let flags = analyzer.current_flags();
        assert!(
            flags.iter().any(|flag| flagged_text(flag) == "mispelling"),
            "expected a recheck to have found the misspelling by now: {flags:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_changed_sentence_in_a_two_sentence_document_is_rechecked() {
        let fake = Arc::new(FakeCapture::new("This has a mispelling. This one is fine."));
        let capture: Arc<dyn Capture> = Arc::clone(&fake) as Arc<dyn Capture>;
        let analyzer = Analyzer::start(
            capture,
            test_spell_checker(),
            None,
            Arc::new(LoadedRuleset::empty()),
            "default".to_owned(),
        );

        step(6).await;
        let first_pass = analyzer.current_flags();
        assert!(
            first_pass
                .iter()
                .any(|flag| flagged_text(flag) == "mispelling"),
            "expected the first recheck to find the misspelling: {first_pass:#?}"
        );
        // Both sentences are new to the cache on this first pass, so both were checked for real.
        assert_eq!(analyzer.check_sentence_call_count(), 2);

        // Only the second sentence changes; the first sentence's text is untouched.
        fake.set_text("This has a mispelling. This one has an eror too.");
        step(6).await;
        let second_pass = analyzer.current_flags();

        assert!(
            second_pass
                .iter()
                .any(|flag| flagged_text(flag) == "mispelling"),
            "the unchanged first sentence's flag should still be present: {second_pass:#?}"
        );
        assert!(
            second_pass.iter().any(|flag| flagged_text(flag) == "eror"),
            "the changed second sentence's new misspelling should be flagged: {second_pass:#?}"
        );
        // Only the one changed sentence should have gone through a real check this time; the
        // unchanged first sentence should have been served from the LRU cache instead.
        assert_eq!(analyzer.check_sentence_call_count(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn the_same_misspelling_in_two_sentences_gets_two_distinct_ids() {
        // Each sentence is checked on its own, so both flags arrive numbered from that
        // sentence's own start. Without a document-level scope they are both `spelling:3`,
        // and the overlay keys its underlines and its hover lookup by id.
        let fake = Arc::new(FakeCapture::new("An eror here. An eror there."));
        let capture: Arc<dyn Capture> = Arc::clone(&fake) as Arc<dyn Capture>;
        let analyzer = Analyzer::start(
            capture,
            test_spell_checker(),
            None,
            Arc::new(LoadedRuleset::empty()),
            "default".to_owned(),
        );

        step(6).await;
        let flags = analyzer.current_flags();
        let misspellings: Vec<&Flag> = flags
            .iter()
            .filter(|flag| flagged_text(flag) == "eror")
            .collect();
        assert_eq!(misspellings.len(), 2, "{flags:#?}");
        assert_ne!(misspellings[0].id, misspellings[1].id, "{flags:#?}");
        // The two sentences are different text, so each flag also anchors on its own sentence,
        // which is what lets the overlay resolve them to two different places on screen.
        assert_ne!(misspellings[0].span.anchor, misspellings[1].span.anchor);
    }

    fn analyzer_on(fake: &Arc<FakeCapture>) -> Analyzer {
        Analyzer::start(
            Arc::clone(fake) as Arc<dyn Capture>,
            test_spell_checker(),
            None,
            Arc::new(LoadedRuleset::empty()),
            "default".to_owned(),
        )
    }

    /// The spans of the published flags on "eror", in the order the analyzer published them.
    fn eror_spans(analyzer: &Analyzer) -> Vec<Span> {
        analyzer
            .current_flags()
            .into_iter()
            .filter(|flag| flagged_text(flag) == "eror")
            .map(|flag| flag.span)
            .collect()
    }

    fn eror_here(occurrence: usize) -> Span {
        Span {
            anchor: "An eror here.".to_owned(),
            local_start: 3,
            local_length: 4,
            occurrence,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn two_identical_sentences_publish_one_span_at_two_occurrences() {
        let fake = Arc::new(FakeCapture::new("An eror here. An eror here."));
        let analyzer = analyzer_on(&fake);
        step(6).await;
        assert_eq!(eror_spans(&analyzer), [eror_here(0), eror_here(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_occurrence_index_counts_the_anchors_text_inside_an_earlier_longer_sentence() {
        let longer = "She typed An eror here. on the board.";
        let fake = Arc::new(FakeCapture::new(&format!("{longer} An eror here.")));
        let analyzer = analyzer_on(&fake);
        step(6).await;
        let in_longer = Span {
            anchor: longer.to_owned(),
            local_start: 13,
            local_length: 4,
            occurrence: 0,
        };
        assert_eq!(eror_spans(&analyzer), [in_longer, eror_here(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cached_sentence_that_moves_later_publishes_with_its_new_occurrence_index() {
        let fake = Arc::new(FakeCapture::new("Fine start. An eror here."));
        let analyzer = analyzer_on(&fake);
        step(6).await;
        assert_eq!(eror_spans(&analyzer), [eror_here(0)]);
        assert_eq!(analyzer.check_sentence_call_count(), 2);

        // A copy inserted at the start moves the original to third place, where the diff marks it
        // unchanged and the cache serves it.
        fake.set_text("An eror here. Fine start. An eror here.");
        step(6).await;
        assert_eq!(eror_spans(&analyzer), [eror_here(0), eror_here(1)]);
        assert_eq!(analyzer.check_sentence_call_count(), 3);
        let cached = analyzer
            .inner
            .cache
            .lock()
            .unwrap()
            .get(&hash_text("An eror here."))
            .cloned()
            .unwrap();
        assert!(
            cached.iter().all(|flag| flag.span.occurrence == 0),
            "{cached:#?}"
        );
    }

    /// The flags an analyzer started with `ruleset_version` publishes for a misspelled sentence
    /// and a telltale one, once the first sentence's cached result also holds a grammar flag. No
    /// test runs LanguageTool, so the grammar flag enters where a checked sentence's grammar flags
    /// wait between rechecks, carrying a version the analyzer does not hold. Only the second
    /// sentence then changes, so the first is served from the cache.
    async fn flags_of_every_origin(ruleset_version: Option<RulesetVersion>) -> Vec<Flag> {
        let misspelled = "This has a mispelling.";
        let fake = Arc::new(FakeCapture::new(&format!(
            "{misspelled} It stands as a testament to the team."
        )));
        let capture: Arc<dyn Capture> = Arc::clone(&fake) as Arc<dyn Capture>;
        let ruleset = ruleset_version.map_or_else(LoadedRuleset::empty, LoadedRuleset::empty_at);
        let analyzer = Analyzer::start(
            capture,
            test_spell_checker(),
            None,
            Arc::new(ruleset),
            "default".to_owned(),
        );
        step(6).await;

        let grammar = Flag {
            id: "grammar:0:TEST_RULE".to_owned(),
            origin: FlagOrigin::Grammar,
            span: Span {
                anchor: misspelled.to_owned(),
                local_start: 0,
                local_length: 4,
                occurrence: 0,
            },
            message: "a grammar flag".to_owned(),
            suggestions: Vec::new(),
            source_detail: "TEST_RULE".to_owned(),
            ruleset_version: Some(RulesetVersion(99)),
            catalogue_hash: None,
        };
        {
            let mut cache = analyzer.inner.cache.lock().unwrap();
            let key = hash_text(misspelled);
            let mut cached = cache
                .get(&key)
                .cloned()
                .expect("the first recheck cached the sentence's flags");
            cached.push(grammar);
            cache.put(key, cached);
        }
        fake.set_text(&format!(
            "{misspelled} It stands as a testament to the town."
        ));
        step(6).await;
        let flags = analyzer.current_flags();
        for origin in [
            FlagOrigin::Spelling,
            FlagOrigin::Grammar,
            FlagOrigin::AiTell,
        ] {
            assert!(
                flags.iter().any(|flag| flag.origin == origin),
                "no {origin:?} flag: {flags:#?}"
            );
        }
        flags
    }

    #[tokio::test(start_paused = true)]
    async fn every_published_flag_carries_the_ruleset_version_whatever_its_origin() {
        let flags = flags_of_every_origin(Some(RulesetVersion(7))).await;
        assert!(
            flags
                .iter()
                .all(|flag| flag.ruleset_version == Some(RulesetVersion(7))),
            "{flags:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn with_no_ruleset_loaded_every_published_flag_carries_no_version() {
        let flags = flags_of_every_origin(None).await;
        assert!(
            flags.iter().all(|flag| flag.ruleset_version.is_none()),
            "{flags:#?}"
        );
    }
}
