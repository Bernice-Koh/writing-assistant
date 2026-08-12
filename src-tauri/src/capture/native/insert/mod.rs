//! The insertion cascade: value-set, then synthetic input, then clipboard paste, stopping at
//! the first stage whose result is verified against the element's own reported text rather
//! than assumed from the underlying call's exit status.

use std::thread::sleep;
use std::time::Duration;

use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationValuePattern, UIA_TextPatternId,
    UIA_ValuePatternId,
};

use super::error::NativeCaptureError;

pub mod clipboard;
pub mod select;
pub mod synthetic;
pub mod value;

/// A read-back immediately after a synthetic-input or clipboard-paste call can race the target
/// app's own processing of that input, reporting an attempt as failed when it has, in fact,
/// already landed correctly, causing a needless fall-through to the next stage. This delay sits
/// between an attempt and reading its result back, everywhere insertion or replacement is
/// verified.
const SETTLE_DELAY: Duration = Duration::from_millis(300);

/// A selection made by `select::select_text` or `synthetic::select_left` can likewise race the
/// very next `SendInput`-based call: `select_left`'s Shift+Left keystrokes may not yet have
/// registered as an active selection by the time `type_text`'s keystrokes start arriving, so
/// those keystrokes land as a plain insert at the still-collapsed cursor instead of overwriting
/// a selection, appending the replacement next to the original text rather than replacing it.
/// This delay sits between selecting and the overwrite call that assumes the selection is
/// already active.
const SELECTION_SETTLE_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InsertionMethod {
    ValueSet,
    SyntheticInput,
    ClipboardPaste,
}

/// Tries each stage in degrading order, stopping at the first one whose effect is confirmed by
/// reading the element's text back. Returns which stage worked, or the last stage's error if
/// none did.
///
/// A naive "does the text now contain what we tried to insert" check is not safe to reuse
/// across stages unmodified: `value::set_value` *replaces* the element's entire value, so a
/// verification failure there does not guarantee the field is back to its original state
/// before the next stage runs, and a bare containment check on the next stage could then
/// false-positive on residue left behind by the failed attempt rather than on that stage's own
/// effect. Capturing the text immediately before each append-style stage and requiring it to
/// have actually changed, not just to contain the target text, closes that gap without needing
/// full rollback machinery.
pub fn insert(
    element: &IUIAutomationElement,
    text: &str,
) -> Result<InsertionMethod, NativeCaptureError> {
    let value_result = value::set_value(element, text);
    sleep(SETTLE_DELAY);
    if value_result.is_ok()
        && current_text(element)
            .map(|now| now == text)
            .unwrap_or(false)
    {
        return Ok(InsertionMethod::ValueSet);
    }

    let before_synthetic = current_text(element).unwrap_or_default();
    let synthetic_result = synthetic::type_text(text);
    sleep(SETTLE_DELAY);
    if synthetic_result.is_ok() && changed_and_contains(element, &before_synthetic, text) {
        return Ok(InsertionMethod::SyntheticInput);
    }
    restore_after_failed_attempt(element, &before_synthetic);

    let before_clipboard = current_text(element).unwrap_or_default();
    let clipboard_result = clipboard::paste_text(text);
    sleep(SETTLE_DELAY);
    if clipboard_result.is_ok() && changed_and_contains(element, &before_clipboard, text) {
        return Ok(InsertionMethod::ClipboardPaste);
    }

    // Every stage either errored outright or completed without its effect being verifiable;
    // surface whichever concrete error is most relevant, falling back to a dedicated variant
    // when every call technically succeeded but none could be confirmed.
    clipboard_result?;
    synthetic_result?;
    Err(NativeCaptureError::InsertionUnverified)
}

