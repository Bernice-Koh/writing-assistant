//! Harness for the web capture bridge: starts the bridge server and logs connection activity
//! while the browser extension connects to it. It exercises the WebSocket handshake, the origin
//! check, and the heartbeat. The bridge's protocol is a request from the core and a reply from
//! the extension, and this harness never issues a request, so it reads no text and replaces
//! none.
//!
//! The one argument is the extension's origin, built from the ID that `chrome://extensions`
//! shows once `extension/` is loaded unpacked in Chrome, for example `cargo run --example
//! browser_bridge_spike -- chrome-extension://abcdefghijklmnopqrstuvwxyzabcdef`.
//!
//! The log is off unless `RUST_LOG` sets a level. At `debug` a pass shows an accepted connection
//! from the extension, a rejected handshake for a connection from any other origin, and a
//! received-bytes line with a heartbeat line each time the extension's keep-alive alarm fires.

use std::io::IsTerminal;
use std::time::Duration;

use writing_assistant::capture::web::WebCapture;

/// Generous enough to load the extension and watch several heartbeats arrive.
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
