//! Creates and drives the full-viewport overlay window: the always-on-top, transparent,
//! non-activating surface that mirrors the focused document's own on-screen area, underlines every
//! flagged span in place, and shows a card on hover. `create`'s window mechanics were proven in
//! #9; `track_document_view` replaces its placeholder position and fixed size with the native
//! backend's live document-view data, per #23 and #42, and hides the window while the document is
//! behind another window. #42's other two loops have a module each: `flags` resolves each flag's
//! on-screen position, and `hover` toggles the window's click-through state so the underlines
//! stay interactive without blocking every other click to the document underneath.

mod flags;
mod hover;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    ShowWindow, PBT_APMRESUMEAUTOMATIC, SW_HIDE, SW_SHOW, WM_POWERBROADCAST,
};

use wa_engine::capture::{Capture, CursorRect, Foreground};
use wa_engine::flag::Flag;

pub use flags::{relative_flags, track_flags};
pub use hover::track_hover;

// Arbitrary and deliberately not (0, 0), so it's visually distinguishable from a stray
// full-screen window during manual verification. Only ever on screen for the moment between
// the window's own creation and the first `track_document_view` poll, which replaces it with the
// real document-view rectangle.
const INITIAL_X: f64 = 200.0;
const INITIAL_Y: f64 = 200.0;
const INITIAL_WIDTH: f64 = 360.0;
const INITIAL_HEIGHT: f64 = 120.0;

/// How often `track_document_view`, `track_hover`, and `track_flags`'s own position refresh each
/// poll. The three loops poll by design: scrolling and window movement raise no dependable event,
/// so a position is known only by asking for it. They are throttled only if measured CPU calls
/// for it.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A [`POLL_INTERVAL`] interval that drops missed ticks instead of firing them back to back.
/// Tokio's default, `MissedTickBehavior::Burst`, treats a tick whose work overran the interval as
/// a debt to repay immediately, and every one of these loops spends its tick on UI Automation
/// calls against one shared capture thread: `track_flags` issues one `span_rect` per flag. A
/// tick slow enough to be missed would then queue more of exactly the calls that made it slow.
/// None of these loops needs a fixed number of ticks, only a bounded gap between refreshes, so a
/// missed one is worth nothing and dropping it is free.
fn poll_interval() -> tokio::time::Interval {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    // Non-activating: a click on the overlay, the working card's buttons among them, never takes
    // keyboard focus from the document, whose window the insertion cascade types into. An action
    // that needs the keyboard activates the window explicitly; none exists yet. `focusable(false)`
    // is how tao gives the window `WS_EX_NOACTIVATE`. Set on the handle directly, the style would
    // be dropped at the first hover: tao rebuilds the extended style from its own window flags on
    // every flag change, and `set_ignore_cursor_events` is one.
    let window = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html".into()))
        .title("Writing Assistant Overlay")
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .position(INITIAL_X, INITIAL_Y)
        .inner_size(INITIAL_WIDTH, INITIAL_HEIGHT)
        .build()?;

    // Starts click-through: until the first `track_hover` poll finds the cursor over a flagged
    // span's own rect, every click and keystroke should reach the document underneath rather than
    // this window, which has nothing worth receiving input yet.
    if let Err(error) = window.set_ignore_cursor_events(true) {
        log::warn!("could not start the overlay click-through: {error}");
    }

    // Tauri's own `hwnd()` links against a different windows-rs version than this crate's own
    // dependency (0.61 against this crate's 0.62, both present in Cargo.lock because tauri pins
    // its own minor version); `HWND` has been a bare wrapped pointer across both, unchanged, so
    // reconstructing this crate's own `HWND` from the same raw pointer value is a sound, if
    // ceremonious, way to call this crate's own windows-rs APIs against tauri's window.
    let hwnd = HWND(window.hwnd()?.0);
    // SAFETY: `hwnd` is the live overlay window just built above, on the thread that owns it
    // (Tauri's `setup` closure runs on the main thread, the same thread that later pumps this
    // window's messages); `resume_subclass_proc` matches `SUBCLASSPROC`'s required signature.
    if !unsafe { SetWindowSubclass(hwnd, Some(resume_subclass_proc), RESUME_SUBCLASS_ID, 0) }
        .as_bool()
    {
        log::warn!("could not subclass the overlay window to detect sleep/resume");
    }
    Ok(())
}

