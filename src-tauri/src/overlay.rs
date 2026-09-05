//! Creates and drives the full-viewport overlay window: the always-on-top, transparent surface
//! that mirrors the focused document's own on-screen area, underlines every flagged span in
//! place, and shows a card on hover. `create`'s window mechanics were proven in #9;
//! `track_document_view` is what replaces its placeholder position and fixed size with the
//! native backend's live document-view data, per #23 and #42. `track_flags` and `track_hover` are
//! #42's own additions: resolving each flag's on-screen position and toggling the window's
//! click-through state so the underlines stay interactive without blocking every other click to
//! the document underneath.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, ShowWindow, PBT_APMRESUMEAUTOMATIC, SW_HIDE, SW_SHOW, WM_POWERBROADCAST,
};

use crate::analyzer::Analyzer;
use crate::capture::{Capture, CursorRect};
use crate::flag::Flag;

// Arbitrary and deliberately not (0, 0), so it's visually distinguishable from a stray
// full-screen window during manual verification. Only ever on screen for the moment between
// the window's own creation and the first `track_document_view` poll, which replaces it with the
// real document-view rectangle.
const INITIAL_X: f64 = 200.0;
const INITIAL_Y: f64 = 200.0;
const INITIAL_WIDTH: f64 = 360.0;
const INITIAL_HEIGHT: f64 = 120.0;

/// How often `track_document_view`, `track_hover`, and `track_flags`'s own position refresh each
/// poll. Not the real Tier 0 pipeline's event-driven push for cursor and document-view tracking,
/// out of scope for this phase (see #18), a polling bridge good enough to prove the overlay can
/// reflect live capture data at all; a later phase replaces this with a push from the capture
/// backend's own focus/text-change events.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let window = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html".into()))
        .title("Writing Assistant Overlay")
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
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

/// Spawns a background task that resizes and repositions the overlay window to `capture`'s live
/// document-view rectangle: the whole visible area of whatever document is focused, not just a
/// small box near the caret, so every flagged span the analyzer surfaces has somewhere on the
/// overlay to be underlined, per #42's full-viewport decision.
///
/// `CursorRect`'s coordinates come from UI Automation as physical screen pixels, so this uses
/// `PhysicalPosition`/`PhysicalSize` rather than their logical counterparts deliberately: Tauri's
/// position and size types differ by the display's DPI scale factor, and using the wrong one
/// misplaces or missizes the overlay on any scaled display. Confirmed in manual verification on a
/// 200%-scaled display for the caret-following window this replaces (#23); the same reasoning
/// applies unchanged to a whole document-view rectangle.
pub fn track_document_view(app: AppHandle, capture: Arc<dyn Capture>) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            interval.tick().await;
            let rect = match capture.document_view_rect().await {
                Ok(rect) => rect,
                Err(error) => {
                    log::debug!("no document view to track: {error}");
                    continue;
                }
            };
            let Some(window) = app.get_webview_window("overlay") else {
                continue;
            };
            let (position, size) = to_physical(rect);
            if let Err(error) = window.set_position(position) {
                log::warn!("failed to reposition the overlay: {error}");
            }
            if let Err(error) = window.set_size(size) {
                log::warn!("failed to resize the overlay: {error}");
            }
        }
    });
}

/// Converts a document-view rectangle straight into the overlay's own position and size: unlike
/// the fixed-size caret-following box this replaces, there is no work area to clamp against here,
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

/// One flag together with every on-screen rectangle its span currently resolves to. `Clone` so a
/// snapshot can be taken out from under [`SharedFlags`]'s lock before the (`await`-ing) work of
/// relativising it in [`relative_flags`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionedFlag {
    pub flag: Flag,
    pub rects: Vec<CursorRect>,
}