/// Replaces the first occurrence of `target` with `replacement`, rather than `insert`'s
/// whole-field-replace or insert-at-cursor behaviour. `value::set_value` is deliberately not
/// part of this cascade: it replaces an element's entire value, so it cannot target a specific
/// selection at all. Selecting `target` first via `select::select_text`, then typing or pasting
/// over that selection, is what makes a real correction possible: replace this specific span,
/// leave the rest of the document untouched.
///
/// Verification here checks that `target` is no longer present and `replacement` now is. That
/// is not a fully precise "did this exact span change" check: if `target` occurs more than
/// once, a correct replacement of the first occurrence still leaves later occurrences matching
/// `target`, which this check would misreport as a failure. Fine for this spike, where the
/// target text is a marker chosen to be unique; a real correction-application caller would need
/// range-identity tracking across the operation instead, which is out of scope here.
pub fn replace_span(
    element: &IUIAutomationElement,
    target: &str,
    replacement: &str,
) -> Result<InsertionMethod, NativeCaptureError> {
    select::select_text(element, target)?;
    sleep(SELECTION_SETTLE_DELAY);
    let before = current_text(element).unwrap_or_default();

    let synthetic_result = synthetic::type_text(replacement);
    sleep(SETTLE_DELAY);
    if synthetic_result.is_ok() && replaced(element, &before, target, replacement) {
        return Ok(InsertionMethod::SyntheticInput);
    }

    // The failed attempt may have left a partial edit or a stale selection.
    // `restore_after_failed_attempt` cleans up any fragment an interrupted `type_text` left
    // behind. When it finds and removes one, `target` was necessarily consumed by that attempt
    // (ordinary text-editing semantics: the first keystroke over a selection deletes it), so a
    // fresh `select_text` search for `target` would only fail, and the cursor the cleanup
    // leaves behind already sits exactly where `target` used to be, so skip re-selecting and
    // paste straight there. Otherwise, nothing needed cleaning up, so re-select `target` before
    // trying the next technique rather than assuming the prior selection still holds.
    if !restore_after_failed_attempt(element, &before) {
        select::select_text(element, target)?;
        sleep(SELECTION_SETTLE_DELAY);
    }
    let clipboard_result = clipboard::paste_text(replacement);
    sleep(SETTLE_DELAY);
    if clipboard_result.is_ok() && replaced(element, &before, target, replacement) {
        return Ok(InsertionMethod::ClipboardPaste);
    }

    clipboard_result?;
    synthetic_result?;
    Err(NativeCaptureError::InsertionUnverified)
}

/// Replaces the `typed_char_count` characters immediately before the cursor with
/// `replacement`, using pure keyboard-simulated selection (`synthetic::select_left`) rather
/// than `replace_span`'s accessibility-tree search (`select::select_text`'s `FindText`).
/// Exists because that search is unreliable against canvas-rendered editors: Google Docs
/// exposes a "side DOM" purely for accessibility that only partially backs UIA's text-search
/// and value-set operations, while real keyboard input reaches Google Docs' own event handlers
/// directly, the same path `synthetic::type_text` already relies on. Meant to be called
/// immediately after typing or pasting `typed_char_count` characters, so the selection covers
/// exactly what was just inserted.
///
/// `target` is what those `typed_char_count` characters are expected to read. This function's
/// whole premise is that the cursor is still sitting right after `target`, and nothing
/// guarantees that stays true: an unrelated edit elsewhere in the same document running in
/// between, even a different correction from the same batch, moves the real text cursor to
/// wherever that edit's replacement landed, and `select_left` then selects whatever happens to
/// be `typed_char_count` characters back from there instead. Without checking this, the
/// function corrupts silently rather than failing, since `changed_and_contains` only checks
/// that `replacement` landed somewhere, not that it landed over `target`. Verifying the
/// selection actually holds `target` before typing over it, and handing off to
/// `replace_last_typed_by_anchor` when it doesn't, turns that silent corruption into a safe,
/// correct recovery instead.
pub fn replace_last_typed(
    element: &IUIAutomationElement,
    target: &str,
    typed_char_count: usize,
    replacement: &str,
) -> Result<InsertionMethod, NativeCaptureError> {
    let before = current_text(element).unwrap_or_default();

    synthetic::select_left(typed_char_count as u32)?;
    sleep(SELECTION_SETTLE_DELAY);
    let selection_matches = current_selection(element)
        .map(|selected| selected == target)
        .unwrap_or(false);
    if !selection_matches {
        return replace_last_typed_by_anchor(element, &before, target, replacement);
    }
    let synthetic_result = synthetic::type_text(replacement);
    sleep(SETTLE_DELAY);
    if synthetic_result.is_ok() && changed_and_contains(element, &before, replacement) {
        return Ok(InsertionMethod::SyntheticInput);
    }

    // If a fragment needed cleaning up, the cursor it leaves behind already sits exactly where
    // the originally-selected `typed_char_count` characters used to be. A fresh
    // `select_left(typed_char_count)` from there would instead select characters further back,
    // past the intended span rather than the now-empty gap it left. Skip re-selecting in that
    // case and paste straight into the gap; otherwise, nothing needed cleaning up, so re-select
    // as before.
    if !restore_after_failed_attempt(element, &before) {
        synthetic::select_left(typed_char_count as u32)?;
        sleep(SELECTION_SETTLE_DELAY);
    }
    let clipboard_result = clipboard::paste_text(replacement);
    sleep(SETTLE_DELAY);
    if clipboard_result.is_ok() && changed_and_contains(element, &before, replacement) {
        return Ok(InsertionMethod::ClipboardPaste);
    }

    clipboard_result?;
    synthetic_result?;
    Err(NativeCaptureError::InsertionUnverified)
}

