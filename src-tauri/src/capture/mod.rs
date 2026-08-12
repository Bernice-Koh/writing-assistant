//! One contract for text delivery, cursor reporting, and replacement, with the native UI
//! Automation backend and the insertion cascade behind it. The rest of the engine never
//! learns which backend served a request.
//!
//! The shared contract itself is not yet defined: `native` is the first of three backends
//! (native, web, word) to exist, and the contract is designed once there is more than one to
//! abstract over, not guessed from a single data point.

pub mod native;
