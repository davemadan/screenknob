//! A short plain-text log at ~/Library/Logs/ScreenKnob.log, to see what
//! permissions and the volume keys were doing when something doesn't work.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn log(msg: impl AsRef<str>) {
    let Some(dir) = dirs::home_dir().map(|h| h.join("Library/Logs")) else { return };
    let path = dir.join("ScreenKnob.log");
    // Keep it small: start over past 256 KB.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::write(&path, b"");
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{secs}] pid {} {}", std::process::id(), msg.as_ref());
    }
}
