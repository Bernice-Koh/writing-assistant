fn main() {
    // `.commands(&[...])` is what makes tauri-build autogenerate the `allow-get-current-flags`
    // permission `capabilities/default.json` references: without it, an app-defined command's
    // permission is never generated at all, and every invocation of it is denied at runtime.
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&["get_current_flags"]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
