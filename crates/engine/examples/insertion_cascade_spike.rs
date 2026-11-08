//! Manual verification harness for issue #11: exercises value-set, synthetic input, and
//! clipboard paste independently, each against whatever currently has focus.
//!
//! Paced with a fixed delay rather than stdin, per the lesson from issue #10's harness: a
//! backgrounded run's stdin is not reliably distinguishable from a real Enter press. Focus the
//! named app during the countdown; nothing needs to be typed manually.
//!
//! Takes exactly one argument: which app slot to test (its label and marker suffix), so each
//! app can be run, and its focus explicitly confirmed, as a separate invocation rather than
//! trusting a multi-app run's later phases to still be pointed at the right window. Run from the
//! repository root with `cargo run -p wa-engine --example insertion_cascade_spike -- <slot>`,
//! where the slot is one of the names in the usage line `main` prints.
//!
//! The `refocus` slot (#157) measures the cascade's refocus: it places another window in front of
//! the target between the request and the first keystroke, and takes a run count, a title the
//! target window's must hold, and a cover script after the slot (see `run_refocus_test`).
//!
//! The `correct` slot (#158) replaces the first misspelt word of a field focused by hand, such as
//! one in a browser tab, by the spell checker's first suggestion, through the span the analyzer
//! would publish for it, and takes a title the target window's must hold after the slot (see
//! `run_correct_test`).

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use unicode_segmentation::UnicodeSegmentation;
use wa_engine::capture::native::client::Uia;
use wa_engine::capture::native::error::NativeCaptureError;
use wa_engine::capture::native::foreground::{self, Window};
use wa_engine::capture::native::insert::{
    changed_region, clipboard, current_selection, current_text, replace_last_typed, replace_span,
    replace_within, restore_after_failed_attempt, select, synthetic, value,
};
use wa_engine::flag::Span;
use wa_engine::spelling::{self, SpellChecker};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Accessibility::{IUIAutomationCacheRequest, IUIAutomationElement};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, GetWindowTextW, SetProcessDPIAware};

const FOCUS_DELAY: Duration = Duration::from_secs(14);
const SETTLE_DELAY: Duration = Duration::from_millis(500);

fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("notepad") => run_app_tests("Notepad", 1),
        Some("edge") => run_app_tests("Edge (a focused text field, not the browser chrome)", 2),
        Some("word") => run_app_tests("Word's document canvas", 3),
        Some("replace-live") => run_live_replace_test(true),
        Some("replace-within-live") => run_live_replace_test(false),
        Some("diagnose-selection") => run_selection_diagnostic(),
        Some("restore-drill") => run_restore_drill(),
        Some("refocus") => run_refocus_test(&args[2..]),
        Some("correct") => run_correct_test(&args[2..]),
        _ => {
            eprintln!(
                "usage: insertion_cascade_spike <notepad|edge|word|replace-live|replace-within-live|diagnose-selection|restore-drill|refocus|correct>"
            );
            std::process::exit(1);
        }
    }
    println!("Done.");
}

