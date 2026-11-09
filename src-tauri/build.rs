fn main() {
    tauri_build::build();

    // A test that reaches the boot runner links the Windows GUI stack (dialogs and comctl32).
    // Tauri embeds its manifest into the application target only, so a test binary would load
    // comctl32 v5, which has no TaskDialogIndirect, and the loader would refuse to start it.
    // Embed the common-controls v6 dependency in every test target too.
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-test.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