/// Byte width of the trailing context window `replace_last_typed_by_anchor` anchors on.
/// Generous enough to usually cover a whole just-typed clause, not just `target` itself: the
/// window needs to be long and specific enough that `FindText` cannot mistake it for an
/// unrelated occurrence elsewhere in the document, the way a bare word easily can.
const FALLBACK_ANCHOR_BYTES: usize = 64;

/// `replace_last_typed`'s recovery path once the cursor has moved off `target`. Deliberately
/// not `replace_span(element, target, replacement)`: that method's `select::select_text` does
/// an unanchored, whole-document `FindText` search for `target` alone, and a short, non-unique
/// `target` already present elsewhere in the document from unrelated typing lands the search
/// on that unrelated occurrence instead of the one just typed, silently correcting the wrong
/// span rather than failing safely. `before` is the document text as it stood at the very top
/// of `replace_last_typed`, right when this correction was requested, with `target` still
/// sitting at its very end, since nothing appends after a just-typed word before its own
/// correction is applied. A trailing window of `before` ending at `target` is therefore both
/// guaranteed present and, being much longer than `target` alone, far less likely to collide
/// with unrelated text elsewhere: the same anchor-over-bare-search reasoning `select_within`
/// already establishes for `replace_at`'s list-marker-drift problem, applied here to a
/// non-uniqueness problem instead of a counting-drift one.
///
/// Falls back to `replace_span` itself, inheriting its non-uniqueness risk, only in the
/// degenerate case where `before` does not actually end with `target`, meaning something
/// already changed the document's own tail since the original typing, and no trustworthy
/// anchor is available at all.
fn replace_last_typed_by_anchor(
    element: &IUIAutomationElement,
    before: &str,
    target: &str,
    replacement: &str,
) -> Result<InsertionMethod, NativeCaptureError> {
    if !before.ends_with(target) {
        return replace_span(element, target, replacement);
    }
    // Capped at `target`'s own start, never past it: otherwise, if `target` alone is longer
    // than `FALLBACK_ANCHOR_BYTES`, the plain byte-count window below would land inside
    // `target`, and the char-boundary search that follows, which only ever moves forward
    // hunting for the nearest boundary at or after its starting point, would then settle for a
    // boundary partway through `target` instead of `target`'s own start, making the anchor
    // shorter than `target` itself and underflowing `local_start` below.
    let window_start = before
        .len()
        .saturating_sub(FALLBACK_ANCHOR_BYTES)
        .min(before.len() - target.len());
    // SAFETY net, not memory safety: snaps inward to the nearest char boundary at or after
    // `window_start` rather than outward, so the anchor never starts mid-character; `find`
    // cannot fail to produce a boundary since `before.len()` is always one, and the range
    // always includes it.
    let anchor_start = (window_start..=before.len())
        .find(|&i| before.is_char_boundary(i))
        .unwrap_or(before.len());
    let anchor = &before[anchor_start..];
    let local_start = anchor.len() - target.len();
    let local_start_utf16 = anchor[..local_start].encode_utf16().count();
    let local_length_utf16 = target.encode_utf16().count();
    replace_within(
        element,
        anchor,
        local_start_utf16,
        local_length_utf16,
        replacement,
    )
}

