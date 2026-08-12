//! Clipboard paste: the universal last resort. Writes UTF-16 text to the system clipboard as
//! `CF_UNICODETEXT`, then simulates Ctrl+V via SendInput.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

use super::super::error::NativeCaptureError;
use super::synthetic;

pub fn paste_text(text: &str) -> Result<(), NativeCaptureError> {
    set_clipboard_text(text)?;
    synthetic::key_combo_ctrl_v()
}

fn set_clipboard_text(text: &str) -> Result<(), NativeCaptureError> {
    // SAFETY: no window handle needed for a background clipboard write.
    unsafe { OpenClipboard(None) }?;
    let result = write_clipboard_data(text);
    // SAFETY: closes the clipboard opened immediately above, regardless of write_clipboard_data's outcome.
    let _ = unsafe { CloseClipboard() };
    result
}

fn write_clipboard_data(text: &str) -> Result<(), NativeCaptureError> {
    // EmptyClipboard first: every following early return is then only a "we hold the
    // clipboard but never allocated anything" case, so there is no path here that allocates a
    // global handle and then returns without either freeing it or handing ownership to
    // SetClipboardData.
    unsafe { EmptyClipboard() }?;

    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = units.len() * size_of::<u16>();
    // SAFETY: a fresh, appropriately sized moveable allocation for the CF_UNICODETEXT handle.
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) }
        .map_err(|_| NativeCaptureError::ClipboardAlloc)?;

    // SAFETY: `handle` was just allocated above with room for `units`; unlocked before
    // SetClipboardData takes ownership. This crate's bindings don't expose GlobalFree at all,
    // so the locked.is_null() early return leaks `handle` rather than freeing it, the same
    // small, bounded, rare-path leak already accepted below on SetClipboardData failure, not a
    // double free either way.
    let locked = unsafe { GlobalLock(handle) };
    if locked.is_null() {
        return Err(NativeCaptureError::ClipboardAlloc);
    }
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), locked.cast(), units.len()) };
    let _ = unsafe { GlobalUnlock(handle) };

    // SAFETY: `handle` is a valid GMEM_MOVEABLE allocation; ownership passes to the OS on
    // success. On failure here, `handle` leaks, a small, bounded, rare-path leak rather than a
    // double free, out of scope to fully close for this spike.
    unsafe { SetClipboardData(CF_UNICODETEXT.0.into(), Some(HANDLE(handle.0))) }?;
    Ok(())
}
