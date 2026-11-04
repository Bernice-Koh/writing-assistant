//! The foreground window: which process owns it, the scoped document's, this app's, or another's;
//! and bringing the document's own window to the front before an insertion types into it. Split
//! into the unsafe Win32 calls and pure decisions any test can exercise without a live desktop.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{IUIAutomation, IUIAutomationElement};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetDesktopWindow, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
    SetForegroundWindow, ShowWindow, GA_ROOT, SW_RESTORE,
};

use super::error::NativeCaptureError;
use super::process;
use crate::capture::Foreground;

/// Caps the walk from an element up to the nearest ancestor with a window handle. A document
/// element sits a few levels below its window; the cap only matters for a malformed tree.
const MAX_ANCESTORS: usize = 32;

/// A top-level window and the id of the process that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub hwnd: HWND,
    pub process_id: u32,
}

/// The foreground window, or `None` when no window is in the foreground, as during a switch
/// between windows, or when its owning process cannot be read.
pub fn window() -> Option<Window> {
    // SAFETY: GetForegroundWindow takes no arguments; it returns a null handle when no window is
    // in the foreground, checked by `owned_by`.
    owned_by(unsafe { GetForegroundWindow() })
}

/// `hwnd` with the id of its owning process, or `None` for a null handle or a window whose owner
/// cannot be read.
fn owned_by(hwnd: HWND) -> Option<Window> {
    if hwnd.is_invalid() {
        return None;
    }
    let mut process_id = 0;
    // SAFETY: `hwnd` was just confirmed non-null; `process_id` is a valid, uniquely-owned out
    // parameter for the duration of this call. A window destroyed in between leaves it at 0,
    // checked below.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut process_id)) };
    (process_id != 0).then_some(Window { hwnd, process_id })
}

/// Classifies the foreground window's process, `None` when no window is in the foreground,
/// against the process that owns the document in scope. The document's process is compared
/// first, so `belongs_to_this_app`, which can read the whole process table, is asked only about
/// a process that is not the document's.
///
/// By process, not by window: a second window of the document's own process in front of it
/// answers [`Foreground::Document`].
pub fn classify(
    foreground_process: Option<u32>,
    document_process: u32,
    belongs_to_this_app: impl FnOnce(u32) -> bool,
) -> Foreground {
    match foreground_process {
        None => Foreground::Other,
        Some(process) if process == document_process => Foreground::Document,
        Some(process) if belongs_to_this_app(process) => Foreground::ThisApp,
        Some(_) => Foreground::Other,
    }
}

/// The top-level window `element` sits in: the element's own window handle, or that of its
/// nearest ancestor with one, walked to its root with `GetAncestor(GA_ROOT)`. `None` when no
/// ancestor below the desktop has a handle, or the root's owning process cannot be read. A debug
/// line records how many levels above the element the handle came from, which differs by
/// application: 0 for a document element with a window of its own.
pub fn target_window(client: &IUIAutomation, element: &IUIAutomationElement) -> Option<Window> {
    // SAFETY: GetDesktopWindow takes no arguments and always returns the desktop's handle.
    let desktop = unsafe { GetDesktopWindow() };
    // SAFETY: `client` is a live UI Automation client; the raw view walker it returns reaches
    // every element, so no ancestor with a handle is skipped.
    let walker = unsafe { client.RawViewWalker() }.ok()?;
    let mut current = element.clone();
    for level in 0..MAX_ANCESTORS {
        // SAFETY: `current` is a live element; the property read fails safely (Err) for an element
        // whose provider has gone.
        let hwnd = unsafe { current.CurrentNativeWindowHandle() }.ok()?;
        if hwnd == desktop {
            return None;
        }
        if !hwnd.is_invalid() {
            // SAFETY: `hwnd` is a window handle just read from the element; GetAncestor returns a
            // null handle for a window destroyed since, which `owned_by` refuses.
            let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
            log::debug!("the target's window handle came from {level} levels above the element");
            return owned_by(root);
        }
        // SAFETY: `walker` and `current` are live; the walk fails safely (Err) at the root.
        current = unsafe { walker.GetParentElement(&current) }.ok()?;
    }
    None
}

/// What to do before a keystroke, given the target's window and the foreground window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The target's window is in front: type.
    Proceed,
    /// Another window, or none, is in front: bring this window, the target's, to the front first.
    Refocus(Window),
    /// The target has no window to bring to the front, so a keystroke could land anywhere.
    Refuse,
}

