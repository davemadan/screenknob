// Embeds Info.plist in the program so macOS can show why it asks to use the
// system sound, even when it runs outside an .app bundle. Also builds the
// ScreenKnob sound driver (driver/) into target/driver.noindex/.
fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let plist = root.join("Info.plist");
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-changed=icon/AppIcon.icns");
    println!("cargo:rerun-if-changed=driver/ScreenKnobAudio.c");
    println!("cargo:rerun-if-changed=driver/ScreenKnobRing.h");
    println!("cargo:rerun-if-changed=driver/Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    build_driver(&root);
}

/// target/<profile>/build/<pkg>/out → target/driver.noindex/ScreenKnobAudio.driver
fn build_driver(root: &std::path::Path) {
    let Ok(out) = std::env::var("OUT_DIR") else { return };
    let Some(target) = std::path::Path::new(&out).ancestors().nth(4) else { return };
    let bundle = target.join("driver.noindex/ScreenKnobAudio.driver");
    let macos = bundle.join("Contents/MacOS");
    if std::fs::create_dir_all(&macos).is_err() {
        return;
    }
    let _ = std::fs::copy(root.join("driver/Info.plist"), bundle.join("Contents/Info.plist"));
    let ok = std::process::Command::new("clang")
        .args(["-arch", "arm64", "-arch", "x86_64", "-mmacosx-version-min=14.2", "-O2", "-bundle", "-o"])
        .arg(macos.join("ScreenKnobAudio"))
        .arg(root.join("driver/ScreenKnobAudio.c"))
        .args(["-framework", "CoreAudio", "-framework", "CoreFoundation"])
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        let _ = std::process::Command::new("codesign").args(["--force", "--sign", "-"]).arg(&bundle).output();
    } else {
        println!("cargo:warning=couldn't build the ScreenKnob driver (clang missing?); the app works without it");
    }
}
