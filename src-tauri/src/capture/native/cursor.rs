//! Caret bounding-rectangle retrieval via TextPattern, split into the unsafe UIA call and a
//! pure conversion any test can exercise without a live accessibility tree.

use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationTextRange,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
    UIA_TextPatternId,
};

use super::error::NativeCaptureError;

/// The `Capture` trait's shared type, not a native-specific one: reconciled here rather than
/// kept as a duplicate now that #19 gives the whole capture module one `CursorRect` to agree
/// on.
pub use crate::capture::CursorRect;

/// Reads the caret's screen rectangle for `element`, or the specific reason it couldn't.
pub fn caret_rect(element: &IUIAutomationElement) -> Result<CursorRect, NativeCaptureError> {
    // SAFETY: `element` is a live element from a focus-changed callback; GetCurrentPatternAs
    // fails safely (Err) when the pattern is unsupported, mapped to NoTextPattern below.
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
            .map_err(|_| NativeCaptureError::NoTextPattern)?;

    // SAFETY: `pattern` was just obtained from a live element.
    let selection = unsafe { pattern.GetSelection() }?;
    // SAFETY: `selection` is a live text-range array from the call above.
    let caret = unsafe { selection.GetElement(0) }.map_err(|_| NativeCaptureError::NoCaret)?;
    if !is_degenerate(&caret)? {
        return Err(NativeCaptureError::SelectionNotCaret);
    }
    // SAFETY: `caret` is a live range from the call above.
    unsafe { caret.ExpandToEnclosingUnit(TextUnit_Character) }?;
    // SAFETY: `caret` is a live, expanded range; the returned SAFEARRAY is drained and
    // destroyed exactly once by `drain_f64_safearray` below, which takes ownership.
    let array = unsafe { caret.GetBoundingRectangles() }?;
    // SAFETY: `array` was just returned by GetBoundingRectangles above, not read elsewhere.
    let floats = unsafe { drain_f64_safearray(array) };
    let rect = rect_from_floats(&floats).ok_or(NativeCaptureError::NoCaret)?;
    if !looks_like_caret(&rect) {
        return Err(NativeCaptureError::ImplausibleCaretShape {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        });
    }
    Ok(rect)
}

/// Whether `range` is a caret rather than a span of selected text. A caret is *degenerate*: its
/// start and end resolve to the same point, which is what `CompareEndpoints` reporting zero
/// means.
///
/// `GetSelection` answers with a non-degenerate range whenever text is actually selected, and
/// some documents hand one back covering their whole body when there is no caret at all.
/// Expanding either to a character unit yields the first character or embedded object inside
/// it, whose rectangle bears no relation to a caret: in manual verification a browser reported
/// an entire 1912x914 viewport this way, and a video player element 141x139, each of which
/// threw the overlay to a meaningless part of the screen. Tested before expanding, because
/// expansion destroys the very property being tested.
fn is_degenerate(range: &IUIAutomationTextRange) -> Result<bool, NativeCaptureError> {
    // SAFETY: `range` is live; comparing a range's own start endpoint against its own end
    // endpoint reads only that range and creates nothing needing release.
    let spread = unsafe {
        range.CompareEndpoints(
            TextPatternRangeEndpoint_Start,
            range,
            TextPatternRangeEndpoint_End,
        )
    }?;
    Ok(spread == 0)
}

/// # Safety
/// `array` must be a `SAFEARRAY` of `f64` owned by the caller (as `GetBoundingRectangles`
/// returns, or a null pointer for "no rectangles"); this function takes ownership and destroys
/// it.
unsafe fn drain_f64_safearray(array: *mut SAFEARRAY) -> Vec<f64> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: forwarded from this function's contract; each element is read by index within
    // the array's own reported bounds, and the array is destroyed exactly once before return.
    unsafe {
        let mut out = Vec::new();
        if let (Ok(lower), Ok(upper)) = (SafeArrayGetLBound(array, 1), SafeArrayGetUBound(array, 1))
        {
            for index in lower..=upper {
                let mut value: f64 = 0.0;
                if SafeArrayGetElement(array, &raw const index, (&raw mut value).cast()).is_ok() {
                    out.push(value);
                }
            }
        }
        let _ = SafeArrayDestroy(array);
        out
    }
}

/// A genuine caret's bounding rectangle, in manual verification against real typing, took one
/// of two shapes: a thin bar (a pixel or so wide against a line-height-tall rectangle, in either
/// orientation, since a caret at certain layout moments reports its width and height swapped),
/// or a small rectangle collapsed close to a point (as small as 1x1) whose position still
/// advanced correctly with every keystroke. `#28` found a third shape that is neither: VS Code's
/// chat input, when this app's own UI Automation client presence switches Monaco into its
/// screen-reader-optimized rendering, exposes an accessible element (`messageInput_cKsPxg`) that
/// answers `GetBoundingRectangles` with a fixed 20x20 square, unmoving no matter what is typed,
/// instead of the caret's real position. A rectangle that is neither a thin bar nor collapsed to
/// near-nothing, a mid-sized square such as that one, is what gets rejected; both genuine shapes
/// pass.
const MAX_CARET_THIN_TO_LONG_RATIO: f64 = 0.5;
const MAX_CARET_COLLAPSED_SIZE: f64 = 3.0;

