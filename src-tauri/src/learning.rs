//! Signal logging and the batch scheduler: rolling-average updates, corpus pruning, and
//! Style Card regeneration on idle mains power. The split that matters here is which profile
//! a signal moves. Measured behaviour updates the observed profile only, so habit never drags
//! the target back toward how the user already writes.
//!
//! Not yet implemented.
