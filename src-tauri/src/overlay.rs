//! Creates the overlay window: the always-on-top surface that will host flag callouts and
//! drift indicators once the capture layer has data to show. This spike proves the window
//! mechanics; content stays a placeholder until #10/#11 supply real data.

use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder};

// Arbitrary and deliberately not (0, 0), so it's visually distinguishable from a stray
// full-screen window during manual verification.
const INITIAL_X: f64 = 200.0;
const INITIAL_Y: f64 = 200.0;
const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 120.0;

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html".into()))
        .title("Writing Assistant Overlay")
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .position(INITIAL_X, INITIAL_Y)
        .inner_size(WIDTH, HEIGHT)
        .build()?;
    Ok(())
}