/// Detects exactly what the user typed by diffing a snapshot of the focused element's text
/// taken before the typing window against one taken after (`changed_region`), then replaces
/// just that new text: the same certainty the automated app tests have (they know exactly what
/// they inserted because they inserted it themselves), applied to real user-authored content
/// instead of a marker this harness controls. Works no matter where in the document the typing
/// happened, not only at the end. `also_test_last_typed` runs the cursor-relative
/// `replace_last_typed` probe on the last word after the `replace_within` probe on the 2nd
/// word; pass `false` to test `replace_within` in isolation, since `replace_last_typed`'s
/// cursor-relative assumption only holds if nothing has moved the cursor since the original
/// typing, and `replace_within` running first in the same document can do exactly that.
fn run_live_replace_test(also_test_last_typed: bool) {
    let uia = match Uia::new() {
        Ok(uia) => uia,
        Err(error) => {
            println!("  could not create UIA client: {error}");
            return;
        }
    };
    let cache = match uia.base_cache_request() {
        Ok(cache) => cache,
        Err(error) => {
            println!("  could not build cache request: {error}");
            return;
        }
    };
    let element = match uia.focused_element(&cache) {
        Ok(element) => element,
        Err(error) => {
            println!("  no focused element: {error}");
            return;
        }
    };
    let window = foreground::target_window(uia.client(), &element);

    let before = match current_text(&element) {
        Ok(text) => text,
        Err(error) => {
            println!("  could not read baseline text: {error}");
            return;
        }
    };
    println!(
        "\nBaseline read ({} chars). Type a short sentence anywhere in the document now; reading the diff in {}s.",
        before.chars().count(),
        FOCUS_DELAY.as_secs()
    );
    sleep(FOCUS_DELAY);

    let after = match current_text(&element) {
        Ok(text) => text,
        Err(error) => {
            println!("  could not read post-typing text: {error}");
            return;
        }
    };
    let Some((_, region)) = changed_region(&before, &after) else {
        println!("  no change detected between the two snapshots");
        return;
    };
    let typed = region.trim();
    if typed.is_empty() {
        println!("  no new text detected between the two snapshots");
        return;
    }
    let Some(target) = typed.split_whitespace().next_back() else {
        println!("  no word detected in the newly typed text \"{typed}\"");
        return;
    };
    let target = target.to_owned();
    // Deliberately shares no characters with `target`: a replacement built from `target` (e.g.
    // `format!("{target}-CORRECTED")`) can make a leaked fragment of the original word
    // indistinguishable from a correct replace by eye, since the leak just looks like a
    // (correct) prefix of the replacement rather than a visibly wrong extra character.
    let replacement = "QQ-REPLACED-QQ".to_owned();
    println!("  detected newly typed text: {typed:?}");

    // A probe against a word that is not where the cursor currently sits: replace_last_typed
    // (below) only tests the just-typed word, selecting backward from the live cursor, so it
    // cannot stand in for a flagged word no longer under the cursor. Uses replace_within
    // (anchor-relative), not replace_span (content-search based, which can match the wrong
    // occurrence of a short target) or a document-absolute offset (which drifts near
    // auto-numbered list items, since TextUnit_Character-based Move() does not advance through
    // a list marker the same number of units GetText() counts it as). Anchoring on `typed` and
    // moving a short local distance avoids both problems. Uses the 2nd word, not the target's
    // own occurrence, to keep this a separate span from what replace-last-typed edits below,
    // and runs first because it needs the full, still-unmodified `typed` phrase as its anchor.
    if let Some(second_word) = typed
        .split_whitespace()
        .nth(1)
        .filter(|word| *word != target)
    {
        // Not `typed.find(second_word)`: a naive substring search can land inside an earlier
        // word instead of on `second_word` itself, for example the word "is" inside "this".
        // `typed` is trimmed (no leading whitespace), so the first token starts at byte 0; the
        // 2nd token's own start is exactly the first token's length plus however much
        // whitespace follows it, with no search step to land on the wrong occurrence.
        let first_word = typed
            .split_whitespace()
            .next()
            .expect("typed is non-empty, checked above");
        let after_first_word = &typed[first_word.len()..];
        let leading_whitespace = after_first_word.len() - after_first_word.trim_start().len();
        let local_offset = first_word.len() + leading_whitespace;
        let word_start = typed[..local_offset].encode_utf16().count();
        let word_len = second_word.encode_utf16().count();
        let span_replacement = "PP-SPAN-PP".to_owned();
        println!(
            "  targeting its 2nd word {second_word:?} (local offset {word_start} within {typed:?}) via replace_within (mid-sentence, not at the cursor), replacing with {span_replacement:?}"
        );
        sleep(SETTLE_DELAY);
        let span_result = replace_within(
            &element,
            window,
            typed,
            0,
            word_start,
            word_len,
            &span_replacement,
        );
        sleep(SETTLE_DELAY);
        report_replace(
            "replace-within (anchor-relative, live-detected, mid-sentence)",
            span_result,
            &element,
            second_word,
            &span_replacement,
        );
    }

    if !also_test_last_typed {
        return;
    }
    println!("  targeting its last word \"{target}\", replacing with \"{replacement}\"");

    // The cursor is still sitting right after what was just typed, so selecting backward by
    // character count (replace_last_typed) is unambiguous regardless of whether the target text
    // repeats elsewhere in the document.
    let replace_result = replace_last_typed(
        &element,
        window,
        &target,
        target.chars().count(),
        &replacement,
    );
    sleep(SETTLE_DELAY);
    report_replace(
        "replace-last-typed (keyboard, live-detected)",
        replace_result,
        &element,
        &target,
        &replacement,
    );
}

