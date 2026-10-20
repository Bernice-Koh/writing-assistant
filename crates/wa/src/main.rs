//! `wa`, the engine on the command line, for the app's scheduler, the Python project, and the
//! owner by hand. Its one subcommand so far is `backup`; each other arrives with its own issue.
//!
//! Every subcommand opens the store through [`Store::open_existing`], so `wa` never creates it.
//! A failure prints one line naming the path at fault and exits non-zero. Nothing `wa` prints or
//! logs holds a row's text.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use wa_engine::store::{self, Store, StoreError};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The data root that holds the store. Defaults to the engine's data root,
    /// %LOCALAPPDATA%\com.bernicekoh.writing-assistant; another directory serves the tests and a
    /// copied root.
    #[arg(long, global = true, value_name = "DIR")]
    data_root: Option<PathBuf>,

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
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wa: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), StoreError> {
    let data_root = match cli.data_root {
        Some(data_root) => data_root,
        None => store::data_root()?,
    };
    match cli.command {
        Command::Backup { destination } => backup(&data_root, &destination),
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
