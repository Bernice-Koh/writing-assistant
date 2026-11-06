//! Typed failures for the native capture backend, raised at the deepest layer that can name
//! them, degrading rather than crashing the caller.

#[derive(Debug, thiserror::Error)]
pub enum NativeCaptureError {
    #[error("UI Automation call failed: {0}")]
    Com(#[from] windows::core::Error),
    #[error("element does not expose TextPattern")]
    NoTextPattern,
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
    #[error("no foreground window to fall back to")]
    NoForegroundWindow,
    #[error("the foreground window is this app's own, so it is no fallback for the document view")]
    ForegroundIsThisApp,
    #[error("the target element has no window to bring to the foreground")]
    NoTargetWindow,
    #[error("could not bring {target}'s window to the foreground; {foreground} holds it")]
    RefocusFailed { target: String, foreground: String },
}

/// Maps this backend's own, specific failure modes onto the `Capture` trait's shared
/// vocabulary at the trait boundary, per #19: a caller of the trait never matches on
/// `NativeCaptureError` directly. An element with no `TextPattern` or `ValuePattern`, or a
/// read-only one, becomes `Unsupported`: from the trait's perspective it cannot answer the
/// request right now. `NoForegroundWindow` and `ForegroundIsThisApp` join the same group for
/// `document_view_rect`'s own fallback: no window to report a rectangle for, or only one of this
/// app's own, is the same practical "cannot answer right now" outcome, and the overlay's handling
/// of an `Err` from its document-view poll, holding the overlay where it last placed it, is what
/// both need. An element with no window joins it too: an insertion into it cannot be checked
/// against the foreground window, so it is not attempted. A refocus that did not take keeps its
/// own variant, so a caller can tell a target it could not reach from a cascade that ran and
/// failed.
impl From<NativeCaptureError> for crate::capture::CaptureError {
    fn from(error: NativeCaptureError) -> Self {
        use crate::capture::CaptureError;
        let message = error.to_string();
        match error {
            NativeCaptureError::TextNotFound => CaptureError::TextNotFound,
            NativeCaptureError::InsertionUnverified => CaptureError::Unverified,
            NativeCaptureError::RefocusFailed { target, foreground } => {
                CaptureError::RefocusFailed { target, foreground }
            }
            NativeCaptureError::NoTextPattern
            | NativeCaptureError::NoValuePattern
            | NativeCaptureError::ReadOnly
            | NativeCaptureError::NoForegroundWindow
            | NativeCaptureError::ForegroundIsThisApp
            | NativeCaptureError::NoTargetWindow => CaptureError::Unsupported,
            NativeCaptureError::Com(_)
            | NativeCaptureError::ThreadSpawn(_)
            | NativeCaptureError::ThreadNotReady
            | NativeCaptureError::SendInputIncomplete { .. }
            | NativeCaptureError::ClipboardAlloc => CaptureError::Communication(message),
        }
    }
}