/// Non-destructive counterpart to `run_live_replace_test`: selects a just-typed word by both
/// techniques (`synthetic::select_left`, cursor-relative; `select::select_range`,
/// position-based) and reads the resulting selection straight back via UIA, without ever typing
/// or pasting over it. Exists because `replace_last_typed` reporting "no
/// verifiable effect" is ambiguous between several distinct causes: the selection itself never
/// landed, or it landed correctly but the subsequent type or paste call is what failed.
/// Distinguishing those needs to observe the selection in isolation, not infer it from the
/// end-to-end result.
fn run_selection_diagnostic() {
    let uia = match Uia::new() {
        Ok(uia) => uia,
        Err(error) => {
            println!("  could not create UIA client: {error}");
            return;
        }
    };
    let cache = match uia.base_cache_request() {
        Ok(cache) => cache,
        Err(error) => {
            println!("  could not build cache request: {error}");
            return;
        }
    };
    let element = match uia.focused_element(&cache) {
        Ok(element) => element,
        Err(error) => {
            println!("  no focused element: {error}");
            return;
        }
    };

    let before = match current_text(&element) {
        Ok(text) => text,
        Err(error) => {
            println!("  could not read baseline text: {error}");
            return;
        }
    };
    println!(
        "\nBaseline read ({} chars). Type or edit a short span now; reading the diff in {}s.",
        before.chars().count(),
        FOCUS_DELAY.as_secs()
    );
    sleep(FOCUS_DELAY);

    let after = match current_text(&element) {
        Ok(text) => text,
        Err(error) => {
            println!("  could not read post-edit text: {error}");
            return;
        }
    };
    let Some((region_start, region)) = changed_region(&before, &after) else {
        println!("  no change detected between the two snapshots");
        return;
    };
    let leading_whitespace = region.len() - region.trim_start().len();
    let typed = region.trim();
    let typed_start = region_start + leading_whitespace;
    if typed.is_empty() {
        println!("  no new text detected between the two snapshots");
        return;
    }
    let Some(target) = typed.split_whitespace().next_back() else {
        println!("  no word detected in the newly typed text \"{typed}\"");
        return;
    };
    println!("  detected newly typed text: {typed:?}");

    let count = target.chars().count();
    println!("  select_left({count}) targeting its last word {target:?} (cursor-relative)...");
    match synthetic::select_left(count as u32) {
        Err(error) => println!("    select_left failed: {error}"),
        Ok(()) => {
            sleep(SETTLE_DELAY);
            match current_selection(&element) {
                Ok(selected) => {
                    println!("    UIA reports the active selection is {selected:?} (expected {target:?})")
                }
                Err(error) => println!("    could not read the selection back: {error}"),
            }
        }
    }

    if let Some(second_word) = typed
        .split_whitespace()
        .nth(1)
        .filter(|word| *word != target)
    {
        // See run_live_replace_test's identical fix for why this isn't `typed.find(second_word)`:
        // a naive substring search can land inside an earlier word instead of on `second_word`
        // itself.
        let first_word = typed
            .split_whitespace()
            .next()
            .expect("typed is non-empty, checked above");
        let after_first_word = &typed[first_word.len()..];
        let leading_whitespace = after_first_word.len() - after_first_word.trim_start().len();
        let local_offset = first_word.len() + leading_whitespace;
        let word_start = after[..typed_start + local_offset].encode_utf16().count();
        let word_len = second_word.encode_utf16().count();
        println!(
            "  select_range({word_start}, {word_len}) targeting its 2nd word {second_word:?} (position-based)..."
        );
        match select::select_range(&element, word_start, word_len) {
            Err(error) => println!("    select_range failed: {error}"),
            Ok(()) => {
                sleep(SETTLE_DELAY);
                match current_selection(&element) {
                    Ok(selected) => println!(
                        "    UIA reports the active selection is {selected:?} (expected {second_word:?})"
                    ),
                    Err(error) => println!("    could not read the selection back: {error}"),
                }
            }
        }

        // select_within anchors on `typed` itself (the freshly-typed phrase, long enough to be
        // effectively unique) instead of counting from the document's absolute start, so
        // `local_offset`, already computed above relative to `typed`, is used directly rather
        // than added to `typed_start`.
        let local_start = typed[..local_offset].encode_utf16().count();
        println!(
            "  select_within({typed:?}, 0, {local_start}, {word_len}) targeting its 2nd word {second_word:?} (anchor-relative)..."
        );
        match select::select_within(&element, typed, 0, local_start, word_len) {
            Err(error) => println!("    select_within failed: {error}"),
            Ok(()) => {
                sleep(SETTLE_DELAY);
                match current_selection(&element) {
                    Ok(selected) => println!(
                        "    UIA reports the active selection is {selected:?} (expected {second_word:?})"
                    ),
                    Err(error) => println!("    could not read the selection back: {error}"),
                }
            }
        }
    }
}

/// Deliberately manufactures the exact broken state `restore_after_failed_attempt` exists to
/// clean up: a fragment typed but never verified, standing in for an interrupted `SendInput`
/// call. Exercises that cleanup directly rather than waiting for the real flake, which is rare,
/// to recur on its own. Types the stand-in fragment via raw `synthetic::type_text`, bypassing
/// the cascade's own verify-and-fall-through logic, so this drill controls exactly when the
/// cleanup runs. Safe to run with the cursor anywhere, including inside a list item, a
/// structurally distinct region worth exercising directly rather than assuming plain-text
/// coverage generalises.
fn run_restore_drill() {
    let uia = match Uia::new() {
        Ok(uia) => uia,
        Err(error) => {
            println!("  could not create UIA client: {error}");
            return;
        }
    };
    let cache = match uia.base_cache_request() {
        Ok(cache) => cache,
        Err(error) => {
            println!("  could not build cache request: {error}");
            return;
        }
    };
    let element = match uia.focused_element(&cache) {
        Ok(element) => element,
        Err(error) => {
            println!("  no focused element: {error}");
            return;
        }
    };

    println!(
        "\nFocus the target now (cursor wherever you want the drill to run, inside a list \
         item to check the list-marker case); the stand-in fragment types in {}s.",
        FOCUS_DELAY.as_secs()
    );
    sleep(FOCUS_DELAY);

    let before = match current_text(&element) {
        Ok(text) => text,
        Err(error) => {
            println!("  could not read baseline text: {error}");
            return;
        }
    };

    let fragment = "WA-RESTORE-DRILL-FRAGMENT";
    println!("  typing stand-in fragment {fragment:?} (not verified, standing in for a truncated send)...");
    if let Err(error) = synthetic::type_text(fragment) {
        println!("  could not type the stand-in fragment: {error}");
        return;
    }
    sleep(SETTLE_DELAY);

    let mid = match current_text(&element) {
        Ok(text) => text,
        Err(error) => {
            println!("  could not read post-fragment text: {error}");
            return;
        }
    };
    match changed_region(&before, &mid) {
        Some((start, region)) => println!("  fragment landed at byte offset {start}: {region:?}"),
        None => println!("  fragment did not land at all (nothing changed), drill is a no-op"),
    }

    println!("  running restore_after_failed_attempt...");
    let cleaned = restore_after_failed_attempt(&element, &before);
    sleep(SETTLE_DELAY);

    let after = match current_text(&element) {
        Ok(text) => text,
        Err(error) => {
            println!("  could not read post-cleanup text: {error}");
            return;
        }
    };
    let restored = after == before;
    println!(
        "  cleanup reported: {cleaned}; document exactly restored to baseline: {restored}{}",
        if restored {
            ""
        } else {
            " (MISMATCH, inspect the document)"
        }
    );
}

