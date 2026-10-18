//! The rule layer. A checker, in [`checker`], is a Style Card clause's id and a rule: a matcher, a
//! unit and a band, declared for the form modes and the registers it runs in. [`evaluator`]
//! compiles and evaluates a rule. The AI-telltale catalogue has its own matcher in [`ai_tell`]
//! until it moves onto the rule layer (#96). Form-mode classification (#54) and the analyzer passes
//! that run checkers (#100) are not yet implemented. Everything here is deterministic: nothing in
//! this module calls a model or the network.

pub mod ai_tell;
pub mod checker;
pub mod evaluator;
mod phrase;