/// Replaces the exact `[start, start + length)` character span with `replacement`, identified
/// by position via `select::select_range` rather than by content via `replace_span`'s
/// `select::select_text`. The robust choice whenever the caller already knows where the target
/// text lives, as a real correction-applying caller always would, from whatever diagnostic
/// flagged the span in the first place, since it has no search step to land on the wrong
/// occurrence, unlike `replace_span`, which can mismatch on non-unique or substring-contained
/// targets. `start` and `length` are UTF-16 code units; see `select::select_range`'s
/// documentation for why.
pub fn replace_at(
    element: &IUIAutomationElement,
    start: usize,
    length: usize,
    replacement: &str,
) -> Result<InsertionMethod, NativeCaptureError> {
    select::select_range(element, start, length)?;
    sleep(SELECTION_SETTLE_DELAY);
    let before = current_text(element).unwrap_or_default();

    let synthetic_result = synthetic::type_text(replacement);
    sleep(SETTLE_DELAY);
    if synthetic_result.is_ok() && changed_and_contains(element, &before, replacement) {
        return Ok(InsertionMethod::SyntheticInput);
    }

    // Same re-select-before-retry reasoning as replace_span, and the same
    // restore_after_failed_attempt cleanup: a prior attempt that actually landed a fragment
    // shifts the document, so re-selecting the same `[start, start + length)` is only correct
    // once that fragment has been cleaned up; when cleanup ran, the cursor it leaves behind
    // already sits where the fragment was.
    if !restore_after_failed_attempt(element, &before) {
        select::select_range(element, start, length)?;
        sleep(SELECTION_SETTLE_DELAY);
    }
    let clipboard_result = clipboard::paste_text(replacement);
    sleep(SETTLE_DELAY);
    if clipboard_result.is_ok() && changed_and_contains(element, &before, replacement) {
        return Ok(InsertionMethod::ClipboardPaste);
    }

    clipboard_result?;
    synthetic_result?;
    Err(NativeCaptureError::InsertionUnverified)
}

/// Replaces the exact `[local_start, local_start + local_length)` character span measured from
/// the start of the first occurrence of `anchor` with `replacement`, via
/// `select::select_within` rather than `replace_at`'s document-absolute `select::select_range`.
/// The robust choice whenever a suitable anchor is available. See `select_within`'s
/// documentation for why `replace_at`'s absolute counting drifts near Word's auto-numbered list
/// items, and why anchoring locally avoids the problem.
pub fn replace_within(
    element: &IUIAutomationElement,
    anchor: &str,
    local_start: usize,
    local_length: usize,
    replacement: &str,
) -> Result<InsertionMethod, NativeCaptureError> {
    select::select_within(element, anchor, local_start, local_length)?;
    sleep(SELECTION_SETTLE_DELAY);
    let before = current_text(element).unwrap_or_default();

    let synthetic_result = synthetic::type_text(replacement);
    sleep(SETTLE_DELAY);
    if synthetic_result.is_ok() && changed_and_contains(element, &before, replacement) {
        return Ok(InsertionMethod::SyntheticInput);
    }

    // Same re-select-before-retry reasoning as replace_span and replace_at, and the same
    // restore_after_failed_attempt cleanup. A landed fragment changes the characters inside
    // `anchor` itself, so a fresh `select_within` search for `anchor` would then, correctly,
    // fail to find it. Skip re-selecting in that case and paste straight into the gap the
    // cleanup leaves behind.
    if !restore_after_failed_attempt(element, &before) {
        select::select_within(element, anchor, local_start, local_length)?;
        sleep(SELECTION_SETTLE_DELAY);
    }
    let clipboard_result = clipboard::paste_text(replacement);
    sleep(SETTLE_DELAY);
    if clipboard_result.is_ok() && changed_and_contains(element, &before, replacement) {
        return Ok(InsertionMethod::ClipboardPaste);
    }

    clipboard_result?;
    synthetic_result?;
    Err(NativeCaptureError::InsertionUnverified)
}

