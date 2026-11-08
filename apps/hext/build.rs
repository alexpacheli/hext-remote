fn main() {
    // The interface (Tauri) exists on Windows and macOS for now: on Linux,
    // the app stays on the command line until F7 (no webkit to build with).
    #[cfg(any(windows, target_os = "macos"))]
    {
        println!("cargo:rerun-if-changed=windows-app.manifest");
        let windows = tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("windows-app.manifest"));
        tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
            .expect("tauri build");
    }
}
