//! Manual verification harness for issue #10: exercises focus subscription, text-change
//! notification, and cursor-rect retrieval against whatever application currently has focus.
//!
//! Run with `RUST_LOG=debug` to see every attempt, including ones that failed (a control
//! without TextPattern support, for instance) rather than only successes. Switch focus between
//! a plain edit control, a Chromium-hosted text field, and a rich-text control while this runs,
//! typing in each, and watch the log output.

use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::Duration;

use writing_assistant::capture::native::NativeCapture;

/// Generous enough to alt-tab through several target apps and type in each; also the entire
/// runtime when there's no attached terminal to press Enter on (see `IsTerminal` check below).
const MAX_RUNTIME: Duration = Duration::from_secs(600);

fn main() {
    env_logger::init();

    let capture = NativeCapture::start().expect("failed to start native UIA capture");
    println!("Native UIA capture running. Focus different apps and type to see events logged.");

    // A piped or redirected stdin (no attached terminal, exactly a headless or backgrounded
    // invocation) can deliver bytes immediately that have nothing to do with a person pressing
    // Enter; racing that against the timeout risks stopping right away regardless of
    // `MAX_RUNTIME`. Only race stdin at all when it's a real terminal.
    if std::io::stdin().is_terminal() {
        println!("Press Enter to stop, or wait {}s.", MAX_RUNTIME.as_secs());
        let (stdin_tx, stdin_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut discard = String::new();
            let _ = std::io::stdin().read_line(&mut discard);
            let _ = stdin_tx.send(());
        });
        let _ = stdin_rx.recv_timeout(MAX_RUNTIME);
    } else {
        println!(
            "No attached terminal; running for {}s.",
            MAX_RUNTIME.as_secs()
        );
        std::thread::sleep(MAX_RUNTIME);
    }

    drop(capture);
    println!("Stopped.");
}