fn run_app_tests(label: &str, index: u32) {
    println!(
        "\nFocus {label}; testing starts in {}s.",
        FOCUS_DELAY.as_secs()
    );
    sleep(FOCUS_DELAY);

    let uia = match Uia::new() {
        Ok(uia) => uia,
        Err(error) => {
            println!("  could not create UIA client: {error}");
            return;
        }
    };
    let cache = match uia.base_cache_request() {
        Ok(cache) => cache,
        Err(error) => {
            println!("  could not build cache request: {error}");
            return;
        }
    };
    let element = match uia.focused_element(&cache) {
        Ok(element) => element,
        Err(error) => {
            println!("  no focused element: {error}");
            return;
        }
    };
    let window = foreground::target_window(uia.client(), &element);

    let value_marker = format!("WA-VALUE-{index}");
    let value_call = value::set_value(&element, &value_marker);
    report("value-set", value_call, &element, &value_marker);

    let synthetic_marker = format!("WA-SYNTH-{index}");
    let synthetic_call = synthetic::type_text(&synthetic_marker);
    report(
        "synthetic-input",
        synthetic_call,
        &element,
        &synthetic_marker,
    );

    let clipboard_marker = format!("WA-CLIP-{index}");
    let clipboard_call = clipboard::paste_text(&clipboard_marker);
    report(
        "clipboard-paste",
        clipboard_call,
        &element,
        &clipboard_marker,
    );

    // Targeted replacement, accessibility-tree search: find the clipboard-paste marker
    // specifically and overwrite only that span, proving the cascade can do a real correction
    // (replace this text, leave the rest alone) rather than only the whole-field-replace or
    // blind-append tested above.
    sleep(SETTLE_DELAY);
    let replaced_marker = format!("WA-REPLACED-{index}");
    let replace_result = replace_span(&element, window, &clipboard_marker, &replaced_marker);
    sleep(SETTLE_DELAY);
    report_replace(
        "replace-span (FindText)",
        replace_result,
        &element,
        &clipboard_marker,
        &replaced_marker,
    );

    // Targeted replacement, pure keyboard selection: same goal, but selects backward from the
    // cursor by character count instead of searching the accessibility tree for the target
    // text, for targets (Google Docs among them) where that search is unreliable. Re-inserts
    // the clipboard marker first, since the previous stage may already have replaced it.
    sleep(SETTLE_DELAY);
    let _ = clipboard::paste_text(&clipboard_marker);
    sleep(SETTLE_DELAY);
    let replaced_marker_2 = format!("WA-REPLACED2-{index}");
    let replace_result_2 = replace_last_typed(
        &element,
        window,
        &clipboard_marker,
        clipboard_marker.chars().count(),
        &replaced_marker_2,
    );
    sleep(SETTLE_DELAY);
    report_replace(
        "replace-last-typed (keyboard)",
        replace_result_2,
        &element,
        &clipboard_marker,
        &replaced_marker_2,
    );
}

/// How far into a paragraph the refocus probe's anchor reaches, in bytes: long enough to be found
/// at one place, short enough to stay inside one paragraph of the generated text.
const PROBE_ANCHOR_BYTES: usize = 48;
const PROBE_MIN_ANCHOR_BYTES: usize = 24;

/// The longest wait for the target's window or the cover's to be in front.
const FRONT_WAIT: Duration = Duration::from_secs(5);

/// The refocus before an insertion (#157), against a real application. After the countdown the
/// focused element is the target; each of `runs` runs then starts `cover_script` in PowerShell,
/// which places a window of its own over the target and makes it the foreground window, waits for
/// that window to be in front by reading the foreground window, and calls the cascade's
/// `replace_within` on the first word of the paragraph after the document's middle. The refocus
/// runs inside the cascade, not here. Each run reports whether the cover was in front at the call,
/// the cascade's result, whether the refocus took, which window was in front after it, where the
/// replacement landed by reading the target's text back, and whether the cover's own text changed,
/// which a keystroke reaching the cover would do. It prints lengths, offsets and outcomes, never
/// text of either window.
///
/// The script is started with the arguments given after its path, then `-X`, `-Y`, `-Width` and
/// `-Height` with the target window's rectangle in physical pixels and `-HarnessPid` with this
/// process's id, and prints a line starting with `ready` once its window holds the foreground.
/// The script is stopped at the end of each run, which closes its window. A run that finds the
/// target not in front at its start, or the cover not in front after the script is ready, stops
/// the loop.
///
/// `title` guards the document: the target window's title, which names the open document in Notepad
/// and Word, must hold it when the runs start, at each run's start and just before each probe, or
/// nothing is typed. It keeps the probe off any document but the one written for the check, such
/// as another Notepad tab shown in the same document area. The title is not printed.
///
/// `cargo run -p wa-engine --example insertion_cascade_spike -- refocus <runs> <title>
/// <cover-script> [script arguments]`, with `RUST_LOG=info` for the cascade's refocus lines.
fn run_refocus_test(args: &[String]) {
    let (Some(runs), Some(title), Some(script)) = (
        args.first().and_then(|runs| runs.parse::<u32>().ok()),
        args.get(1),
        args.get(2),
    ) else {
        eprintln!(
            "usage: insertion_cascade_spike refocus <runs> <title> <cover-script> [script arguments]"
        );
        std::process::exit(1);
    };
    let cover = CoverScript {
        path: script,
        args: &args[3..],
    };
    // SAFETY: SetProcessDPIAware takes no arguments. Without it this process reads window
    // rectangles scaled to 96 DPI, and the cover script places its window in physical pixels.
    let _ = unsafe { SetProcessDPIAware() };

    println!(
        "\nFocus the target document; {runs} refocus runs start in {}s.",
        FOCUS_DELAY.as_secs()
    );
    sleep(FOCUS_DELAY);

    let Some(client) = Client::new() else {
        return;
    };
    let Some(target) = client.focused_target(title) else {
        return;
    };

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() % 10_000);
    let mut tally = Tally::default();
    for run in 1..=runs {
        let marker = format!("QQ{stamp:04}R{run:02}QQ");
        let Some(outcome) = refocus_run(&client, &target, &cover, &marker) else {
            println!("  run {run}: stopped");
            break;
        };
        println!("  run {run}: {outcome}");
        tally.add(&outcome);
        sleep(SETTLE_DELAY);
    }
    println!("  {tally}");
}

