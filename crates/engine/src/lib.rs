//! The checking engine: capture of the focused document, spelling, LanguageTool's grammar, the
//! telltale matcher, the analyzer that runs them over each pause, the flag types they produce,
//! the settings file, and the store. It has two callers, the Tauri app and the `wa` command-line
//! binary, and depends on neither, so what `wa` measures is what the app ships.
//!
//! It holds the rule layer too, in [`style`]. A checker there is a Style Card clause made
//! mechanical: the clause's id and a rule, which is a matcher, a unit and a band, declared for the
//! form modes and the registers it runs in. A rule evaluates as a pure function of one unit's
//! sentences, and nothing runs a checker until the analyzer's passes for checkers arrive (#100).

pub mod analyzer;
pub mod capture;
pub mod flag;
mod hash;
pub mod languagetool;
pub mod learning;
pub mod rewrite;
pub mod spelling;
pub mod store;
pub mod style;
mod words;

#[cfg(test)]
mod test_support;
