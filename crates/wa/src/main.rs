//! `wa`, the engine on the command line, for the app's scheduler, the Python project, and use by
//! hand. Its subcommands so far are `backup`, which copies the store file alone, and `screen`, which
//! runs the screen over JSONL; each other arrives with its own issue.
//!
//! Every subcommand opens the store through [`Store::open_existing`], so `wa` never creates it.
//! A failure prints one line naming what is at fault and exits non-zero. Nothing `wa` logs or
//! prints to standard error holds a row's text or any text of a passage or a candidate.

mod resources;
mod screen;
#[cfg(test)]
mod test_support;

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use thiserror::Error;
use wa_engine::flag::RulesetVersion;
use wa_engine::languagetool::{self, LanguageToolError, LanguageToolSupervisor};
use wa_engine::screen::Screen;
use wa_engine::spelling::{self, SpellChecker, SpellingError};
use wa_engine::store::{self, Store, StoreError};

use screen::StreamError;

/// The exit code of a screen that finished and wrote at least one error line. A run that stopped
/// without finishing, or never started, exits 1.
const SOME_LINES_ERRORED: u8 = 2;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The data root that holds the store and the settings file. Defaults to the engine's data
    /// root, %LOCALAPPDATA%\com.bernicekoh.writing-assistant; another directory serves the tests
    /// and a copied root.
    #[arg(long, global = true, value_name = "DIR")]
    data_root: Option<PathBuf>,

    /// The resource directory holding the dictionaries, LanguageTool and its Java runtime.
    /// Defaults, for a build under a checkout's target folder, to the checkout's
    /// src-tauri\resources, and otherwise to the directory holding wa, where the app's bundle
    /// places them.
    #[arg(long, global = true, value_name = "DIR")]
    resources: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Copies the store file alone to a destination that must not exist
    ///
    /// Writes `store.sqlite` as one consistent snapshot, taken while the app runs and writes. The
    /// settings file, model files and adapters are not part of it. The store is the one under
    /// --data-root, which defaults to the engine's data root.
    Backup {
        /// The file to write the copy to. Refused when it exists.
        destination: PathBuf,
    },

    /// Screens rewrite candidates read as JSONL on standard input, one output line for each
    ///
    /// Each input line is a JSON object with "id", any JSON value, and "passage", "candidate" and
    /// "register", each a string. Each gets one line of standard output, in input order, written
    /// as it completes. A screened line is its id beside the screening's fields: hardResults,
    /// softResults, verdict, killingRule, rulesetVersion, catalogueHash, enabledPatternIds and
    /// appVersion. A line that cannot be screened is {"id": ..., "error": {"class": ...,
    /// "message": ...}}, its id null where none could be read, with the class invalidInput for a
    /// line that is not JSON, not an object, or lacks a field or holds one of the wrong type, and
    /// grammarCheckDidNotRun when a grammar check failed. The output is UTF-8 with LF endings.
    ///
    /// The dictionaries, LanguageTool and its Java runtime come from --resources. The ruleset and
    /// the telltale toggles come from the store and the settings file under --data-root, with the
    /// default toggles where there is no settings file. wa screen starts its own LanguageTool on
    /// the first free port from 8081, so it runs beside the app, and stops it before it exits.
    ///
    /// Exit codes: 0 when every line was screened. 2 when the run finished and wrote at least one
    /// error line. 1 when it stopped without finishing: before reading any input, for a missing
    /// resource file, an absent store, a ruleset version the store lacks, a settings file that
    /// does not parse, or LanguageTool failing to start, each named on standard error; or partway,
    /// when standard input or standard output failed. A usage error exits 1 too.
    Screen {
        /// The stored ruleset version to screen under. Defaults to the latest, and to none when the
        /// store holds no version.
        #[arg(long, value_name = "N")]
        ruleset_version: Option<i64>,
    },
}

/// What stops `wa`, printed as its one line on standard error.
#[derive(Debug, Error)]
enum WaError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(
        "the resource file {path} does not exist; --resources names the directory to read the \
         resources from",
        path = path.display()
    )]
    MissingResource { path: PathBuf },

    #[error("could not locate the running executable to find the resource directory: {0}")]
    NoExecutable(#[source] io::Error),

    #[error(transparent)]
    Spelling(#[from] SpellingError),

    #[error("LanguageTool did not start: {0}")]
    LanguageTool(#[from] LanguageToolError),

    #[error("could not start the async runtime: {0}")]
    Runtime(#[source] io::Error),

    #[error(transparent)]
    Stream(#[from] StreamError),
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // Printed as clap prints it, but a usage error exits 1, since a screen's exit code 2 means
        // that some lines errored.
        Err(error) => {
            // A usage message that cannot be written to the terminal has nowhere else to go.
            let _ = error.print();
            return if error.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("wa: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, WaError> {
    let data_root = match cli.data_root {
        Some(data_root) => data_root,
        None => store::data_root()?,
    };
    match cli.command {
        Command::Backup { destination } => {
            backup(&data_root, &destination)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Screen { ruleset_version } => run_screen(
            &data_root,
            cli.resources,
            ruleset_version.map(RulesetVersion),
        ),
    }
}

/// Backs the store up to `destination`, made absolute first so that the printed path and any error
/// name the file wherever `wa` ran from. A path that cannot be made absolute, the empty one, goes
/// to the backup as given, which refuses it by name.
fn backup(data_root: &Path, destination: &Path) -> Result<(), StoreError> {
    let destination =
        std::path::absolute(destination).unwrap_or_else(|_| destination.to_path_buf());
    Store::open_existing(data_root)?.backup_blocking(&destination)?;
    println!("{}", destination.display());
    Ok(())
}

/// Runs `wa screen`. The resource files, the store, the ruleset, the settings and the dictionaries
/// are each checked or loaded before LanguageTool starts, and LanguageTool before any input is
/// read, so every startup failure leaves the input unread. Once LanguageTool runs, it is shut down
/// before this returns, whatever the run's outcome.
fn run_screen(
    data_root: &Path,
    resources: Option<PathBuf>,
    ruleset_version: Option<RulesetVersion>,
) -> Result<ExitCode, WaError> {
    let resources = match resources {
        Some(resources) => resources,
        None => resources::default_dir(&std::env::current_exe().map_err(WaError::NoExecutable)?),
    };
    log::info!("reading the resources under {}", resources.display());
    if let Some(path) = resources::first_missing(&resources) {
        return Err(WaError::MissingResource { path });
    }
    // The store closes once the ruleset is read, so the run holds no connection to it.
    let ruleset = screen::ruleset(&Store::open_existing(data_root)?, ruleset_version)?;
    let catalogue = screen::catalogue(data_root)?;
    let dictionaries = spelling::default_paths(&resources);
    let spelling = SpellChecker::load(
        &dictionaries.aff,
        &dictionaries.dic,
        &dictionaries.supplement,
    )?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(WaError::Runtime)?;
    let errored = runtime.block_on(async {
        let supervisor = LanguageToolSupervisor::start(
            languagetool::default_paths(&resources),
            languagetool::PREFERRED_PORT,
        )
        .await?;
        log::info!("LanguageTool is ready on port {}", supervisor.port());
        let screen = Screen::new(
            Arc::new(spelling),
            Arc::new(supervisor.clone()),
            Arc::new(ruleset),
            Arc::new(catalogue),
        );
        let screened = screen::screen_lines(&screen, io::stdin().lock(), io::stdout().lock()).await;
        supervisor.shutdown().await;
        Ok::<_, WaError>(screened?)
    })?;
    Ok(if errored == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(SOME_LINES_ERRORED)
    })
}
