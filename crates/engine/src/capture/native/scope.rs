//! What the capture thread does with the focused element it has resolved: move the scope to it,
//! or keep the scope it has, and whether it raises the change notification.
//!
//! Since #157 the foreground watch re-resolves the focused element after a foreground change that
//! no focus event followed, and a switch to another window and back resolves the element already
//! scoped. Tearing that scope down and registering it again raised the notification, which cost
//! one whole read of the document and one re-resolution of every flag for a document that had not
//! changed (#167). A focus event that resolves the scoped element is a different case: Notepad
//! shows a newly selected tab's text in the same document element, so the element is unchanged and
//! its text is a new document to check.

use windows::Win32::UI::Accessibility::{IUIAutomation, IUIAutomationElement};

/// Where a resolution of the focused element came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Resolution {
    /// A focus event, or the capture thread's start, which resolves whatever already has focus.
    FocusEvent,
    /// The foreground watch, after a foreground change that no focus event followed.
    ForegroundWatch,
}

/// What the capture thread does with a resolved element.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ScopeAction {
    /// Keep the scope and its registration, and raise nothing: nothing in the document changed.
    Keep,
    /// Keep the registration and raise the change notification, since the element may show
    /// another document.
    KeepAndNotify,
    /// Remove the scope's registration, register the resolved element, and raise the change
    /// notification.
    Move,
}

/// The action for an element that `same_element` says is, or is not, the scoped one, resolved
/// from `source`. An element is never the scoped one while there is no scope.
pub(super) fn decide(same_element: bool, source: Resolution) -> ScopeAction {
    match (same_element, source) {
        (true, Resolution::ForegroundWatch) => ScopeAction::Keep,
        (true, Resolution::FocusEvent) => ScopeAction::KeepAndNotify,
        (false, _) => ScopeAction::Move,
    }
}

/// Whether `scoped` and `resolved` are one element by UI Automation's own comparison, which
/// compares their runtime ids, since two reads of one element give two element objects. A failed
/// comparison reads as two elements, so the scope moves, as it did before the comparison.
pub(super) fn same_element(
    client: &IUIAutomation,
    scoped: &IUIAutomationElement,
    resolved: &IUIAutomationElement,
) -> bool {
    // SAFETY: `client`, `scoped` and `resolved` are live and owned by the calling thread.
    match unsafe { client.CompareElements(scoped, resolved) } {
        Ok(same) => same.as_bool(),
        Err(error) => {
            log::debug!(
                "could not compare the focused element with the scoped one, so the scope moves: \
                 {error}"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scoped_element_resolved_by_the_foreground_watch_keeps_the_scope_and_raises_nothing() {
        assert_eq!(decide(true, Resolution::ForegroundWatch), ScopeAction::Keep);
    }

    #[test]
    fn the_scoped_element_resolved_by_a_focus_event_keeps_the_registration_and_notifies() {
        assert_eq!(
            decide(true, Resolution::FocusEvent),
            ScopeAction::KeepAndNotify
        );
    }

    #[test]
    fn another_element_resolved_by_the_foreground_watch_moves_the_scope() {
        assert_eq!(
            decide(false, Resolution::ForegroundWatch),
            ScopeAction::Move
        );
    }

    #[test]
    fn another_element_resolved_by_a_focus_event_moves_the_scope() {
        assert_eq!(decide(false, Resolution::FocusEvent), ScopeAction::Move);
    }
}
