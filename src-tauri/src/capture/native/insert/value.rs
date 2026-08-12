//! Value-set: the cheapest insertion path, replacing an element's entire value via
//! `IUIAutomationValuePattern`. Works only on simple controls that expose the pattern and
//! aren't read-only; the caller degrades to synthetic input when this fails.
//!
//! `SetValue` fully replaces the element's value. If verification elsewhere finds this didn't
//! land correctly, the field's prior content is not guaranteed to still be there: an accepted
//! risk for a spike run against scratch documents, not something solved here with a rollback.

use windows::core::BSTR;
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationValuePattern, UIA_ValuePatternId,
};

use super::super::error::NativeCaptureError;

pub fn set_value(element: &IUIAutomationElement, text: &str) -> Result<(), NativeCaptureError> {
    // SAFETY: `element` is a live element from Uia::focused_element; GetCurrentPatternAs fails
    // safely (Err) when the pattern is unsupported, mapped to NoValuePattern below.
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
            .map_err(|_| NativeCaptureError::NoValuePattern)?;
    // SAFETY: `pattern` was just obtained from a live element.
    if unsafe { pattern.CurrentIsReadOnly() }?.as_bool() {
        return Err(NativeCaptureError::ReadOnly);
    }
    // SAFETY: `pattern` is live. `SetValue`'s parameter is generic over `Param<PCWSTR>`,
    // implemented for `&BSTR`/`&HSTRING`/`PWSTR` but not `&str` directly, so the conversion is
    // spelled out rather than left to a bare `.into()`, which is ambiguous between `BSTR` and
    // `HSTRING` (both valid `Param<PCWSTR>` targets) and fails to compile without a concrete
    // type to infer against.
    unsafe { pattern.SetValue(&BSTR::from(text)) }?;
    Ok(())
}
