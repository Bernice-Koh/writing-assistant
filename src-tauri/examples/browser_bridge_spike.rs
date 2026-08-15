//! Manual verification harness for issue #12: starts the web capture bridge server and logs
//! connection and message activity so the browser extension side can be exercised against it.
//!
//! Before running: load `extension/` unpacked in Chrome, copy its ID from `chrome://extensions`,
//! and pass it as the one argument, for example `cargo run --example browser_bridge_spike --
//! chrome-extension://abcdefghijklmnopqrstuvwxyzabcdef`. Run with `RUST_LOG=debug` to see
//! rejected handshakes and message sizes, never message content.
//!
//! With the extension loaded and this running, type into the GitHub comment box and Gmail
//! compose targets and confirm the round trip: the content script should read the typed text,
//! this server should log an accepted connection and a received-bytes line per message, and the
//! page should show the `[core] ...`-prefixed echo written back by the content script.

use std::io::IsTerminal;
use std::time::Duration;

use writing_assistant::capture::web::WebCapture;

/// Generous enough to load the extension, switch to each target site, and type a few messages.
const MAX_RUNTIME: Duration = Duration::from_secs(600);

#[tokio::main]
async fn main() {
    env_logger::init();

    let allowed_origin = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!(
            "no extension origin given; pass chrome-extension://<id> from chrome://extensions"
        );
        std::process::exit(1);
    });

    let capture = WebCapture::start(writing_assistant::capture::web::PORT, &allowed_origin)
        .await
        .expect("failed to start the web capture bridge server");
    println!(
        "Web capture bridge listening on 127.0.0.1:{}, accepting only {allowed_origin}.",
        writing_assistant::capture::web::PORT
    );

    if std::io::stdin().is_terminal() {
        println!("Press Enter to stop, or wait {}s.", MAX_RUNTIME.as_secs());
        let mut discard = String::new();
        let stdin = tokio::task::spawn_blocking(move || {
            let _ = std::io::stdin().read_line(&mut discard);
        });
        let _ = tokio::time::timeout(MAX_RUNTIME, stdin).await;
    } else {
        println!(
            "No attached terminal; running for {}s.",
            MAX_RUNTIME.as_secs()
        );
        tokio::time::sleep(MAX_RUNTIME).await;
    }

    capture.stop().await;
    println!("Stopped.");
}