/// A UI Automation client and the cache request its reads take.
struct Client {
    uia: Uia,
    cache: IUIAutomationCacheRequest,
}

impl Client {
    /// The client, or `None`, with the reason printed, when it could not be set up.
    fn new() -> Option<Self> {
        let uia = Uia::new()
            .map_err(|error| println!("  could not create UIA client: {error}"))
            .ok()?;
        let cache = uia
            .base_cache_request()
            .map_err(|error| println!("  could not build cache request: {error}"))
            .ok()?;
        Some(Self { uia, cache })
    }

    /// The focused element as the target, once its top-level window is in front and the window's
    /// title holds `title`, or `None`, with the reason printed, when any of that fails. Prints the
    /// target window's process name.
    fn focused_target<'a>(&self, title: &'a str) -> Option<Target<'a>> {
        let element = self
            .uia
            .focused_element(&self.cache)
            .map_err(|error| println!("  no focused element: {error}"))
            .ok()?;
        let Some(window) = foreground::target_window(self.uia.client(), &element) else {
            println!("  the focused element has no window");
            return None;
        };
        if !wait_for_front(|front| front.hwnd == window.hwnd) {
            println!("  the focused element's window is not in front");
            return None;
        }
        let target = Target {
            element,
            window,
            title,
        };
        if !target.title_holds() {
            println!(
                "  the focused window's title does not hold the given title; nothing is typed"
            );
            return None;
        }
        println!(
            "  target: {}'s window",
            foreground::process_name(window.process_id)
        );
        Some(target)
    }
}

/// The `refocus` and `correct` slots' target: the focused element, its top-level window, and the
/// text the window's title must hold.
struct Target<'a> {
    element: IUIAutomationElement,
    window: Window,
    title: &'a str,
}

impl Target<'_> {
    /// Whether the target window's title, read with `GetWindowTextW`, holds [`Self::title`].
    fn title_holds(&self) -> bool {
        let mut buffer = [0_u16; 512];
        // SAFETY: `self.window.hwnd` is a window handle; a window destroyed since answers 0.
        // `buffer` is a valid, uniquely-owned out buffer for the duration of the call.
        let length = unsafe { GetWindowTextW(self.window.hwnd, &mut buffer) };
        usize::try_from(length).is_ok_and(|length| {
            String::from_utf16_lossy(&buffer[..length.min(buffer.len())]).contains(self.title)
        })
    }
}

/// The cover script's path and the arguments given for it on the command line.
struct CoverScript<'a> {
    path: &'a str,
    args: &'a [String],
}

