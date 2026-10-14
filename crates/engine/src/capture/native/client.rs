//! The UI Automation client handle: joins the calling thread to the multithreaded apartment
//! and creates an `IUIAutomation` client bound to it.

use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
};

use super::error::NativeCaptureError;

/// `RPC_E_CHANGED_MODE`: this thread already joined a different apartment kind. Harmless: a
/// UIA client works from a thread already in some apartment, so this is treated as success.
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106_u32.cast_signed();

pub struct Uia {
    client: IUIAutomation,
}

impl Uia {
    pub fn new() -> Result<Self, NativeCaptureError> {
        // SAFETY: CoInitializeEx with no reserved pointer is always sound; the HRESULT is
        // inspected rather than assumed successful.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if !(hr.is_ok() || hr.0 == RPC_E_CHANGED_MODE) {
            hr.ok()?;
        }
        // SAFETY: CUIAutomation8 is a registered in-process COM server; CUIAutomation8 rather
        // than the older CUIAutomation because only the former's objects implement the newer
        // client interfaces this module relies on.
        let client = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };
        Ok(Self { client })
    }

    pub fn client(&self) -> &IUIAutomation {
        &self.client
    }

    /// A cache request with no properties prefetched. Callers add whatever properties they
    /// read on the delivered element before passing this to a registration call.
    pub fn base_cache_request(&self) -> Result<IUIAutomationCacheRequest, NativeCaptureError> {
        // SAFETY: `self.client` is a live IUIAutomation instance.
        Ok(unsafe { self.client.CreateCacheRequest() }?)
    }

    /// The currently focused element, cross-process. `IUIAutomationElement` is `!Send`, so this
    /// is only ever called from the thread that owns this `Uia` client, never from a callback
    /// delivered on a different UIA thread.
    pub fn focused_element(
        &self,
        cache: &IUIAutomationCacheRequest,
    ) -> Result<IUIAutomationElement, NativeCaptureError> {
        // SAFETY: `self.client` is a live IUIAutomation instance; `cache` is a live cache
        // request built from it.
        Ok(unsafe { self.client.GetFocusedElementBuildCache(cache) }?)
    }
}