fn looks_like_caret(rect: &CursorRect) -> bool {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return false;
    }
    let (thin, long) = if rect.width <= rect.height {
        (rect.width, rect.height)
    } else {
        (rect.height, rect.width)
    };
    thin <= long * MAX_CARET_THIN_TO_LONG_RATIO || long <= MAX_CARET_COLLAPSED_SIZE
}

/// UIA reports bounding rectangles as flat `[x, y, width, height]` groups, one group per
/// visible line the range spans (a caret range is zero-width, so this is normally one group,
/// but a caret at a wrapped line boundary can report two). The first group is what overlay
/// placement needs.
fn rect_from_floats(floats: &[f64]) -> Option<CursorRect> {
    let [x, y, width, height] = floats.get(0..4)?.try_into().ok()?;
    Some(CursorRect {
        x,
        y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
    use windows::Win32::System::Variant::VT_R8;

    use super::*;

    // Builds a real in-memory SAFEARRAY of f64 via plain Win32 COM calls, no accessibility
    // tree needed, so `drain_f64_safearray`'s bounds handling and partial-failure behaviour
    // are exercised for real, not approximated.
    fn make_f64_safearray(values: &[f64]) -> *mut SAFEARRAY {
        // SAFETY: SafeArrayCreateVector allocates a fresh array of the requested length;
        // SafeArrayPutElement writes each index within those bounds. The array is handed to
        // the caller, who owns it from here (matching drain_f64_safearray's contract).
        unsafe {
            let array = SafeArrayCreateVector(VT_R8, 0, values.len() as u32);
            for (index, &value) in values.iter().enumerate() {
                let idx = i32::try_from(index).expect("test arrays stay well under i32::MAX");
                let cell = value;
                let _ = SafeArrayPutElement(array, &raw const idx, (&raw const cell).cast());
            }
            array
        }
    }

    #[test]
    fn null_array_drains_to_empty() {
        // SAFETY: a null pointer is drain_f64_safearray's documented "no rectangles" case.
        assert_eq!(
            unsafe { drain_f64_safearray(std::ptr::null_mut()) },
            Vec::<f64>::new()
        );
    }

    #[test]
    fn well_formed_array_drains_in_order() {
        let array = make_f64_safearray(&[10.0, 20.0, 30.0, 40.0]);
        // SAFETY: `array` was just built by make_f64_safearray, not read elsewhere.
        assert_eq!(
            unsafe { drain_f64_safearray(array) },
            vec![10.0, 20.0, 30.0, 40.0]
        );
    }

    #[test]
    fn empty_array_drains_to_empty() {
        let array = make_f64_safearray(&[]);
        // SAFETY: `array` was just built by make_f64_safearray, not read elsewhere.
        assert_eq!(unsafe { drain_f64_safearray(array) }, Vec::<f64>::new());
    }

    #[test]
    fn empty_input_has_no_rect() {
        assert_eq!(rect_from_floats(&[]), None);
    }

    #[test]
    fn short_input_has_no_rect() {
        assert_eq!(rect_from_floats(&[1.0, 2.0, 3.0]), None);
    }

    #[test]
    fn first_group_becomes_the_rect() {
        let floats = [10.0, 20.0, 30.0, 40.0, 999.0, 999.0, 999.0, 999.0];
        assert_eq!(
            rect_from_floats(&floats),
            Some(CursorRect {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0
            })
        );
    }

    fn rect(width: f64, height: f64) -> CursorRect {
        CursorRect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        }
    }

    #[test]
    fn a_thin_bar_looks_like_a_caret() {
        // The exact values #28's manual verification read from a real, per-character-advancing
        // caret: 1 wide against 18, 22, 24, or 32 tall, and 11 wide against 32 tall.
        assert!(looks_like_caret(&rect(1.0, 18.0)));
        assert!(looks_like_caret(&rect(1.0, 22.0)));
        assert!(looks_like_caret(&rect(1.0, 24.0)));
        assert!(looks_like_caret(&rect(11.0, 32.0)));
    }

    #[test]
    fn a_thin_bar_with_width_and_height_swapped_still_looks_like_a_caret() {
        // #28's manual verification read the same real, correctly-positioned caret as both
        // 1x24 and, moments later at the same position, 24x1: UIA sometimes reports a caret's
        // bounding rectangle with its dimensions transposed.
        assert!(looks_like_caret(&rect(24.0, 1.0)));
    }

    #[test]
    fn a_collapsed_near_point_rect_still_looks_like_a_caret() {
        // #28's manual verification read a real caret advancing correctly, character by
        // character, while its reported rectangle stayed collapsed to 1x1 throughout.
        assert!(looks_like_caret(&rect(1.0, 1.0)));
    }

    #[test]
    fn the_fixed_20x20_square_from_28_does_not() {
        // Neither a thin bar nor collapsed to near-nothing: the fabricated rectangle VS Code's
        // chat input reports, fixed regardless of typing, that this check exists to catch.
        assert!(!looks_like_caret(&rect(20.0, 20.0)));
    }

    #[test]
    fn a_zero_size_rect_does_not() {
        assert!(!looks_like_caret(&rect(0.0, 0.0)));
    }

    #[test]
    fn exactly_half_as_wide_as_tall_still_counts() {
        assert!(looks_like_caret(&rect(10.0, 20.0)));
    }

    #[test]
    fn just_over_the_collapsed_size_threshold_and_not_thin_enough_does_not() {
        assert!(!looks_like_caret(&rect(4.0, 4.0)));
    }
}
