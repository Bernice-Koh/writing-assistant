// Release builds detach from the console subsystem so launching the app does not open a
// terminal window alongside it. Debug builds keep the console for log output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    writing_assistant::run()
}
