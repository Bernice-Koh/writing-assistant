//! The hover loop: hit-tests the pointer against the underlines and against the card shown for
//! the hovered flag, whose rectangle the frontend reports.

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use wa_engine::capture::CursorRect;

use super::{lock, poll_interval, scale_factor, PositionedFlag, SharedOverlay, ShownCard};

/// A rectangle in CSS pixels relative to the overlay window's own top-left, as the frontend
/// measures the shown card with `getBoundingClientRect`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub struct CssRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Records the card the frontend rendered in `window` for the flag `flag_id`, at `rect`, in the
/// screen coordinates `track_hover` hit-tests in, so the hover holds while the pointer is on the
/// card. Converted with the window's own position and scale factor at the time of the report.
pub fn record_card(window: &WebviewWindow, shared: &SharedOverlay, flag_id: String, rect: CssRect) {
    let origin = match window.inner_position() {
        Ok(origin) => origin,
        Err(error) => {
            log::warn!("could not read the overlay window's position for the card: {error}");
            return;
        }
    };
    let rect = card_on_screen(rect, origin, scale_factor(window));
    lock(shared).card = Some(ShownCard { flag_id, rect });
}

/// `rect`, in CSS pixels relative to a window whose client area starts at `origin` in physical
/// screen pixels, in absolute physical screen pixels: the inverse of `flags::to_relative`.
fn card_on_screen(rect: CssRect, origin: PhysicalPosition<i32>, scale: f64) -> CursorRect {
    CursorRect {
        x: f64::from(origin.x) + rect.x * scale,
        y: f64::from(origin.y) + rect.y * scale,
        width: rect.width * scale,
        height: rect.height * scale,
    }
}

/// Spawns a background task that polls the real cursor position and hit-tests it against
/// `shared`'s cached flag rectangles and shown card, toggling the overlay window's click-through
/// state so the window is transparent to clicks and keystrokes everywhere except over an
/// underline or the card. A fully click-through window would never receive the hover in the first
/// place, which is why this toggles rather than leaving the window permanently click-through, per
/// #42's decision. Emits `flag-hovered` (carrying the flag id) on entering an underline's rect and
/// `flag-unhovered` on leaving both the hovered flag's rects and its card, so the frontend knows
/// which card, if any, to show, and the pointer can move from the underline onto the card.
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
            let hit = {
                let mut state = lock(&shared);
                let hit = hover_target(
                    f64::from(point.x),
                    f64::from(point.y),
                    &state.flags,
                    hovered.as_deref(),
                    state.card.as_ref(),
                );
                // The card shown was the previously hovered flag's.
                if hit != hovered {
                    state.card = None;
                }
                hit
            };

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

/// The id of the flag the pointer at (`x`, `y`) hovers. The `hovered` flag holds while it is still
/// positioned and the pointer is inside one of its rectangles or inside the `card` shown for it,
/// which takes precedence over another flag's underline the card covers. Otherwise it is the
/// first flag with a rectangle under the pointer, or none.
fn hover_target(
    x: f64,
    y: f64,
    flags: &[PositionedFlag],
    hovered: Option<&str>,
    card: Option<&ShownCard>,
) -> Option<String> {
    let under_pointer =
        |positioned: &&PositionedFlag| positioned.rects.iter().any(|rect| contains(rect, x, y));
    if let Some(id) = hovered {
        if let Some(current) = flags.iter().find(|positioned| positioned.flag.id == id) {
            let on_card = card.is_some_and(|card| card.flag_id == id && contains(&card.rect, x, y));
            if on_card || under_pointer(&current) {
                return Some(id.to_owned());
            }
        }
    }
    flags
        .iter()
        .find(under_pointer)
        .map(|positioned| positioned.flag.id.clone())
}

fn contains(rect: &CursorRect, x: f64, y: f64) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

#[cfg(test)]
mod tests {
    use super::super::flags::to_relative;
    use super::super::test_support::{positioned, rect};
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

    #[test]
    fn card_on_screen_scales_and_offsets_by_the_windows_client_origin() {
        // At 200%, a card 60 CSS pixels down a window whose client area starts at (-1, 133)
        // physical sits 120 physical pixels below that origin.
        let css = CssRect {
            x: 120.0,
            y: 60.0,
            width: 200.0,
            height: 80.0,
        };
        assert_eq!(
            card_on_screen(css, PhysicalPosition::new(-1, 133), 2.0),
            rect(239.0, 253.0, 400.0, 160.0)
        );
    }

    #[test]
    fn card_on_screen_inverts_to_relative() {
        let origin = rect(-1.0, 133.0, 3074.0, 1628.0);
        let screen = rect(239.0, 233.0, 336.0, 34.0);
        let relative = to_relative(&[positioned("a", vec![screen])], origin, 2.0)[0].rects[0];
        let css = CssRect {
            x: relative.x,
            y: relative.y,
            width: relative.width,
            height: relative.height,
        };
        assert_eq!(
            card_on_screen(css, PhysicalPosition::new(-1, 133), 2.0),
            screen
        );
    }

    /// Flag "a" underlined at y 100 to 120, flag "b" at y 200 to 220, and the card of "a" below
    /// its underline, from y 120 to 300, covering the underline of "b".
    fn hover_scene() -> (Vec<PositionedFlag>, ShownCard) {
        let flags = vec![
            positioned("a", vec![rect(100.0, 100.0, 50.0, 20.0)]),
            positioned("b", vec![rect(120.0, 200.0, 40.0, 20.0)]),
        ];
        let card = ShownCard {
            flag_id: "a".to_owned(),
            rect: rect(100.0, 120.0, 300.0, 180.0),
        };
        (flags, card)
    }

    #[test]
    fn the_pointer_on_an_underline_hovers_its_flag() {
        let (flags, _) = hover_scene();
        assert_eq!(
            hover_target(110.0, 110.0, &flags, None, None),
            Some("a".to_owned())
        );
    }

    #[test]
    fn the_pointer_moving_from_the_underline_onto_the_card_keeps_the_hover() {
        let (flags, card) = hover_scene();
        assert_eq!(
            hover_target(300.0, 150.0, &flags, Some("a"), Some(&card)),
            Some("a".to_owned())
        );
    }

    #[test]
    fn the_card_holds_the_hover_over_another_flags_underline_it_covers() {
        let (flags, card) = hover_scene();
        assert_eq!(
            hover_target(130.0, 210.0, &flags, Some("a"), Some(&card)),
            Some("a".to_owned())
        );
    }

    #[test]
    fn the_pointer_leaving_both_the_underline_and_the_card_clears_the_hover() {
        let (flags, card) = hover_scene();
        assert_eq!(
            hover_target(600.0, 600.0, &flags, Some("a"), Some(&card)),
            None
        );
    }

    #[test]
    fn the_card_holds_nothing_before_its_flag_is_hovered() {
        let (flags, card) = hover_scene();
        assert_eq!(hover_target(300.0, 150.0, &flags, None, Some(&card)), None);
    }

    #[test]
    fn a_card_reported_for_another_flag_does_not_hold_the_hover() {
        let (flags, card) = hover_scene();
        assert_eq!(
            hover_target(300.0, 150.0, &flags, Some("b"), Some(&card)),
            None
        );
    }

    #[test]
    fn the_card_of_a_flag_no_longer_positioned_does_not_hold_the_hover() {
        let (flags, card) = hover_scene();
        let without_a = vec![flags[1].clone()];
        assert_eq!(
            hover_target(300.0, 150.0, &without_a, Some("a"), Some(&card)),
            None
        );
    }
}
