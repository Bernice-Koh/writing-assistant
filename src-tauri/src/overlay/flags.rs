//! The flags loop: resolves each flag's on-screen rectangles through the capture backend for the
//! overlay to render and the hover loop to hit-test.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use wa_engine::analyzer::Analyzer;
use wa_engine::capture::{Capture, CursorRect};
use wa_engine::flag::Flag;

use super::{lock, poll_interval, scale_factor, PositionedFlag, SharedOverlay};

/// Spawns a background task that resolves the on-screen positions of `analyzer`'s current flags
/// through `capture`, stores them in `shared` for `track_hover`'s hit-testing, and emits them,
/// relativised to the overlay window, as a `flags-updated` event for the overlay's own webview to
/// render.
///
/// Triggered two ways: reactively, the moment `analyzer`'s own `subscribe` channel reports a new
/// flag set, and periodically, on the same `POLL_INTERVAL` cadence
/// [`track_document_view`](super::track_document_view) already polls on. The periodic leg exists
/// because a flag's *position* can go stale for reasons that have nothing to do with the flag set
/// itself changing: the focused document scrolling, the window resizing, or focus having moved to a
/// different document entirely, whose text no longer contains a still-cached flag's anchor at all.
/// Manual verification against #48 found exactly that last case: closing a document and focusing a
/// new one left the previous document's flags rendered, misplaced, inside the newly repositioned
/// overlay window, because nothing re-resolved their positions until the analyzer's own debounced
/// recheck eventually caught up, seconds later. Re-resolving on every poll tick, not only on a
/// flag-set change, means a stale or momentarily unresolvable position self-heals within one
/// `POLL_INTERVAL` instead of lingering, since `resolve_positions` already drops any flag whose
/// anchor cannot currently be found.
pub fn track_flags(
    app: AppHandle,
    capture: Arc<dyn Capture>,
    analyzer: Arc<Analyzer>,
    shared: SharedOverlay,
) {
    tauri::async_runtime::spawn(async move {
        let mut updates = analyzer.subscribe();
        let mut interval = poll_interval();
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
            lock(&shared).flags = positioned;
            let relative = relative_flags(&app, &shared);
            if let Err(error) = app.emit("flags-updated", &relative) {
                log::warn!("failed to emit flags-updated: {error}");
            }
        }
    });
}

/// Resolves each of `flags`' on-screen rectangles through `capture`, in absolute screen
/// coordinates, each at the occurrence of its anchor that its span's index names. A flag whose
/// span cannot currently be resolved on screen (`span_rect` errors, or returns no rectangles) is
/// dropped rather than rendered with no known position. The usual causes are a span scrolled out
/// of view, which UI Automation answers with no rectangles, and a document edited since the
/// analyzer checked it, which no longer holds the anchor, or no longer holds it as many times as
/// the index counts.
async fn resolve_positions(capture: &Arc<dyn Capture>, flags: Vec<Flag>) -> Vec<PositionedFlag> {
    let mut positioned = Vec::with_capacity(flags.len());
    for flag in flags {
        match capture.span_rect(&flag.span).await {
            Ok(rects) if !rects.is_empty() => positioned.push(PositionedFlag { flag, rects }),
            Ok(_) => log::debug!("flag {} resolved to no on-screen rectangles", flag.id),
            Err(error) => {
                log::debug!("could not resolve an on-screen position for a flag: {error}");
            }
        }
    }
    positioned
}

/// `shared`'s positioned flags, expressed relative to the document-view rectangle the overlay
/// window was last placed at and divided down into CSS pixels: the coordinate space the window's
/// own webview renders in, since the window sits exactly at that rectangle's origin. Used both by
/// [`track_flags`]'s own emit and by the `get_current_flags` Tauri command, so a freshly mounted
/// frontend and a live update agree on the same coordinate space. Empty while the window has not
/// been placed yet, since a position relative to an unknown origin cannot be usefully rendered.
///
/// The division by the overlay window's scale factor is what makes this a function rather than a
/// subtraction the frontend could do for itself. UI Automation reports physical screen pixels; a
/// webview lays out in CSS pixels, which are physical pixels divided by the display's scale
/// factor. Rendering a physical offset as a CSS offset draws every underline at `scale_factor`
/// times its true distance from the window's origin, so at 200% scaling, twice: the symptom #48's
/// manual verification found against both Notepad and Word, with the overlay window itself
/// correctly placed, since `track_document_view` positions that in physical pixels through
/// `PhysicalPosition`.
pub fn relative_flags(app: &AppHandle, shared: &SharedOverlay) -> Vec<PositionedFlag> {
    let (view, absolute) = {
        let state = lock(shared);
        (state.view, state.flags.clone())
    };
    let Some(origin) = view else {
        return Vec::new();
    };
    let scale = app
        .get_webview_window("overlay")
        .map_or(1.0, |window| scale_factor(&window));
    to_relative(&absolute, origin, scale)
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{positioned, rect};
    use super::*;

    #[test]
    fn offset_shifts_a_rect_by_the_given_delta() {
        assert_eq!(
            offset(rect(500.0, 600.0, 30.0, 18.0), -100.0, -200.0),
            rect(400.0, 400.0, 30.0, 18.0)
        );
    }

    #[test]
    fn to_relative_subtracts_the_origin_from_every_rect_of_every_flag() {
        let flags = vec![
            positioned("a", vec![rect(150.0, 250.0, 40.0, 20.0)]),
            positioned(
                "b",
                vec![
                    rect(160.0, 400.0, 20.0, 20.0),
                    rect(100.0, 420.0, 10.0, 20.0),
                ],
            ),
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
        let flags = vec![positioned("a", vec![rect(239.0, 233.0, 336.0, 34.0)])];
        let relative = to_relative(&flags, rect(-1.0, 133.0, 3074.0, 1628.0), 2.0);
        assert_eq!(relative[0].rects, vec![rect(120.0, 50.0, 168.0, 17.0)]);
    }
}
