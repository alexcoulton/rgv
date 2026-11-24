// A windowed application on Windows: optimised builds open no console window behind the viewer.
// Debug builds keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    rgv_desktop::run()
}
