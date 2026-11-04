fn main() {
    // The interface (Tauri) exists on Windows and macOS for now: on Linux,
    // the app stays on the command line until F7 (no webkit to build with).
    #[cfg(any(windows, target_os = "macos"))]
    tauri_build::build();
}
