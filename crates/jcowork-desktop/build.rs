fn main() {
    // Register app commands with the ACL so they can be granted to the
    // webview via capabilities. The main window loads http://localhost:3000,
    // which Tauri treats as a *remote* origin — without an explicit
    // permission + remote capability, invoking these commands is silently
    // denied by the IPC ACL check.
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&["open_in_browser"]),
        ),
    )
    .expect("failed to run tauri build script");
}
