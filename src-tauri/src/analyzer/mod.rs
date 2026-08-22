//! The analyzer pipeline that sits between capture and the overlay: debounce, incremental
//! diffing, merge and dedupe by span, ranking, and an LRU cache. Style flags are suppressed
//! on spans that already carry a hard grammar error.
//!
//! `sentences` covers the incremental diffing, `dedupe` the span-overlap suppression rule; both
//! are private, this module's own [`Analyzer`] is the only public surface.
//!
//! Not wired into the running app yet: that is #42's job, once the overlay has something to
//! render this pipeline's output into.

mod dedupe;
mod sentences;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use lru::LruCache;
use tokio::time::Instant;

use crate::capture::Capture;
use crate::flag::Flag;
use crate::languagetool::LanguageToolSupervisor;
use crate::spelling::SpellChecker;
use crate::style::ai_tell;

/// How often the debounce loop polls the capture backend's current text for a change. Matches
/// `overlay.rs`'s `track_cursor` cadence, the established polling interval elsewhere in this
/// layer, itself a bridge until a later phase replaces polling with a push from the capture
/// backend's own text-change events (see that constant's own doc comment for why).
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
    flags: RwLock<Vec<Flag>>,
    cache: Mutex<LruCache<u64, Vec<Flag>>>,
    previous_sentences: Mutex<Vec<String>>,
    /// Counts calls into [`check_sentence`], real work a cache hit skips. Per-instance rather
    /// than a global counter so concurrently running tests never contaminate each other's count;
    /// only ever read in tests, but kept outside `#[cfg(test)]` so it costs nothing to leave
    /// wired through the one non-test call site rather than feature-gating that call site too.
    #[cfg(test)]
    check_sentence_calls: std::sync::atomic::AtomicUsize,
}

impl Analyzer {
    /// Starts polling `capture` in the background. `languagetool` is `None` when the subprocess
    /// could not be started at all; a `Some` that later degrades keeps answering `check` calls,
    /// just with `None` results, which [`Self::check_sentence`] already treats as no grammar
    /// flags this time rather than a failure.
    pub fn start(
        capture: Arc<dyn Capture>,
        spelling: SpellChecker,
        languagetool: Option<LanguageToolSupervisor>,
    ) -> Self {
        let inner = Arc::new(Inner {
            capture,
            spelling,
            languagetool,
            flags: RwLock::new(Vec::new()),
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
    let changed = {
        let mut previous = inner
            .previous_sentences
            .lock()
            .expect("a poisoned lock here means another thread already panicked");
        let changed = sentences::changed_indices(&previous, &current_sentences);
        *previous = current_sentences.clone();
        changed
    };

    let mut document_flags = Vec::new();
    for (index, sentence) in current_sentences.iter().enumerate() {
        let key = hash_text(sentence);
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
                let flags = check_sentence(inner, sentence).await;
                inner
                    .cache
                    .lock()
                    .expect("a poisoned lock here means another thread already panicked")
                    .put(key, flags.clone());
                flags
            }
        };
        document_flags.extend(sentence_flags);
    }

    *inner
        .flags
        .write()
        .expect("a poisoned lock here means another thread already panicked") = document_flags;
}

async fn check_sentence(inner: &Inner, sentence: &str) -> Vec<Flag> {
    #[cfg(test)]
    inner
        .check_sentence_calls
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let mut flags = inner.spelling.check(sentence);
    flags.extend(ai_tell::scan(sentence));
    if let Some(languagetool) = &inner.languagetool {
        if let Some(grammar_flags) = languagetool.check(sentence).await {
            flags.extend(grammar_flags);
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
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;

    use crate::capture::{CaptureError, CursorRect};

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
    }

    fn resource_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("dictionaries")
            .join(name)
    }

    fn test_spell_checker() -> SpellChecker {
        SpellChecker::load(
            &resource_path("en_GB.aff"),
            &resource_path("en_GB.dic"),
            &resource_path("en_sg_supplement.txt"),
        )
        .expect("the vendored dictionary pair and supplement are well-formed, checked in CI")
    }

    /// Advances the paused clock in `POLL_INTERVAL` steps rather than one combined-duration
    /// jump. A single large `tokio::time::advance` call was found not to reliably let a spawned
    /// task observe every intermediate `tokio::time::interval` tick within it (the debounce
    /// loop's own polling relies on seeing each one); stepping matches how `overlay.rs`'s
    /// `track_cursor` polling actually runs in production too, one tick at a time.
    async fn step(steps: u32) {
        for _ in 0..steps {
            tokio::time::advance(POLL_INTERVAL).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn debounces_rapid_changes_into_one_recheck_after_the_quiet_threshold() {
        let fake = Arc::new(FakeCapture::new("This has a mispelling."));
        let capture: Arc<dyn Capture> = Arc::clone(&fake) as Arc<dyn Capture>;
        let analyzer = Analyzer::start(capture, test_spell_checker(), None);

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
            flags.iter().any(|flag| flag.span.anchor == "mispelling"),
            "expected a recheck to have found the misspelling by now: {flags:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_changed_sentence_in_a_two_sentence_document_is_rechecked() {
        let fake = Arc::new(FakeCapture::new("This has a mispelling. This one is fine."));
        let capture: Arc<dyn Capture> = Arc::clone(&fake) as Arc<dyn Capture>;
        let analyzer = Analyzer::start(capture, test_spell_checker(), None);

        step(6).await;
        let first_pass = analyzer.current_flags();
        assert!(
            first_pass
                .iter()
                .any(|flag| flag.span.anchor == "mispelling"),
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
                .any(|flag| flag.span.anchor == "mispelling"),
            "the unchanged first sentence's flag should still be present: {second_pass:#?}"
        );
        assert!(
            second_pass.iter().any(|flag| flag.span.anchor == "eror"),
            "the changed second sentence's new misspelling should be flagged: {second_pass:#?}"
        );
        // Only the one changed sentence should have gone through a real check this time; the
        // unchanged first sentence should have been served from the LRU cache instead.
        assert_eq!(analyzer.check_sentence_call_count(), 3);
    }
}
