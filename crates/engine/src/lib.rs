//! The checking engine: capture of the focused document, spelling, LanguageTool's grammar, the
//! telltale matcher, the analyzer that runs them over each pause, the flag types they produce,
//! the settings file, and the store. It has two callers, the Tauri app and the `wa` command-line
//! binary, and depends on neither, so what `wa` measures is what the app ships.

pub mod analyzer;
pub mod capture;
pub mod flag;
pub mod languagetool;
pub mod learning;
pub mod rewrite;
pub mod spelling;
pub mod store;
pub mod style;

#[cfg(test)]
mod test_support;
