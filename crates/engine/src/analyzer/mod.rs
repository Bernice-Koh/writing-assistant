//! The analyzer, between capture and the overlay. It waits for the capture backend's change
//! notification, or for its own safety poll to find the text changed, then for the typing pause,
//! reads the document once, splits the text into paragraphs, classifies each paragraph's form mode,
//! and splits each paragraph into sentences. Checking keys to the pause because a flag on a
//! half-typed word is noise.
//!
//! A sentence is checked under its paragraph's mode, and every check of a recheck runs under one
//! register: the one the settings map the capture backend's surface to, read at the recheck, or the
//! default register when the backend cannot name the surface. Nothing checks a sentence of code. Spelling, the telltale catalogue
//! and the ruleset's checkers check a sentence of a fragment or of prose, a pattern or a checker
//! only where it declares the sentence's mode, and LanguageTool checks a sentence of prose alone, so
//! a fragment's grammar part is complete and empty from the start.
//!
//! Each recheck runs two stages, and each publishes the whole document's flags on the watch
//! channel the overlay subscribes to. The deterministic stage runs three passes, in `passes`, and
//! publishes at once, with the grammar flags the cache holds for sentences whose grammar check
//! completed on an earlier recheck. The sentence pass runs spelling, the catalogue's patterns and
//! the ruleset's checkers of a span or a sentence unit over every sentence the sentence cache holds
//! no entry for. The paragraph pass runs the patterns and the checkers of a paragraph unit over each
//! paragraph the paragraph cache holds no entry for, which is every paragraph that holds a changed
//! sentence, and the document pass runs those of a document unit over the whole document on every
//! recheck. The grammar stage then sends LanguageTool every sentence whose grammar part is not
//! complete, one at a time, and publishes a second update adding their grammar flags, among them
//! the style flags of the checkers whose matcher is a LanguageTool rule id. It runs as a task of its
//! own, so the analyzer goes on watching for changes while LanguageTool answers, and the
//! deterministic stage's flags never wait on it.
//!
//! The sentence cache holds each sentence's two parts under a hash of the ruleset version, the
//! register, its paragraph's mode and its text, as their sources raised them, so one text in a
//! prose paragraph and in a fragment has an entry for each, as has one text checked under two
//! registers. The paragraph cache holds the paragraph pass's flags for a paragraph under a hash of
//! the ruleset version, the register, its mode and its text, so an unchanged paragraph is served
//! wherever it has moved. With the version and the register in both keys, an entry made under one
//! ruleset version or one register is never served under another. Suppression by severity is applied each time
//! the document's flags are assembled for a publish, over each sentence's parts in hand and the
//! paragraph and document passes' flags anchored on it, so a soft flag that a grammar flag overlaps
//! is in the first update and gone from the second. A grammar check that fails, times out, or meets
//! LanguageTool degraded or absent leaves the sentence's grammar part incomplete, and the next
//! recheck sends it again. Each span's occurrence index is counted from its sentence's position in
//! the document as each update is assembled.
//!
//! Every recheck carries a number. A new recheck aborts the grammar stage of the one before, and a
//! stage publishes only while its number is the latest, so a stage that a newer recheck replaced
//! never publishes over the newer text's flags.
//!
//! [`form_mode`] covers the paragraphs and their modes, `sentences` the split of a paragraph,
//! `passes` the rule layer's passes, and `dedupe` suppression. [`form_mode`] is public for the
//! native capture harness; [`Analyzer`] is the only other public surface. The screen segments a
//! rewrite candidate through `form_mode` and `sentences`, checks it through `passes`, and places
//! its flags through `placed_in_document`, as the analyzer does live text.

mod dedupe;
pub mod form_mode;
pub(crate) mod passes;
pub(crate) mod sentences;

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lru::LruCache;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::capture::Capture;
use crate::flag::{Flag, RulesetVersion};
use crate::languagetool::GrammarSource;
use crate::spelling::SpellChecker;
use crate::store::Config;
use crate::style::ai_tell::Catalogue;
use crate::style::checker::FormMode;
use crate::style::evaluator::ModeSentence;
use crate::style::ruleset::LoadedRuleset;
use passes::{Sources, UnitFlag};

/// How long after the last read the debounce loop reads the document again while no change is
/// pending, so that a control that raises no text-changed event, and the web backend, which never
/// raises the change notification, are still checked.
///
/// Each read is of the whole document. #160 measured it in a release build over 30 reads per
/// surface of a 20,200-word text, at medians of 3.4 ms on a repeat run and 12.0 ms on a first run
/// on a Word document, 1.6 ms on Notepad, 24.4 ms on a Chromium `textarea` and 16.6 ms on a
/// Chromium `contenteditable` field, with every Word read under the 20 ms line that #153's
/// protocol set for lengthening this interval. At two seconds the read takes 1.2% of one core on
/// the `textarea` and under 1% on every other surface.
const SAFETY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How long after the last change a recheck fires, so a burst of keystrokes produces one read and
/// one recheck after typing pauses rather than one per keystroke, per AC #40(a).
const QUIET_THRESHOLD: Duration = Duration::from_millis(500);

/// How many entries each LRU cache holds at once: sentences' in the sentence cache, and
/// paragraphs', which a document holds fewer of, in the paragraph cache. Sized for a long document
/// (a few thousand words is on the order of a few hundred sentences) without growing unbounded
/// for the life of a long writing session.
const CACHE_CAPACITY: usize = 1000;

/// Runs the debounce, the two checking stages, suppression, and the cache against a [`Capture`]
/// backend's live text, keeping the latest suppressed [`Flag`] list for the whole document.
pub struct Analyzer {
    inner: Arc<Inner>,
}

struct Inner {
    capture: Arc<dyn Capture>,
    spelling: SpellChecker,
    grammar: Option<Arc<dyn GrammarSource>>,
    ruleset: Arc<LoadedRuleset>,
    catalogue: Arc<Catalogue>,
    /// The settings loaded at startup, whose register map and default register each recheck
    /// resolves its register from.
    settings: Arc<Config>,
    /// The whole document's flags as the latest update published them, empty until the first. The
    /// channel holds the analyzer's one copy of them, which a subscriber reads through `borrow`.
    flags_tx: watch::Sender<Vec<Flag>>,
    /// The sentence cache, keyed by [`cache_key`] of a sentence.
    cache: Mutex<LruCache<u64, SentenceParts>>,
    /// The paragraph pass's flags for each paragraph it evaluated, keyed by [`cache_key`] of the
    /// paragraph, each flag with the index of its sentence within the paragraph.
    paragraph_cache: Mutex<LruCache<u64, Vec<UnitFlag>>>,
    /// The number of the latest recheck, raised as each starts. Every publish compares its
    /// recheck's number with this one under this lock, and the raise takes the same lock, so no
    /// update of a recheck lands once a newer recheck has started.
    latest_recheck: Mutex<u64>,
    /// Counts the sentences the deterministic stage checked, the work a cache hit skips.
    /// Test-only, and held per instance rather than in a global, so tests running in parallel never
    /// share a count.
    #[cfg(test)]
    deterministic_checks: std::sync::atomic::AtomicUsize,
    /// Counts the paragraphs the paragraph pass evaluated, as `deterministic_checks` counts
    /// sentences. Test-only.
    #[cfg(test)]
    paragraph_checks: std::sync::atomic::AtomicUsize,
    /// Counts the document pass's evaluations. Test-only.
    #[cfg(test)]
    document_checks: std::sync::atomic::AtomicUsize,
    /// Every update published, in order. Test-only: the watch channel keeps only the latest.
    #[cfg(test)]
    updates: Mutex<Vec<Vec<Flag>>>,
}

/// One sentence's flags as the cache holds them, each part as its sources raised it, with no
/// suppression applied.
#[derive(Debug, Clone)]
struct SentenceParts {
    /// The sentence pass's flags: spelling's, and those of the catalogue's patterns and the
    /// ruleset's checkers of a span or a sentence unit.
    deterministic: Vec<Flag>,
    /// LanguageTool's flags, routed through the ruleset. `Some` once a grammar check of the
    /// sentence has answered, which is the part's completion mark; `None` while no check has, which
    /// sends the sentence to the grammar stage of every recheck until one does. A sentence of a
    /// fragment or of code, which grammar does not run on, has it `Some` and empty from the
    /// deterministic stage, so only a sentence of prose is ever `None`.
    grammar: Option<Vec<Flag>>,
}

/// The document one recheck checks, owned so that its grammar stage's task can carry it.
struct Recheck {
    number: u64,
    /// The register every check of the recheck runs under.
    register: String,
    text: String,
    sentences: Vec<DocumentSentence>,
    /// Each sentence's parts in hand, in the order of `sentences`.
    parts: Vec<SentenceParts>,
    /// The paragraph pass's flags and then the document pass's anchored on each sentence, in the
    /// order of `sentences`.
    unit_flags: Vec<Vec<Flag>>,
}

/// One sentence of a [`Recheck`]'s document.
struct DocumentSentence {
    /// Where the trimmed sentence sits in the document's text, in bytes.
    range: Range<usize>,
    /// The form mode of the sentence's paragraph.
    mode: FormMode,
    /// The sentence's key in the sentence cache.
    key: u64,
}

