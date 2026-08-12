//! Synthetic input: keystroke simulation via `SendInput` with `KEYEVENTF_UNICODE`, injecting
//! into the real system input queue rather than posting window messages. Modern XAML/UWP-hosted
//! controls, Windows 11's Notepad among them, require this, since their input dispatcher only
//! consumes the system queue, not posted messages. Delivers to whichever element currently
//! holds real OS keyboard focus, matching this project's case, where the user is always
//! actively focused on the target when a correction is applied.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_DELETE,
    VK_LEFT, VK_SHIFT, VK_V,
};

use super::super::error::NativeCaptureError;

pub fn type_text(text: &str) -> Result<(), NativeCaptureError> {
    let inputs: Vec<INPUT> = text
        .encode_utf16()
        .flat_map(|unit| {
            [
                unicode_key_input(unit, false),
                unicode_key_input(unit, true),
            ]
        })
        .collect();
    send(&inputs)
}

/// Simulates the Ctrl+V paste shortcut: Ctrl down, V down, V up, Ctrl up.
pub fn key_combo_ctrl_v() -> Result<(), NativeCaptureError> {
    let inputs = [
        virtual_key_input(VK_CONTROL, false, false),
        virtual_key_input(VK_V, false, false),
        virtual_key_input(VK_V, true, false),
        virtual_key_input(VK_CONTROL, true, false),
    ];
    send(&inputs)
}

/// Selects `count` characters immediately to the left of the cursor by holding Shift and
/// pressing Left Arrow `count` times. Pure keyboard simulation, so it works anywhere
/// `type_text` does, including targets where `select::select_text`'s accessibility-tree-based
/// `FindText` search is unreliable: Google Docs' canvas-rendered editing surface exposes a
/// "side DOM" for accessibility that only partially backs UIA's text-search and value-set
/// operations, while real keyboard input reaches its own event handlers directly. Meant to be
/// called right after typing or pasting `count` characters, so the selection covers exactly
/// what was just inserted.
pub fn select_left(count: u32) -> Result<(), NativeCaptureError> {
    let mut inputs = Vec::with_capacity((count as usize + 1) * 2);
    inputs.push(virtual_key_input(VK_SHIFT, false, false));
    for _ in 0..count {
        inputs.push(virtual_key_input(VK_LEFT, false, true));
        inputs.push(virtual_key_input(VK_LEFT, true, true));
    }
    inputs.push(virtual_key_input(VK_SHIFT, true, false));
    send(&inputs)
}

/// Presses Delete: a single VK_DELETE down/up pair. Used to remove whatever fragment an
/// interrupted `type_text` call actually landed (see `insert::restore_after_failed_attempt`),
/// so the next cascade stage starts from a clean, known state instead of pasting on top of a
/// stray leftover.
pub fn delete_selection() -> Result<(), NativeCaptureError> {
    let inputs = [
        virtual_key_input(VK_DELETE, false, true),
        virtual_key_input(VK_DELETE, true, true),
    ];
    send(&inputs)
}