/// One run of [`run_refocus_test`], or `None`, with the reason printed, when the run could not
/// reach the probe.
fn refocus_run(
    client: &Client,
    target: &Target,
    cover_script: &CoverScript,
    marker: &str,
) -> Option<RunOutcome> {
    let element = &target.element;
    if !wait_for_front(|front| front.hwnd == target.window.hwnd) {
        println!("  the target's window is not in front at the run's start");
        return None;
    }
    if !target.title_holds() {
        println!("  the target window's title no longer holds the given title");
        return None;
    }
    let before = current_text(element)
        .map_err(|error| println!("  could not read the target: {error}"))
        .ok()?;
    let Some(span) = probe_span(&before) else {
        println!("  no paragraph after the document's middle to anchor on");
        return None;
    };
    let mut rect = RECT::default();
    // SAFETY: `target.window.hwnd` is the target's top-level window, just confirmed in front;
    // `rect` is a valid, uniquely-owned out parameter for the duration of the call.
    if let Err(error) = unsafe { GetWindowRect(target.window.hwnd, &raw mut rect) } {
        println!("  could not read the target window's rectangle: {error}");
        return None;
    }

    let mut cover = start_cover(cover_script, rect)?;
    let cover_pid = cover.id();
    if !wait_for_front(|front| front.process_id == cover_pid) {
        println!("  the cover did not take the foreground");
        stop_cover(&mut cover);
        return None;
    }
    if !target.title_holds() {
        println!("  the target window's title no longer holds the given title");
        stop_cover(&mut cover);
        return None;
    }
    let cover_element = client
        .uia
        .focused_element(&client.cache)
        .ok()
        .filter(|focused| {
            // SAFETY: `focused` is the live element just returned; the property read fails safely.
            let owner = unsafe { focused.CurrentProcessId() };
            owner.is_ok_and(|owner| owner.cast_unsigned() == cover_pid)
        });
    let cover_before = cover_element
        .as_ref()
        .and_then(|cover| current_text(cover).ok());
    let cover_in_front = foreground::window().is_some_and(|front| front.process_id == cover_pid);

    let result = replace_within(
        element,
        Some(target.window),
        &span.anchor,
        span.occurrence,
        0,
        span.local_length,
        marker,
    );
    sleep(SETTLE_DELAY);
    let front_after = foreground::window();
    let after = current_text(element).ok();
    let cover_after = cover_element
        .as_ref()
        .and_then(|cover| current_text(cover).ok());
    stop_cover(&mut cover);

    let expected = format!(
        "{}{marker}{}",
        &before[..span.start],
        &before[span.start + span.length..]
    );
    let landing = Landing::of(&before, after.as_deref(), &expected);
    Some(RunOutcome {
        cover_in_front,
        refocus_failed: matches!(result, Err(NativeCaptureError::RefocusFailed { .. })),
        result: match result {
            Ok(method) => format!("ok via {method:?}"),
            Err(error) => format!("failed ({error})"),
        },
        front_after: match front_after {
            Some(front) if front.hwnd == target.window.hwnd => "the target",
            Some(front) if front.process_id == cover_pid => "the cover",
            Some(_) => "another window",
            None => "no window",
        },
        span_at: before[..span.start].chars().count(),
        landing,
        cover_changed: match (cover_before, cover_after) {
            (Some(before), Some(after)) => Some(before != after),
            _ => None,
        },
    })
}

/// The refocus probe's span: the first word of the first paragraph after the document's middle,
/// addressed by an anchor at the paragraph's start at the occurrence index the analyzer's counting
/// rule gives it.
struct ProbeSpan {
    anchor: String,
    occurrence: usize,
    /// The span's byte offset in the text it was chosen from.
    start: usize,
    /// The span's length in bytes.
    length: usize,
    /// The span's length in UTF-16 code units.
    local_length: usize,
}

fn probe_span(text: &str) -> Option<ProbeSpan> {
    let mut from = (text.len() / 2..=text.len()).find(|&at| text.is_char_boundary(at))?;
    loop {
        let paragraph_break = from + text[from..].find(['\r', '\n'])?;
        let start =
            paragraph_break + text[paragraph_break..].find(|c: char| c != '\r' && c != '\n')?;
        let paragraph = &text[start..];
        let paragraph_end = paragraph.find(['\r', '\n']).unwrap_or(paragraph.len());
        let anchor_end = (0..=paragraph_end.min(PROBE_ANCHOR_BYTES))
            .rev()
            .find(|&at| paragraph.is_char_boundary(at))?;
        let anchor = &paragraph[..anchor_end];
        let word_end = anchor.find(' ').unwrap_or(anchor.len());
        let occurrence = text.match_indices(anchor).position(|(at, _)| at == start);
        if let (true, Some(occurrence)) = (
            anchor.len() >= PROBE_MIN_ANCHOR_BYTES && word_end >= 2 && word_end < anchor.len(),
            occurrence,
        ) {
            return Some(ProbeSpan {
                anchor: anchor.to_owned(),
                occurrence,
                start,
                length: word_end,
                local_length: anchor[..word_end].encode_utf16().count(),
            });
        }
        from = start;
    }
}

/// Starts the cover script without a console window of its own and waits for its `ready` line,
/// echoing each line it prints, which hold states and numbers only.
fn start_cover(script: &CoverScript, rect: RECT) -> Option<Child> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cover = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            script.path,
        ])
        .args(script.args)
        .args(["-X", &rect.left.to_string(), "-Y", &rect.top.to_string()])
        .args(["-Width", &(rect.right - rect.left).to_string()])
        .args(["-Height", &(rect.bottom - rect.top).to_string()])
        .args(["-HarnessPid", &std::process::id().to_string()])
        .stdout(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| println!("  could not start the cover script: {error}"))
        .ok()?;
    let stdout = cover.stdout.take()?;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        println!("    cover: {line}");
        if line.starts_with("ready") {
            return Some(cover);
        }
    }
    println!("  the cover script ended before it was ready");
    stop_cover(&mut cover);
    None
}

/// Ends the cover script's process, which closes its window.
fn stop_cover(cover: &mut Child) {
    let _ = cover.kill();
    let _ = cover.wait();
}

/// Whether the foreground window meets `wanted` within [`FRONT_WAIT`], read every 50 ms.
fn wait_for_front(wanted: impl Fn(Window) -> bool) -> bool {
    let deadline = Instant::now() + FRONT_WAIT;
    loop {
        if foreground::window().is_some_and(&wanted) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(50));
    }
}

/// Where the `correct` slot loads the dictionaries from, relative to the repository root it runs
/// from.
const RESOURCES_DIR: &str = "src-tauri/resources";

