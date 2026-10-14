//! The rule layer. Today it holds the AI-telltale catalogue matcher in [`ai_tell`]. The Style
//! Card's checkers, each a matcher, a unit and a band, form-mode classification, and the style
//! flags that cite a Card clause are not yet implemented. Everything here is deterministic:
//! nothing in this module calls a model or the network.

pub mod ai_tell;
