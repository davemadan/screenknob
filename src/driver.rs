//! Installing and removing ScreenKnob's own sound driver (driver/). Both need
//! your Mac password once (macOS's own prompt) and restart the Mac's sound,
//! which takes about a second.

use std::path::PathBuf;

pub const BUNDLE: &str = "ScreenKnobAudio.driver";
const HAL: &str = "/Library/Audio/Plug-Ins/HAL";

/// Installed in the system's audio plug-ins folder.
pub fn installed() -> bool {
    std::path::Path::new(HAL).join(BUNDLE).exists()
}

fn bundle_version(bundle: &std::path::Path) -> Option<u32> {
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).ok()?;
    let after = plist.split("<key>CFBundleVersion</key>").nth(1)?;
    let v = after.split("<string>").nth(1)?.split("</string>").next()?;
    v.trim().parse().ok()
}

/// The installed driver is older than the one in this copy of ScreenKnob.
pub fn outdated() -> bool {
    let installed = bundle_version(&std::path::Path::new(HAL).join(BUNDLE));
    let bundled = source().and_then(|s| bundle_version(&s));
    matches!((installed, bundled), (Some(i), Some(b)) if i < b)
}

/// The driver that came with this copy of ScreenKnob: inside the app, or built
/// next to it in the project.
pub fn source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    for dir in exe.ancestors().take(8) {
        for candidate in [dir.join("Resources").join(BUNDLE), dir.join("driver.noindex").join(BUNDLE)] {
            if candidate.join("Contents/Info.plist").exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Runs a shell script as administrator through macOS's password prompt.
fn as_admin(script: &str, prompt: &str) -> Result<(), String> {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let apple = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"{}\"",
        esc(script),
        esc(prompt)
    );
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &apple])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.contains("-128") { "Cancelled".into() } else { err })
    }
}

const RESTART_SOUND: &str = "killall coreaudiod || launchctl kickstart -k system/com.apple.audio.coreaudiod";

pub fn install() -> Result<(), String> {
    let src = source().ok_or("This copy of ScreenKnob doesn't include the driver")?;
    let src = src.to_str().ok_or("Odd path")?;
    let dst = format!("{HAL}/{BUNDLE}");
    let script = format!(
        "set -e; mkdir -p {hal}; rm -rf {dst}; cp -R {src} {hal}/; chown -R root:wheel {dst}; chmod -R go-w {dst}; {RESTART_SOUND}",
        hal = quote(HAL),
        dst = quote(&dst),
        src = quote(src),
    );
    as_admin(&script, "ScreenKnob wants to install its sound driver, so screens get the macOS volume controls without recording.")
}

pub fn remove() -> Result<(), String> {
    let dst = format!("{HAL}/{BUNDLE}");
    let script = format!("rm -rf {}; {RESTART_SOUND}", quote(&dst));
    as_admin(&script, "ScreenKnob wants to remove its sound driver.")
}
