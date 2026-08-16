//! Creates the overlay window: the always-on-top surface that will host flag callouts and
//! drift indicators once the style engine has something to show. `create`'s window mechanics
//! were proven in #9; `track_cursor` is what replaces its placeholder position with the native
//! backend's live cursor data, per #23.

use std::sync::Arc;
use std::time::Duration;

use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewUrl,
    WebviewWindowBuilder,
};

use crate::capture::{Capture, CursorRect};

// Arbitrary and deliberately not (0, 0), so it's visually distinguishable from a stray
// full-screen window during manual verification.
const INITIAL_X: f64 = 200.0;
const INITIAL_Y: f64 = 200.0;
const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 120.0;

/// How often `track_cursor` checks for a new cursor position. Not the real Tier 0 pipeline's
/// event-driven push, out of scope for this phase (see #18), a polling bridge good enough to
/// prove the overlay can reflect live capture data at all; a later phase replaces this with a
/// push from the capture backend's own focus/text-change events.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html".into()))
        .title("Writing Assistant Overlay")
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .position(INITIAL_X, INITIAL_Y)
        .inner_size(WIDTH, HEIGHT)
        .build()?;
    Ok(())
}

/// Spawns a background task that repositions the overlay window to `capture`'s live cursor
/// rectangle; [`place`] decides where exactly, relative to the caret and the display holding
/// it.
///
/// `CursorRect`'s coordinates come from UI Automation as physical screen pixels, so this uses
/// `PhysicalPosition` rather than `LogicalPosition` deliberately: Tauri's two position types
/// differ by the display's DPI scale factor, and using the wrong one misplaces the overlay on
/// any scaled display. Confirmed in manual verification on a 200%-scaled display, where the
/// window landed at exactly the requested physical coordinates.
pub fn track_cursor(app: AppHandle, capture: Arc<dyn Capture>) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            interval.tick().await;
            let rect = match capture.cursor_rect().await {
                Ok(rect) => rect,
                Err(error) => {
                    log::debug!("no cursor rect to track: {error}");
                    continue;
                }
            };
            let Some(window) = app.get_webview_window("overlay") else {
                continue;
            };
            let size = match window.outer_size() {
                Ok(size) => size,
                Err(error) => {
                    log::warn!("could not read the overlay's own size: {error}");
                    continue;
                }
            };
            // The monitor holding the caret, not the one holding the overlay: the overlay is
            // about to move to the caret, and on a multi-monitor desktop the two are routinely
            // different displays with different work areas.
            let work_area = match window.monitor_from_point(rect.x, rect.y) {
                Ok(Some(monitor)) => Some(*monitor.work_area()),
                Ok(None) => {
                    log::debug!("no monitor contains the caret at ({}, {})", rect.x, rect.y);
                    None
                }
                Err(error) => {
                    log::warn!("could not resolve the caret's monitor: {error}");
                    None
                }
            };
            let position = place(rect, size, work_area);
            match window.set_position(position) {
                Ok(()) => log::debug!("overlay repositioned to {position:?}"),
                Err(error) => log::warn!("failed to reposition the overlay: {error}"),
            }
        }
    });
}