/// Arbitrary, only needs to be unique among subclasses registered on the same window; this is
/// the overlay window's only one.
const RESUME_SUBCLASS_ID: usize = 1;

/// Recovers the overlay from a DWM/WebView2 composition bug reported in #27: after the machine
/// sleeps and resumes, the overlay window keeps reporting itself visible, keeps its topmost
/// z-order and requested position, and `PrintWindow` still captures its correct rendered
/// content, but nothing of that reaches the screen. Restarting the app was the only fix found
/// by hand. That points at DWM's redirection surface for the window, or WebView2's own
/// DirectComposition visual tree feeding it, going stale across the sleep, not at anything
/// `overlay.rs` itself computes; a known WebView2 defect
/// (MicrosoftEdge/WebView2Feedback#3429) reports display corruption after sleep with no
/// automatic recovery, in the same GPU-composition family as this bug even though its exact
/// symptom differs. Manual verification against a real sleep, lid close and reopen, confirmed
/// that toggling this window's visibility on resume is enough to force it to recomposite: the
/// overlay stayed visible and kept tracking the caret afterward, where it previously required
/// restarting the app.
///
/// Chained via `SetWindowSubclass` rather than by replacing `GWLP_WNDPROC` directly: that keeps
/// this addition from breaking any other subclass already registered on this same window, such
/// as one WebView2 or tao, tauri's own windowing layer, installs, unlike a raw `GWLP_WNDPROC`
/// swap, which would need to preserve and call whichever procedure was already there.
unsafe extern "system" fn resume_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _ref_data: usize,
) -> LRESULT {
    if msg == WM_POWERBROADCAST && wparam.0 == PBT_APMRESUMEAUTOMATIC as usize {
        log::info!("system resume detected; nudging the overlay window to recomposite");
        // SAFETY: `hwnd` is the same live window this subclass is installed on, supplied by
        // the message dispatcher that invoked this callback.
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
    }
    // SAFETY: `hwnd`, `msg`, `wparam`, and `lparam` are exactly what the message dispatcher
    // passed to this callback; forwarding every message, handled or not, to the next subclass
    // (or the window's own procedure, once the chain ends) is `SetWindowSubclass`'s own
    // contract, without which the window stops handling everything this callback does not.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// One flag together with every on-screen rectangle its span currently resolves to. `Clone` so a
/// snapshot can be taken out from under [`SharedOverlay`]'s lock before it is relativised in
/// [`relative_flags`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionedFlag {
    pub flag: Flag,
    pub rects: Vec<CursorRect>,
}

/// What the overlay's loops and its command share, in absolute physical screen coordinates: the
/// positioned flags, which `track_flags` writes, and the document-view rectangle the window was
/// last placed at, which `track_document_view` writes. `track_hover` hit-tests a `GetCursorPos`
/// result, itself absolute and physical, against the flags. Kept absolute and physical rather than
/// pre-relativised into the overlay window's own CSS pixel space, so hit-testing compares physical
/// against physical, and only the boundary that renders ([`relative_flags`]) converts.
#[derive(Debug, Default)]
pub struct OverlayState {
    flags: Vec<PositionedFlag>,
    view: Option<CursorRect>,
}

pub type SharedOverlay = Arc<Mutex<OverlayState>>;

fn lock(shared: &SharedOverlay) -> MutexGuard<'_, OverlayState> {
    shared
        .lock()
        .expect("a poisoned lock here means another thread already panicked")
}