/// The stage of a recheck an update comes from, which its log line names.
#[derive(Debug, Clone, Copy)]
enum Stage {
    Deterministic,
    Grammar,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Grammar => "grammar",
        }
    }
}

/// What found the document changed, which a recheck's log line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// The capture backend's change notification.
    Event,
    /// The safety poll, which read a text other than the one last checked.
    Poll,
}

impl Trigger {
    fn name(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Poll => "safety poll",
        }
    }
}

/// A change waiting for the quiet threshold to pass.
#[derive(Debug, Clone, Copy)]
struct Pending {
    trigger: Trigger,
    /// When the quiet threshold passes, unless another change restarts it.
    quiet_until: Instant,
    /// For a change the safety poll found, the hash of the text it read. No event restarts the
    /// threshold on such a control, so the text has held through the threshold only when the read
    /// after it hashes the same.
    polled_hash: Option<u64>,
}

impl Pending {
    fn starting_now(trigger: Trigger, polled_hash: Option<u64>) -> Self {
        Self {
            trigger,
            quiet_until: Instant::now() + QUIET_THRESHOLD,
            polled_hash,
        }
    }
}

/// The register a recheck runs under, with what it was resolved from, for the recheck's log line.
struct Register {
    name: String,
    /// The surface's process name and its page URL's host alone, or the error that left the
    /// surface unnamed.
    source: String,
}

/// One read of the document, timed by the wall clock.
struct Read {
    text: String,
    took: Duration,
}

impl Read {
    /// The read's length and time for a log line, which never holds the text.
    fn describe(&self) -> String {
        format!(
            "{} characters in {:.2} ms",
            self.text.chars().count(),
            self.took.as_secs_f64() * 1000.0
        )
    }
}

impl Analyzer {
    /// Starts watching `capture` for changes in the background. `grammar` is `None` when the
    /// LanguageTool subprocess could not be started at all; a `Some` that later degrades fails
    /// every check, which leaves every sentence's grammar part incomplete. `ruleset` is the
    /// ruleset loaded at startup: every flag the analyzer publishes carries its version whatever
    /// its origin, `None` when no version is loaded, and LanguageTool's matches route through its
    /// checkers. `catalogue` is the telltale catalogue compiled at startup from the settings'
    /// toggles: every flag the analyzer publishes carries its hash whatever its origin, beside the
    /// ruleset version. `settings` are the settings loaded at startup: each recheck reads the
    /// capture backend's surface and checks under the register [`Config::register_for`] maps it
    /// to, or under the default register when the backend cannot name the surface.
    pub fn start(
        capture: Arc<dyn Capture>,
        spelling: SpellChecker,
        grammar: Option<Arc<dyn GrammarSource>>,
        ruleset: Arc<LoadedRuleset>,
        catalogue: Arc<Catalogue>,
        settings: Arc<Config>,
    ) -> Self {
        let (flags_tx, _) = watch::channel(Vec::new());
        let inner = Arc::new(Inner {
            capture,
            spelling,
            grammar,
            ruleset,
            catalogue,
            settings,
            flags_tx,
            cache: Mutex::new(LruCache::new(cache_capacity())),
            paragraph_cache: Mutex::new(LruCache::new(cache_capacity())),
            latest_recheck: Mutex::new(0),
            #[cfg(test)]
            deterministic_checks: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            paragraph_checks: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            document_checks: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            updates: Mutex::new(Vec::new()),
        });
        tokio::spawn(debounce_loop(Arc::clone(&inner)));
        Self { inner }
    }

    /// The most recently published, suppressed flag set for the whole document, empty until the
    /// first update, and notified at every update, the deterministic stage's and the grammar
    /// stage's, so a subscriber (the overlay's own flag-positioning task) reacts the moment new
    /// flags are ready instead of polling on its own schedule. Delivers only the latest value, not
    /// every intermediate one: a subscriber that is mid-reaction to one update when another lands
    /// sees the newer flags next, never a queue of stale ones to catch up on, which is the
    /// behaviour this receiver needs since only the current flag set is ever meaningful to render.
    pub fn subscribe(&self) -> watch::Receiver<Vec<Flag>> {
        self.inner.flags_tx.subscribe()
    }

