//! Manual verification harness for issue #10: exercises focus subscription and text-change
//! notification against whatever application currently has focus. Also exercises the `Capture`
//! trait added in #20: `current_text` is queried once at the start and once before stopping,
//! against whatever has focus at each point.
//! Each `current_text` query also prints how many of each paragraph mark the text holds, and one
//! line per paragraph, split and classified by the analyzer's own form-mode module: its index, its
//! length in characters and its form mode, and, for a list item or a paragraph that begins with a
//! control character, the classes of its first two code points, which show how the application
//! delivers a list marker.
//!
//! Run from the repository root with `cargo run -p wa-engine --example native_capture_spike`.
//! `RUST_LOG=debug` shows every focus change with the class, control type and process of the
//! element it lands on, and every text-changed event. Switch focus between
//! a plain edit control, a Chromium-hosted text field, and a rich-text control while this runs,
//! typing in each, and watch the log output.
//!
//! With `-- --surface <reads>`, it measures the surface read of #155 instead, against whatever has
//! focus: that many reads a second apart, each through `Capture::surface` and timed, printing the
//! process name, whether a page URL was surfaced, the URL's scheme and host alone, and whether the
//! document key is present and its length. Each read then walks the focused element's Document
//! ancestors on a UI Automation client of its own and prints, nearest first, each one's class,
//! framework id and name length, and for each candidate property, `ValuePattern`'s value and
//! `LegacyIAccessiblePattern`'s, the scheme and host of the page URL it carries or why it carries
//! none, with the property the backend's read took the URL through. No URL past its host, and no
//! text of the document, is printed.

use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wa_engine::analyzer::form_mode;
use wa_engine::capture::native::client::Uia;
use wa_engine::capture::native::surface::{self, UrlProperty};
use wa_engine::capture::native::NativeCapture;
use wa_engine::capture::Capture;

/// Generous enough to alt-tab through several target apps and type in each; also the entire
/// runtime when there's no attached terminal to press Enter on (see `IsTerminal` check below).
const MAX_RUNTIME: Duration = Duration::from_secs(600);

/// How long the surface mode waits after starting for the backend to scope the element that
/// already has focus, and between two reads.
const SURFACE_READ_INTERVAL: Duration = Duration::from_secs(1);

fn main() {
    env_logger::init();

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, reads] = arguments.as_slice() {
        if flag == "--surface" {
            let reads = reads.parse().expect("--surface takes a number of reads");
            read_surfaces(reads);
            return;
        }
    }

    let capture = NativeCapture::start().expect("failed to start native UIA capture");
    println!("Native UIA capture running. Focus different apps and type to see events logged.");

    // Scoped to just the Capture trait calls: the rest of this harness is deliberately plain
    // std, not async, and does not need a runtime of its own.
    let runtime = tokio::runtime::Runtime::new().expect("failed to start a Tokio runtime");
    runtime.block_on(query_capture_trait(&capture, "at start"));

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

    runtime.block_on(query_capture_trait(&capture, "before stopping"));
    drop(capture);
    println!("Stopped.");
}

/// Reads the focused element's surface `reads` times, a second apart, as the module comment
/// describes.
fn read_surfaces(reads: usize) {
    let capture = NativeCapture::start().expect("failed to start native UIA capture");
    let uia = Uia::new().expect("failed to start a UI Automation client");
    let cache = uia
        .base_cache_request()
        .expect("failed to create a cache request");
    let runtime = tokio::runtime::Runtime::new().expect("failed to start a Tokio runtime");
    std::thread::sleep(SURFACE_READ_INTERVAL);

    for read in 1..=reads {
        let started = Instant::now();
        let answered = runtime.block_on(capture.surface());
        let took = started.elapsed();
        let process_name = match &answered {
            Ok(surface) => {
                println!(
                    "read {read}: Capture::surface in {:.2} ms: process {:?}, page URL {}, \
                     document key {}",
                    took.as_secs_f64() * 1000.0,
                    surface.process_name,
                    surface
                        .page_url
                        .as_deref()
                        .map_or_else(|| "not surfaced".to_owned(), scheme_and_host),
                    surface.document_key.as_ref().map_or_else(
                        || "absent".to_owned(),
                        |key| format!("present, {} characters", key.chars().count())
                    ),
                );
                surface.process_name.clone()
            }
            Err(error) => {
                println!("read {read}: Capture::surface failed: {error}");
                String::new()
            }
        };
        print_document_ancestors(&uia, &cache, process_name);
        std::thread::sleep(SURFACE_READ_INTERVAL);
    }
    drop(capture);
}