/// One replacement of a misspelt word by its correction (#158), against a field focused by hand
/// that holds text written for the check, such as one in a browser tab, where the per-application
/// slots' markers and value-set would overwrite the field. After the countdown the focused element
/// is the target; its window must be in front and its title must hold `title`, as for the
/// `refocus` slot, or nothing is typed. The span is the one the analyzer publishes for the first
/// misspelt word with a suggestion: the spell checker's flag on the sentence that holds it,
/// anchored on that sentence at the occurrence index the analyzer counts for it, and the
/// correction is the flag's first suggestion. `replace_within`, the path `Capture::replace` takes,
/// replaces it, with the cascade's foreground check before each stage that sends a keystroke. The
/// text read back is compared with the text before with the span, and only the span, replaced.
/// Prints counts, offsets, lengths and outcomes, never text.
///
/// `cargo run -p wa-engine --example insertion_cascade_spike -- correct <title>`, from the
/// repository root, with `RUST_LOG=info` for the cascade's refocus lines.
fn run_correct_test(args: &[String]) {
    let Some(title) = args.first() else {
        eprintln!("usage: insertion_cascade_spike correct <title>");
        std::process::exit(1);
    };
    let paths = spelling::default_paths(Path::new(RESOURCES_DIR));
    let Ok(checker) = SpellChecker::load(&paths.aff, &paths.dic, &paths.supplement)
        .map_err(|error| println!("  could not load the dictionaries: {error}"))
    else {
        return;
    };

    println!(
        "\nFocus the target field; the replacement starts in {}s.",
        FOCUS_DELAY.as_secs()
    );
    sleep(FOCUS_DELAY);

    let Some(client) = Client::new() else {
        return;
    };
    let Some(target) = client.focused_target(title) else {
        return;
    };
    let element = &target.element;
    let window = target.window;
    let Ok(before) =
        current_text(element).map_err(|error| println!("  could not read the target: {error}"))
    else {
        return;
    };
    let Some(correction) = first_misspelling(&checker, &before) else {
        println!(
            "  the target's {} characters hold no misspelt word with a suggestion",
            before.chars().count()
        );
        return;
    };
    let span_at = before[..correction.start].chars().count();
    println!(
        "  text: {} characters in {} sentences; the first misspelt word with a suggestion is in \
         sentence {}, at character {span_at}, {} characters, occurrence index {} of its sentence; \
         its correction, the first of {} suggestions, is {} characters",
        before.chars().count(),
        correction.sentences,
        correction.sentence,
        before[correction.start..correction.end].chars().count(),
        correction.span.occurrence,
        correction.suggestions,
        correction.replacement.chars().count()
    );
    if !target.title_holds() {
        println!("  the target window's title no longer holds the given title; nothing is typed");
        return;
    }

    let in_front_at_call = foreground::window().is_some_and(|front| front.hwnd == window.hwnd);
    let result = replace_within(
        element,
        Some(window),
        &correction.span.anchor,
        correction.span.occurrence,
        correction.span.local_start,
        correction.span.local_length,
        &correction.replacement,
    );
    sleep(SETTLE_DELAY);
    let front_after = foreground::window();
    let after = current_text(element).ok();
    let expected = format!(
        "{}{}{}",
        &before[..correction.start],
        correction.replacement,
        &before[correction.end..]
    );
    let landing = Landing::of(&before, after.as_deref(), &expected);
    println!(
        "  the target in front at the call: {in_front_at_call}; result: {}; in front after: {}; \
         read back: {}; replacement landed {}",
        match result {
            Ok(method) => format!("ok via {method:?}"),
            Err(error) => format!("failed ({error})"),
        },
        match front_after {
            Some(front) if front.hwnd == window.hwnd => "the target",
            Some(_) => "another window",
            None => "no window",
        },
        after.as_ref().map_or_else(
            || "nothing".to_owned(),
            |after| format!("{} characters", after.chars().count())
        ),
        landing.describe(span_at)
    );
}

/// The first misspelt word of a text, as the analyzer would publish it, and where it sits.
struct Correction {
    /// The spell checker's span on the word's sentence, with the occurrence index the analyzer
    /// counts for it.
    span: Span,
    /// The checker's first suggestion.
    replacement: String,
    /// How many suggestions the checker gave.
    suggestions: usize,
    /// The word's sentence, numbered from 1, and the text's count of sentences.
    sentence: usize,
    sentences: usize,
    /// The word's byte range in the text.
    start: usize,
    end: usize,
}

/// The first word of `text` the spell checker flags with a suggestion, checked a sentence at a
/// time as the analyzer checks it: the sentences of Unicode's sentence boundaries, trimmed, and
/// the occurrence index the count of the sentence's text in the text before it.
fn first_misspelling(checker: &SpellChecker, text: &str) -> Option<Correction> {
    let sentences: Vec<(usize, &str)> = text
        .unicode_sentences()
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        // `unicode_sentences` and `trim` both return slices of `text`, so the distance between the
        // two addresses is the sentence's byte offset in it.
        .map(|sentence| (sentence.as_ptr().addr() - text.as_ptr().addr(), sentence))
        .collect();
    sentences
        .iter()
        .enumerate()
        .find_map(|(index, &(sentence_start, sentence))| {
            let flag = checker
                .check(sentence)
                .into_iter()
                .find(|flag| !flag.suggestions.is_empty())?;
            let start = sentence_start + byte_offset(sentence, flag.span.local_start)?;
            let end = sentence_start
                + byte_offset(sentence, flag.span.local_start + flag.span.local_length)?;
            Some(Correction {
                replacement: flag.suggestions[0].clone(),
                suggestions: flag.suggestions.len(),
                span: Span {
                    occurrence: text[..sentence_start].matches(sentence).count(),
                    ..flag.span
                },
                sentence: index + 1,
                sentences: sentences.len(),
                start,
                end,
            })
        })
}