/// Where the overlay's top-left corner goes for a caret at `caret`, given the overlay's own
/// `size` and the work area of the display the caret sits on.
///
/// Sits below the caret line by default so the overlay never covers the text being typed, and
/// flips to sit above it when the space below cannot hold the whole window. Both axes are then
/// clamped into `work_area`, because a caret near a screen edge otherwise pushes most of the
/// window off the desktop: manual verification put it 202px past a monitor's right edge from a
/// caret in a browser, and 114px below another's bottom edge from a chat box, in both cases
/// leaving only a sliver on screen.
///
/// A `work_area` of `None` means no display claimed the caret's position, which leaves nothing
/// to clamp against; the unclamped placement is still better than not moving at all.
fn place(
    caret: CursorRect,
    size: PhysicalSize<u32>,
    work_area: Option<PhysicalRect<i32, u32>>,
) -> PhysicalPosition<i32> {
    let caret_x = caret.x.round() as i64;
    let caret_y = caret.y.round() as i64;
    let below = caret_y + caret.height.round() as i64;
    let Some(area) = work_area else {
        return PhysicalPosition::new(caret_x as i32, below as i32);
    };

    let (width, height) = (i64::from(size.width), i64::from(size.height));
    let (left, top) = (i64::from(area.position.x), i64::from(area.position.y));
    let (right, bottom) = (
        left + i64::from(area.size.width),
        top + i64::from(area.size.height),
    );

    let y = if below + height <= bottom {
        below
    } else {
        // Above the caret line rather than merely shoved up from the bottom, so the overlay
        // still reads as attached to the caret instead of parked over unrelated text.
        caret_y - height
    };

    // `min` before `max` so that a window taller or wider than the work area lands at the
    // area's own origin rather than at a negative offset outside it.
    let x = caret_x.min(right - width).max(left);
    let y = y.min(bottom - height).max(top);
    PhysicalPosition::new(x as i32, y as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay's real size on a 200%-scaled display, where WIDTH/HEIGHT are logical.
    const OVERLAY: PhysicalSize<u32> = PhysicalSize::new(720, 240);

    fn caret(x: f64, y: f64, height: f64) -> CursorRect {
        CursorRect {
            x,
            y,
            width: 1.0,
            height,
        }
    }

    fn area(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect<i32, u32> {
        PhysicalRect {
            position: PhysicalPosition::new(x, y),
            size: PhysicalSize::new(width, height),
        }
    }

    /// The primary display from manual verification: 3072x1920 at 200%.
    fn primary() -> PhysicalRect<i32, u32> {
        area(0, 0, 3072, 1920)
    }

    /// The secondary display from manual verification: above and to the right, at 100%.
    fn secondary() -> PhysicalRect<i32, u32> {
        area(1478, -1080, 1920, 1080)
    }

    #[test]
    fn sits_directly_below_the_caret_line_when_there_is_room() {
        let placed = place(caret(500.0, 600.0, 34.0), OVERLAY, Some(primary()));
        assert_eq!(placed, PhysicalPosition::new(500, 634));
    }

    #[test]
    fn flips_above_the_caret_when_the_space_below_is_too_short() {
        // Caret near the bottom: 1794 + 34 + 240 overruns the 1920 edge.
        let placed = place(caret(500.0, 1794.0, 34.0), OVERLAY, Some(primary()));
        assert_eq!(placed, PhysicalPosition::new(500, 1794 - 240));
    }

    #[test]
    fn exact_fit_below_stays_below() {
        // 1646 + 34 + 240 == 1920 exactly, so the window still fits underneath.
        let placed = place(caret(0.0, 1646.0, 34.0), OVERLAY, Some(primary()));
        assert_eq!(placed, PhysicalPosition::new(0, 1680));
    }

    #[test]
    fn clamps_back_from_the_right_edge() {
        // The regression this exists for: a caret at 3240 on a display ending at 3398 put
        // 202 of the overlay's 360 logical pixels off the desktop.
        let placed = place(caret(3240.0, -193.0, 20.0), OVERLAY, Some(secondary()));
        assert_eq!(placed.x, 1478 + 1920 - 720);
        assert!(placed.x < 3240);
    }

    #[test]
    fn clamps_up_to_the_left_edge_of_its_own_display() {
        let placed = place(caret(1500.0, -500.0, 20.0), OVERLAY, Some(secondary()));
        assert_eq!(placed.x, 1500);
        let further_left = place(caret(1000.0, -500.0, 20.0), OVERLAY, Some(secondary()));
        assert_eq!(further_left.x, 1478);
    }

    #[test]
    fn respects_a_display_whose_origin_is_negative() {
        let placed = place(caret(1600.0, -1000.0, 20.0), OVERLAY, Some(secondary()));
        assert_eq!(placed, PhysicalPosition::new(1600, -980));
        assert!(placed.y >= -1080);
    }

    #[test]
    fn a_window_wider_than_the_work_area_lands_on_its_left_edge() {
        let narrow = area(100, 100, 400, 2000);
        let placed = place(caret(300.0, 200.0, 20.0), OVERLAY, Some(narrow));
        assert_eq!(placed.x, 100);
    }

    #[test]
    fn a_window_taller_than_the_work_area_lands_on_its_top_edge() {
        let short = area(100, 100, 2000, 200);
        let placed = place(caret(300.0, 150.0, 20.0), OVERLAY, Some(short));
        assert_eq!(placed.y, 100);
    }

    #[test]
    fn without_a_work_area_it_falls_back_to_the_unclamped_position() {
        let placed = place(caret(3240.0, -193.0, 20.0), OVERLAY, None);
        assert_eq!(placed, PhysicalPosition::new(3240, -173));
    }

    #[test]
    fn a_caret_reported_on_the_secondary_display_is_not_clamped_to_the_primary() {
        // Both displays overlap in x, so picking the wrong one would look plausible but wrong:
        // only the secondary's work area allows a negative y.
        let placed = place(caret(2000.0, -600.0, 34.0), OVERLAY, Some(secondary()));
        assert_eq!(placed, PhysicalPosition::new(2000, -566));
    }
}
