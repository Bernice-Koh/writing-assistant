//! The error type every [`super::Capture`] trait method returns, so a caller never needs to match
//! on the backend's own error type. The native backend's richer error,
//! [`super::native::error::NativeCaptureError`], maps into one of these variants at the point where
//! it implements the trait.

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no element is currently focused")]
    NoFocus,
    #[error("the target does not support this operation")]
    Unsupported,
    #[error("the target text to replace was not found")]
    TextNotFound,
    #[error("the backend could not verify its effect took place")]
    Unverified,
    /// Another window held the foreground before a keystroke of a replacement, and the target's
    /// window could not be brought back to the front, so the replacement stopped before that
    /// keystroke. `target` and `foreground` name the two windows' processes.
    #[error("could not bring {target}'s window to the foreground; {foreground} holds it")]
    RefocusFailed { target: String, foreground: String },
    #[error("communication with the backend failed: {0}")]
    Communication(String),
}
