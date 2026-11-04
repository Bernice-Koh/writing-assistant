//! On-screen rectangles through UI Automation and Win32: the document view's, and every line of a
//! text range's, split into the unsafe calls and pure conversions any test can exercise without a
//! live accessibility tree.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::UI::Accessibility::{IUIAutomationElement, IUIAutomationTextRange};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use super::error::NativeCaptureError;
use super::foreground;
use crate::capture::Foreground;

/// The `Capture` trait's shared type, not a native-specific one: reconciled here rather than
/// kept as a duplicate now that #19 gives the whole capture module one `CursorRect` to agree
/// on.
pub use crate::capture::CursorRect;

/// The on-screen rectangle of `element`'s own bounds, or the foreground window's when the
/// element cannot usefully answer for itself, for sizing and positioning the full-viewport
/// overlay. Tried in that order: `element`'s own `BoundingRectangle` is the more precise answer
/// when it is usable, since it is Word's actual document pane rather than the whole application
/// window ribbon and all, but some accessibility implementations report it as an empty rectangle
/// (no width or height) for elements that do not carry a meaningful bounds of their own, which is
/// what the fallback exists for.
///
/// `element` is whichever element holds the capture thread's text-change scope, and
/// `document_process` the id of the process that owns it. The thread keeps that scope while one
/// of the app's own windows has focus, and the foreground window is then the app's, so the
/// fallback asks `belongs_to_this_app` about the foreground window's process and refuses a window
/// of this app's (`fallback_rect`) rather than place the overlay over it.
pub fn document_view_rect(
    element: &IUIAutomationElement,
    document_process: u32,
    belongs_to_this_app: impl FnOnce(u32) -> bool,
) -> Result<CursorRect, NativeCaptureError> {
    if let Some(rect) = element_bounding_rect(element) {
        return Ok(rect);
    }
    let window = foreground::window().ok_or(NativeCaptureError::NoForegroundWindow)?;
    let owner = foreground::classify(
        Some(window.process_id),
        document_process,
        belongs_to_this_app,
    );
    fallback_rect(owner, || window_rect(window.hwnd))
}

/// The document view's fallback: the foreground window's rectangle, read by `read_window` only
/// when that window is not one of this app's. A window of this app's in front answers
/// [`NativeCaptureError::ForegroundIsThisApp`], so the overlay keeps the rectangle it last placed
/// itself at instead of moving onto the app's own window.
fn fallback_rect(
    owner: Foreground,
    read_window: impl FnOnce() -> Result<CursorRect, NativeCaptureError>,
) -> Result<CursorRect, NativeCaptureError> {
    match owner {
        Foreground::ThisApp => Err(NativeCaptureError::ForegroundIsThisApp),
        Foreground::Document | Foreground::Other => read_window(),
    }
}

/// `element`'s own `BoundingRectangle`, or `None` when the call fails or reports an empty
/// rectangle: either way, not a usable answer for [`document_view_rect`], which falls back to the
/// foreground window instead of handing the overlay a zero-sized target.
fn element_bounding_rect(element: &IUIAutomationElement) -> Option<CursorRect> {
    // SAFETY: `element` is live; CurrentBoundingRectangle fails safely (Err) when the property
    // is unavailable rather than trapping.
    let rect = unsafe { element.CurrentBoundingRectangle() }.ok()?;
    let converted = rect_from_win32(rect);
    (converted.width > 0.0 && converted.height > 0.0).then_some(converted)
}

/// `hwnd`'s own rectangle, via plain Win32 rather than a UI Automation tree walk.
fn window_rect(hwnd: HWND) -> Result<CursorRect, NativeCaptureError> {
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is a window handle just read from GetForegroundWindow; a window destroyed
    // since fails safely (Err). `rect` is a valid, uniquely-owned out parameter for the duration
    // of this call.
    unsafe { GetWindowRect(hwnd, &raw mut rect) }?;
    Ok(rect_from_win32(rect))
}

