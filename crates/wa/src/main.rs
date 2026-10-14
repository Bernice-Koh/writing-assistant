//! `wa`, the engine on the command line, for the app's scheduler, the Python project, and the
//! owner by hand. It has no subcommands yet; each arrives with its own issue.

use clap::Parser;

// Linked before the first subcommand calls into it, so a change that breaks the engine's build
// for `wa` fails now rather than with that subcommand.
use wa_engine as _;

#[derive(Parser)]
#[command(version, about)]
struct Cli {}

fn main() {
    Cli::parse();
}
