//! One contract for text delivery, cursor reporting, and replacement, with the native UI
//! Automation backend and the insertion cascade behind it. The rest of the engine never learns
//! which backend served a request.
//!
//! [`Capture`] has two implementations: [`native`], which also covers Microsoft Word's desktop
//! document surface (UI Automation's `TextPattern` covers it like any other rich-text control,
//! no Word-specific integration needed), and [`web`], which covers browser-based editors with
//! real DOM text content. A third backend built on Office.js was scoped in #22 to give Word its
//! own document-object-model path; it was dropped once manual verification confirmed native's
//! coverage against real Word, with the reasoning recorded on that issue.
//!
//! Canvas-rendered web editors, which draw their document onto a canvas instead of real DOM
//! text nodes for cross-browser layout fidelity, defeat `web`'s DOM-based read: it sees only the
//! decoy input such an editor positions at the caret to capture keystrokes and IME composition,
//! not the document. `native` is not automatically exempt, since it depends just as much on a
//! trustworthy accessibility tree, only reached a different way (UI Automation against the
//! browser tab's own bridged tree, rather than the page's DOM). Manual verification against
//! each editor individually, not an assumption from one covering the other, is what #31 settled.
//! Word for the web turned out to be readable through `native` regardless: Chromium bridges its
//! full accessibility tree to UI Automation for screen-reader support, and Word for the web's own
//! editing surface backs that tree completely, so `native`'s ordinary `TextPattern`/caret read
//! sees the real document there, with no Word-for-the-web-specific code needed, the same way
//! Word's desktop surface needed none. Google Docs did not: like `native/insert/mod.rs`'s
//! `replace_last_typed` doc comment already found from the write side, Google Docs exposes a
//! "side DOM" purely for accessibility that does not back a trustworthy read either, so `native`
//! against it returns empty text and no caret rect, not a decoy but not the document. Recorded
//! here as explicitly unsupported rather than solved: neither backend reads Google Docs' real
//! content.

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
/// a thread or network boundary: native forwards through its dedicated UIA thread, web
/// round-trips a message.
#[async_trait::async_trait]
pub trait Capture: Send + Sync {
    /// The full current text of whatever the backend considers "the document": an element's
    /// value for native ([`native::insert::current_text`]), or the captured element's text for
    /// web.
    async fn current_text(&self) -> Result<String, CaptureError>;

    /// The caret's on-screen rectangle, for overlay placement. Native answers this from UI
    /// Automation's TextPattern ([`native::cursor::caret_rect`]), including for Word's desktop
    /// document surface. Web returns [`CaptureError::Unsupported`]: a browser content script
    /// cannot reliably convert a DOM position to absolute screen coordinates (no way to learn
    /// the browser chrome's height from page JavaScript). That surface needs a presentation
    /// mechanism other than a desktop overlay window, a decision left to whoever designs that
    /// presentation, not this trait.
    async fn cursor_rect(&self) -> Result<CursorRect, CaptureError>;

    /// Replaces the `local_length`-UTF-16-code-unit span starting `local_start` code units into
    /// the first occurrence of `anchor`, with `replacement`.
    ///
    /// Anchored on found text rather than an absolute document offset for the reason
    /// [`native::insert::replace_within`] already documents: absolute character counting
    /// drifts near auto-numbered list items in UI Automation, including in Word's own document
    /// surface, while a local offset from a freshly-found anchor never crosses the boundary
    /// that causes the drift.
    ///
    /// UTF-16 code units because that is what UI Automation counts by and what JavaScript
    /// strings are natively: a genuine convergence across both backends, not a
    /// Windows-specific artifact.
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
