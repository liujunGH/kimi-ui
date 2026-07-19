fn main() {
    // Release builds embed web-dist/ (include_dir!) — rebuild when it changes,
    // without dropping the default tracking of src/.
    println!("cargo:rerun-if-changed=src");
    if std::path::Path::new("web-dist").exists() {
        println!("cargo:rerun-if-changed=web-dist");
    }
    tauri_build::build()
}
