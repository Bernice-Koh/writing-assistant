//! One contract for text delivery, cursor reporting, and replacement, with the native UI
//! Automation backend and the insertion cascade behind it. The rest of the engine never learns
//! which backend served a request.
//!
//! [`Capture`] is that contract, designed now that there is more than one backend ([`native`],
//! [`web`], word) to abstract over rather than guessed from a single data point. `word` has no
//! Rust module yet: #22 builds its Rust-side bridge from scratch and implements this trait
//! directly, unlike native and web, whose existing spike code this trait's doc comments map
//! onto.

pub mod error;
pub mod native;
pub mod web;

pub use error::CaptureError;

/// Screen-space rectangle for placing UI relative to the cursor. Shape matches
/// [`native::cursor::CursorRect`] deliberately; #20 reconciles the two into one type when the
/// native backend conforms to this trait.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One contract for text delivery, cursor reporting, and replacement, so the rest of the engine
/// never learns which backend served a request. Async because every real implementation crosses
/// a thread or network boundary: native forwards through its dedicated UIA thread, web and word
/// both round-trip a message.
#[async_trait::async_trait]
pub trait Capture: Send + Sync {
    /// The full current text of whatever the backend considers "the document": an element's
    /// value for native ([`native::insert::current_text`]), the captured element's text for
    /// web, and the document body's text for word.
    ///
    /// Web's current spike ([`web::message::ClientMessage::Capture`]) is push-only: the
    /// extension sends text on its own trigger, the core does not ask for it. Conforming to
    /// this method needs a new pull-capable request added to the bridge protocol, not just a
    /// signature wrapped around the existing message. Word has no protocol yet at all; #22
    /// designs one pull-capable from the start.
    async fn current_text(&self) -> Result<String, CaptureError>;

    /// The caret's on-screen rectangle, for overlay placement. Native answers this from UI
    /// Automation's TextPattern ([`native::cursor::caret_rect`]). Web and word are expected to
    /// return [`CaptureError::Unsupported`]: a browser content script cannot reliably convert a
    /// DOM position to absolute screen coordinates (no way to learn the browser chrome's
    /// height from page JavaScript), and Office.js exposes no on-screen pixel position at all.
    /// Those surfaces need a presentation mechanism other than a desktop overlay window, a
    /// decision left to whoever designs that presentation, not this trait.
    async fn cursor_rect(&self) -> Result<CursorRect, CaptureError>;

    /// Replaces the `local_length`-UTF-16-code-unit span starting `local_start` code units into
    /// the first occurrence of `anchor`, with `replacement`.
    ///
    /// Anchored on found text rather than an absolute document offset for the reason
    /// [`native::insert::replace_within`] already documents: absolute character counting
    /// drifts near auto-numbered list items in UI Automation, and plausibly in Word's own
    /// object model too, while a local offset from a freshly-found anchor never crosses the
    /// boundary that causes the drift.
    ///
    /// UTF-16 code units because that is what UI Automation counts by and what JavaScript
    /// strings are natively, in both the browser extension and Office.js: a genuine
    /// convergence across all three backends, not a Windows-specific artifact.
    ///
    /// Native's spike also has `replace_at` (absolute offset), `replace_span` and
    /// `replace_last_typed` (content search without a caller-supplied anchor), and bare
    /// `insert`. Those exist for the spike's own manual test harness, which has no diagnostic
    /// supplying an anchor; a real caller (the analyzer or rewrite orchestrator) already knows
    /// the span from whatever flagged it, so only the anchor-based shape becomes this trait's
    /// method. The other functions stay as native-internal helpers `replace`'s implementation
    /// can still use.
    async fn replace(
        &self,
        anchor: &str,
        local_start: usize,
        local_length: usize,
        replacement: &str,
    ) -> Result<(), CaptureError>;
}
