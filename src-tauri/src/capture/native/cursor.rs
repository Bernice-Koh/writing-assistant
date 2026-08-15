//! Caret bounding-rectangle retrieval via TextPattern, split into the unsafe UIA call and a
//! pure conversion any test can exercise without a live accessibility tree.

use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationTextPattern, TextUnit_Character, UIA_TextPatternId,
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
    // SAFETY: `caret` is a live range from the call above.
    unsafe { caret.ExpandToEnclosingUnit(TextUnit_Character) }?;
    // SAFETY: `caret` is a live, expanded range; the returned SAFEARRAY is drained and
    // destroyed exactly once by `drain_f64_safearray` below, which takes ownership.
    let array = unsafe { caret.GetBoundingRectangles() }?;
    // SAFETY: `array` was just returned by GetBoundingRectangles above, not read elsewhere.
    let floats = unsafe { drain_f64_safearray(array) };
    rect_from_floats(&floats).ok_or(NativeCaptureError::NoCaret)
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
}
