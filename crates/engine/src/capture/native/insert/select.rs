//! Locates and selects a specific substring within an element's text via `TextPattern`, so a
//! later insertion stage overwrites only that span rather than the whole field
//! (`value::set_value`'s behaviour) or wherever the cursor happens to be
//! (`synthetic::type_text`/`clipboard::paste_text`'s behaviour when nothing is selected).
//! Typing or pasting over an active selection replaces it, which is the mechanism a real
//! correction needs: replace this specific misspelled word, leave the rest of the document
//! untouched.

use windows::core::BSTR;
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationTextRange,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
    UIA_TextPatternId,
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
/// the start of the occurrence of `anchor` that `occurrence` names, found by the rule
/// [`find_within_range`] states, rather than `select_range`'s absolute offset from the
/// document's very start. The more robust choice whenever a suitable anchor is
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
/// `local_start` and `local_length` are UTF-16 code units measured from `anchor`'s own start,
/// not the document's. See `select_range`'s documentation for why UTF-16, not Rust `char`s or
/// UTF-8 bytes.
pub fn select_within(
    element: &IUIAutomationElement,
    anchor: &str,
    occurrence: usize,
    local_start: usize,
    local_length: usize,
) -> Result<(), NativeCaptureError> {
    let range = find_within_range(element, anchor, occurrence, local_start, local_length)?;
    // SAFETY: `range` is the live range `find_within_range` just returned, now covering exactly
    // `[local_start, local_start + local_length)` relative to `anchor`'s start.
    unsafe { range.Select() }?;
    Ok(())
}

/// The same `[local_start, local_start + local_length)` range `select_within` selects, without
/// selecting it: `span_rect` (`cursor::span_rects`) needs a span's on-screen rectangles to
/// underline it, on every tick of the overlay's position refresh, and calling `Select` there
/// would repeatedly steal the user's own selection out from under them just to compute where to
/// draw an underline. `select_within` itself now delegates here, so the two never drift apart.
///
/// `anchor` is found at the occurrence `occurrence` names, counting from 0, by the rule on
/// [`crate::flag::Span::occurrence`], which the analyzer counts the index by: `FindText` runs on
/// the document range for the first occurrence, then, for each later one, on a range from the
/// end of the previous match to the document's end, case-sensitively each time, so occurrences
/// that overlap count once. When the document holds no occurrence at that index, as a document
/// edited since its check can, the answer is [`NativeCaptureError::TextNotFound`], the same as
/// for an absent anchor, never a range on another occurrence.
///
/// The analyzer counts the occurrences in the text before the anchor's sentence. An occurrence
/// that starts before the sentence and runs into it is found by this search and was not counted,
/// so the index would resolve to it and the sentence's own occurrence would be skipped. For a
/// whole-sentence anchor, which every checking source builds, the case is unreachable: that
/// occurrence would hold a sentence break inside the anchor's text, which happens only where the
/// splitter left two sentences as one.
pub fn find_within_range(
    element: &IUIAutomationElement,
    anchor: &str,
    occurrence: usize,
    local_start: usize,
    local_length: usize,
) -> Result<IUIAutomationTextRange, NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let text_pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(|_| NativeCaptureError::NoTextPattern)?;
    // SAFETY: `text_pattern` was just obtained from a live element.
    let document = unsafe { text_pattern.DocumentRange() }?;
    let range = find_occurrence(&document, anchor, occurrence)?;
    // SAFETY: `range` is the live range the last FindText returned; collapsing its own end to its
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
    Ok(range)
}

/// A range of an element's text that [`find_occurrence`] searches: a live
/// `IUIAutomationTextRange` in the backend, and a slice of a string in this module's tests, so
/// the search sequence is tested without a live element.
trait SearchRange: Sized {
    /// The first occurrence of `text` within this range, matched case-sensitively, or
    /// [`NativeCaptureError::TextNotFound`].
    fn find(&self, text: &str) -> Result<Self, NativeCaptureError>;

    /// The part of this range from `found`'s end to this range's end.
    fn after(&self, found: &Self) -> Result<Self, NativeCaptureError>;
}

impl SearchRange for IUIAutomationTextRange {
    fn find(&self, text: &str) -> Result<Self, NativeCaptureError> {
        // SAFETY: `self` is a live range; see `select_text`'s SAFETY comment for why any FindText
        // error maps to TextNotFound. The third argument, `ignoreCase`, is false, so the match is
        // case-sensitive, as the analyzer counts.
        unsafe { self.FindText(&BSTR::from(text), false, false) }
            .map_err(|_| NativeCaptureError::TextNotFound)
    }