/// Best-effort cleanup after a synthetic-input attempt whose verification failed: finds
/// whatever fragment actually landed and removes it, so the next cascade stage pastes into a
/// clean, known state instead of compounding a partial fragment left behind by a `SendInput`
/// call that reports full success while the target application processes only a truncated
/// prefix of the intended text. Diffs `before` against the element's current text via
/// `changed_region`, rather than assuming the fragment sits at the end, because a `replace_*`
/// caller's failed attempt types over a selection, so the change can land wherever that
/// selection was, not only at a cursor that started at the document's end.
///
/// Selects the fragment via `select::select_text`'s content search rather than
/// `select::select_range`'s document-absolute offset, even though `changed_region` already
/// hands back an exact byte position: converting that position to a `select_range` call
/// inherits the same `TextUnit_Character`-counting drift near auto-numbered list markers
/// already documented on `select::select_within`, which corrupts the cleanup itself rather than
/// skipping it. A freshly landed fragment is exactly the kind of long, distinctive text
/// `select_text` needs to be effectively unique, the same assumption `replace_span` already
/// relies on for its own, larger target strings.
///
/// Returns whether a fragment was actually found and removed. `false` means either nothing
/// changed (the attempt was a clean no-op, for example `SendInput` failed before any keystroke
/// landed) or the cleanup itself could not be carried out; either way, the caller's existing
/// content-search-based re-select before the clipboard stage is still safe to run unchanged.
/// `true` means the fragment that landed necessarily replaced whatever it was selected over
/// (ordinary text-editing semantics: the first keystroke over a selection deletes it), so that
/// original content is gone and a content search for it would now, correctly, fail to find it.
/// The caller should skip re-selecting and paste directly at the cursor this cleanup leaves
/// behind, which already sits exactly where the fragment was.
///
/// `pub`, not just an internal cascade helper, for the same reason as `current_text`: the
/// manual-verification harness needs to exercise this directly against a manufactured fragment,
/// since the real `SendInput` flake this exists to recover from is rare.
pub fn restore_after_failed_attempt(element: &IUIAutomationElement, before: &str) -> bool {
    let Ok(after) = current_text(element) else {
        return false;
    };
    if after == before {
        return false;
    }
    let Some((_, region)) = changed_region(before, &after) else {
        return false;
    };
    if region.is_empty() {
        return false;
    }
    if select::select_text(element, region).is_err() {
        return false;
    }
    sleep(SELECTION_SETTLE_DELAY);
    let deleted = synthetic::delete_selection().is_ok();
    sleep(SETTLE_DELAY);
    deleted
}

