fn main() {
    let app_manifest = tauri_build::AppManifest::new().commands(&[
        "prepare_update_restart",
        "get_staged_desktop_update",
        "stage_desktop_update",
        "install_staged_desktop_update",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(app_manifest))
        .expect("failed to build Tauri desktop application");
}
