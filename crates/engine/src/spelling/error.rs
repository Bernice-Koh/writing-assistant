//! Errors from loading the dictionary pair and the Singapore supplement.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpellingError {
    #[error("could not read {path}: {source}", path = path.display())]
    ReadDictionary {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `spellbook::ParseDictionaryError` does not implement `std::error::Error` as of the
    /// version this module was written against (it implements `Display` only), so its message
    /// is captured as a string rather than kept as a `#[source]`.
    #[error("could not parse the en_GB dictionary: {0}")]
    ParseDictionary(String),

    /// `spellbook::ParseFlagError` has the same `Display`-only gap as
    /// [`SpellingError::ParseDictionary`] above.
    #[error("could not add supplement word {word:?}: {message}")]
    ParseSupplementWord { word: String, message: String },
}
