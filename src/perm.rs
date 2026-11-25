//! The "System Audio Recording" permission and who it belongs to.
//!
//! Without it, macOS hands the app silence while still muting the real sound,
//! so the software volume must never start before it's granted.

use block2::RcBlock;
use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    Granted,
    Denied,
    NotAsked,
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn posix_spawnattr_init(attr: *mut *mut c_void) -> c_int;
    fn posix_spawnattr_destroy(attr: *mut *mut c_void) -> c_int;
    fn posix_spawn(
        pid: *mut c_int,
        path: *const c_char,
        actions: *const c_void,
        attr: *const *mut c_void,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
}

const RTLD_NOW: c_int = 2;
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
const SERVICE: &str = "kTCCServiceAudioCapture";
const CHILD_ENV: &str = "MONITOR_VOLUME_OWN_PERMISSIONS";

fn tcc() -> *mut c_void {
    unsafe { dlopen(c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC".as_ptr(), RTLD_NOW) }
}

/// Current state of the permission. If macOS can't be asked, assume it's allowed.
pub fn audio_capture() -> Access {
    access(SERVICE)
}

fn access(service: &str) -> Access {
    let h = tcc();
    if h.is_null() {
        return Access::Granted;
    }
    let f = unsafe { dlsym(h, c"TCCAccessPreflight".as_ptr()) };
    if f.is_null() {
        return Access::Granted;
    }
    type Preflight = unsafe extern "C" fn(CFStringRef, *const c_void) -> isize;
    let f: Preflight = unsafe { std::mem::transmute(f) };
    let s = CFString::new(service);
    match unsafe { f(s.as_concrete_TypeRef(), std::ptr::null()) } {
        0 => Access::Granted,
        1 => Access::Denied,
        _ => Access::NotAsked,
    }
}

static ASKED: AtomicBool = AtomicBool::new(false);

/// Shows macOS's "allow this app to record system audio" prompt once; `done`
/// runs when the person has answered.
pub fn request_audio_capture(done: impl Fn() + Send + Sync + 'static) {
    if ASKED.swap(true, Ordering::Relaxed) {
        return;
    }
    request(SERVICE, done);
}

fn request(service: &str, done: impl Fn() + Send + Sync + 'static) {
    let h = tcc();
    let f = if h.is_null() { std::ptr::null_mut() } else { unsafe { dlsym(h, c"TCCAccessRequest".as_ptr()) } };
    if f.is_null() {
        done();
        return;
    }
    type Request = unsafe extern "C" fn(CFStringRef, *const c_void, *const block2::Block<dyn Fn(u8)>);
    let f: Request = unsafe { std::mem::transmute(f) };
    let block = RcBlock::new(move |_granted: u8| done());
    let s = CFString::new(service);
    unsafe { f(s.as_concrete_TypeRef(), std::ptr::null(), &*block) };
}

/// Opens Privacy & Security at `pane` (e.g. Privacy_AudioCapture).
pub fn open_privacy(pane: &str) {
    let _ = std::process::Command::new("open")
        .arg(format!("x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?{pane}"))
        .spawn();
}

/// When another app starts this one (a launcher, a terminal), macOS would
/// treat that app as the one asking for permissions, and quietly refuse. So the
/// program starts a copy of itself that answers for itself, waits for it, and
/// exits with its result. The copy quits if this one goes away.
/// Returns `Some(exit code)` in the starter; `None` in the copy (carry on).
pub fn run_as_own_app() -> Option<i32> {
    if std::env::var_os(CHILD_ENV).is_some() {
        watch_parent();
        return None;
    }
    let disclaim = unsafe { dlsym(RTLD_DEFAULT, c"responsibility_spawnattrs_setdisclaim".as_ptr()) };
    if disclaim.is_null() {
        return None;
    }
    type Disclaim = unsafe extern "C" fn(*mut *mut c_void, c_int) -> c_int;
    let disclaim: Disclaim = unsafe { std::mem::transmute(disclaim) };
    let exe = std::env::current_exe().ok()?;
    // Run from a real Mac app bundle so macOS files its permissions under the
    // app's name; fall back to this program as it is.
    // Installed as ScreenKnob.app already: run as it is.
    let in_bundle = exe.to_str().is_some_and(|p| p.contains(".app/Contents/MacOS/"));
    let exe = if in_bundle { Ok(exe.clone()) } else { app_bundle(&exe) };
    let exe = match exe {
        Ok(inner) => inner,
        Err(e) => {
            let exe = std::env::current_exe().ok()?;
            crate::diag::log(format!("couldn't make the app bundle ({e}); running as a plain program"));
            stable_signature(&exe);
            exe
        }
    };
    let exe = CString::new(exe.to_str()?).ok()?;
    let args: Vec<CString> = std::env::args().filter_map(|a| CString::new(a).ok()).collect();
    let mut envs: Vec<CString> = std::env::vars()
        .filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok())
        .collect();
    envs.push(CString::new(format!("{CHILD_ENV}=1")).ok()?);
    let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect();
    let envp: Vec<*const c_char> = envs.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect();
    unsafe {
        let mut attr: *mut c_void = std::ptr::null_mut();
        if posix_spawnattr_init(&mut attr) != 0 {
            return None;
        }
        disclaim(&mut attr, 1);
        let mut pid: c_int = 0;
        let st = posix_spawn(&mut pid, exe.as_ptr(), std::ptr::null(), &attr, argv.as_ptr(), envp.as_ptr());
        posix_spawnattr_destroy(&mut attr);
        if st != 0 {
            return None; // couldn't: just run here
        }
        let mut status: c_int = 0;
        while waitpid(pid, &mut status, 0) < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
        let code = if status & 0x7f == 0 { (status >> 8) & 0xff } else { 1 };
        Some(code)
    }
}

const SIGN_ID: &str = "craft.monitor-volume";
const INFO_PLIST: &str = include_str!("../Info.plist");

/// Keeps "ScreenKnob.app" next to this program up to date with it (copied
/// only when it changed), signed with a fixed identity and known to macOS.
/// Returns the program inside the bundle.
fn app_bundle(exe: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::io::{Error, ErrorKind};
    let dir = exe.parent().ok_or_else(|| Error::new(ErrorKind::NotFound, "no folder"))?;
    let bundle = dir.join("ScreenKnob.app");
    let macos = bundle.join("Contents/MacOS");
    let inner = macos.join("screenknob");
    std::fs::create_dir_all(&macos)?;

    let plist = bundle.join("Contents/Info.plist");
    let plist_changed = std::fs::read_to_string(&plist).map(|p| p != INFO_PLIST).unwrap_or(true);
    if plist_changed {
        std::fs::write(&plist, INFO_PLIST)?;
    }
    let new = std::fs::read(exe)?;
    // The stamp must live outside the bundle: anything extra inside breaks its signature.
    let _ = std::fs::remove_file(bundle.join("Contents/source-size-mtime"));
    let stamp = dir.join(".screenknob-bundle-stamp");
    let meta = std::fs::metadata(exe)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    let id = format!("{} {}", new.len(), mtime);
    let exe_changed = std::fs::read_to_string(&stamp).map(|s| s != id).unwrap_or(true) || !inner.exists();
    if exe_changed || plist_changed {
        let tmp = macos.join(".screenknob.new");
        std::fs::write(&tmp, &new)?;
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        std::fs::rename(&tmp, &inner)?;
        let req = format!("=designated => identifier \"{SIGN_ID}\"");
        let signed = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--identifier", SIGN_ID, "--requirements", &req])
            .arg(&bundle)
            .output()?;
        if !signed.status.success() {
            return Err(Error::other(String::from_utf8_lossy(&signed.stderr).to_string()));
        }
        let _ = std::process::Command::new(
            "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
        )
        .arg("-f")
        .arg(&bundle)
        .output();
        std::fs::write(&stamp, id)?;
        crate::diag::log(format!("app bundle updated: {}", bundle.display()));
    }
    Ok(inner)
}

/// macOS remembers permissions against the program's signature. A plain build's
/// signature changes every time it's rebuilt, so permissions you granted would be
/// forgotten. Re-sign it (locally, no certificate) so its identity is just its name.
fn stable_signature(exe: &std::path::Path) {
    let want = format!("designated => identifier \"{SIGN_ID}\"");
    let current = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(exe)
        .output();
    let ok = current.is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout).contains(&want) || String::from_utf8_lossy(&o.stderr).contains(&want)
    });
    if ok {
        return;
    }
    let _ = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier", SIGN_ID, "--requirements"])
        .arg(format!("={want}"))
        .arg(exe)
        .output();
}

fn watch_parent() {
    let start = std::os::unix::process::parent_id();
    std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(700));
                if std::os::unix::process::parent_id() != start {
                    // Whoever started us is gone: leave too, giving the sound back to the screen.
                    crate::engine::restore_output_on_exit();
                    std::process::exit(0);
                }
            }
        })
        .ok();
}


unsafe extern "C" {
    fn flock(fd: c_int, op: c_int) -> c_int;
}

fn support_dir() -> Option<std::path::PathBuf> {
    let d = dirs::config_dir()?.join("ScreenKnob");
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// Only one copy may run (two would both process the sound). Returns false if
/// another copy already runs; that copy is asked to show its window instead.
pub fn single_instance() -> bool {
    use std::os::fd::AsRawFd;
    let Some(dir) = support_dir() else { return true };
    let Ok(file) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("running.lock"))
    else {
        return true;
    };
    if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
        let _ = std::fs::write(dir.join("show-window"), b"");
        return false;
    }
    std::mem::forget(file); // held until the app exits
    true
}

/// True once if another launch asked this copy to show its window.
pub fn take_show_request() -> bool {
    support_dir().is_some_and(|d| std::fs::remove_file(d.join("show-window")).is_ok())
}
