//! Locates and selects a specific substring within an element's text via `TextPattern`, so a
//! later insertion stage overwrites only that span rather than the whole field
//! (`value::set_value`'s behaviour) or wherever the cursor happens to be
//! (`synthetic::type_text`/`clipboard::paste_text`'s behaviour when nothing is selected).
//! Typing or pasting over an active selection replaces it, which is the mechanism a real
//! correction needs: replace this specific misspelled word, leave the rest of the document
//! untouched.

use windows::core::BSTR;
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationTextPattern, TextPatternRangeEndpoint_End,
    TextPatternRangeEndpoint_Start, TextUnit_Character, UIA_TextPatternId,
};

use super::super::error::NativeCaptureError;

/// Finds the first occurrence of `target` within `element`'s full text and makes it the active
/// selection.
pub fn select_text(element: &IUIAutomationElement, target: &str) -> Result<(), NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let text_pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(|_| NativeCaptureError::NoTextPattern)?;
    // SAFETY: `text_pattern` was just obtained from a live element.
    let document = unsafe { text_pattern.DocumentRange() }?;
    // SAFETY: `document` is a live range from the call above. A provider reports "not found" as
    // a genuine error, not a null range wrapped in success, so mapping any error here to
    // TextNotFound is correct.
    let found = unsafe { document.FindText(&BSTR::from(target), false, false) }
        .map_err(|_| NativeCaptureError::TextNotFound)?;
    // SAFETY: `found` is the live range FindText just returned.
    unsafe { found.Select() }?;
    Ok(())
}

/// Selects the exact `[start, start + length)` character span of `element`'s document text,
/// identified by position rather than content. The robust alternative to `select_text`:
/// `FindText`'s unanchored, whole-document substring search can land on the wrong occurrence,
/// or even match inside an unrelated word ("am" inside "dynamic"), whenever the target text is
/// not unique, and no real document of any length can guarantee that. Selecting by position
/// instead has no search step to land on the wrong span in the first place.
///
/// Collapsing `DocumentRange`'s end to its start, then moving both endpoints forward by `start`
/// character units, then expanding the end endpoint by `length` more, is UI Automation's
/// standard pattern for constructing a range at an exact offset; there is no direct
/// "range from offset" constructor in the API.
///
/// `start` and `length` are UTF-16 code units, what `TextUnit_Character` counts by and what
/// `synthetic::type_text` already encodes text as, not Rust `char`s or UTF-8 bytes. A caller
/// holding a Rust `&str` offset must convert via `encode_utf16().count()`, not `.chars().count()`
/// or a byte index.
pub fn select_range(
    element: &IUIAutomationElement,
    start: usize,
    length: usize,
) -> Result<(), NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let text_pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(|_| NativeCaptureError::NoTextPattern)?;
    // SAFETY: `text_pattern` was just obtained from a live element.
    let range = unsafe { text_pattern.DocumentRange() }?;
    // SAFETY: `range` is a live range from the call above; collapsing its own end to its own
    // start is the documented UI Automation pattern for degenerating a range to a single point.
    unsafe {
        range.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &range,
            TextPatternRangeEndpoint_Start,
        )
    }?;
    // SAFETY: `range` is still the same live, now-degenerate range.
    unsafe { range.Move(TextUnit_Character, start as i32) }?;
    // SAFETY: `range` is still the same live range, now positioned at `start`.
    unsafe {
        range.MoveEndpointByUnit(
            TextPatternRangeEndpoint_End,
            TextUnit_Character,
            length as i32,
        )
    }?;
    // SAFETY: `range` now covers exactly `[start, start + length)`.
    unsafe { range.Select() }?;
    Ok(())
}

/// Selects the exact `[local_start, local_start + local_length)` character span measured from
/// the start of the first occurrence of `anchor`, rather than `select_range`'s absolute offset
/// from the document's very start. The more robust choice whenever a suitable anchor is
/// available, which a real caller almost always has, since the sentence or paragraph a
/// correction was found in is exactly what the diagnostic that flagged it already captured.
///
/// `select_range`'s absolute counting drifts near auto-numbered list items: `TextUnit_Character`
/// based `Move()` does not advance through a list marker the same number of units `GetText()`'s
/// string output counts it as, and that mismatch accumulates across every marker between the
/// document start and the target. Anchoring on a short local move from `anchor`'s own start,
/// instead of a long one from the document's start, avoids the problem: as long as `anchor` and
/// the target span sit within the same structural region (the same list item or paragraph), the
/// move never crosses a boundary that could be miscounted.
///
/// `anchor` still goes through `FindText` under the hood, so it inherits that method's
/// first-occurrence behaviour; it must be specific enough to be effectively unique, a full
/// sentence or paragraph, not a single word, unlike `select_text`'s target, which is exactly the
/// ambiguity this function exists to avoid for anything shorter.
///
/// `local_start` and `local_length` are UTF-16 code units measured from `anchor`'s own start,
/// not the document's. See `select_range`'s documentation for why UTF-16, not Rust `char`s or
/// UTF-8 bytes.
pub fn select_within(
    element: &IUIAutomationElement,
    anchor: &str,
    local_start: usize,
    local_length: usize,
) -> Result<(), NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let text_pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(|_| NativeCaptureError::NoTextPattern)?;
    // SAFETY: `text_pattern` was just obtained from a live element.
    let document = unsafe { text_pattern.DocumentRange() }?;
    // SAFETY: `document` is a live range from the call above; see `select_text`'s SAFETY comment
    // for why any FindText error maps to TextNotFound.
    let range = unsafe { document.FindText(&BSTR::from(anchor), false, false) }
        .map_err(|_| NativeCaptureError::TextNotFound)?;
    // SAFETY: `range` is the live range FindText just returned; collapsing its own end to its
    // own start is the documented UI Automation pattern for degenerating a range to a point.
    unsafe {
        range.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &range,
            TextPatternRangeEndpoint_Start,
        )
    }?;
    // SAFETY: `range` is still the same live, now-degenerate range, positioned at `anchor`'s
    // start rather than the document's start.
    unsafe { range.Move(TextUnit_Character, local_start as i32) }?;
    // SAFETY: `range` is still the same live range, now positioned at `anchor`'s start plus
    // `local_start`.
    unsafe {
        range.MoveEndpointByUnit(
            TextPatternRangeEndpoint_End,
            TextUnit_Character,
            local_length as i32,
        )
    }?;
    // SAFETY: `range` now covers exactly `[local_start, local_start + local_length)` relative to
    // `anchor`'s start.
    unsafe { range.Select() }?;
    Ok(())
}
