//! The analyzer pipeline that sits between capture and the overlay: debounce, incremental
//! diffing, merge and dedupe by span, ranking, and an LRU cache. Style flags are suppressed
//! on spans that already carry a hard grammar error.
//!
//! Not yet implemented.
