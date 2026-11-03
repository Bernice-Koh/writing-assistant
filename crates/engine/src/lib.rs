//! The checking engine: capture of the focused document, spelling, LanguageTool's grammar, the
//! rule layer with the telltale catalogue on it, the analyzer that runs them over each pause, the
//! flag types they produce, the settings file, and the store. It has two callers, the Tauri app
//! and the `wa` command-line binary, and depends on neither, so what `wa` measures is what the app
//! ships.
//!
//! The rule layer is in [`style`]. A checker there is a Style Card clause made mechanical: the
//! clause's id and a rule, which is a matcher, a unit and a band, declared for the form modes and
//! the registers it runs in. Each pattern of the telltale catalogue is a rule too, under its
//! pattern id. A rule evaluates as a pure function of one unit's sentences, each with its
//! paragraph's form mode. The analyzer's deterministic stage runs spelling, the telltale catalogue
//! and the ruleset's checkers in a sentence pass, a paragraph pass and a document pass, and its
//! grammar stage evaluates the checkers whose matcher is a LanguageTool rule id.
//!
//! The screen, in [`screen`], holds its content check: a pure function of a passage and a rewrite
//! candidate that tells whether the candidate keeps the passage's facts.

pub mod analyzer;
pub mod capture;
pub mod flag;
mod hash;
pub mod languagetool;
pub mod learning;
pub mod rewrite;
pub mod screen;
pub mod spelling;
pub mod store;
pub mod style;
mod words;

#[cfg(test)]
mod test_support;
