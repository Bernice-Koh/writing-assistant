//! The hover loop: hit-tests the pointer against the underlines and toggles the window's
//! click-through state.

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use wa_engine::capture::CursorRect;

use super::{lock, poll_interval, SharedOverlay};

/// Spawns a background task that polls the real cursor position and hit-tests it against
/// `shared`'s cached flag rectangles, toggling the overlay window's click-through state so the
/// window is transparent to clicks and keystrokes everywhere except directly over an underline.
/// A fully click-through window would never receive the hover in the first place, which is why
/// this toggles rather than leaving the window permanently click-through, per #42's decision.
/// Emits `flag-hovered` (carrying the flag id) on entering an underline's rect and
/// `flag-unhovered` on leaving it, so the frontend knows which card, if any, to show.
pub fn track_hover(app: AppHandle, shared: SharedOverlay) {
    tauri::async_runtime::spawn(async move {
        let mut interval = poll_interval();
        let mut hovered: Option<String> = None;
        loop {
            interval.tick().await;
            let mut point = POINT { x: 0, y: 0 };
            // SAFETY: `point` is a valid, uniquely-owned out parameter for the duration of this
            // call; GetCursorPos takes no other input.
            if unsafe { GetCursorPos(&raw mut point) }.is_err() {
                continue;
            }
            let hit = lock(&shared)
                .flags
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
    use super::super::test_support::rect;
    use super::*;

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