/// Compares the target's window with the foreground window by handle, not by process: a second
/// window of the target's own process in front of it, a dialog for one, takes the keystrokes as
/// readily as any other.
pub fn decide(target: Option<Window>, foreground: Option<Window>) -> Decision {
    match (target, foreground) {
        (None, _) => Decision::Refuse,
        (Some(target), Some(foreground)) if target.hwnd == foreground.hwnd => Decision::Proceed,
        (Some(target), _) => Decision::Refocus(target),
    }
}

/// Whether a refocus of `target` took, from the foreground window read before it (`before`) and
/// after it (`after`). A refocus that did not take names the target's process and the process
/// holding the foreground after the attempt, or the one that held it before when no window is in
/// front after, by `name_of`.
pub fn refocus_outcome(
    target: Window,
    before: Option<Window>,
    after: Option<Window>,
    name_of: impl Fn(u32) -> String,
) -> Result<(), NativeCaptureError> {
    if after.is_some_and(|after| after.hwnd == target.hwnd) {
        return Ok(());
    }
    Err(NativeCaptureError::RefocusFailed {
        target: name_of(target.process_id),
        foreground: after.or(before).map_or_else(
            || "no window".to_owned(),
            |holder| name_of(holder.process_id),
        ),
    })
}

/// Makes `target` the foreground window before a keystroke: proceeds when it already is, and
/// otherwise restores it if it is minimised, calls `SetForegroundWindow`, and reads the foreground
/// window again. Answers whether a refocus ran and took; a refocus that did not take is
/// [`NativeCaptureError::RefocusFailed`], and a target with no window
/// [`NativeCaptureError::NoTargetWindow`].
///
/// Windows decides whether the call takes. The remarks on `SetForegroundWindow`'s reference page
/// list the conditions, one of which must hold for the calling process: among them, that it
/// received the last input event, or that the foreground lock time-out has expired. Nothing here
/// works around them; when none holds, the read after the call shows another window in front, and
/// the insertion stops before the keystroke it was about to send.
pub fn bring_to_front(target: Option<Window>) -> Result<bool, NativeCaptureError> {
    let before = window();
    let target = match decide(target, before) {
        Decision::Proceed => return Ok(false),
        Decision::Refuse => return Err(NativeCaptureError::NoTargetWindow),
        Decision::Refocus(target) => target,
    };
    log::info!(
        "refocusing {}'s window before a keystroke; in front was {}",
        process_name(target.process_id),
        before.map_or_else(
            || "no window".to_owned(),
            |holder| format!("{}'s window", process_name(holder.process_id))
        )
    );
    // SAFETY: `target.hwnd` is a window handle read from the target element; each call fails
    // safely for a window destroyed since, and the foreground read after them is what decides.
    unsafe {
        if IsIconic(target.hwnd).as_bool() {
            let _ = ShowWindow(target.hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(target.hwnd);
    }
    let outcome = refocus_outcome(target, before, window(), process_name);
    if let Err(error) = &outcome {
        log::warn!("{error}; the insertion stops before its next keystroke");
    }
    outcome.map(|()| true)
}

/// A process's executable name for a log line or an error, or its id when the name cannot be read.
pub fn process_name(pid: u32) -> String {
    process::executable_name(pid).unwrap_or_else(|| format!("process {pid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: u32 = 100;
    const APP: u32 = 200;
    const ANOTHER: u32 = 300;

    fn window_of(handle: usize, process_id: u32) -> Window {
        Window {
            hwnd: HWND(handle as *mut core::ffi::c_void),
            process_id,
        }
    }

    fn named(pid: u32) -> String {
        match pid {
            DOCUMENT => "Notepad.exe".to_owned(),
            ANOTHER => "powershell.exe".to_owned(),
            _ => format!("process {pid}"),
        }
    }

    #[test]
    fn the_documents_own_process_in_front_is_the_document() {
        let classified = classify(Some(DOCUMENT), DOCUMENT, |_| {
            panic!("the process table is not read for the document's own process")
        });
        assert_eq!(classified, Foreground::Document);
    }

    #[test]
    fn a_process_of_this_app_in_front_is_this_app() {
        assert_eq!(
            classify(Some(APP), DOCUMENT, |process| process == APP),
            Foreground::ThisApp
        );
    }

    #[test]
    fn any_other_process_in_front_is_other() {
        assert_eq!(
            classify(Some(ANOTHER), DOCUMENT, |process| process == APP),
            Foreground::Other
        );
    }

    #[test]
    fn no_foreground_window_is_other() {
        assert_eq!(classify(None, DOCUMENT, |_| true), Foreground::Other);
    }

    #[test]
    fn the_targets_window_in_front_proceeds() {
        let target = window_of(1, DOCUMENT);
        assert_eq!(decide(Some(target), Some(target)), Decision::Proceed);
    }

    #[test]
    fn another_processs_window_in_front_refocuses_the_target() {
        let target = window_of(1, DOCUMENT);
        assert_eq!(
            decide(Some(target), Some(window_of(2, ANOTHER))),
            Decision::Refocus(target)
        );
    }

    #[test]
    fn this_apps_window_in_front_refocuses_the_target() {
        let target = window_of(1, DOCUMENT);
        assert_eq!(
            decide(Some(target), Some(window_of(3, APP))),
            Decision::Refocus(target)
        );
    }

    #[test]
    fn another_window_of_the_targets_own_process_refocuses_the_target() {
        let target = window_of(1, DOCUMENT);
        assert_eq!(
            decide(Some(target), Some(window_of(4, DOCUMENT))),
            Decision::Refocus(target)
        );
    }

    #[test]
    fn no_foreground_window_refocuses_the_target() {
        let target = window_of(1, DOCUMENT);
        assert_eq!(decide(Some(target), None), Decision::Refocus(target));
    }

    #[test]
    fn a_target_without_a_window_is_refused_whatever_is_in_front() {
        assert_eq!(decide(None, Some(window_of(2, ANOTHER))), Decision::Refuse);
        assert_eq!(decide(None, None), Decision::Refuse);
    }

    #[test]
    fn a_refocus_took_when_the_target_is_in_front_after_it() {
        let target = window_of(1, DOCUMENT);
        let outcome = refocus_outcome(target, Some(window_of(2, ANOTHER)), Some(target), |_| {
            panic!("no process is named when the refocus took")
        });
        assert!(outcome.is_ok());
    }

    #[test]
    fn a_refocus_that_did_not_take_names_the_target_and_the_window_still_in_front() {
        let cover = window_of(2, ANOTHER);
        let outcome = refocus_outcome(window_of(1, DOCUMENT), Some(cover), Some(cover), named);
        let Err(NativeCaptureError::RefocusFailed { target, foreground }) = outcome else {
            panic!("expected RefocusFailed, got {outcome:?}");
        };
        assert_eq!(target, "Notepad.exe");
        assert_eq!(foreground, "powershell.exe");
    }

    #[test]
    fn a_refocus_that_did_not_take_names_the_window_in_front_after_it() {
        let outcome = refocus_outcome(
            window_of(1, DOCUMENT),
            Some(window_of(2, ANOTHER)),
            Some(window_of(3, APP)),
            named,
        );
        let Err(NativeCaptureError::RefocusFailed { foreground, .. }) = outcome else {
            panic!("expected RefocusFailed, got {outcome:?}");
        };
        assert_eq!(foreground, "process 200");
    }

    #[test]
    fn a_refocus_that_left_no_window_in_front_names_the_one_that_held_it() {
        let outcome = refocus_outcome(
            window_of(1, DOCUMENT),
            Some(window_of(2, ANOTHER)),
            None,
            named,
        );
        let Err(NativeCaptureError::RefocusFailed { foreground, .. }) = outcome else {
            panic!("expected RefocusFailed, got {outcome:?}");
        };
        assert_eq!(foreground, "powershell.exe");
    }

    #[test]
    fn a_refocus_with_no_window_in_front_before_or_after_says_so() {
        let outcome = refocus_outcome(window_of(1, DOCUMENT), None, None, named);
        let Err(NativeCaptureError::RefocusFailed { target, foreground }) = outcome else {
            panic!("expected RefocusFailed, got {outcome:?}");
        };
        assert_eq!(target, "Notepad.exe");
        assert_eq!(foreground, "no window");
    }

    #[test]
    fn a_refocus_that_did_not_take_maps_to_its_own_capture_error() {
        let cover = window_of(2, ANOTHER);
        let error = refocus_outcome(window_of(1, DOCUMENT), Some(cover), Some(cover), named)
            .expect_err("the cover stayed in front");
        assert!(matches!(
            crate::capture::CaptureError::from(error),
            crate::capture::CaptureError::RefocusFailed { ref target, ref foreground }
                if target == "Notepad.exe" && foreground == "powershell.exe"
        ));
    }
}
