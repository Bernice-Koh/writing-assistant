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