    fn after(&self, found: &Self) -> Result<Self, NativeCaptureError> {
        // SAFETY: `self` is a live range. UI Automation's `Clone` makes a new range object, unlike
        // Rust's `clone`, which would share this one, so moving the copy's start leaves `self` as
        // it was.
        let rest = unsafe { self.Clone() }?;
        // SAFETY: `rest` is the live copy just made, and `found` a live range FindText returned
        // from within `self`.
        unsafe {
            rest.MoveEndpointByRange(
                TextPatternRangeEndpoint_Start,
                found,
                TextPatternRangeEndpoint_End,
            )
        }?;
        Ok(rest)
    }
}

/// The occurrence of `anchor` within `document` that `occurrence` names, by the rule
/// [`find_within_range`] states.
fn find_occurrence<R: SearchRange>(
    document: &R,
    anchor: &str,
    occurrence: usize,
) -> Result<R, NativeCaptureError> {
    let mut found = document.find(anchor)?;
    for _ in 0..occurrence {
        found = document.after(&found)?.find(anchor)?;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A byte range of `text`, searched with `str::find`, which matches case-sensitively as
    /// `FindText` does with `ignoreCase` false.
    struct TextSlice<'a> {
        text: &'a str,
        start: usize,
        end: usize,
    }

    impl<'a> TextSlice<'a> {
        fn document(text: &'a str) -> Self {
            Self {
                text,
                start: 0,
                end: text.len(),
            }
        }
    }

    impl SearchRange for TextSlice<'_> {
        fn find(&self, target: &str) -> Result<Self, NativeCaptureError> {
            let at = self.text[self.start..self.end]
                .find(target)
                .ok_or(NativeCaptureError::TextNotFound)?;
            Ok(Self {
                text: self.text,
                start: self.start + at,
                end: self.start + at + target.len(),
            })
        }

        fn after(&self, found: &Self) -> Result<Self, NativeCaptureError> {
            Ok(Self {
                text: self.text,
                start: found.end,
                end: self.end,
            })
        }
    }

    /// Where the search for `anchor`'s occurrence at `occurrence` lands in `text`, or `None` when
    /// it answers text-not-found.
    fn start_of(text: &str, anchor: &str, occurrence: usize) -> Option<usize> {
        match find_occurrence(&TextSlice::document(text), anchor, occurrence) {
            Ok(found) => Some(found.start),
            Err(NativeCaptureError::TextNotFound) => None,
            Err(error) => panic!("only text-not-found is expected: {error}"),
        }
    }

    const TWO_SENTENCES: &str = "An eror here. An eror here.";
    const SENTENCE: &str = "An eror here.";

    #[test]
    fn two_identical_sentences_resolve_to_their_own_positions() {
        assert_eq!(start_of(TWO_SENTENCES, SENTENCE, 0), Some(0));
        assert_eq!(start_of(TWO_SENTENCES, SENTENCE, 1), Some(14));
    }

    #[test]
    fn an_index_past_the_last_occurrence_is_text_not_found() {
        assert_eq!(start_of(TWO_SENTENCES, SENTENCE, 2), None);
    }

    #[test]
    fn an_absent_anchor_is_text_not_found() {
        assert_eq!(start_of(TWO_SENTENCES, "No eror here.", 0), None);
    }

    #[test]
    fn an_occurrence_inside_a_longer_sentence_counts() {
        let text = "He said An eror here. twice. An eror here.";
        assert_eq!(start_of(text, SENTENCE, 0), Some(8));
        assert_eq!(start_of(text, SENTENCE, 1), Some(29));
    }

    #[test]
    fn a_search_resumes_at_the_previous_match_end_so_an_overlapping_occurrence_is_skipped() {
        // "Go. Go." occurs at 0 and again at 4, overlapping the first; the second search starts
        // at 7, where only "Go." is left.
        assert_eq!(start_of("Go. Go. Go.", "Go. Go.", 0), Some(0));
        assert_eq!(start_of("Go. Go. Go.", "Go. Go.", 1), None);
    }

    #[test]
    fn every_index_the_analyzer_counts_resolves_to_the_occurrence_it_was_counted_at() {
        // The analyzer sets the index to the number of `str::matches` of the anchor in the text
        // before it, and `match_indices` yields exactly the occurrences that count leaves
        // reachable.
        let cases = [
            (TWO_SENTENCES, SENTENCE),
            (
                "An eror here. Then more. An eror here. An eror here.",
                SENTENCE,
            ),
            ("Go. Go. Go. Go. Go.", "Go. Go."),
            ("aaaaa", "aa"),
        ];
        for (text, anchor) in cases {
            for (position, _) in text.match_indices(anchor) {
                let occurrence = text[..position].matches(anchor).count();
                assert_eq!(
                    start_of(text, anchor, occurrence),
                    Some(position),
                    "{anchor:?} at {position} of {text:?}"
                );
            }
        }
    }
}