/// The focused element's Document ancestors and their candidate properties, nearest first, by a
/// walk on `uia`, a client of the harness's own, and the property the backend's read takes the page
/// URL through.
fn print_document_ancestors(
    uia: &Uia,
    cache: &windows::Win32::UI::Accessibility::IUIAutomationCacheRequest,
    process_name: String,
) {
    let element = match uia.focused_element(cache) {
        Ok(element) => element,
        Err(error) => {
            println!("  no focused element: {error}");
            return;
        }
    };
    match surface::document_elements(uia.client(), &element) {
        Ok(documents) if documents.is_empty() => {
            println!("  no element of the Document control type among the element's ancestors");
        }
        Ok(documents) => {
            for (index, document) in documents.iter().enumerate() {
                let properties: Vec<String> = UrlProperty::ALL
                    .into_iter()
                    .map(|property| {
                        let carried = match surface::page_url_through(&document.element, property) {
                            Ok(Some(url)) => scheme_and_host(&url),
                            Ok(None) => "not a page URL".to_owned(),
                            Err(error) => format!("unreadable ({error})"),
                        };
                        format!("{property:?} {carried}")
                    })
                    .collect();
                println!(
                    "  document {index}: class {:?}, framework {:?}, name {} characters; {}",
                    document.class_name,
                    document.framework_id,
                    document.name.chars().count(),
                    properties.join("; ")
                );
            }
        }
        Err(error) => println!("  the walk of the element's ancestors failed: {error}"),
    }
    let read = surface::read(uia.client(), &element, process_name);
    println!(
        "  the backend's read takes the page URL through {}",
        read.url_property.map_or_else(
            || "no property".to_owned(),
            |property| format!("{property:?}")
        )
    );
}

/// `url`'s scheme and host alone.
fn scheme_and_host(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => format!(
            "surfaced, scheme {:?}, host {:?}",
            parsed.scheme(),
            parsed.host_str().unwrap_or("")
        ),
        Err(_) => "surfaced, and not parsable".to_owned(),
    }
}

/// Never prints the text itself, only its length: this channel carries the user's own draft.
async fn query_capture_trait(capture: &NativeCapture, when: &str) {
    match capture.current_text().await {
        Ok(text) => {
            println!("current_text() {when}: {} characters", text.chars().count());
            print_marks(&text);
            print_paragraphs(&text);
        }
        Err(error) => println!("current_text() {when}: {error}"),
    }
}

/// How many of each paragraph mark `text` holds, which shows the mark the application delivers.
fn print_marks(text: &str) {
    let pairs = text.matches("\r\n").count();
    let carriage_returns = text.matches('\r').count() - pairs;
    let newlines = text.matches('\n').count() - pairs;
    println!(
        "  marks: {carriage_returns} carriage returns alone, {newlines} newlines alone, \
         {pairs} carriage returns followed by a newline"
    );
}

/// One line per paragraph of `text`, with no text in it: a list marker's code points are the
/// most it shows.
fn print_paragraphs(text: &str) {
    for (index, paragraph) in form_mode::split(text).iter().enumerate() {
        let mut line = format!(
            "  paragraph {index}: {} characters, {:?}",
            paragraph.text.chars().count(),
            paragraph.mode
        );
        if form_mode::is_list_item(paragraph.text) || paragraph.text.starts_with(char::is_control) {
            let classes: Vec<String> = paragraph.text.chars().take(2).map(class_of).collect();
            line.push_str(", starts with ");
            line.push_str(&classes.join(" then "));
        }
        println!("{line}");
    }
}

/// The class of `c`, with its value in hex unless it is a letter or a digit, so a marker's code
/// point shows and a word's letters do not.
fn class_of(c: char) -> String {
    let class = match c {
        '\u{2022}' => "bullet",
        '-' => "hyphen",
        '*' => "asterisk",
        '+' => "plus sign",
        '.' => "full stop",
        ')' => "closing parenthesis",
        '\t' => "tab",
        ' ' => "space",
        _ if c.is_ascii_digit() => return "digit".to_owned(),
        _ if c.is_alphabetic() => return "letter".to_owned(),
        _ if c.is_control() => "control character",
        _ if c.is_whitespace() => "whitespace",
        _ => "other",
    };
    format!("{class} U+{:04X}", u32::from(c))
}