/// The latest positioned flags, in absolute physical screen coordinates, shared between
/// [`track_flags`] (writer) and [`track_hover`] (reader, hit-testing against a `GetCursorPos`
/// result, itself absolute and physical). Kept absolute and physical rather than pre-relativised
/// and converted into the overlay window's own CSS pixel space, so the two tasks never need to
/// agree on which document-view snapshot to relativise against, and so hit-testing keeps
/// comparing physical against physical; only the boundary that actually renders
/// ([`relative_flags`]) needs to know either.
pub type SharedFlags = Arc<Mutex<Vec<PositionedFlag>>>;

/// Spawns a background task that resolves each of `analyzer`'s current flags' on-screen positions
/// through `capture`, updates `shared` for [`track_hover`]'s hit-testing, and emits the same
/// flags, relativised to the document view's current origin, as a `flags-updated` event for the
/// overlay's own webview to render.
///
/// Triggered two ways: reactively, the moment `analyzer`'s own `subscribe` channel reports a new
/// flag set, and periodically, on the same [`POLL_INTERVAL`] cadence [`track_document_view`]
/// already polls on. The periodic leg exists because a flag's *position* can go stale for reasons
/// that have nothing to do with the flag set itself changing: the focused document scrolling, the
/// window resizing, or focus having moved to a different document entirely, whose text no longer
/// contains a still-cached flag's anchor at all. Manual verification against #48 found exactly
/// that last case: closing a document and focusing a new one left the previous document's flags
/// rendered, misplaced, inside the newly repositioned overlay window, because nothing re-resolved
/// their positions until the analyzer's own debounced recheck eventually caught up, seconds later.
/// Re-resolving on every poll tick, not only on a flag-set change, means a stale or momentarily
/// unresolvable position self-heals within one `POLL_INTERVAL` instead of lingering, since
/// [`resolve_positions`] already drops any flag whose anchor cannot currently be found.
pub fn track_flags(
    app: AppHandle,
    capture: Arc<dyn Capture>,
    analyzer: Arc<Analyzer>,
    shared: SharedFlags,
) {
    tauri::async_runtime::spawn(async move {
        let mut updates = analyzer.subscribe();
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            tokio::select! {
                changed = updates.changed() => {
                    if changed.is_err() {
                        log::debug!(
                            "the analyzer's flag channel closed; no more flag updates to render"
                        );
                        return;
                    }
                }
                _ = interval.tick() => {}
            }
            let flags = updates.borrow_and_update().clone();
            let positioned = resolve_positions(&capture, flags).await;
            *shared
                .lock()
                .expect("a poisoned lock here means another thread already panicked") = positioned;
            let relative = relative_flags(&app, &capture, &shared).await;
            if let Err(error) = app.emit("flags-updated", &relative) {
                log::warn!("failed to emit flags-updated: {error}");
            }
        }
    });
}

/// Resolves each of `flags`' on-screen rectangles through `capture`, in absolute screen
/// coordinates. A flag whose span cannot currently be resolved on screen (`span_rect` errors, or
/// returns no rectangles) is dropped rather than rendered with no known position: most likely the
/// document has moved on since the analyzer found it, and the text it names is no longer there.
async fn resolve_positions(capture: &Arc<dyn Capture>, flags: Vec<Flag>) -> Vec<PositionedFlag> {
    let mut positioned = Vec::with_capacity(flags.len());
    for flag in flags {
        match capture
            .span_rect(
                &flag.span.anchor,
                flag.span.local_start,
                flag.span.local_length,
            )
            .await
        {
            Ok(rects) if !rects.is_empty() => positioned.push(PositionedFlag { flag, rects }),
            Ok(_) => log::debug!("flag {} resolved to no on-screen rectangles", flag.id),
            Err(error) => {
                log::debug!("could not resolve an on-screen position for a flag: {error}");
            }
        }
    }
    positioned
}

