//! The flags loop: resolves each flag's on-screen rectangles through the capture backend, and
//! re-resolves them on a change, or when a sentinel flag has moved, rather than on every tick.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::watch;

use wa_engine::analyzer::Analyzer;
use wa_engine::capture::{Capture, CaptureError, CursorRect};
use wa_engine::flag::Flag;

use super::{lock, poll_interval, scale_factor, PositionedFlag, SharedOverlay};

/// While no flag is positioned, as when every flag is scrolled out of view, the ticks between two
/// re-resolutions of every flag: once a second. With no positioned flag there is no sentinel to
/// notice a scroll, and a flag scrolled back into view is otherwise underlined only at the next
/// flag update.
const UNPOSITIONED_REFRESH_TICKS: u32 = 10;

/// Spawns a background task that resolves the on-screen positions of `analyzer`'s current flags
/// through `capture`, stores them in `shared` for `track_hover`'s hit-testing, and emits them,
/// relativised to the overlay window, as a `flags-updated` event for the overlay's own webview to
/// render.
///
/// Every flag is re-resolved, at one `span_rect` call each, on a flag update from the analyzer and
/// on a tick where the document-view rectangle changed since the last re-resolution. While the
/// text is changing, a tick that saw the capture backend's change notification re-resolves
/// nothing, the sentinel included, and the first tick that saw none after one or more that did
/// re-resolves every flag: one re-resolution per burst of typing, a tick after its last keystroke,
/// where re-resolving on every tick that saw the notification cost every tick of the burst (#167).
/// On any other tick only the sentinel, the first positioned flag, is re-resolved, and every flag
/// is re-resolved when the sentinel's rectangles moved: scrolling moves every visible flag, so a
/// scroll is noticed within one tick at the cost of one `FindText` instead of one per flag (#153
/// measured the per-flag cost on every tick at 58% of one core for 135 flags in a debug build). A
/// sentinel the document no longer holds counts as moved. While no flag is positioned, every flag
/// is re-resolved once a second. An edit between two visible flags moves the later one without
/// moving the sentinel, and the change notification it raises re-resolves every flag on the first
/// tick after the edit stops, before the analyzer's recheck after its quiet period.
///
/// The periodic leg exists because a flag's *position* can go stale for reasons that have
/// nothing to do with the flag set changing: the document scrolling, the window resizing, or focus
/// moving to a different document, whose text no longer contains a cached flag's anchor. Manual
/// verification against #48 found that last case: closing a document and focusing a new one left
/// the previous document's flags rendered, misplaced, inside the newly placed overlay window until
/// the analyzer's recheck caught up, seconds later. A document focused in place of another either
/// changes the document-view rectangle or fails the sentinel's search, so every flag is
/// re-resolved on the next tick and the stale ones are dropped.
pub fn track_flags(
    app: AppHandle,
    capture: Arc<dyn Capture>,
    analyzer: Arc<Analyzer>,
    shared: SharedOverlay,
) {
    tauri::async_runtime::spawn(async move {
        let mut updates = analyzer.subscribe();
        let mut changes = capture.changes();
        let mut interval = poll_interval();
        let mut resolved_view: Option<CursorRect> = None;
        let mut ticks_since_all: u32 = 0;
        let mut text_was_changing = false;
        loop {
            let flags_updated = tokio::select! {
                changed = updates.changed() => {
                    if changed.is_err() {
                        log::debug!(
                            "the analyzer's flag channel closed; no more flag updates to render"
                        );
                        return;
                    }
                    true
                }
                _ = interval.tick() => false,
            };
            ticks_since_all = ticks_since_all.saturating_add(1);
            let (view, sentinel) = {
                let state = lock(&shared);
                (state.view, state.flags.first().cloned())
            };
            let tick = Tick {
                flags_updated,
                text_changed: take_change(&mut changes),
                text_was_changing,
                view_changed: view != resolved_view,
                has_sentinel: sentinel.is_some(),
                has_flags: !updates.borrow().is_empty(),
                ticks_since_all,
            };
            text_was_changing = tick.text_changed;
            match refresh_for(tick) {
                Refresh::Nothing => continue,
                Refresh::Sentinel => {
                    let sentinel =
                        sentinel.expect("the sentinel is only chosen when a flag is positioned");
                    let fresh = capture.span_rect(&sentinel.flag.span).await;
                    if !sentinel_moved(&sentinel.rects, &fresh) {
                        continue;
                    }
                }
                Refresh::All => {}
            }
            ticks_since_all = 0;
            let flags = updates.borrow().clone();
            let positioned = resolve_positions(&capture, flags).await;
            lock(&shared).flags = positioned;
            resolved_view = view;
            let relative = relative_flags(&app, &shared);
            if let Err(error) = app.emit("flags-updated", &relative) {
                log::warn!("failed to emit flags-updated: {error}");
            }
        }
    });
}