/// The byte offset in `text` of the character `utf16` UTF-16 code units into it, or `None` when
/// no character starts there.
fn byte_offset(text: &str, utf16: usize) -> Option<usize> {
    let mut units = 0;
    for (at, character) in text.char_indices() {
        if units == utf16 {
            return Some(at);
        }
        units += character.len_utf16();
    }
    (units == utf16).then_some(text.len())
}

/// Where a run's replacement landed, from the target's text read back after it.
enum Landing {
    /// The text read back is the text before with the span, and only the span, replaced.
    AtTheSpan,
    /// The text read back is the text before.
    Unchanged,
    /// The text changed otherwise: the changed region's offset and length in characters.
    Elsewhere(Option<(usize, usize)>),
    /// The text could not be read back.
    Unread,
}

impl Landing {
    /// Where a replacement landed, from the text `before` it, the text read back `after` it, or
    /// `None` when it could not be read back, and the text `expected` with the span, and only the
    /// span, replaced.
    fn of(before: &str, after: Option<&str>, expected: &str) -> Self {
        match after {
            None => Self::Unread,
            Some(after) if after == expected => Self::AtTheSpan,
            Some(after) if after == before => Self::Unchanged,
            Some(after) => Self::Elsewhere(
                changed_region(before, after)
                    .map(|(at, region)| (after[..at].chars().count(), region.chars().count())),
            ),
        }
    }

    /// The landing in words, with `span_at`, the span's offset in characters.
    fn describe(&self, span_at: usize) -> String {
        match self {
            Self::AtTheSpan => format!("at the span, character {span_at}"),
            Self::Unchanged => "nowhere, the target's text is unchanged".to_owned(),
            Self::Elsewhere(Some((at, length))) => format!(
                "elsewhere: {length} characters changed at character {at}, the span at {span_at}"
            ),
            Self::Elsewhere(None) => "elsewhere".to_owned(),
            Self::Unread => "unknown, the target could not be read back".to_owned(),
        }
    }
}

struct RunOutcome {
    cover_in_front: bool,
    refocus_failed: bool,
    result: String,
    front_after: &'static str,
    /// The span's offset in characters.
    span_at: usize,
    landing: Landing,
    /// Whether the cover's text changed, or `None` when it could not be read.
    cover_changed: Option<bool>,
}

impl std::fmt::Display for RunOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let landing = self.landing.describe(self.span_at);
        let cover = match self.cover_changed {
            Some(true) => "changed",
            Some(false) => "unchanged",
            None => "unread",
        };
        write!(
            f,
            "cover in front at the call: {}; result: {}; in front after: {}; replacement landed {landing}; cover's text {cover}",
            self.cover_in_front, self.result, self.front_after
        )
    }
}

/// The runs' counts: a refocus ran when the cover was in front at the call, took when the cascade
/// did not answer `RefocusFailed`, and verified when the replacement landed at the span.
#[derive(Default)]
struct Tally {
    runs: u32,
    ran: u32,
    took: u32,
    verified: u32,
    cover_changed: u32,
    cover_unread: u32,
}

impl Tally {
    fn add(&mut self, outcome: &RunOutcome) {
        self.runs += 1;
        if outcome.cover_in_front {
            self.ran += 1;
            if !outcome.refocus_failed {
                self.took += 1;
            }
        }
        if matches!(outcome.landing, Landing::AtTheSpan) {
            self.verified += 1;
        }
        match outcome.cover_changed {
            Some(true) => self.cover_changed += 1,
            Some(false) => {}
            None => self.cover_unread += 1,
        }
    }
}

impl std::fmt::Display for Tally {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n = {}: refocus ran {}, took {}; replacement at the span {}; cover's text changed {}, unread {}",
            self.runs, self.ran, self.took, self.verified, self.cover_changed, self.cover_unread
        )
    }
}

fn report_replace(
    label: &str,
    replace_result: Result<wa_engine::capture::native::insert::InsertionMethod, NativeCaptureError>,
    element: &IUIAutomationElement,
    target: &str,
    replacement: &str,
) {
    let replace_status = match &replace_result {
        Ok(method) => format!("ok via {method:?}"),
        Err(error) => format!("failed ({error})"),
    };
    let verified = current_text(element)
        .map(|text| text.contains(replacement) && !text.contains(target))
        .unwrap_or(false);
    let verified_status = if verified {
        "confirmed: target gone, replacement present"
    } else {
        "NOT confirmed"
    };
    println!("  {label}: call {replace_status}, {verified_status}");
}

fn report(
    label: &str,
    call_result: Result<(), NativeCaptureError>,
    element: &IUIAutomationElement,
    marker: &str,
) {
    // Gives the target app a moment to process the simulated input before reading it back.
    sleep(SETTLE_DELAY);
    let call_status = match &call_result {
        Ok(()) => "ok".to_owned(),
        Err(error) => format!("failed ({error})"),
    };
    let landed = current_text(element)
        .map(|text| text.contains(marker))
        .unwrap_or(false);
    let landed_status = if landed {
        "found on read-back"
    } else {
        "NOT found on read-back"
    };
    println!("  {label}: call {call_status}, marker {landed_status}");
}
