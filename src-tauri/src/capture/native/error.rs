//! Typed failures for the native capture backend, raised at the deepest layer that can name
//! them, degrading rather than crashing the caller.

#[derive(Debug, thiserror::Error)]
pub enum NativeCaptureError {
    #[error("UI Automation call failed: {0}")]
    Com(#[from] windows::core::Error),
    #[error("element does not expose TextPattern")]
    NoTextPattern,
    #[error("TextPattern exposed no caret range")]
    NoCaret,
    #[error("text selection is a range, not a caret")]
    SelectionNotCaret,
    #[error("could not spawn the UI Automation thread: {0}")]
    ThreadSpawn(String),
    #[error("UI Automation thread ended before signalling readiness")]
    ThreadNotReady,
    #[error("element does not expose ValuePattern")]
    NoValuePattern,
    #[error("element is read-only")]
    ReadOnly,
    #[error("SendInput sent {sent} of {expected} inputs")]
    SendInputIncomplete { sent: u32, expected: usize },
    #[error("could not allocate clipboard memory")]
    ClipboardAlloc,
    #[error("insertion completed without a verifiable effect on the target")]
    InsertionUnverified,
    #[error("target text not found in the element")]
    TextNotFound,
}

/// Maps this backend's own, specific failure modes onto the `Capture` trait's shared
/// vocabulary at the trait boundary, per #19: a caller of the trait never matches on
/// `NativeCaptureError` directly. `NoCaret` becomes `Unsupported` rather than a dedicated
/// variant: from the trait's perspective, an element whose TextPattern reports no caret range
/// cannot answer a cursor-rect request right now, the same practical outcome as an element
/// with no TextPattern at all. `SelectionNotCaret` joins them for that same reason; the two
/// stay separate variants only so a log line can say which of the two happened.
impl From<NativeCaptureError> for crate::capture::CaptureError {
    fn from(error: NativeCaptureError) -> Self {
        use crate::capture::CaptureError;
        let message = error.to_string();
        match error {
            NativeCaptureError::TextNotFound => CaptureError::TextNotFound,
            NativeCaptureError::InsertionUnverified => CaptureError::Unverified,
            NativeCaptureError::NoTextPattern
            | NativeCaptureError::NoCaret
            | NativeCaptureError::SelectionNotCaret
            | NativeCaptureError::NoValuePattern
            | NativeCaptureError::ReadOnly => CaptureError::Unsupported,
            NativeCaptureError::Com(_)
            | NativeCaptureError::ThreadSpawn(_)
            | NativeCaptureError::ThreadNotReady
            | NativeCaptureError::SendInputIncomplete { .. }
            | NativeCaptureError::ClipboardAlloc => CaptureError::Communication(message),
        }
    }
}
