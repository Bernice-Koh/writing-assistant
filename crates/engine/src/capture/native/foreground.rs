//! Which process owns the foreground window: the scoped document's, this app's, or another's.
//! Split into the unsafe Win32 calls and a pure classification any test can exercise without a
//! live desktop.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::capture::Foreground;

/// The foreground window and the id of the process that owns it.
pub(super) struct ForegroundWindow {
    pub(super) hwnd: HWND,
    pub(super) process_id: u32,
}

/// The foreground window, or `None` when no window is in the foreground, as during a switch
/// between windows, or when its owning process cannot be read.
pub(super) fn window() -> Option<ForegroundWindow> {
    // SAFETY: GetForegroundWindow takes no arguments; it returns a null handle when no window is
    // in the foreground, checked below before the handle is used for anything.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }
    let mut process_id = 0;
    // SAFETY: `hwnd` was just confirmed non-null; `process_id` is a valid, uniquely-owned out
    // parameter for the duration of this call. A window destroyed in between leaves it at 0,
    // checked below.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut process_id)) };
    (process_id != 0).then_some(ForegroundWindow { hwnd, process_id })
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

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: u32 = 100;
    const APP: u32 = 200;
    const ANOTHER: u32 = 300;

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
}
