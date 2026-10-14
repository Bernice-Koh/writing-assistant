//! Text-change event registration, scoped to a single element's subtree at a time. The scope
//! moves with focus: `capture::native` removes the previous registration and adds a new one
//! each time focus changes, rather than accumulating one registration per element ever
//! focused.

use std::sync::Arc;

use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement, IUIAutomationEventHandler,
    TreeScope_Subtree, UIA_Text_TextChangedEventId,
};

use super::error::NativeCaptureError;

/// Invoked on the UIA callback thread for each text-change event within the registered
/// element's subtree. Must be `Send + Sync` because UIA delivers on library-managed apartment
/// threads.
pub type TextChangeCallback = Arc<dyn Fn(&IUIAutomationElement) + Send + Sync>;

pub use handler::TextChangeHandler;

/// The `#[implement]`-generated COM object lives in its own module, matching `focus`'s reason.
mod handler {
    #![allow(clippy::inline_always, clippy::ref_as_ptr)]

    use windows::Win32::UI::Accessibility::{
        IUIAutomationElement, IUIAutomationEventHandler_Impl, UIA_EVENT_ID,
    };
    use windows_core::implement;

    use super::TextChangeCallback;

    #[implement(windows::Win32::UI::Accessibility::IUIAutomationEventHandler)]
    pub struct TextChangeHandler {
        pub callback: TextChangeCallback,
    }

    impl IUIAutomationEventHandler_Impl for TextChangeHandler_Impl {
        fn HandleAutomationEvent(
            &self,
            sender: windows_core::Ref<IUIAutomationElement>,
            _eventid: UIA_EVENT_ID,
        ) -> windows_core::Result<()> {
            if let Some(element) = sender.as_ref() {
                (self.callback)(element);
            }
            Ok(())
        }
    }
}

/// Registers a text-change handler over `element`'s subtree, returning the handler so a later
/// call to [`remove`] can unregister exactly this registration.
///
/// # Safety
/// `client`, `cache`, and `element` must be live and owned by the calling thread's `Uia`.
pub unsafe fn register(
    client: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    element: &IUIAutomationElement,
    callback: TextChangeCallback,
) -> Result<IUIAutomationEventHandler, NativeCaptureError> {
    let handler: IUIAutomationEventHandler = TextChangeHandler { callback }.into();
    // SAFETY: forwarded from this function's contract.
    unsafe {
        client.AddAutomationEventHandler(
            UIA_Text_TextChangedEventId,
            element,
            TreeScope_Subtree,
            cache,
            &handler,
        )?;
    }
    Ok(handler)
}

/// Unregisters a handler previously returned by [`register`].
///
/// # Safety
/// `client`, `element`, and `handler` must be the same live values passed to and returned from
/// the matching [`register`] call.
pub unsafe fn remove(
    client: &IUIAutomation,
    element: &IUIAutomationElement,
    handler: &IUIAutomationEventHandler,
) -> Result<(), NativeCaptureError> {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        client.RemoveAutomationEventHandler(UIA_Text_TextChangedEventId, element, handler)?;
    }
    Ok(())
}