    /// How many sentences the deterministic stage checked, as opposed to serving from the cache.
    #[cfg(test)]
    fn deterministic_check_count(&self) -> usize {
        self.inner
            .deterministic_checks
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many paragraphs the paragraph pass evaluated, as opposed to serving from the cache.
    #[cfg(test)]
    fn paragraph_check_count(&self) -> usize {
        self.inner
            .paragraph_checks
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many times the document pass ran.
    #[cfg(test)]
    fn document_check_count(&self) -> usize {
        self.inner
            .document_checks
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Every update published so far, in order.
    #[cfg(test)]
    fn updates(&self) -> Vec<Vec<Flag>> {
        self.inner
            .updates
            .lock()
            .expect("a poisoned lock here means another thread already panicked")
            .clone()
    }
}

/// Waits on the capture backend's change notification and on the safety poll, and rechecks the
/// document once per pause in its changes.
///
/// A notification marks the document changed and restarts the quiet threshold, and reads nothing,
/// so a burst of typing costs no read until it pauses. Once the threshold passes with a change
/// pending, the loop reads the document once and rechecks it when its hash differs from the text
/// last checked; a read that hashes the same, as after a change of formatting alone, rechecks
/// nothing. A focus change arrives as a notification, so a new document takes the same path.
///
/// The read after the threshold is joined by one read of the surface, which resolves the register
/// the recheck runs under. A text that hashes the same as the text last checked is rechecked when
/// that register differs from the one it was last checked under, as when focus moves between two
/// documents of one text on surfaces the settings map to different registers.
///
/// The safety poll reads only while no change is pending, [`SAFETY_POLL_INTERVAL`] after the last
/// read. A text other than the one last checked marks the document changed and starts the
/// threshold, and the recheck follows with a read of its own once the text has held through the
/// threshold; a text still changing starts the threshold again. A control that raises no event is
/// therefore checked at most once per pause, and never in the middle of a burst. A new analyzer has
/// checked nothing, so the poll reads at once.
async fn debounce_loop(inner: Arc<Inner>) {
    let mut changes = inner.capture.changes();
    let mut notifications_open = true;
    let mut pending: Option<Pending> = None;
    let mut last_checked: Option<u64> = None;
    let mut last_register: Option<String> = None;
    let mut next_poll = Instant::now();
    let mut grammar_stage: Option<JoinHandle<()>> = None;

    loop {
        let wake_at = pending.map_or(next_poll, |change| change.quiet_until);
        tokio::select! {
            changed = changes.changed(), if notifications_open => {
                if changed.is_err() {
                    log::debug!(
                        "the capture backend's change notification closed, so the safety poll \
                         alone finds a change from now on"
                    );
                    notifications_open = false;
                    continue;
                }
                pending = Some(Pending::starting_now(Trigger::Event, None));
            }
            () = tokio::time::sleep_until(wake_at) => {
                next_poll = Instant::now() + SAFETY_POLL_INTERVAL;
                let Some(read) = read_document(&inner).await else {
                    pending = None;
                    continue;
                };
                let hash = hash_text(&read.text);
                match pending.take() {
                    None if last_checked == Some(hash) => {
                        log::debug!(
                            "the safety poll read the text last checked, {}",
                            read.describe()
                        );
                    }
                    None => {
                        log::debug!(
                            "the safety poll read a text other than the one last checked, {}, so \
                             the quiet threshold starts",
                            read.describe()
                        );
                        pending = Some(Pending::starting_now(Trigger::Poll, Some(hash)));
                    }
                    Some(change) if change.polled_hash.is_some_and(|polled| polled != hash) => {
                        log::debug!(
                            "the text the safety poll found changed was still changing at the \
                             quiet threshold, {}, so the threshold starts again",
                            read.describe()
                        );
                        pending = Some(Pending::starting_now(Trigger::Poll, Some(hash)));
                    }
                    Some(change) => {
                        let register = resolve_register(&inner).await;
                        if last_checked == Some(hash)
                            && last_register.as_deref() == Some(register.name.as_str())
                        {
                            log::debug!(
                                "the read after the {}'s quiet threshold holds the text last \
                                 checked, {}, under the register it was checked under, so nothing \
                                 is rechecked",
                                change.trigger.name(),
                                read.describe()
                            );
                            continue;
                        }
                        last_checked = Some(hash);
                        last_register = Some(register.name.clone());
                        // The previous recheck's checks were of text the document no longer holds,
                        // or under a register it is no longer checked under.
                        if let Some(stale) = grammar_stage.take() {
                            stale.abort();
                        }
                        grammar_stage = recheck(&inner, read, change.trigger, register);
                    }
                }
            }
        }
    }
}

/// The document's text from the capture backend, timed, or `None` when it has none to give, as
/// with no focused document.
async fn read_document(inner: &Inner) -> Option<Read> {
    let started = std::time::Instant::now();
    match inner.capture.current_text().await {
        Ok(text) => Some(Read {
            text,
            took: started.elapsed(),
        }),
        Err(error) => {
            log::debug!("no document text to analyze: {error}");
            None
        }
    }
}

/// The register the settings map the capture backend's surface to, from one read of the surface,
/// or the default register when the backend cannot name the surface.
async fn resolve_register(inner: &Inner) -> Register {
    match inner.capture.surface().await {
        Ok(surface) => Register {
            name: inner.settings.register_for(&surface).to_owned(),
            source: format!("resolved from {}", surface.describe()),
        },
        Err(error) => Register {
            name: inner.settings.default_register.clone(),
            source: format!("as the default, since the surface could not be read: {error}"),
        },
    }
}

/// Runs the deterministic stage over `read`'s text under `register` and publishes the first
/// update, then spawns the grammar stage over the sentences whose grammar part is incomplete,
/// returning its task. No stage is spawned with no grammar source or no such sentence. `trigger` is
/// what found the change, for the log.
fn recheck(
    inner: &Arc<Inner>,
    read: Read,
    trigger: Trigger,
    register: Register,
) -> Option<JoinHandle<()>> {
    let number = start_recheck(inner);
    log::debug!(
        "recheck {number} on the {} after the quiet threshold, over a read of {}, under the \
         register {:?} {}",
        trigger.name(),
        read.describe(),
        register.name,
        register.source
    );
    let recheck = deterministic_stage(inner, number, read.text, register.name);
    publish(
        inner,
        number,
        Stage::Deterministic,
        recheck.document_flags(inner),
    );

    let grammar = inner.grammar.clone()?;
    if recheck.incomplete_sentences().is_empty() {
        return None;
    }
    Some(tokio::spawn(grammar_stage(
        Arc::clone(inner),
        grammar,
        recheck,
    )))
}

/// Raises the latest recheck's number and returns it, the new recheck's.
fn start_recheck(inner: &Inner) -> u64 {
    let mut latest = inner
        .latest_recheck
        .lock()
        .expect("a poisoned lock here means another thread already panicked");
    *latest += 1;
    *latest
}

/// Publishes `flags` as `stage`'s update of recheck `number`, unless a newer recheck has started.
fn publish(inner: &Inner, number: u64, stage: Stage, flags: Vec<Flag>) {
    let latest = inner
        .latest_recheck
        .lock()
        .expect("a poisoned lock here means another thread already panicked");
    if *latest != number {
        log::debug!(
            "recheck {number}'s {} stage publishes nothing, since recheck {latest} has started",
            stage.name()
        );
        return;
    }
    log::debug!(
        "recheck {number} published its {} stage's update of {} flags",
        stage.name(),
        flags.len()
    );
    #[cfg(test)]
    inner
        .updates
        .lock()
        .expect("a poisoned lock here means another thread already panicked")
        .push(flags.clone());
    // The channel holds the only copy, and `send` discards the value while no receiver is
    // subscribed, as before the overlay's flag-tracking task starts or with the engine headless in
    // a test. `send_replace` stores it regardless.
    inner.flags_tx.send_replace(flags);
}

/// Splits `text` into its paragraphs and each paragraph into its sentences, and runs the three
/// passes under `register`. Each sentence's parts come from the sentence cache where it holds an
/// entry, and otherwise from a fresh check under its paragraph's mode, cached as
/// [`deterministic_parts`] leaves them. Each paragraph's flags come from
/// [`passes::cached_paragraph_pass`], and the document's from [`passes::counted_document_pass`].
/// An unchanged sentence or paragraph whose entry aged out under the LRU's capacity is checked
/// afresh, the correct fallback rather than showing no flags.
fn deterministic_stage(inner: &Inner, number: u64, text: String, register: String) -> Recheck {
    let version = inner.ruleset.version();
    let mut sentences = Vec::new();
    let mut parts = Vec::new();
    let mut unit_flags: Vec<Vec<Flag>> = Vec::new();
    for paragraph in form_mode::split(&text) {
        let first_sentence = sentences.len();
        let paragraph_sentences = sentences::split(paragraph.text);
        for sentence in &paragraph_sentences {
            let key = cache_key(version, &register, paragraph.mode, sentence.text);
            let cached = inner
                .cache
                .lock()
                .expect("a poisoned lock here means another thread already panicked")
                .get(&key)
                .cloned();
            let sentence_parts = cached.unwrap_or_else(|| {
                let checked = deterministic_parts(inner, &register, sentence.text, paragraph.mode);
                inner
                    .cache
                    .lock()
                    .expect("a poisoned lock here means another thread already panicked")
                    .put(key, checked.clone());
                checked
            });
            let start = paragraph.start + sentence.start;
            sentences.push(DocumentSentence {
                range: start..start + sentence.text.len(),
                mode: paragraph.mode,
                key,
            });
            parts.push(sentence_parts);
            unit_flags.push(Vec::new());
        }
        for UnitFlag { sentence, flag } in
            passes::cached_paragraph_pass(inner, &register, &paragraph, &paragraph_sentences)
        {
            unit_flags[first_sentence + sentence].push(flag);
        }
    }
    let mut recheck = Recheck {
        number,
        register,
        text,
        sentences,
        parts,
        unit_flags,
    };
    let document_flags =
        passes::counted_document_pass(inner, &recheck.register, &recheck.mode_sentences());
    for UnitFlag { sentence, flag } in document_flags {
        recheck.unit_flags[sentence].push(flag);
    }
    log::debug!(
        "recheck {number} holds {} sentences of prose, {} of fragments and {} of code, and {} of \
         them have an incomplete grammar part",
        recheck.count_in(FormMode::Prose),
        recheck.count_in(FormMode::Fragment),
        recheck.count_in(FormMode::Code),
        recheck.incomplete_sentences().len()
    );
    recheck
}

/// The parts of `sentence`, a sentence of a paragraph of `mode`, as the deterministic stage leaves
/// them under `register`. No source checks code, so both parts are complete and empty. The
/// sentence pass checks a fragment and prose. A fragment's grammar part is complete and empty,
/// since grammar does not run on fragments, and prose's is left incomplete for the grammar stage.
fn deterministic_parts(
    inner: &Inner,
    register: &str,
    sentence: &str,
    mode: FormMode,
) -> SentenceParts {
    match mode {
        FormMode::Code => SentenceParts {
            deterministic: Vec::new(),
            grammar: Some(Vec::new()),
        },
        FormMode::Fragment => SentenceParts {
            deterministic: deterministic_flags(inner, register, sentence, mode),
            grammar: Some(Vec::new()),
        },
        FormMode::Prose => SentenceParts {
            deterministic: deterministic_flags(inner, register, sentence, mode),
            grammar: None,
        },
    }
}

/// The analyzer's sentence pass over `sentence`, a sentence of a paragraph of `mode`, under
/// `register`, by [`passes::sentence_pass`].
fn deterministic_flags(inner: &Inner, register: &str, sentence: &str, mode: FormMode) -> Vec<Flag> {
    #[cfg(test)]
    inner
        .deterministic_checks
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    passes::sentence_pass(&inner.sources(register), sentence, mode)
}

/// Sends `grammar` each sentence of `recheck` whose grammar part is incomplete, which is each
/// sentence of prose with no complete entry, in document order and one at a time. Each answer is
/// cached as it arrives, so a newer recheck that aborts the stage keeps the answers it already had.
/// A sentence that two share, by mode and text, is sent once. Publishes the second update once any
/// check has answered; with none answered, the first update already holds every flag in hand.
async fn grammar_stage(inner: Arc<Inner>, grammar: Arc<dyn GrammarSource>, mut recheck: Recheck) {
    let mut checked: HashMap<u64, Option<Vec<Flag>>> = HashMap::new();
    let mut any_answered = false;
    for index in recheck.incomplete_sentences() {
        let key = recheck.sentences[index].key;
        let known = checked.get(&key).cloned();
        let grammar_flags = match known {
            Some(grammar_flags) => grammar_flags,
            None => {
                let sentence = recheck.sentence(index);
                let mode = recheck.sentences[index].mode;
                let grammar_flags =
                    check_grammar(&inner, grammar.as_ref(), &recheck.register, sentence, mode)
                        .await;
                if let Some(flags) = &grammar_flags {
                    let complete = SentenceParts {
                        deterministic: recheck.parts[index].deterministic.clone(),
                        grammar: Some(flags.clone()),
                    };
                    inner
                        .cache
                        .lock()
                        .expect("a poisoned lock here means another thread already panicked")
                        .put(key, complete);
                }
                checked.insert(key, grammar_flags.clone());
                grammar_flags
            }
        };
        any_answered |= grammar_flags.is_some();
        recheck.parts[index].grammar = grammar_flags;
    }
    if any_answered {
        publish(
            &inner,
            recheck.number,
            Stage::Grammar,
            recheck.document_flags(&inner),
        );
    }
}

/// `sentence`'s grammar flags from `grammar`, checked under `mode`, its paragraph's, and
/// `register`, or `None` when the check failed, which leaves the sentence's grammar part
/// incomplete.
async fn check_grammar(
    inner: &Inner,
    grammar: &dyn GrammarSource,
    register: &str,
    sentence: &str,
    mode: FormMode,
) -> Option<Vec<Flag>> {
    let checked = grammar
        .check(sentence, &inner.ruleset, mode, register)
        .await;
    match checked {
        Ok(grammar_flags) => Some(grammar_flags),
        Err(error) => {
            log::debug!(
                "the grammar check of a sentence of {} bytes failed, so it has no grammar flags \
                 this time and is sent again on the next recheck: {error}",
                sentence.len()
            );
            None
        }
    }
}

impl Inner {
    /// The analyzer's own handles as the passes read them under `register`, the recheck's. Typed
    /// text rewrites nothing, so spelling has no source text.
    fn sources<'s>(&'s self, register: &'s str) -> Sources<'s> {
        Sources {
            spelling: &self.spelling,
            spelling_source: None,
            catalogue: &self.catalogue,
            ruleset: &self.ruleset,
            register,
        }
    }
}

impl Recheck {
    fn sentence(&self, index: usize) -> &str {
        &self.text[self.sentences[index].range.clone()]
    }

    /// Every sentence of the document with its paragraph's mode, in document order.
    fn mode_sentences(&self) -> Vec<ModeSentence<'_>> {
        self.sentences
            .iter()
            .enumerate()
            .map(|(index, sentence)| ModeSentence {
                text: self.sentence(index),
                mode: sentence.mode,
            })
            .collect()
    }

    /// The indices of the sentences whose grammar part is incomplete, each a sentence of prose.
    fn incomplete_sentences(&self) -> Vec<usize> {
        (0..self.parts.len())
            .filter(|&index| self.parts[index].grammar.is_none())
            .collect()
    }

    /// How many of the sentences are of paragraphs of `mode`.
    fn count_in(&self, mode: FormMode) -> usize {
        self.sentences
            .iter()
            .filter(|sentence| sentence.mode == mode)
            .count()
    }

    /// The whole document's flags from the parts in hand: for each sentence, its deterministic
    /// part, the paragraph and document passes' flags anchored on it, and its grammar part where
    /// complete, suppressed together, then scoped to the document and stamped with the ruleset
    /// version and the catalogue hash in force.
    fn document_flags(&self, inner: &Inner) -> Vec<Flag> {
        let mut document_flags = Vec::new();
        let sentences = self.sentences.iter().zip(&self.parts).zip(&self.unit_flags);
        for (index, ((sentence, parts), unit_flags)) in sentences.enumerate() {
            let in_hand: Vec<Flag> = parts
                .deterministic
                .iter()
                .chain(unit_flags)
                .chain(parts.grammar.iter().flatten())
                .cloned()
                .collect();
            let text_before_sentence = &self.text[..sentence.range.start];
            let suppressed = dedupe::apply(self.sentence(index), in_hand);
            document_flags.extend(suppressed.into_iter().map(|flag| {
                placed_in_document(
                    flag,
                    index,
                    text_before_sentence,
                    &inner.ruleset,
                    &inner.catalogue,
                )
            }));
        }
        document_flags
    }
}

/// `flag`, found in the sentence of index `sentence_index` with `text_before_sentence` before it in
/// the document, as it is published: scoped to the document by [`with_document_scope`], given its
/// occurrence index by [`with_occurrence`], and stamped with the version of `ruleset` and the hash
/// of `catalogue`, the ruleset and the catalogue in force. The screen places a candidate's flags in
/// the candidate by it too.
pub(crate) fn placed_in_document(
    flag: Flag,
    sentence_index: usize,
    text_before_sentence: &str,
    ruleset: &LoadedRuleset,
    catalogue: &Catalogue,
) -> Flag {
    Flag {
        ruleset_version: ruleset.version(),
        catalogue_hash: Some(catalogue.hash().clone()),
        ..with_document_scope(sentence_index, with_occurrence(text_before_sentence, flag))
    }
}

/// Prefixes `flag`'s id with the position in the document of the sentence it was found in. Every
/// checking source numbers its flags within the sentence it anchors them on, so two sentences
/// whose flags number the same way arrive with the same id: a misspelled first word in two
/// sentences produces `spelling:0` twice. The overlay keys both its rendered underlines
/// and its hover lookup by id, so a duplicate makes one flag's card open for the other.
///
/// The sentence's index, rather than the hash the cache is keyed by, because the index is the
/// only part of a sentence's identity that separates two textually identical sentences. It shifts
/// when a sentence is inserted earlier in the document, which re-keys the flags after it; that
/// costs a remount of those underlines on the next emit, which is already a whole-set emit. Within
/// one recheck the index is fixed, so a flag in both of its updates has one id.
fn with_document_scope(sentence_index: usize, flag: Flag) -> Flag {
    Flag {
        id: format!("{sentence_index}:{}", flag.id),
        ..flag
    }
}

/// Sets `flag`'s occurrence index from the document text before the sentence it was found in, by
/// the rule on [`crate::flag::Span::occurrence`]: `matches` scans left to right and does not
/// overlap, as the native backend's search does. Counted here, as each update is assembled, and
/// not cached with the flag, so a cached sentence that an identical one now precedes gets its new
/// index.
fn with_occurrence(text_before_sentence: &str, mut flag: Flag) -> Flag {
    flag.span.occurrence = text_before_sentence
        .matches(flag.span.anchor.as_str())
        .count();
    flag
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The key of `text`, a sentence or a paragraph of `mode`, in the sentence cache or the paragraph
/// cache, checked under the ruleset of `version` and `register`: the hash of the four together. One
/// text is checked by different sources in a paragraph of each mode, an entry made under one
/// ruleset version holds flags of that version's checkers, and one made under one register holds
/// the flags of the checkers and the patterns that run in it, so neither is served under another.
fn cache_key(version: Option<RulesetVersion>, register: &str, mode: FormMode, text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    version.hash(&mut hasher);
    register.hash(&mut hasher);
    mode.hash(&mut hasher);
    text.hash(&mut hasher);
    hasher.finish()
}

fn cache_capacity() -> NonZeroUsize {
    NonZeroUsize::new(CACHE_CAPACITY).expect("CACHE_CAPACITY is a nonzero constant")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::discriminant;
    use std::sync::Mutex as StdMutex;

    use crate::capture::{CaptureError, CursorRect, Foreground, Surface};
    use crate::flag::{FlagOrigin, RulesetVersion, Span};
    use crate::test_support::{test_spell_checker, Answer, FakeGrammar};

    /// The catalogue compiled from the toggles the settings file is first written with.
    pub(super) fn default_catalogue() -> Catalogue {
        Catalogue::load(&Config::default())
    }

    /// A surface of the process `process_name` with no page URL and no document key.
    pub(super) fn surface_of(process_name: &str) -> Surface {
        Surface {
            process_name: process_name.to_owned(),
            page_url: None,
            document_key: None,
        }
    }

    /// A capture backend holding one text and one surface, which counts its reads of each and
    /// raises the change notification when a test says so.
    pub(super) struct FakeCapture {
        text: StdMutex<String>,
        /// The surface it answers, or `None` to answer [`CaptureError::Unsupported`], as the web
        /// backend does.
        surface: StdMutex<Option<Surface>>,
        changes: watch::Sender<u64>,
        reads: std::sync::atomic::AtomicUsize,
        surface_reads: std::sync::atomic::AtomicUsize,
    }

    impl FakeCapture {
        /// Holding `text`, on a surface of a process the default settings map no register to.
        pub(super) fn new(text: &str) -> Self {
            Self {
                text: StdMutex::new(text.to_string()),
                surface: StdMutex::new(Some(surface_of("fake.exe"))),
                changes: watch::channel(0).0,
                reads: std::sync::atomic::AtomicUsize::new(0),
                surface_reads: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        /// Holding `text` on `surface`, `None` for one the backend cannot name.
        pub(super) fn on(surface: Option<Surface>, text: &str) -> Self {
            let fake = Self::new(text);
            *fake.surface.lock().unwrap() = surface;
            fake
        }

        /// Moves focus to a document holding `text` on `surface`, `None` for one the backend
        /// cannot name, and raises the change notification, as the native backend does when focus
        /// scopes a new element.
        pub(super) fn focus(&self, surface: Option<Surface>, text: &str) {
            *self.surface.lock().unwrap() = surface;
            self.set_text(text);
        }

        /// How many times the surface has been read.
        pub(super) fn surface_reads(&self) -> usize {
            self.surface_reads.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// Replaces the text and raises the change notification, as a control that raises the
        /// text-changed event does, and as the native backend does on a focus change.
        pub(super) fn set_text(&self, text: &str) {
            self.set_text_without_event(text);
            self.raise_event();
        }

        /// Replaces the text with no notification, as a control that raises no text-changed event.
        pub(super) fn set_text_without_event(&self, text: &str) {
            *self.text.lock().unwrap() = text.to_string();
        }

        /// Raises the change notification with the text unchanged, as a change of formatting
        /// alone does.
        pub(super) fn raise_event(&self) {
            self.changes.send_modify(|count| *count += 1);
        }

        /// How many times the text has been read.
        pub(super) fn reads(&self) -> usize {
            self.reads.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Capture for FakeCapture {
        async fn current_text(&self) -> Result<String, CaptureError> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.text.lock().unwrap().clone())
        }

        fn changes(&self) -> watch::Receiver<u64> {
            self.changes.subscribe()
        }

        async fn replace(&self, _span: &Span, _replacement: &str) -> Result<(), CaptureError> {
            Ok(())
        }

        async fn document_view_rect(&self) -> Result<CursorRect, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        async fn foreground(&self) -> Result<Foreground, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        async fn span_rect(&self, _span: &Span) -> Result<Vec<CursorRect>, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        async fn surface(&self) -> Result<Surface, CaptureError> {
            self.surface_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.surface
                .lock()
                .unwrap()
                .clone()
                .ok_or(CaptureError::Unsupported)
        }
    }

    /// The paused clock's step in [`step`].
    const STEP: Duration = Duration::from_millis(200);

    /// Advances the paused clock in [`STEP`]s rather than one combined-duration jump. A single
    /// large `tokio::time::advance` call was found not to reliably let a spawned task observe every
    /// intermediate timer within it, and the debounce loop sets a new deadline at each change and
    /// each read. Each step sleeps rather than calling `advance`, which does not wait for the tasks
    /// whose timers it passes: the paused clock then advances timer by timer once every task is
    /// idle, so the loop wakes at each of its deadlines, as outside a test, and a change a test
    /// makes reaches the loop at the time the test made it.
    pub(super) async fn step(steps: u32) {
        for _ in 0..steps {
            tokio::time::sleep(STEP).await;
        }
    }

    /// The fewest [`STEP`]s that span `duration`.
    fn steps_in(duration: Duration) -> u32 {
        let steps = duration.as_millis().div_ceil(STEP.as_millis());
        u32::try_from(steps).expect("a test's durations are a few seconds")
    }

    /// The exact text a flag's span addresses, sliced back out of its own anchor, so a test can
    /// assert on what was flagged without depending on how the span happens to be anchored.
    pub(super) fn flagged_text(flag: &Flag) -> String {
        let anchor: Vec<u16> = flag.span.anchor.encode_utf16().collect();
        let end = flag.span.local_start + flag.span.local_length;
        String::from_utf16_lossy(&anchor[flag.span.local_start..end])
    }

    /// The flags `analyzer` last published, read from a fresh subscription.
    pub(super) fn published_flags(analyzer: &Analyzer) -> Vec<Flag> {
        analyzer.subscribe().borrow().clone()
    }

    pub(super) fn is_telltale(flag: &Flag) -> bool {
        matches!(flag.origin, FlagOrigin::AiTell { .. })
    }

    /// The texts `flags` flag with `origin`'s kind, in order.
    fn flagged_by(flags: &[Flag], origin: &FlagOrigin) -> Vec<String> {
        flags
            .iter()
            .filter(|flag| discriminant(&flag.origin) == discriminant(origin))
            .map(flagged_text)
            .collect()
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
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        );

        // Let the loop see the initial text and start its quiet timer; no recheck yet.
        step(1).await;
        assert!(
            published_flags(&analyzer).is_empty(),
            "no recheck should fire before the quiet threshold elapses"
        );

        // An edit shortly after resets the timer instead of letting the original one fire.
        step(1).await;
        fake.set_text("This has a mispelling, still.");
        step(1).await;
        assert!(
            published_flags(&analyzer).is_empty(),
            "the edit should have reset the quiet timer"
        );

        // Now let it sit unchanged long enough (comfortably past QUIET_THRESHOLD) for the reset
        // timer to elapse.
        step(6).await;
        let flags = published_flags(&analyzer);
        assert!(
            flags.iter().any(|flag| flagged_text(flag) == "mispelling"),
            "expected a recheck to have found the misspelling by now: {flags:#?}"
        );
    }

    /// An analyzer with no grammar source over `fake`, stepped past its first recheck: the safety
    /// poll reads the initial text at once, and the recheck reads it again after the quiet
    /// threshold.
    async fn checked_analyzer(fake: &Arc<FakeCapture>) -> Analyzer {
        let analyzer = analyzer_on(fake);
        step(6).await;
        assert_eq!(analyzer.updates().len(), 1);
        assert_eq!(fake.reads(), 2);
        analyzer
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_events_is_read_once_and_rechecked_once_after_the_quiet_threshold() {
        let fake = Arc::new(FakeCapture::new("A fine start."));
        let analyzer = checked_analyzer(&fake).await;

        // One character a step, each with an event, for longer than the safety poll's interval.
        let typed = " Then a mispeling";
        assert!(STEP * u32::try_from(typed.len()).unwrap() > SAFETY_POLL_INTERVAL);
        let mut text = String::from("A fine start.");
        for character in typed.chars() {
            text.push(character);
            fake.set_text(&text);
            step(1).await;
            assert_eq!(fake.reads(), 2, "nothing is read while the typing goes on");
        }
        assert_eq!(analyzer.updates().len(), 1);

        step(3).await;
        assert_eq!(fake.reads(), 3, "the pause is read once");
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "and rechecked once: {updates:#?}");
        assert_eq!(
            flagged_by(&updates[1], &FlagOrigin::Spelling),
            ["mispeling"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn typing_that_never_pauses_for_the_quiet_threshold_is_never_rechecked() {
        let fake = Arc::new(FakeCapture::new("A fine start."));
        let analyzer = checked_analyzer(&fake).await;

        // An edit every 400 ms for twelve seconds, six of the safety poll's intervals.
        let mut text = String::from("A fine start.");
        for _ in 0..30 {
            text.push_str(" an eror");
            fake.set_text(&text);
            step(2).await;
        }
        assert_eq!(fake.reads(), 2);
        assert_eq!(analyzer.updates().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_with_no_event_is_found_by_the_safety_poll() {
        let fake = Arc::new(FakeCapture::new("A fine start."));
        let analyzer = checked_analyzer(&fake).await;

        fake.set_text_without_event("A fine start. Then a mispeling.");
        let mut stepped = 0;
        while fake.reads() == 2 {
            assert!(
                stepped < steps_in(SAFETY_POLL_INTERVAL),
                "the poll reads within its interval"
            );
            step(1).await;
            stepped += 1;
        }
        assert_eq!(fake.reads(), 3);
        assert_eq!(
            analyzer.updates().len(),
            1,
            "the poll's read waits for the quiet threshold"
        );

        step(steps_in(QUIET_THRESHOLD)).await;
        assert_eq!(fake.reads(), 4, "the recheck reads the text again");
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        assert_eq!(
            flagged_by(&updates[1], &FlagOrigin::Spelling),
            ["mispeling"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_control_with_no_event_is_not_rechecked_while_its_text_keeps_changing() {
        let fake = Arc::new(FakeCapture::new("A fine start."));
        let analyzer = checked_analyzer(&fake).await;

        // An edit every 200 ms for eight seconds, four of the safety poll's intervals, with no
        // event.
        let mut text = String::from("A fine start.");
        for _ in 0..40 {
            text.push_str(" an eror");
            fake.set_text_without_event(&text);
            step(1).await;
            assert_eq!(
                analyzer.updates().len(),
                1,
                "no recheck in the middle of the burst"
            );
        }
        assert!(fake.reads() > 2, "the poll found the change");

        step(steps_in(SAFETY_POLL_INTERVAL)).await;
        let updates = analyzer.updates();
        assert_eq!(
            updates.len(),
            2,
            "one recheck once the text holds: {updates:#?}"
        );
        assert_eq!(flagged_by(&updates[1], &FlagOrigin::Spelling).len(), 40);
    }

    #[tokio::test(start_paused = true)]
    async fn the_safety_poll_reads_an_unchanged_document_at_its_interval_and_rechecks_nothing() {
        let fake = Arc::new(FakeCapture::new("A fine start. Then a mispeling."));
        let analyzer = checked_analyzer(&fake).await;

        step(5 * steps_in(SAFETY_POLL_INTERVAL)).await;
        assert_eq!(fake.reads(), 2 + 5);
        assert_eq!(analyzer.updates().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_whose_read_holds_the_text_last_checked_rechecks_nothing() {
        let fake = Arc::new(FakeCapture::new("A fine start."));
        let analyzer = checked_analyzer(&fake).await;

        // A change of formatting alone raises the event and leaves the text as it was.
        fake.raise_event();
        step(4).await;
        assert_eq!(fake.reads(), 3, "the pause is read");
        assert_eq!(analyzer.updates().len(), 1, "and nothing is rechecked");
    }

    #[tokio::test(start_paused = true)]
    async fn a_focus_change_checks_the_new_document_and_drops_the_previous_documents_flags() {
        let fake = Arc::new(FakeCapture::new("An eror here."));
        let analyzer = checked_analyzer(&fake).await;
        let flags = published_flags(&analyzer);
        assert_eq!(flagged_by(&flags, &FlagOrigin::Spelling), ["eror"]);

        // The native backend raises the notification when focus scopes a new element.
        fake.set_text("Another document with a mispeling.");
        step(4).await;
        assert_eq!(fake.reads(), 3);
        let flags = published_flags(&analyzer);
        assert_eq!(
            flagged_by(&flags, &FlagOrigin::Spelling),
            ["mispeling"],
            "{flags:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_while_the_grammar_stage_is_in_flight_abandons_its_remaining_sentences() {
        let stale = ["He go to school.", "She go home."];
        let current = "We go home.";
        let fake = Arc::new(FakeCapture::new(&stale.join(" ")));
        let grammar = FakeGrammar::flagging(&["go"]);
        grammar.hold();
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        assert_eq!(grammar.sent(), [stale[0]]);

        fake.set_text(current);
        step(4).await;
        grammar.release();
        step(3).await;
        assert_eq!(
            grammar.sent(),
            [stale[0], current],
            "the abandoned stage sent nothing more"
        );
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 3, "{updates:#?}");
        let grammar_anchors: Vec<&str> = updates[2]
            .iter()
            .filter(|flag| flag.origin == FlagOrigin::Grammar)
            .map(|flag| flag.span.anchor.as_str())
            .collect();
        assert_eq!(grammar_anchors, [current], "{updates:#?}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_paragraph_of_a_document_past_words_part_length_is_checked() {
        // Word's document range returns 65,000 characters a call (#153), and the native read joins
        // the parts (#160); the analyzer checks the whole text it is handed.
        let filler = vec!["This sentence is fine."; 3000].join("\r");
        let text = format!("{filler}\rThe last paragraph has a mispelling.");
        assert!(text.chars().count() > 65_000);
        let fake = Arc::new(FakeCapture::new(&text));
        let capture: Arc<dyn Capture> = Arc::clone(&fake) as Arc<dyn Capture>;
        let analyzer = Analyzer::start(
            capture,
            test_spell_checker(),
            None,
            Arc::new(LoadedRuleset::empty()),
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        );

        step(6).await;
        let flags = published_flags(&analyzer);
        let misspelt: Vec<&Flag> = flags
            .iter()
            .filter(|flag| flagged_text(flag) == "mispelling")
            .collect();
        assert_eq!(misspelt.len(), 1, "{flags:#?}");
        assert_eq!(
            misspelt[0].span.anchor,
            "The last paragraph has a mispelling."
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
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        );

        step(6).await;
        let first_pass = published_flags(&analyzer);
        assert!(
            first_pass
                .iter()
                .any(|flag| flagged_text(flag) == "mispelling"),
            "expected the first recheck to find the misspelling: {first_pass:#?}"
        );
        // Both sentences are new to the cache on this first pass, so both were checked for real.
        assert_eq!(analyzer.deterministic_check_count(), 2);

        // Only the second sentence changes; the first sentence's text is untouched.
        fake.set_text("This has a mispelling. This one has an eror too.");
        step(6).await;
        let second_pass = published_flags(&analyzer);

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
        assert_eq!(analyzer.deterministic_check_count(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sentence_whose_text_returns_to_a_cached_value_is_served_from_the_cache() {
        let fake = Arc::new(FakeCapture::new("This has a mispelling. This one is fine."));
        let analyzer = analyzer_on(&fake);
        step(6).await;
        assert_eq!(analyzer.deterministic_check_count(), 2);

        fake.set_text("This has a mispelling. This one has an eror.");
        step(6).await;
        assert_eq!(analyzer.deterministic_check_count(), 3);

        // The second sentence goes back to the text its first check cached.
        fake.set_text("This has a mispelling. This one is fine.");
        step(6).await;
        assert_eq!(analyzer.deterministic_check_count(), 3);
        let flags = published_flags(&analyzer);
        let flagged: Vec<String> = flags.iter().map(flagged_text).collect();
        assert_eq!(flagged, ["mispelling"], "{flags:#?}");
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
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        );

        step(6).await;
        let flags = published_flags(&analyzer);
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
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        )
    }

    fn analyzer_with_grammar(fake: &Arc<FakeCapture>, grammar: &Arc<FakeGrammar>) -> Analyzer {
        Analyzer::start(
            Arc::clone(fake) as Arc<dyn Capture>,
            test_spell_checker(),
            Some(Arc::clone(grammar) as Arc<dyn GrammarSource>),
            Arc::new(LoadedRuleset::empty()),
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        )
    }

    /// The spans of the published flags on "eror", in the order the analyzer published them.
    fn eror_spans(analyzer: &Analyzer) -> Vec<Span> {
        published_flags(analyzer)
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
        assert_eq!(analyzer.deterministic_check_count(), 2);

        // A copy inserted at the start moves the original to third place, and the cache serves
        // both copies.
        fake.set_text("An eror here. Fine start. An eror here.");
        step(6).await;
        assert_eq!(eror_spans(&analyzer), [eror_here(0), eror_here(1)]);
        assert_eq!(analyzer.deterministic_check_count(), 2);
        let cached = cached_parts(&analyzer, FormMode::Prose, "An eror here.").unwrap();
        assert!(
            cached
                .deterministic
                .iter()
                .all(|flag| flag.span.occurrence == 0),
            "{cached:#?}"
        );
    }

    /// Three sentences: a misspelling, a telltale, and a grammar error the fake flags on "go".
    const THREE_SOURCES: &str = "This has a mispelling. It stands as a testament to the team. \
                                 He go to school.";

    #[tokio::test(start_paused = true)]
    async fn the_first_update_holds_the_deterministic_flags_before_any_grammar_check_answers() {
        let fake = Arc::new(FakeCapture::new(THREE_SOURCES));
        let grammar = FakeGrammar::flagging(&["go"]);
        grammar.hold();
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;

        assert!(!grammar.sent().is_empty(), "the grammar stage is in flight");
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 1, "{updates:#?}");
        let first = &updates[0];
        assert_eq!(flagged_by(first, &FlagOrigin::Spelling), ["mispelling"]);
        let telltales: Vec<String> = first
            .iter()
            .filter(|flag| is_telltale(flag))
            .map(flagged_text)
            .collect();
        assert_eq!(telltales, ["stands as a testament to"], "{first:#?}");
        assert!(
            flagged_by(first, &FlagOrigin::Grammar).is_empty(),
            "{first:#?}"
        );

        grammar.release();
        step(1).await;
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        assert_eq!(grammar.sent().len(), 3);
        let added: Vec<&Flag> = updates[1]
            .iter()
            .filter(|flag| !updates[0].contains(flag))
            .collect();
        assert_eq!(added.len(), 1, "{:#?}", updates[1]);
        assert_eq!(added[0].origin, FlagOrigin::Grammar);
        assert_eq!(added[0].id, "2:grammar:0:TEST_RULE");
        assert_eq!(flagged_text(added[0]), "go");
        assert!(
            updates[0].iter().all(|flag| updates[1].contains(flag)),
            "{updates:#?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_soft_flag_a_grammar_flag_overlaps_is_in_the_first_update_and_gone_from_the_second() {
        let fake = Arc::new(FakeCapture::new("It stands as a testament to the team."));
        let grammar = FakeGrammar::flagging(&["a testament"]);
        grammar.hold();
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        grammar.release();
        step(1).await;

        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        assert!(updates[0].iter().any(is_telltale), "{:#?}", updates[0]);
        assert!(!updates[1].iter().any(is_telltale), "{:#?}", updates[1]);
        assert_eq!(
            flagged_by(&updates[1], &FlagOrigin::Grammar),
            ["a testament"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_soft_flag_a_spelling_flag_overlaps_is_absent_from_the_first_update() {
        // The telltale runs from "It's not just" to "it's", over the misspelling between.
        let misspelled = "It's not just a mispeling, it's a habit.";
        let spelled = "It's not just a misspelling, it's a habit.";
        let fake = Arc::new(FakeCapture::new(spelled));
        let grammar = FakeGrammar::flagging(&[]);
        grammar.hold();
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        let first = published_flags(&analyzer);
        assert_eq!(
            first.iter().filter(|flag| is_telltale(flag)).count(),
            1,
            "{first:#?}"
        );

        fake.set_text(misspelled);
        step(6).await;
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        let first = &updates[1];
        assert_eq!(flagged_by(first, &FlagOrigin::Spelling), ["mispeling"]);
        assert!(!first.iter().any(is_telltale), "{first:#?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unchanged_complete_sentence_sends_no_check_and_its_grammar_flags_publish_first() {
        let unchanged = "It stands as a testament to the team.";
        let fake = Arc::new(FakeCapture::new(&format!("{unchanged} He go to school.")));
        let grammar = FakeGrammar::flagging(&["a testament", "go"]);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        assert_eq!(analyzer.updates().len(), 2);
        assert_eq!(grammar.sent(), [unchanged, "He go to school."]);

        grammar.hold();
        fake.set_text(&format!("{unchanged} She went to school."));
        step(6).await;
        assert_eq!(
            grammar.sent(),
            [unchanged, "He go to school.", "She went to school."]
        );
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 3, "{updates:#?}");
        let first = &updates[2];
        assert_eq!(flagged_by(first, &FlagOrigin::Grammar), ["a testament"]);
        assert!(!first.iter().any(is_telltale), "{first:#?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_or_timed_out_check_leaves_the_part_incomplete_and_the_next_recheck_resends() {
        let sentence = "He go to school.";
        for failure in [Answer::RequestFailure, Answer::Timeout] {
            let fake = Arc::new(FakeCapture::new(sentence));
            let grammar = FakeGrammar::flagging(&["go"]);
            grammar.answer_with(failure);
            let analyzer = analyzer_with_grammar(&fake, &grammar);
            step(6).await;
            assert_eq!(grammar.sent(), [sentence], "{failure:?}");
            // No check answered, so the first update stands alone.
            assert_eq!(analyzer.updates().len(), 1, "{failure:?}");

            grammar.answer_with(Answer::Flags);
            fake.set_text(&format!("{sentence} Fine start."));
            step(6).await;
            assert_eq!(
                grammar.sent(),
                [sentence, sentence, "Fine start."],
                "{failure:?}"
            );
            assert_eq!(
                flagged_by(&published_flags(&analyzer), &FlagOrigin::Grammar),
                ["go"],
                "{failure:?}"
            );
            assert_eq!(analyzer.deterministic_check_count(), 2, "{failure:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_degraded_source_marks_nothing_complete() {
        let sentences = ["He go to school.", "Fine start."];
        let fake = Arc::new(FakeCapture::new(&sentences.join(" ")));
        let grammar = FakeGrammar::flagging(&["go"]);
        grammar.answer_with(Answer::Degraded);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        assert_eq!(grammar.sent(), sentences);
        assert_eq!(analyzer.updates().len(), 1);
        for sentence in sentences {
            let parts = cached_parts(&analyzer, FormMode::Prose, sentence).unwrap();
            assert!(parts.grammar.is_none(), "{sentence}: {parts:#?}");
        }

        fake.set_text(&format!("{} Another one.", sentences.join(" ")));
        step(6).await;
        assert_eq!(
            grammar.sent(),
            [
                sentences[0],
                sentences[1],
                sentences[0],
                sentences[1],
                "Another one."
            ]
        );
        assert_eq!(analyzer.updates().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_edit_in_flight_rechecks_the_new_text_and_the_stale_stage_never_publishes() {
        let stale = "He go to school.";
        let current = "We go home.";
        let fake = Arc::new(FakeCapture::new(stale));
        let grammar = FakeGrammar::flagging(&["go"]);
        grammar.hold();
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        assert_eq!(grammar.sent(), [stale]);
        assert_eq!(analyzer.updates().len(), 1);

        // The stale stage's check is still held when the edit lands.
        fake.set_text(current);
        step(6).await;
        assert_eq!(grammar.sent(), [stale, current]);
        assert_eq!(analyzer.updates().len(), 2);

        grammar.release();
        step(3).await;
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 3, "{updates:#?}");
        let grammar_anchors: Vec<&str> = updates
            .iter()
            .flatten()
            .filter(|flag| flag.origin == FlagOrigin::Grammar)
            .map(|flag| flag.span.anchor.as_str())
            .collect();
        assert_eq!(grammar_anchors, [current], "{updates:#?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_recheck_that_a_newer_one_replaced_publishes_nothing() {
        let fake = Arc::new(FakeCapture::new(""));
        let analyzer = analyzer_on(&fake);
        let flags = FakeGrammar::flagging(&["go"]).grammar_flags("He go to school.");

        let replaced = start_recheck(&analyzer.inner);
        let latest = start_recheck(&analyzer.inner);
        publish(&analyzer.inner, replaced, Stage::Grammar, flags.clone());
        assert!(analyzer.updates().is_empty());
        assert!(published_flags(&analyzer).is_empty());

        publish(&analyzer.inner, latest, Stage::Grammar, flags.clone());
        assert_eq!(published_flags(&analyzer), flags);
        assert_eq!(analyzer.updates(), [flags]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_flag_in_both_updates_of_a_recheck_has_one_id() {
        let fake = Arc::new(FakeCapture::new(
            "He go to the shcool. It stands as a testament to the team.",
        ));
        let grammar = FakeGrammar::flagging(&["go"]);
        grammar.hold();
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        grammar.release();
        step(1).await;

        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        assert_eq!(updates[0].len(), 2, "{:#?}", updates[0]);
        for flag in &updates[0] {
            let again: Vec<&Flag> = updates[1]
                .iter()
                .filter(|later| later.id == flag.id)
                .collect();
            assert_eq!(again, [flag], "{updates:#?}");
        }
        let ids: Vec<&str> = updates[1].iter().map(|flag| flag.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "0:spelling:13",
                "0:grammar:0:TEST_RULE",
                "1:ai-tell:significance-legacy-emphasis:3:21",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_recheck_checks_exactly_the_sentences_with_no_complete_entry() {
        let fake = Arc::new(FakeCapture::new(
            "First one is here. Second one is here. Third one is here.",
        ));
        let grammar = FakeGrammar::flagging(&[]);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;
        assert_eq!(
            grammar.sent(),
            [
                "First one is here.",
                "Second one is here.",
                "Third one is here."
            ]
        );
        assert_eq!(analyzer.deterministic_check_count(), 3);

        // One sentence edited, and one added twice, which is one text and so one check.
        fake.set_text(
            "First one is here. Second one changed. Third one is here. Fourth one is here. \
             Fourth one is here.",
        );
        step(6).await;
        assert_eq!(
            grammar.sent()[3..],
            ["Second one changed.", "Fourth one is here."]
        );
        assert_eq!(analyzer.deterministic_check_count(), 5);
        assert_eq!(analyzer.updates().len(), 4);
    }

    /// The flags an analyzer started with `ruleset_version` publishes for a misspelled sentence
    /// and a telltale one, with a grammar flag the fake grammar source raises on the first
    /// sentence, carrying a version the analyzer does not hold and no catalogue hash. Only the
    /// second sentence then changes, so the first is served from the cache, its grammar part
    /// complete.
    async fn flags_of_every_origin(ruleset_version: Option<RulesetVersion>) -> Vec<Flag> {
        let misspelled = "This has a mispelling.";
        let fake = Arc::new(FakeCapture::new(&format!(
            "{misspelled} It stands as a testament to the team."
        )));
        let grammar = FakeGrammar::flagging(&["This"]);
        let ruleset = ruleset_version.map_or_else(LoadedRuleset::empty, LoadedRuleset::empty_at);
        let analyzer = Analyzer::start(
            Arc::clone(&fake) as Arc<dyn Capture>,
            test_spell_checker(),
            Some(Arc::clone(&grammar) as Arc<dyn GrammarSource>),
            Arc::new(ruleset),
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        );
        step(6).await;

        fake.set_text(&format!(
            "{misspelled} It stands as a testament to the town."
        ));
        step(6).await;
        assert_eq!(
            grammar
                .sent()
                .iter()
                .filter(|sent| *sent == misspelled)
                .count(),
            1
        );
        let flags = published_flags(&analyzer);
        let telltale = FlagOrigin::AiTell {
            pattern_ids: Vec::new(),
        };
        for origin in [FlagOrigin::Spelling, FlagOrigin::Grammar, telltale] {
            assert!(
                flags
                    .iter()
                    .any(|flag| discriminant(&flag.origin) == discriminant(&origin)),
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
    async fn every_published_flag_carries_the_catalogue_hash_beside_the_ruleset_version() {
        let hash = default_catalogue().hash().clone();
        for version in [Some(RulesetVersion(7)), None] {
            let flags = flags_of_every_origin(version).await;
            assert!(
                flags.iter().all(|flag| {
                    flag.catalogue_hash.as_ref() == Some(&hash) && flag.ruleset_version == version
                }),
                "{flags:#?}"
            );
        }
    }

    /// Five sentences: a misspelling, a telltale, and two identical misspelled sentences around a
    /// clean one.
    pub(super) const PINNED_DOCUMENT: &str =
        "This has a mispelling. It stands as a testament to the team. An eror here. Fine start. \
         An eror here.";

    /// The flags an analyzer at ruleset version 7 publishes for [`PINNED_DOCUMENT`], pinned field
    /// by field from its output, so a change to how a recheck chooses the sentences it checks
    /// shows here as a change to what it publishes. The catalogue hash is read from the compiled
    /// catalogue rather than pinned, since an edit to the catalogue changes it without touching
    /// the analyzer.
    pub(super) fn pinned_flags() -> Vec<Flag> {
        let catalogue_hash = Some(default_catalogue().hash().clone());
        let eror = |id: &str, occurrence| Flag {
            id: id.to_owned(),
            origin: FlagOrigin::Spelling,
            span: eror_here(occurrence),
            message: "\"eror\" is not in the dictionary".to_owned(),
            suggestions: ["err", "error", "er or", "er-or", "emperor"]
                .map(str::to_owned)
                .to_vec(),
            source_detail: "en_GB plus the Singapore supplement".to_owned(),
            ruleset_version: Some(RulesetVersion(7)),
            catalogue_hash: catalogue_hash.clone(),
        };
        vec![
            Flag {
                id: "0:spelling:11".to_owned(),
                origin: FlagOrigin::Spelling,
                span: Span {
                    anchor: "This has a mispelling.".to_owned(),
                    local_start: 11,
                    local_length: 10,
                    occurrence: 0,
                },
                message: "\"mispelling\" is not in the dictionary".to_owned(),
                suggestions: [
                    "misspelling",
                    "dispelling",
                    "mi spelling",
                    "mi-spelling",
                    "mis-selling",
                ]
                .map(str::to_owned)
                .to_vec(),
                source_detail: "en_GB plus the Singapore supplement".to_owned(),
                ruleset_version: Some(RulesetVersion(7)),
                catalogue_hash: catalogue_hash.clone(),
            },
            Flag {
                id: "1:ai-tell:significance-legacy-emphasis:3:21".to_owned(),
                origin: FlagOrigin::AiTell {
                    pattern_ids: [
                        "significance-legacy-emphasis",
                        "copula-avoidance",
                        "ai-vocabulary-words",
                    ]
                    .map(str::to_owned)
                    .to_vec(),
                },
                span: Span {
                    anchor: "It stands as a testament to the team.".to_owned(),
                    local_start: 3,
                    local_length: 24,
                    occurrence: 0,
                },
                message: "Puffs up an ordinary fact by tying it to significance, legacy, or a \
                          broader trend"
                    .to_owned(),
                suggestions: Vec::new(),
                source_detail: "humanizer SKILL.md section 1, Wikipedia:Signs of AI writing"
                    .to_owned(),
                ruleset_version: Some(RulesetVersion(7)),
                catalogue_hash: catalogue_hash.clone(),
            },
            eror("2:spelling:3", 0),
            eror("4:spelling:3", 1),
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn a_fixed_document_publishes_its_pinned_flags_each_time_it_is_reached() {
        let fake = Arc::new(FakeCapture::new(PINNED_DOCUMENT));
        let grammar = FakeGrammar::flagging(&[]);
        let analyzer = Analyzer::start(
            Arc::clone(&fake) as Arc<dyn Capture>,
            test_spell_checker(),
            Some(Arc::clone(&grammar) as Arc<dyn GrammarSource>),
            Arc::new(LoadedRuleset::empty_at(RulesetVersion(7))),
            Arc::new(default_catalogue()),
            Arc::new(Config::default()),
        );
        step(6).await;
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        assert_eq!(updates[1], pinned_flags());

        fake.set_text("Something else entirely.");
        step(6).await;
        fake.set_text(PINNED_DOCUMENT);
        step(6).await;
        assert_eq!(published_flags(&analyzer), pinned_flags());
    }

    #[tokio::test(start_paused = true)]
    async fn with_no_ruleset_loaded_every_published_flag_carries_no_version() {
        let flags = flags_of_every_origin(None).await;
        assert!(
            flags.iter().all(|flag| flag.ruleset_version.is_none()),
            "{flags:#?}"
        );
    }

    /// The sentence cache's entry for `sentence` in a paragraph of `mode`, under the analyzer's
    /// ruleset version and the settings' default register, if it holds one.
    pub(super) fn cached_parts(
        analyzer: &Analyzer,
        mode: FormMode,
        sentence: &str,
    ) -> Option<SentenceParts> {
        let key = cache_key(
            analyzer.inner.ruleset.version(),
            &analyzer.inner.settings.default_register,
            mode,
            sentence,
        );
        analyzer.inner.cache.lock().unwrap().peek(&key).cloned()
    }

    fn telltales_in(flags: &[Flag]) -> Vec<String> {
        flags
            .iter()
            .filter(|flag| is_telltale(flag))
            .map(flagged_text)
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_code_paragraph_raises_no_flag_of_any_origin_and_none_of_its_text_is_sent() {
        let fenced = "This has a mispelling. It stands as a testament to the team. He go home.";
        // A run of two indented lines, code although neither line is symbol-dense.
        let indented = "    He go to the shcool.\n\tShe go there tomorow.";
        let fake = Arc::new(FakeCapture::new(&format!(
            "A fine start.\n```\n{fenced}\n```\n{indented}\nA fine end."
        )));
        let grammar = FakeGrammar::flagging(&["go", "fine"]);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;

        assert_eq!(grammar.sent(), ["A fine start.", "A fine end."]);
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        for update in &updates {
            assert!(
                update.iter().all(|flag| flag.span.anchor.contains("fine")),
                "{update:#?}"
            );
        }
        assert_eq!(
            flagged_by(&updates[1], &FlagOrigin::Grammar),
            ["fine", "fine"]
        );
        assert_eq!(updates[1].len(), 2, "{:#?}", updates[1]);
        for sentence in [
            "This has a mispelling.",
            "It stands as a testament to the team.",
            "He go home.",
            "He go to the shcool.",
            "She go there tomorow.",
        ] {
            let parts = cached_parts(&analyzer, FormMode::Code, sentence).unwrap();
            assert!(parts.deterministic.is_empty(), "{sentence}: {parts:#?}");
            assert_eq!(parts.grammar, Some(Vec::new()), "{sentence}");
        }
        // Only the two sentences of prose went through a deterministic check.
        assert_eq!(analyzer.deterministic_check_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_fragment_paragraph_gets_spelling_and_telltale_flags_and_none_of_its_text_is_sent() {
        let item = "- A mispelling that stands as a testament to the team, and we go";
        let fake = Arc::new(FakeCapture::new(&format!(
            "# We go on\n{item}\nA fine end."
        )));
        let grammar = FakeGrammar::flagging(&["go"]);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;

        assert_eq!(grammar.sent(), ["A fine end."]);
        let flags = published_flags(&analyzer);
        assert_eq!(flagged_by(&flags, &FlagOrigin::Spelling), ["mispelling"]);
        assert_eq!(telltales_in(&flags), ["stands as a testament to"]);
        assert!(
            flagged_by(&flags, &FlagOrigin::Grammar).is_empty(),
            "{flags:#?}"
        );
        assert!(
            flags.iter().all(|flag| flag.span.anchor == item),
            "{flags:#?}"
        );
        let parts = cached_parts(&analyzer, FormMode::Fragment, item).unwrap();
        assert_eq!(parts.grammar, Some(Vec::new()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_prose_paragraph_gets_spelling_telltale_and_grammar_flags() {
        let fake = Arc::new(FakeCapture::new(&format!("# Notes\n{THREE_SOURCES}")));
        let grammar = FakeGrammar::flagging(&["go"]);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;

        assert_eq!(
            grammar.sent(),
            [
                "This has a mispelling.",
                "It stands as a testament to the team.",
                "He go to school."
            ]
        );
        let flags = published_flags(&analyzer);
        assert_eq!(flagged_by(&flags, &FlagOrigin::Spelling), ["mispelling"]);
        assert_eq!(telltales_in(&flags), ["stands as a testament to"]);
        assert_eq!(flagged_by(&flags, &FlagOrigin::Grammar), ["go"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_lone_indented_paragraph_of_sentences_gets_spelling_telltale_and_grammar_flags() {
        // A first-line indent typed with a tab, on a line with no indented line beside it.
        let fake = Arc::new(FakeCapture::new(&format!("# Notes\n\t{THREE_SOURCES}")));
        let grammar = FakeGrammar::flagging(&["go"]);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;

        let sentences = [
            "This has a mispelling.",
            "It stands as a testament to the team.",
            "He go to school.",
        ];
        assert_eq!(grammar.sent(), sentences);
        let updates = analyzer.updates();
        assert_eq!(updates.len(), 2, "{updates:#?}");
        let first = &updates[0];
        assert_eq!(flagged_by(first, &FlagOrigin::Spelling), ["mispelling"]);
        assert_eq!(telltales_in(first), ["stands as a testament to"]);
        assert!(
            flagged_by(first, &FlagOrigin::Grammar).is_empty(),
            "{first:#?}"
        );
        assert_eq!(flagged_by(&updates[1], &FlagOrigin::Grammar), ["go"]);
        assert!(
            first.iter().all(|flag| updates[1].contains(flag)),
            "{updates:#?}"
        );
        for sentence in sentences {
            let parts = cached_parts(&analyzer, FormMode::Prose, sentence).unwrap();
            assert!(parts.grammar.is_some(), "{sentence}: {parts:#?}");
            assert!(
                cached_parts(&analyzer, FormMode::Code, sentence).is_none(),
                "{sentence}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn one_text_in_a_prose_paragraph_and_in_a_fragment_has_an_entry_for_each() {
        let shared = "He go to the shcool.";
        // The second paragraph is short and ends without terminal punctuation: a fragment.
        let fake = Arc::new(FakeCapture::new(&format!(
            "{shared} That was all.\n{shared} And"
        )));
        let grammar = FakeGrammar::flagging(&["go"]);
        let analyzer = analyzer_with_grammar(&fake, &grammar);
        step(6).await;

        assert_eq!(analyzer.deterministic_check_count(), 4);
        assert_eq!(grammar.sent(), [shared, "That was all."]);
        let prose = cached_parts(&analyzer, FormMode::Prose, shared).unwrap();
        let fragment = cached_parts(&analyzer, FormMode::Fragment, shared).unwrap();
        for parts in [&prose, &fragment] {
            assert_eq!(
                flagged_by(&parts.deterministic, &FlagOrigin::Spelling),
                ["shcool"]
            );
        }
        let prose_grammar = prose.grammar.unwrap();
        assert_eq!(flagged_by(&prose_grammar, &FlagOrigin::Grammar), ["go"]);
        assert_eq!(fragment.grammar, Some(Vec::new()));

        let flags = published_flags(&analyzer);
        let ids: Vec<&str> = flags.iter().map(|flag| flag.id.as_str()).collect();
        assert_eq!(
            ids,
            ["0:spelling:13", "0:grammar:0:TEST_RULE", "2:spelling:13"]
        );
        let occurrences: Vec<usize> = flags.iter().map(|flag| flag.span.occurrence).collect();
        assert_eq!(occurrences, [0, 0, 1]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sentence_of_a_later_paragraph_counts_its_occurrence_over_the_whole_document() {
        let fake = Arc::new(FakeCapture::new("An eror here.\r\n\r\nAn eror here."));
        let analyzer = analyzer_on(&fake);
        step(6).await;
        assert_eq!(eror_spans(&analyzer), [eror_here(0), eror_here(1)]);
        let ids: Vec<String> = published_flags(&analyzer)
            .into_iter()
            .map(|flag| flag.id)
            .collect();
        assert_eq!(ids, ["0:spelling:3", "1:spelling:3"]);
    }
}