fn rect_from_win32(rect: RECT) -> CursorRect {
    CursorRect {
        x: f64::from(rect.left),
        y: f64::from(rect.top),
        width: f64::from(rect.right - rect.left),
        height: f64::from(rect.bottom - rect.top),
    }
}

/// Every on-screen bounding rectangle `range` produces, for underlining a flagged span in place:
/// one per visible line the range spans, as `rects_from_floats` groups them.
pub fn span_rects(range: &IUIAutomationTextRange) -> Result<Vec<CursorRect>, NativeCaptureError> {
    // SAFETY: `range` is a live range from `select::find_within_range`; the returned SAFEARRAY
    // is drained and destroyed exactly once by `drain_f64_safearray` below, which takes
    // ownership.
    let array = unsafe { range.GetBoundingRectangles() }?;
    // SAFETY: `array` was just returned by GetBoundingRectangles above, not read elsewhere.
    let floats = unsafe { drain_f64_safearray(array) };
    Ok(rects_from_floats(&floats))
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

/// UIA reports bounding rectangles as flat `[x, y, width, height]` groups, one group per visible
/// line the range spans, so a flagged span that wraps a line break comes back as more than one
/// group. Every group `floats` holds, in order; a trailing partial group is dropped.
fn rects_from_floats(floats: &[f64]) -> Vec<CursorRect> {
    floats
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[x, y, width, height]| CursorRect {
            x,
            y,
            width,
            height,
        })
        .collect()
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
    fn empty_input_has_no_groups() {
        assert_eq!(rects_from_floats(&[]), Vec::new());
    }

    #[test]
    fn a_trailing_partial_group_is_dropped_rather_than_panicking() {
        // `as_chunks` is the point here: five floats is one whole group plus a stray value
        // that cannot form a second one, and should simply be ignored, not indexed out of bounds.
        let floats = [10.0, 20.0, 30.0, 40.0, 999.0];
        assert_eq!(
            rects_from_floats(&floats),
            vec![CursorRect {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0
            }]
        );
    }

    #[test]
    fn a_span_wrapping_a_line_break_keeps_every_group() {
        // The two-rectangle case `rects_from_floats`'s own doc comment describes: a span that
        // wraps a line break, where `span_rects` needs both groups to underline both visible
        // pieces.
        let floats = [10.0, 20.0, 30.0, 40.0, 0.0, 60.0, 15.0, 40.0];
        assert_eq!(
            rects_from_floats(&floats),
            vec![
                CursorRect {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 40.0
                },
                CursorRect {
                    x: 0.0,
                    y: 60.0,
                    width: 15.0,
                    height: 40.0
                },
            ]
        );
    }

    #[test]
    fn rect_from_win32_converts_edges_to_an_origin_and_a_size() {
        assert_eq!(
            rect_from_win32(RECT {
                left: 100,
                top: 200,
                right: 500,
                bottom: 350,
            }),
            CursorRect {
                x: 100.0,
                y: 200.0,
                width: 400.0,
                height: 150.0,
            }
        );
    }

    const WINDOW: CursorRect = CursorRect {
        x: 100.0,
        y: 200.0,
        width: 800.0,
        height: 600.0,
    };

    #[test]
    fn the_fallback_refuses_a_window_of_this_app_without_reading_it() {
        let result = fallback_rect(Foreground::ThisApp, || {
            panic!("the app's own window is not read as the document view")
        });
        assert!(matches!(
            result,
            Err(NativeCaptureError::ForegroundIsThisApp)
        ));
    }

    #[test]
    fn the_fallback_answers_the_documents_window() {
        assert_eq!(
            fallback_rect(Foreground::Document, || Ok(WINDOW)).expect("falls back"),
            WINDOW
        );
    }

    #[test]
    fn the_fallback_answers_a_window_of_another_process() {
        assert_eq!(
            fallback_rect(Foreground::Other, || Ok(WINDOW)).expect("falls back"),
            WINDOW
        );
    }
}