/// Finds the substring inserted, deleted, or changed between `before` and `after` by trimming
/// their longest common prefix and (non-overlapping) longest common suffix: whatever's left in
/// the middle is what changed, wherever in the document it happened. Typing at the end, typing
/// in the middle, and deleting a span all fall out of the same trim. Returns `None` if nothing
/// changed, or the changed region's start byte offset within `after` alongside the region
/// itself. Snaps to UTF-8 char boundaries before slicing, since a document routinely contains
/// multi-byte characters (curly quotes, accented letters) that a byte-index trim could
/// otherwise land inside.
pub fn changed_region<'a>(before: &str, after: &'a str) -> Option<(usize, &'a str)> {
    let before_bytes = before.as_bytes();
    let after_bytes = after.as_bytes();

    let common_prefix = before_bytes
        .iter()
        .zip(after_bytes.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let max_suffix = before_bytes.len().min(after_bytes.len()) - common_prefix;
    let common_suffix = before_bytes[common_prefix..]
        .iter()
        .rev()
        .zip(after_bytes[common_prefix..].iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();

    let start = common_prefix;
    let end = after_bytes.len().checked_sub(common_suffix)?;
    if start >= end {
        return None;
    }
    let start = (0..=start).rev().find(|&i| after.is_char_boundary(i))?;
    let end = (end..=after.len()).find(|&i| after.is_char_boundary(i))?;
    Some((start, &after[start..end]))
}

fn replaced(element: &IUIAutomationElement, before: &str, target: &str, replacement: &str) -> bool {
    current_text(element)
        .map(|now| now != before && now.contains(replacement) && !now.contains(target))
        .unwrap_or(false)
}

fn changed_and_contains(element: &IUIAutomationElement, before: &str, text: &str) -> bool {
    current_text(element)
        .map(|now| now != before && now.contains(text))
        .unwrap_or(false)
}

/// Reads an element's current text: `ValuePattern.CurrentValue()` where available (simple
/// controls), falling back to `TextPattern`'s full document range (rich text, browser fields).
/// `pub`, not just an internal cascade helper: the manual-verification harness needs the same
/// read-back to log whether each independently-tried stage's marker actually landed.
pub fn current_text(element: &IUIAutomationElement) -> Result<String, NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    if let Ok(value_pattern) =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
    {
        // SAFETY: `value_pattern` was just obtained from a live element.
        if let Ok(text) = unsafe { value_pattern.CurrentValue() } {
            return Ok(text.to_string());
        }
    }
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let text_pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(|_| NativeCaptureError::NoTextPattern)?;
    // SAFETY: `text_pattern` was just obtained from a live element.
    let range = unsafe { text_pattern.DocumentRange() }?;
    // SAFETY: `range` is a live range from the call above; -1 requests the full range's text.
    Ok(unsafe { range.GetText(-1) }?.to_string())
}

/// Reads the live selection's own text straight from UI Automation (`TextPattern.GetSelection`)
/// rather than inferring what got selected from a before/after document diff, the same
/// trustworthy read-back principle `current_text` already applies to the whole document,
/// applied to the selection specifically. Empty string if nothing is selected: a degenerate,
/// zero-length selection is a valid UI Automation state, not an error condition here.
///
/// `pub`, not just an internal cascade helper, for the same reason as `current_text`: the
/// manual-verification harness needs the same read-back to log what a selection call actually
/// produced, not just whether the call itself returned an error.
pub fn current_selection(element: &IUIAutomationElement) -> Result<String, NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let text_pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(|_| NativeCaptureError::NoTextPattern)?;
    // SAFETY: `text_pattern` was just obtained from a live element.
    let selection = unsafe { text_pattern.GetSelection() }?;
    // SAFETY: `selection` is the live array just returned above.
    let length = unsafe { selection.Length() }?;
    if length == 0 {
        return Ok(String::new());
    }
    // SAFETY: `selection` is still the same live array; index 0 is valid since Length() > 0.
    let range = unsafe { selection.GetElement(0) }?;
    // SAFETY: `range` is the live range just returned above.
    Ok(unsafe { range.GetText(-1) }?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_returns_none() {
        assert_eq!(changed_region("hello world", "hello world"), None);
    }

    #[test]
    fn append_at_end_is_the_whole_appended_suffix() {
        assert_eq!(changed_region("hello", "hello world"), Some((5, " world")));
    }

    #[test]
    fn insertion_in_the_middle_is_isolated_from_the_unchanged_prefix_and_suffix() {
        assert_eq!(
            changed_region("the cat sat", "the black cat sat"),
            Some((4, "black "))
        );
    }

    #[test]
    fn replacement_of_a_span_is_narrowed_to_just_the_differing_core() {
        // Both strings share a "-3 tail" suffix; the region should exclude it rather than
        // reporting the whole replacement string.
        assert_eq!(
            changed_region("WA-CLIP-3 tail", "WA-REPLACED-3 tail"),
            Some((3, "REPLACED"))
        );
    }

    #[test]
    fn pure_deletion_with_nothing_inserted_returns_none() {
        // Nothing new exists in `after` to point at. `restore_after_failed_attempt` only needs
        // to select and remove content an interrupted attempt actually added, and a
        // `type_text` call over a selection cannot delete without also inserting at least the
        // first landed character, so this case does not arise from that call site. Documented
        // here as this function's behaviour on a pure deletion, unused by that caller.
        assert_eq!(changed_region("hello world", "hello "), None);
    }

    #[test]
    fn does_not_split_a_multi_byte_character_at_the_boundary() {
        // "café" vs "cafés": the changed byte lands inside "é" if the trim isn't snapped to a
        // char boundary, since "é" and the appended "s" share no bytes but sit adjacent to the
        // common prefix.
        let (start, region) = changed_region("café", "cafés").expect("a change was made");
        assert!("café".is_char_boundary(start));
        assert_eq!(region, "s");
    }
}