/// What one tick of [`track_flags`] knows when it decides what to re-resolve.
#[derive(Debug, Clone, Copy)]
struct Tick {
    /// The analyzer published a flag set since the last tick.
    flags_updated: bool,
    /// The capture backend's change notification fired since the last tick.
    text_changed: bool,
    /// The previous tick saw the change notification.
    text_was_changing: bool,
    /// The document-view rectangle differs from the one every flag was last re-resolved against.
    view_changed: bool,
    /// At least one flag is positioned, so the first of them can serve as the sentinel.
    has_sentinel: bool,
    /// The analyzer's flag set is not empty.
    has_flags: bool,
    /// Ticks since every flag was last re-resolved, this one included.
    ticks_since_all: u32,
}

/// What a tick of [`track_flags`] re-resolves.
#[derive(Debug, PartialEq, Eq)]
enum Refresh {
    /// Every flag.
    All,
    /// The sentinel alone, and every flag when its rectangles moved.
    Sentinel,
    /// Nothing.
    Nothing,
}

/// Every flag on a flag update or a changed document view; otherwise nothing on a tick that saw
/// the change notification, and every flag on the first tick that saw none after one that did;
/// otherwise the sentinel when a flag is positioned; otherwise, while the analyzer holds flags none
/// of which is positioned, every flag once [`UNPOSITIONED_REFRESH_TICKS`] ticks have passed;
/// otherwise nothing.
fn refresh_for(tick: Tick) -> Refresh {
    if tick.flags_updated || tick.view_changed {
        Refresh::All
    } else if tick.text_changed {
        Refresh::Nothing
    } else if tick.text_was_changing {
        Refresh::All
    } else if tick.has_sentinel {
        Refresh::Sentinel
    } else if tick.has_flags && tick.ticks_since_all >= UNPOSITIONED_REFRESH_TICKS {
        Refresh::All
    } else {
        Refresh::Nothing
    }
}

/// Whether the change notification fired since the last call, marking it seen, so any number of
/// notifications between two ticks is one refresh. A closed channel reads as no change.
fn take_change(changes: &mut watch::Receiver<u64>) -> bool {
    let changed = changes.has_changed().unwrap_or(false);
    if changed {
        changes.borrow_and_update();
    }
    changed
}

