//! The rule layer. A checker, in [`checker`], is a Style Card clause's id and a rule: a matcher, a
//! unit and a band, declared for the form modes and the registers it runs in. [`evaluator`]
//! compiles and evaluates a rule. [`ruleset`] loads the latest published ruleset version, compiles
//! each of its checkers once, and builds the style flag that cites a checker's clause. The
//! AI-telltale catalogue, in [`ai_tell`], is a rule per pattern under the pattern's id, with no
//! clause, and builds the telltale flag. The analyzer classifies each paragraph's form mode, in
//! [`crate::analyzer::form_mode`], and runs the ruleset's checkers and the catalogue's patterns in
//! a sentence pass, a paragraph pass and a document pass. Everything here is deterministic:
//! nothing in this module calls a model or the network.

pub mod ai_tell;
mod band;
mod capitalised;
pub mod checker;
pub mod evaluator;
mod phrase;
pub mod ruleset;
mod variants;
