//! Tier 0 style engine: deterministic feature extraction, the target and observed profiles,
//! form-mode detection, AI-telltale matching, and the drift flags that name which Style Card
//! rule a sentence missed. Arithmetic and rules only, since the Tier 0 budget has no room for
//! a network round trip.
//!
//! Only [`ai_tell`] is implemented so far. Feature extraction, the target and observed profiles,
//! drift scoring, and form-mode detection all need a target profile that does not exist until the
//! phase that builds onboarding.

pub mod ai_tell;
