//! One contract for text delivery, span positioning, and replacement, with the native UI
//! Automation backend and the insertion cascade behind it.
//!
//! [`Capture`] has one implementation, [`native`], for desktop applications and browser tabs
//! alike. It covers Microsoft Word's desktop document surface (UI Automation's `TextPattern`
//! covers it like any other rich-text control, no Word-specific integration needed) and a Chromium
//! tab, through the accessibility tree Chromium bridges to UI Automation. A backend built on
//! Office.js was scoped in #22 to give Word its own document-object-model path; it was dropped
//! once manual verification confirmed native's coverage against real Word, with the reasoning
//! recorded on that issue. A web backend, a WebSocket server for a browser extension's content
//! script, was removed in #158: the extension's replace rewrote an element's inner text and
//! stripped its formatting, and the app never started it.
//!
//! Canvas-rendered web editors draw their document onto a canvas instead of real DOM text nodes,
//! for cross-browser layout fidelity, and position a decoy input at the caret to capture
//! keystrokes and IME composition. `native` reads such an editor only where its accessibility tree
//! backs the real document, so each editor is verified individually, as #31 settled. Word for the
//! web is readable through `native`: Chromium bridges its full accessibility tree to UI Automation
//! for screen-reader support, and Word for the web's own editing surface backs that tree
//! completely, so `native`'s ordinary `TextPattern` read sees the real document there, with no
//! Word-for-the-web-specific code needed, the same way Word's desktop surface needed none. Google
//! Docs needs a condition Word for the web does not: by
//! default it does not, matching `native/insert/mod.rs`'s `replace_last_typed` doc comment's
//! finding from the write side, that Google Docs exposes a "side DOM" purely for accessibility
//! that does not back a trustworthy read, so `native` against it returns empty text, not a decoy
//! but not the document either. Turning on both of Google Docs' own Tools > Accessibility
//! toggles, "Turn on screen reader support" and then "Enable braille support", changes that:
//! manual verification against a real document with both toggles on found `native`'s ordinary
//! `TextPattern` read landing on a real, growing character count, the same signature Word for
//! the web gives.
//! Screen reader support alone was not enough; only with braille support also on did a
//! `TextPattern`-backed element appear at all. Both toggles are Google's own account-level
//! accessibility settings, not something this project's code sets, so using Google Docs with
//! this tool means turning them on once, the same one-time step a screen reader user takes.

pub mod error;
pub mod native;
mod surface;

pub use error::CaptureError;
pub use surface::Surface;

use crate::flag::Span;

/// A screen-space rectangle in physical pixels: the document view's, or one line of a span's. The
/// capture module's one rectangle type: [`native::cursor`] re-exports it and keeps none of its
/// own. `Serialize` so the app's positioned flag, which carries one, can cross the Tauri IPC
/// boundary to the overlay's own webview.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Whose window is in the foreground, as [`Capture::foreground`] answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Foreground {
    /// A window of the process that owns the document the backend has in scope.
    Document,
    /// A window of this app, or of one of its WebView2 processes.
    ThisApp,
    /// Any other window, or none.
    Other,
}

/// One contract for text delivery, span positioning, and replacement, so the rest of the engine
/// never calls UI Automation itself and its tests can stand a fake in for the backend. Async
/// because the native backend crosses a thread boundary: it forwards each call to its dedicated
/// UIA thread and awaits the reply.
#[async_trait::async_trait]
pub trait Capture: Send + Sync {
    /// The full current text of whatever the backend considers "the document": for native, the
    /// focused element's `TextPattern` document range, read whole in parts, or its value where it
    /// has no `TextPattern` ([`native::insert::current_text`]).
    async fn current_text(&self) -> Result<String, CaptureError>;

    /// A notification that the document changed: a counter the backend raises on each change,
    /// read through the receiver's `changed` or `has_changed`. A watch channel keeps only the
    /// latest value, so a burst of changes between two reads of the receiver is one notification.
    /// Each call returns a receiver of its own with the current value marked seen, so the analyzer
    /// and the overlay each see every notification once.
    ///
    /// Native raises it on UI Automation's text-changed event and when a focus change scopes a new
    /// element, since a new document is a change. In a control that raises no text-changed event,
    /// the analyzer's safety poll is what finds a change.
    fn changes(&self) -> tokio::sync::watch::Receiver<u64>;