/// Whether the sentinel's `fresh` rectangles differ from those it was positioned at, an error,
/// such as an anchor the document no longer holds, counting as a move.
fn sentinel_moved(
    positioned: &[CursorRect],
    fresh: &Result<Vec<CursorRect>, CaptureError>,
) -> bool {
    match fresh {
        Ok(rects) => rects.as_slice() != positioned,
        Err(_) => true,
    }
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

pub(super) fn to_relative(
    flags: &[PositionedFlag],
    origin: CursorRect,
    scale: f64,
) -> Vec<PositionedFlag> {
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

    fn idle() -> Tick {
        Tick {
            flags_updated: false,
            text_changed: false,
            text_was_changing: false,
            view_changed: false,
            has_sentinel: true,
            has_flags: true,
            ticks_since_all: 1,
        }
    }

    /// A tick inside a burst of typing: this tick and the one before it saw the notification.
    fn typing() -> Tick {
        Tick {
            text_changed: true,
            text_was_changing: true,
            ..idle()
        }
    }

    #[test]
    fn a_flag_update_re_resolves_every_flag() {
        let tick = Tick {
            flags_updated: true,
            ..idle()
        };
        assert_eq!(refresh_for(tick), Refresh::All);
    }

    #[test]
    fn a_tick_that_saw_the_notification_re_resolves_nothing_not_even_the_sentinel() {
        let first = Tick {
            text_changed: true,
            ..idle()
        };
        assert_eq!(refresh_for(first), Refresh::Nothing);
        assert_eq!(refresh_for(typing()), Refresh::Nothing);
    }

    #[test]
    fn a_tick_that_saw_the_notification_skips_the_once_a_second_refresh_too() {
        let tick = Tick {
            has_sentinel: false,
            ticks_since_all: UNPOSITIONED_REFRESH_TICKS,
            ..typing()
        };
        assert_eq!(refresh_for(tick), Refresh::Nothing);
    }

    #[test]
    fn the_first_tick_without_the_notification_after_one_with_it_re_resolves_every_flag() {
        let tick = Tick {
            text_changed: false,
            ..typing()
        };
        assert_eq!(refresh_for(tick), Refresh::All);
    }

    #[test]
    fn a_flag_update_during_a_run_of_notifications_re_resolves_every_flag() {
        let tick = Tick {
            flags_updated: true,
            ..typing()
        };
        assert_eq!(refresh_for(tick), Refresh::All);
    }

    #[test]
    fn a_view_change_during_a_run_of_notifications_re_resolves_every_flag() {
        let tick = Tick {
            view_changed: true,
            ..typing()
        };
        assert_eq!(refresh_for(tick), Refresh::All);
    }

    #[test]
    fn a_burst_re_resolves_nothing_while_it_lasts_every_flag_once_after_and_then_the_sentinel() {
        // Each tick's `text_was_changing` is the previous tick's `text_changed`, as in the loop.
        let seen = [true, true, true, false, false, false];
        let mut text_was_changing = false;
        let refreshes: Vec<Refresh> = seen
            .iter()
            .map(|&text_changed| {
                let tick = Tick {
                    text_changed,
                    text_was_changing,
                    ..idle()
                };
                text_was_changing = text_changed;
                refresh_for(tick)
            })
            .collect();
        assert_eq!(
            refreshes,
            vec![
                Refresh::Nothing,
                Refresh::Nothing,
                Refresh::Nothing,
                Refresh::All,
                Refresh::Sentinel,
                Refresh::Sentinel,
            ]
        );
    }

    #[test]
    fn notifications_between_two_ticks_are_one_refresh() {
        let (sender, mut changes) = watch::channel(0_u64);
        assert!(!take_change(&mut changes), "no notification yet");
        for _ in 0..3 {
            sender.send_modify(|count| *count += 1);
        }
        assert!(
            take_change(&mut changes),
            "the first tick after them refreshes"
        );
        assert!(!take_change(&mut changes), "the next tick does not");
        sender.send_modify(|count| *count += 1);
        assert!(
            take_change(&mut changes),
            "a later notification refreshes again"
        );
        drop(sender);
        assert!(!take_change(&mut changes), "a closed channel is no change");
    }

    #[test]
    fn a_changed_document_view_re_resolves_every_flag() {
        let tick = Tick {
            view_changed: true,
            ..idle()
        };
        assert_eq!(refresh_for(tick), Refresh::All);
    }

    #[test]
    fn an_idle_tick_re_resolves_the_sentinel_alone() {
        assert_eq!(refresh_for(idle()), Refresh::Sentinel);
    }

    #[test]
    fn an_idle_tick_with_no_flags_re_resolves_nothing() {
        let tick = Tick {
            has_sentinel: false,
            has_flags: false,
            ticks_since_all: UNPOSITIONED_REFRESH_TICKS,
            ..idle()
        };
        assert_eq!(refresh_for(tick), Refresh::Nothing);
    }

    #[test]
    fn with_no_flag_positioned_every_flag_is_re_resolved_once_a_second() {
        let waiting = Tick {
            has_sentinel: false,
            ticks_since_all: UNPOSITIONED_REFRESH_TICKS - 1,
            ..idle()
        };
        assert_eq!(refresh_for(waiting), Refresh::Nothing);
        let due = Tick {
            ticks_since_all: UNPOSITIONED_REFRESH_TICKS,
            ..waiting
        };
        assert_eq!(refresh_for(due), Refresh::All);
    }

    #[test]
    fn a_sentinel_at_its_positioned_rectangles_has_not_moved() {
        let at = vec![rect(100.0, 100.0, 50.0, 20.0)];
        assert!(!sentinel_moved(&at, &Ok(at.clone())));
    }

    #[test]
    fn a_sentinel_scrolled_up_a_line_has_moved() {
        let at = vec![rect(100.0, 100.0, 50.0, 20.0)];
        assert!(sentinel_moved(
            &at,
            &Ok(vec![rect(100.0, 80.0, 50.0, 20.0)])
        ));
    }

    #[test]
    fn a_sentinel_scrolled_out_of_view_has_moved() {
        let at = vec![rect(100.0, 100.0, 50.0, 20.0)];
        assert!(sentinel_moved(&at, &Ok(Vec::new())));
    }

    #[test]
    fn a_sentinel_the_document_no_longer_holds_has_moved() {
        let at = vec![rect(100.0, 100.0, 50.0, 20.0)];
        assert!(sentinel_moved(&at, &Err(CaptureError::TextNotFound)));
    }
}