/// Spawns a background task that places the overlay window over `capture`'s live document-view
/// rectangle, the whole visible area of whatever document is focused, so every flagged span the
/// analyzer surfaces has somewhere on the overlay to be underlined, per #42's full-viewport
/// decision. The window moves only when the rectangle changed, and stays where it was last placed
/// while the capture cannot answer, as when one of this app's own windows is in front.
///
/// Each tick also asks `capture` whose window is in the foreground, and hides the overlay while
/// it is neither the document's nor this app's, so underlines are never drawn over a window in
/// front of the document. It shows again when the document's window is back in front.
///
/// `CursorRect`'s coordinates come from UI Automation as physical screen pixels, so this uses
/// `PhysicalPosition`/`PhysicalSize` rather than their logical counterparts deliberately: Tauri's
/// position and size types differ by the display's DPI scale factor, and using the wrong one
/// misplaces or missizes the overlay on any scaled display. Confirmed in manual verification on a
/// 200%-scaled display for the caret-following window this replaced (#23); the same reasoning
/// applies unchanged to a whole document-view rectangle.
pub fn track_document_view(app: AppHandle, capture: Arc<dyn Capture>, shared: SharedOverlay) {
    tauri::async_runtime::spawn(async move {
        let mut interval = poll_interval();
        let mut shown = true;
        loop {
            interval.tick().await;
            let Some(window) = app.get_webview_window("overlay") else {
                continue;
            };
            let read = match capture.document_view_rect().await {
                Ok(rect) => Some(rect),
                Err(error) => {
                    log::debug!("no document view to track: {error}");
                    None
                }
            };
            let placed = lock(&shared).view;
            if let Some(rect) = placement(read, placed) {
                let (position, size) = to_physical(rect);
                if let Err(error) = window.set_position(position) {
                    log::warn!("failed to reposition the overlay: {error}");
                }
                if let Err(error) = window.set_size(size) {
                    log::warn!("failed to resize the overlay: {error}");
                }
                lock(&shared).view = Some(rect);
            }

            let foreground = match capture.foreground().await {
                Ok(foreground) => Some(foreground),
                Err(error) => {
                    log::debug!("no foreground window to classify: {error}");
                    None
                }
            };
            let Some(show) = visibility(foreground) else {
                continue;
            };
            if show == shown {
                continue;
            }
            let result = if show {
                log::debug!("the document's window is in front again; showing the overlay");
                window.show()
            } else {
                log::debug!("another window is in front of the document; hiding the overlay");
                window.hide()
            };
            match result {
                Ok(()) => shown = show,
                Err(error) => log::warn!("failed to show or hide the overlay: {error}"),
            }
        }
    });
}

/// Where the overlay window moves on this tick, given the document-view rectangle `read` from
/// the capture, `None` when it could not answer, and the rectangle the window was last `placed`
/// at: a rectangle that differs from the last placement, and otherwise nowhere. A tick whose read
/// failed leaves the window where it was, which keeps the overlay off this app's own window when
/// that is in front.
fn placement(read: Option<CursorRect>, placed: Option<CursorRect>) -> Option<CursorRect> {
    read.filter(|rect| Some(*rect) != placed)
}

/// Whether the overlay should be shown, given whose window is in the foreground, `None` when the
/// capture could not say: shown for the document's, hidden for another process's, and left as it
/// is for this app's own or an unknown answer. Left as it is for this app's own window, so that
/// bringing the app's main window forward neither hides the underlines over the document nor
/// shows them over a document that another window covers.
fn visibility(foreground: Option<Foreground>) -> Option<bool> {
    match foreground? {
        Foreground::Document => Some(true),
        Foreground::Other => Some(false),
        Foreground::ThisApp => None,
    }
}

/// Converts a document-view rectangle straight into the overlay's own position and size: unlike
/// the fixed-size caret-following box this replaced, there is no work area to clamp against here,
/// since `rect` already describes real, currently-on-screen bounds (an already-visible window or
/// element), not a synthesized position near a point that could overrun a display edge.
fn to_physical(rect: CursorRect) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    (
        PhysicalPosition::new(rect.x.round() as i32, rect.y.round() as i32),
        PhysicalSize::new(
            rect.width.round().max(0.0) as u32,
            rect.height.round().max(0.0) as u32,
        ),
    )
}

/// Physical pixels per CSS pixel on whichever display `window` currently sits on, read per call
/// rather than once at startup so a document dragged to a display at a different scale converts
/// by that display's own factor. Falls back to 1.0, leaving coordinates in physical pixels, when
/// the window reports nothing usable: wrong on a scaled display, and the only value that is right
/// on an unscaled one.
fn scale_factor(window: &WebviewWindow) -> f64 {
    match window.scale_factor() {
        Ok(scale) if scale > 0.0 => scale,
        Ok(scale) => {
            log::warn!("the overlay window reported a scale factor of {scale}; falling back to 1");
            1.0
        }
        Err(error) => {
            log::warn!("could not read the overlay window's scale factor: {error}");
            1.0
        }
    }
}

