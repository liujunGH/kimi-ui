fn main() {
    // Release builds embed web-dist/ (include_dir!) — rebuild when it changes,
    // without dropping the default tracking of src/.
    println!("cargo:rerun-if-changed=src");
    if std::path::Path::new("web-dist").exists() {
        println!("cargo:rerun-if-changed=web-dist");
    }
    println!("cargo:rerun-if-changed=capabilities");
    // Autogenerate allow-<cmd>/deny-<cmd> permissions for every app command:
    // remote origins (the daemon-hosted / statically served UI) may only call
    // commands explicitly granted in capabilities/, and declaring the app
    // manifest flips the ACL check on for local origins too.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "notify",
            "focus_window",
            "toggle_maximize",
            "daemon_info",
            "set_overlay",
            "plan_usage",
            "set_scroll_freeze",
            "toggle_devtools",
            "update_info",
            "open_url",
        ]),
    ))
    .expect("failed to run tauri-build");
}
