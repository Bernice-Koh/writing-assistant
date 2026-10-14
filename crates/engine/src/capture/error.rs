//! The error type every [`super::Capture`] trait method returns, common across backends so a
//! caller never needs to match on a specific backend's own error type. Each backend's own,
//! richer error ([`super::native::error::NativeCaptureError`],
//! [`super::web::error::WebCaptureError`]) maps into one of these variants at the point where
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
    #[error("communication with the backend failed: {0}")]
    Communication(String),
}