    /// Replaces `span`'s text with `replacement`: the `local_length` UTF-16 code units starting
    /// `local_start` code units into the occurrence of its `anchor` that its `occurrence` names,
    /// by the rule on [`Span::occurrence`]. Taking the whole span, rather than its parts, means no
    /// caller can address an anchor without an occurrence index.
    ///
    /// Anchored on found text rather than an absolute document offset for the reason
    /// [`native::insert::replace_within`] already documents: absolute character counting
    /// drifts near auto-numbered list items in UI Automation, including in Word's own document
    /// surface, while a local offset from a freshly-found anchor never crosses the boundary
    /// that causes the drift.
    ///
    /// UTF-16 code units because that is what UI Automation's text ranges count by.
    ///
    /// Native types the replacement as keystrokes, which reach whichever window holds the
    /// foreground, so it brings the target's window to the front before each stage that sends one
    /// ([`native::foreground::bring_to_front`]). When that does not take, it answers
    /// [`CaptureError::RefocusFailed`] with no keystroke sent from that stage on and the clipboard
    /// fallback not tried.
    ///
    /// The working card is the caller this is shaped for: it holds the span of the flag it acts
    /// on, so the anchor-based shape is this trait's only replacement method. The card's actions
    /// are not yet built, and nothing in the app calls this today. Native also has
    /// [`native::insert::replace_span`] and [`native::insert::replace_last_typed`], which search
    /// by content without a caller-supplied anchor. Both exist for the insertion harness, which
    /// has no flag to take an anchor from.
    ///
    /// Native resolves a span at any occurrence index, through the search that serves
    /// [`Self::span_rect`] ([`native::insert::select::find_within_range`]), so a replacement lands
    /// on the occurrence its underline marks.
    async fn replace(&self, span: &Span, replacement: &str) -> Result<(), CaptureError>;

    /// The on-screen rectangle of the focused editable element, or the foreground window when
    /// the element's own rectangle is unusable, for sizing and positioning the full-viewport
    /// overlay so every visible flagged span falls inside it. Native answers this from UI
    /// Automation's `BoundingRectangle` ([`native::cursor::document_view_rect`]), and answers
    /// [`CaptureError::Unsupported`] rather than fall back to a foreground window of this app's
    /// own, which would place the overlay over the app instead of the document.
    async fn document_view_rect(&self) -> Result<CursorRect, CaptureError>;

    /// Whose window is in the foreground: the scoped document's process's, this app's, or
    /// another's, so the overlay can hide while the document is behind another window. Native
    /// compares the foreground window's process with the scoped element's
    /// ([`native::foreground::classify`]); a document whose window belongs to a process other
    /// than its element's answers [`Foreground::Other`] while in front.
    async fn foreground(&self) -> Result<Foreground, CaptureError>;

    /// Every on-screen bounding rectangle `span` produces, for underlining a flagged span in
    /// place: the `local_length` UTF-16 code units starting `local_start` code units into the
    /// occurrence of its `anchor` that its `occurrence` names. More than one rectangle comes back
    /// when the span wraps a line break.
    ///
    /// Addressed the same way [`Self::replace`] is, for the same reason: a [`Span`] is directly
    /// usable both to replace its own text and to resolve its on-screen position through this one
    /// anchor contract. Native answers this from UI Automation's `TextPattern`
    /// ([`native::cursor::span_rects`]), through the same anchor and occurrence search,
    /// [`native::insert::select::find_within_range`], that serves `replace`, so an underline and
    /// a replacement land on the same occurrence. An anchor the document no longer holds at that
    /// index answers [`CaptureError::TextNotFound`].
    async fn span_rect(&self, span: &Span) -> Result<Vec<CursorRect>, CaptureError>;

    /// The surface the focused document belongs to: its process's executable name, the page URL
    /// where the document sits in a browser tab that surfaces one, and a durable key for the
    /// document, by the rules on [`Surface`]'s fields. The analyzer reads it at each recheck and
    /// checks under the register the settings map it to.
    ///
    /// Native answers it from the element in scope ([`native::surface::read`]), and answers
    /// [`CaptureError::NoFocus`] with none in scope, as [`Self::current_text`] does.
    async fn surface(&self) -> Result<Surface, CaptureError>;
}
