//! Manual verification harness for issue #11: exercises value-set, synthetic input, and
//! clipboard paste independently (not the early-stopping `insert()` cascade, which would only
//! ever exercise value-set if it succeeds) against whatever currently has focus.
//!
//! Paced with a fixed delay rather than stdin, per the lesson from issue #10's harness: a
//! backgrounded run's stdin is not reliably distinguishable from a real Enter press. Focus the
//! named app during the countdown; nothing needs to be typed manually.
//!
//! Takes exactly one argument: which app slot to test (its label and marker suffix), so each
//! app can be run, and its focus explicitly confirmed, as a separate invocation rather than
//! trusting a multi-app run's later phases to still be pointed at the right window.

use std::thread::sleep;
use std::time::Duration;

use windows::Win32::UI::Accessibility::IUIAutomationElement;
use writing_assistant::capture::native::client::Uia;
use writing_assistant::capture::native::error::NativeCaptureError;
use writing_assistant::capture::native::insert::{
    changed_region, clipboard, current_selection, current_text, replace_last_typed, replace_span,
    replace_within, restore_after_failed_attempt, select, synthetic, value,
};

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
        _ => {
            eprintln!(
                "usage: insertion_cascade_spike <notepad|edge|word|replace-live|replace-within-live|diagnose-selection|restore-drill>"
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
    // occurrence of a short target) or replace_at (document-absolute, which drifts near
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
        let span_result = replace_within(&element, typed, word_start, word_len, &span_replacement);
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
    let replace_result =
        replace_last_typed(&element, &target, target.chars().count(), &replacement);
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
/// or pasting over it. Exists because `replace_last_typed`/`replace_at` reporting "no
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
            "  select_within({typed:?}, {local_start}, {word_len}) targeting its 2nd word {second_word:?} (anchor-relative)..."
        );
        match select::select_within(&element, typed, local_start, word_len) {
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
    let replace_result = replace_span(&element, &clipboard_marker, &replaced_marker);
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

fn report_replace(
    label: &str,
    replace_result: Result<
        writing_assistant::capture::native::insert::InsertionMethod,
        NativeCaptureError,
    >,
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