/// Pure: builds one key-down or key-up `INPUT` for a UTF-16 code unit. No `unsafe`: reading a
/// union field back out needs `unsafe`, but writing one to construct a value does not, which is
/// what keeps this testable without calling `SendInput` at all.
fn unicode_key_input(code_unit: u16, key_up: bool) -> INPUT {
    let flags = if key_up {
        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
    } else {
        KEYEVENTF_UNICODE
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: code_unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Pure: builds one key-down or key-up `INPUT` for a real virtual key, for shortcuts like
/// Ctrl+V where the OS must recognise the actual key rather than a literal Unicode character.
/// `extended` marks navigation keys (arrows, Home/End, Insert/Delete, and similar) that Win32
/// requires `KEYEVENTF_EXTENDEDKEY` for, to disambiguate them from their numpad equivalents.
fn virtual_key_input(vk: VIRTUAL_KEY, key_up: bool, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), NativeCaptureError> {
    // SAFETY: `inputs` is a live, correctly sized slice of INPUT built by this module.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(NativeCaptureError::SendInputIncomplete {
            sent,
            expected: inputs.len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_down_carries_the_code_unit_without_the_key_up_flag() {
        let input = unicode_key_input(b'A' as u16, false);
        // SAFETY: this test wrote the `ki` field itself via unicode_key_input above.
        let ki = unsafe { input.Anonymous.ki };
        assert_eq!(ki.wScan, b'A' as u16);
        assert_eq!(ki.dwFlags, KEYEVENTF_UNICODE);
    }

    #[test]
    fn key_up_sets_both_flags() {
        let input = unicode_key_input(b'A' as u16, true);
        // SAFETY: this test wrote the `ki` field itself via unicode_key_input above.
        let ki = unsafe { input.Anonymous.ki };
        assert_eq!(ki.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
    }

    #[test]
    fn surrogate_pairs_produce_one_down_and_one_up_per_code_unit() {
        // U+1F600 (an emoji outside the BMP) encodes as two UTF-16 surrogates; each needs its
        // own down/up pair, four INPUTs total, not two.
        let text = "\u{1F600}";
        let inputs: Vec<INPUT> = text
            .encode_utf16()
            .flat_map(|unit| {
                [
                    unicode_key_input(unit, false),
                    unicode_key_input(unit, true),
                ]
            })
            .collect();
        assert_eq!(inputs.len(), 4);
    }

    #[test]
    fn virtual_key_down_carries_the_key_with_no_flags() {
        let input = virtual_key_input(VK_V, false, false);
        // SAFETY: this test wrote the `ki` field itself via virtual_key_input above.
        let ki = unsafe { input.Anonymous.ki };
        assert_eq!(ki.wVk, VK_V);
        assert_eq!(ki.dwFlags, KEYBD_EVENT_FLAGS(0));
    }

    #[test]
    fn virtual_key_up_sets_the_key_up_flag() {
        let input = virtual_key_input(VK_V, true, false);
        // SAFETY: this test wrote the `ki` field itself via virtual_key_input above.
        let ki = unsafe { input.Anonymous.ki };
        assert_eq!(ki.dwFlags, KEYEVENTF_KEYUP);
    }

    #[test]
    fn extended_key_sets_the_extended_flag() {
        let input = virtual_key_input(VK_LEFT, false, true);
        // SAFETY: this test wrote the `ki` field itself via virtual_key_input above.
        let ki = unsafe { input.Anonymous.ki };
        assert_eq!(ki.dwFlags, KEYEVENTF_EXTENDEDKEY);
    }

    #[test]
    fn delete_selection_is_a_single_extended_key_down_up_pair() {
        let inputs = [
            virtual_key_input(VK_DELETE, false, true),
            virtual_key_input(VK_DELETE, true, true),
        ];
        assert_eq!(inputs.len(), 2);
        // SAFETY: this test wrote the `ki` field on every element via virtual_key_input above.
        let down = unsafe { inputs[0].Anonymous.ki };
        assert_eq!(down.wVk, VK_DELETE);
        assert_eq!(down.dwFlags, KEYEVENTF_EXTENDEDKEY);
        // SAFETY: this test wrote the `ki` field on every element via virtual_key_input above.
        let up = unsafe { inputs[1].Anonymous.ki };
        assert_eq!(up.wVk, VK_DELETE);
        assert_eq!(up.dwFlags, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
    }

    #[test]
    fn select_left_wraps_the_arrow_presses_in_one_shift_hold() {
        // Shift down, then (Left down, Left up) x count, then Shift up: count=3 selection
        // should be a single Shift press bracketing exactly 3 arrow-key pairs, not one Shift
        // toggle per arrow key (which would only ever select or deselect one character).
        let count = 3;
        let expected_len = 2 + (count as usize) * 2;
        let mut inputs = Vec::with_capacity(expected_len);
        inputs.push(virtual_key_input(VK_SHIFT, false, false));
        for _ in 0..count {
            inputs.push(virtual_key_input(VK_LEFT, false, true));
            inputs.push(virtual_key_input(VK_LEFT, true, true));
        }
        inputs.push(virtual_key_input(VK_SHIFT, true, false));
        assert_eq!(inputs.len(), expected_len);
        // SAFETY: this test wrote the `ki` field on every element via virtual_key_input above.
        assert_eq!(
            unsafe { inputs.first().unwrap().Anonymous.ki }.wVk,
            VK_SHIFT
        );
        // SAFETY: this test wrote the `ki` field on every element via virtual_key_input above.
        assert_eq!(unsafe { inputs.last().unwrap().Anonymous.ki }.wVk, VK_SHIFT);
    }
}
