//! The shared unit every checking source produces and the analyzer, the Tauri command layer, and
//! the overlay all consume. Three unrelated modules produce a [`Flag`]: spelling, LanguageTool,
//! and the AI-telltale matcher in `style`. Owning the type inside any one of them would make the
//! others reach sideways across the crate for it, so it lives here instead.

/// Addresses a span of text the same way [`crate::capture::Capture::replace`] does: an anchor
/// found in the document plus a local offset and length from that anchor, in UTF-16 code units.
/// `capture::mod`'s documentation for `replace` records why an absolute document offset is not
/// used instead: UI Automation's character counting drifts near auto-numbered list items, while a
/// local offset from a freshly found anchor never crosses the boundary that causes the drift. A
/// `Span` is therefore directly usable both to replace its own text later and to resolve its
/// on-screen position, through the same anchor contract in both cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub anchor: String,
    pub local_start: usize,
    pub local_length: usize,
}

/// Which checking source produced a [`Flag`]. The overlay renders each origin with its own
/// visual treatment; the analyzer's dedup rule ranks grammar and spelling above AI-tell on an
/// overlapping span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagOrigin {
    Spelling,
    Grammar,
    AiTell,
}

/// One flagged span: what is wrong with it, what could replace it, and which source found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub id: String,
    pub origin: FlagOrigin,
    pub span: Span,
    pub message: String,
    pub suggestions: Vec<String>,
    pub source_detail: String,
}