/// `shared`'s cached, absolute physical positions, expressed relative to the document view's
/// current on-screen origin and divided down into CSS pixels: the coordinate space the overlay
/// window's own webview renders in, since the window itself sits exactly at that origin
/// (`track_document_view` puts it there). Used both by [`track_flags`]'s own emit and by the
/// `get_current_flags` Tauri command, so a freshly mounted frontend and a live update agree on
/// the same coordinate space. Empty when there is currently no document view to relativise
/// against, since in that case `track_document_view` has nowhere to have put the window either,
/// and a position relative to an unknown origin cannot be usefully rendered.
///
/// The division by the overlay window's scale factor is what makes this a function rather than a
/// subtraction the frontend could do for itself. UI Automation reports physical screen pixels; a
/// webview lays out in CSS pixels, which are physical pixels divided by the display's scale
/// factor. Rendering a physical offset as a CSS offset draws every underline at `scale_factor`
/// times its true distance from the window's origin, so at 200% scaling, twice: the symptom #48's
/// manual verification found against both Notepad and Word, with the overlay window itself
/// correctly placed, since `track_document_view` positions that in physical pixels through
/// `PhysicalPosition`.
pub async fn relative_flags(
    app: &AppHandle,
    capture: &Arc<dyn Capture>,
    shared: &SharedFlags,
) -> Vec<PositionedFlag> {
    let origin = match capture.document_view_rect().await {
        Ok(rect) => rect,
        Err(error) => {
            log::debug!("no document view to position flags against: {error}");
            return Vec::new();
        }
    };
    let absolute = shared
        .lock()
        .expect("a poisoned lock here means another thread already panicked")
        .clone();
    to_relative(&absolute, origin, overlay_scale_factor(app))
}

/// Physical pixels per CSS pixel on whichever display the overlay window currently sits on, read
/// per call rather than once at startup so a document dragged to a display at a different scale
/// converts by that display's own factor. Falls back to 1.0, leaving coordinates in physical
/// pixels, when the window is gone or reports nothing usable: wrong on a scaled display, and the
/// only value that is right on an unscaled one.
fn overlay_scale_factor(app: &AppHandle) -> f64 {
    let Some(window) = app.get_webview_window("overlay") else {
        return 1.0;
    };
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

fn to_relative(flags: &[PositionedFlag], origin: CursorRect, scale: f64) -> Vec<PositionedFlag> {
    flags
        .iter()
        .map(|positioned| PositionedFlag {
            flag: positioned.flag.clone(),
            rects: positioned
                .rects
                .iter()
                .map(|rect| to_css_pixels(offset(*rect, -origin.x, -origin.y), scale))
                .collect(),
        })
        .collect()
}

/// Applied after [`offset`], not before: `origin` is itself a physical coordinate, so subtracting
/// it has to happen in the space it is expressed in.
fn to_css_pixels(rect: CursorRect, scale: f64) -> CursorRect {
    CursorRect {
        x: rect.x / scale,
        y: rect.y / scale,
        width: rect.width / scale,
        height: rect.height / scale,
    }
}

fn offset(rect: CursorRect, dx: f64, dy: f64) -> CursorRect {
    CursorRect {
        x: rect.x + dx,
        y: rect.y + dy,
        ..rect
    }
}

/// Spawns a background task that polls the real cursor position and hit-tests it against
/// `shared`'s cached flag rectangles, toggling the overlay window's click-through state so the
/// window is transparent to clicks and keystrokes everywhere except directly over an underline.
/// A fully click-through window would never receive the hover in the first place, which is why
/// this toggles rather than leaving the window permanently click-through, per #42's decision.
/// Emits `flag-hovered` (carrying the flag id) on entering an underline's rect and
/// `flag-unhovered` on leaving it, so the frontend knows which card, if any, to show.
pub fn track_hover(app: AppHandle, shared: SharedFlags) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        let mut hovered: Option<String> = None;
        loop {
            interval.tick().await;
            let mut point = POINT { x: 0, y: 0 };
            // SAFETY: `point` is a valid, uniquely-owned out parameter for the duration of this
            // call; GetCursorPos takes no other input.
            if unsafe { GetCursorPos(&raw mut point) }.is_err() {
                continue;
            }
            let hit = shared
                .lock()
                .expect("a poisoned lock here means another thread already panicked")
                .iter()
                .find(|positioned| {
                    positioned
                        .rects
                        .iter()
                        .any(|rect| contains(rect, f64::from(point.x), f64::from(point.y)))
                })
                .map(|positioned| positioned.flag.id.clone());

            if hit == hovered {
                continue;
            }
            let Some(window) = app.get_webview_window("overlay") else {
                continue;
            };
            match &hit {
                Some(id) => {
                    if let Err(error) = window.set_ignore_cursor_events(false) {
                        log::warn!("failed to stop overlay click-through: {error}");
                    }
                    let _ = app.emit("flag-hovered", id);
                }
                None => {
                    if let Err(error) = window.set_ignore_cursor_events(true) {
                        log::warn!("failed to restore overlay click-through: {error}");
                    }
                    let _ = app.emit("flag-unhovered", ());
                }
            }
            hovered = hit;
        }
    });
}