#[cfg(test)]
mod test_support {
    use super::*;
    use wa_engine::flag::{FlagOrigin, Span};

    pub(super) fn rect(x: f64, y: f64, width: f64, height: f64) -> CursorRect {
        CursorRect {
            x,
            y,
            width,
            height,
        }
    }

    pub(super) fn test_flag(id: &str) -> Flag {
        Flag {
            id: id.to_owned(),
            origin: FlagOrigin::Spelling,
            span: Span {
                anchor: "word".to_owned(),
                local_start: 0,
                local_length: 4,
                occurrence: 0,
            },
            message: "test".to_owned(),
            suggestions: Vec::new(),
            source_detail: "test".to_owned(),
            ruleset_version: None,
            catalogue_hash: None,
        }
    }

    pub(super) fn positioned(id: &str, rects: Vec<CursorRect>) -> PositionedFlag {
        PositionedFlag {
            flag: test_flag(id),
            rects,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::rect;
    use super::*;

    #[test]
    fn to_physical_converts_origin_and_size_directly() {
        let (position, size) = to_physical(rect(500.0, 600.0, 800.0, 400.0));
        assert_eq!(position, PhysicalPosition::new(500, 600));
        assert_eq!(size, PhysicalSize::new(800, 400));
    }

    #[test]
    fn to_physical_rounds_fractional_coordinates() {
        let (position, size) = to_physical(rect(500.4, 599.6, 800.5, 399.5));
        assert_eq!(position, PhysicalPosition::new(500, 600));
        assert_eq!(size, PhysicalSize::new(801, 400));
    }

    #[test]
    fn to_physical_passes_through_a_negative_origin() {
        // A document view on a secondary display placed above and to the left of the primary,
        // the same negative-coordinate case #23's multi-monitor verification already covers.
        let (position, _) = to_physical(rect(-500.0, -1080.0, 800.0, 400.0));
        assert_eq!(position, PhysicalPosition::new(-500, -1080));
    }

    #[test]
    fn to_physical_clamps_a_degenerate_size_to_zero_rather_than_underflowing() {
        let (_, size) = to_physical(rect(0.0, 0.0, -5.0, -5.0));
        assert_eq!(size, PhysicalSize::new(0, 0));
    }

    #[test]
    fn the_first_placement_moves_the_window_to_the_read() {
        let view = rect(0.0, 133.0, 3072.0, 1628.0);
        assert_eq!(placement(Some(view), None), Some(view));
    }

    #[test]
    fn an_unchanged_read_does_not_move_the_window() {
        let view = rect(0.0, 133.0, 3072.0, 1628.0);
        assert_eq!(placement(Some(view), Some(view)), None);
    }

    #[test]
    fn a_changed_read_moves_the_window() {
        let placed = rect(0.0, 133.0, 3072.0, 1628.0);
        let moved = rect(40.0, 173.0, 3000.0, 1500.0);
        assert_eq!(placement(Some(moved), Some(placed)), Some(moved));
    }

    #[test]
    fn a_failed_read_with_the_apps_own_window_in_front_keeps_the_last_placement() {
        // The capture answers an error rather than the rectangle of the app's own window, and the
        // window stays where it was last placed.
        let placed = rect(0.0, 133.0, 3072.0, 1628.0);
        assert_eq!(placement(None, Some(placed)), None);
    }

    #[test]
    fn the_overlay_shows_with_the_documents_window_in_front() {
        assert_eq!(visibility(Some(Foreground::Document)), Some(true));
    }

    #[test]
    fn the_overlay_hides_with_another_window_in_front() {
        assert_eq!(visibility(Some(Foreground::Other)), Some(false));
    }

    #[test]
    fn the_overlay_is_left_as_it_is_with_the_apps_own_window_in_front() {
        assert_eq!(visibility(Some(Foreground::ThisApp)), None);
    }

    #[test]
    fn the_overlay_is_left_as_it_is_when_the_foreground_is_unknown() {
        assert_eq!(visibility(None), None);
    }
}
