//! Desktop-wide focus-change registration. Focus-changed events are system-wide in UIA; there
//! is no narrower scope to ask for.

use std::sync::Arc;

use windows::Win32::UI::Accessibility::IUIAutomationElement;

/// Invoked on the UIA callback thread for each desktop-wide focus change. Must be
/// `Send + Sync` because UIA delivers on library-managed apartment threads.
pub type FocusCallback = Arc<dyn Fn(&IUIAutomationElement) + Send + Sync>;

pub use handler::FocusHandler;

/// The `#[implement]`-generated COM object lives in its own module so the macro's generated
/// glue (which uses patterns the pedantic lint group flags) doesn't force an allow onto
/// hand-written code elsewhere in this module.
mod handler {
    #![allow(clippy::inline_always, clippy::ref_as_ptr)]

    use windows::Win32::UI::Accessibility::{
        IUIAutomationElement, IUIAutomationFocusChangedEventHandler_Impl,
    };
    use windows_core::implement;

    use super::FocusCallback;

    #[implement(windows::Win32::UI::Accessibility::IUIAutomationFocusChangedEventHandler)]
    pub struct FocusHandler {
        pub callback: FocusCallback,
    }

    impl IUIAutomationFocusChangedEventHandler_Impl for FocusHandler_Impl {
        fn HandleFocusChangedEvent(
            &self,
            sender: windows_core::Ref<IUIAutomationElement>,
        ) -> windows_core::Result<()> {
            log::debug!("HandleFocusChangedEvent invoked by UIA");
            if let Some(element) = sender.as_ref() {
                (self.callback)(element);
            }
            Ok(())
        }
    }
}