fn contains(rect: &CursorRect, x: f64, y: f64) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag::{FlagOrigin, Span};

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CursorRect {
        CursorRect {
            x,
            y,
            width,
            height,
        }
    }

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
    fn offset_shifts_a_rect_by_the_given_delta() {
        assert_eq!(
            offset(rect(500.0, 600.0, 30.0, 18.0), -100.0, -200.0),
            rect(400.0, 400.0, 30.0, 18.0)
        );
    }

    fn test_flag(id: &str) -> Flag {
        Flag {
            id: id.to_owned(),
            origin: FlagOrigin::Spelling,
            span: Span {
                anchor: "word".to_owned(),
                local_start: 0,
                local_length: 4,
            },
            message: "test".to_owned(),
            suggestions: Vec::new(),
            source_detail: "test".to_owned(),
        }
    }

    #[test]
    fn to_relative_subtracts_the_origin_from_every_rect_of_every_flag() {
        let flags = vec![
            PositionedFlag {
                flag: test_flag("a"),
                rects: vec![rect(150.0, 250.0, 40.0, 20.0)],
            },
            PositionedFlag {
                flag: test_flag("b"),
                rects: vec![
                    rect(160.0, 400.0, 20.0, 20.0),
                    rect(100.0, 420.0, 10.0, 20.0),
                ],
            },
        ];
        let relative = to_relative(&flags, rect(100.0, 200.0, 900.0, 700.0), 1.0);
        assert_eq!(relative[0].rects, vec![rect(50.0, 50.0, 40.0, 20.0)]);
        assert_eq!(
            relative[1].rects,
            vec![rect(60.0, 200.0, 20.0, 20.0), rect(0.0, 220.0, 10.0, 20.0)]
        );
    }

    #[test]
    fn to_relative_divides_a_scaled_display_down_into_css_pixels() {
        // The real Notepad numbers from #48's failed manual verification on a 200%-scaled
        // display: a span 240 physical pixels right of the document view's origin belongs 120
        // CSS pixels into the overlay's webview, and rendering it at 240 is what put every
        // underline at twice its true distance from the caret.
        let flags = vec![PositionedFlag {
            flag: test_flag("a"),
            rects: vec![rect(239.0, 233.0, 336.0, 34.0)],
        }];
        let relative = to_relative(&flags, rect(-1.0, 133.0, 3074.0, 1628.0), 2.0);
        assert_eq!(relative[0].rects, vec![rect(120.0, 50.0, 168.0, 17.0)]);
    }

    #[test]
    fn contains_is_true_inside_and_on_the_leading_edges() {
        let target = rect(100.0, 100.0, 50.0, 20.0);
        assert!(contains(&target, 100.0, 100.0));
        assert!(contains(&target, 125.0, 110.0));
    }

    #[test]
    fn contains_is_false_on_the_trailing_edges_and_outside() {
        let target = rect(100.0, 100.0, 50.0, 20.0);
        assert!(!contains(&target, 150.0, 110.0));
        assert!(!contains(&target, 125.0, 120.0));
        assert!(!contains(&target, 99.0, 110.0));
        assert!(!contains(&target, 125.0, 99.0));
    }
}
