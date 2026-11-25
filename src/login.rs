//! "Open at login", through macOS's own Login Items (SMAppService), so it shows
//! in System Settings → General → Login Items under ScreenKnob's name.

use std::ffi::{CStr, c_char, c_void};

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_msgSend();
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Login {
    /// Not in an installed copy (a build run from the project folder).
    #[default]
    Unavailable,
    Off,
    On,
    /// Registered, but switched off in System Settings → Login Items.
    NeedsApproval,
}

/// Only the copy installed in an Applications folder is offered as a login item.
pub fn installed() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.contains("/Applications/") && s.contains(".app/Contents/MacOS/")))
        .unwrap_or(false)
}

type Get = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
type GetInt = unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize;
type Act = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> u8;
type Void = unsafe extern "C" fn(*mut c_void, *mut c_void);

fn sel(name: &CStr) -> *mut c_void {
    unsafe { sel_registerName(name.as_ptr()) }
}

/// Runs `f` with the app's SMAppService object, inside an autorelease pool.
fn with_service<R>(f: impl FnOnce(*mut c_void) -> R) -> Option<R> {
    unsafe {
        let cls = objc_getClass(c"SMAppService".as_ptr());
        if cls.is_null() {
            return None;
        }
        let pool = objc_autoreleasePoolPush();
        let get: Get = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let svc = get(cls, sel(c"mainAppService"));
        let out = if svc.is_null() { None } else { Some(f(svc)) };
        objc_autoreleasePoolPop(pool);
        out
    }
}

pub fn status() -> Login {
    if !installed() {
        return Login::Unavailable;
    }
    let s = with_service(|svc| unsafe {
        let get: GetInt = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        get(svc, sel(c"status"))
    });
    match s {
        Some(1) => Login::On,
        Some(2) => Login::NeedsApproval,
        Some(_) => Login::Off,
        None => Login::Unavailable,
    }
}

/// Turns opening at login on or off. Returns a short reason when macOS refuses.
pub fn set(on: bool) -> Result<(), String> {
    if !installed() {
        return Err("Install ScreenKnob in Applications first".into());
    }
    let res = with_service(|svc| unsafe {
        let act: Act = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let mut err: *mut c_void = std::ptr::null_mut();
        let name = if on { c"registerAndReturnError:" } else { c"unregisterAndReturnError:" };
        let ok = act(svc, sel(name), &mut err) != 0;
        if ok {
            Ok(())
        } else if err.is_null() {
            Err("macOS refused".to_string())
        } else {
            let get: Get = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let desc = get(err, sel(c"localizedDescription"));
            let text = if desc.is_null() {
                std::ptr::null()
            } else {
                get(desc, sel(c"UTF8String")) as *const c_char
            };
            Err(if text.is_null() {
                "macOS refused".to_string()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            })
        }
    });
    res.unwrap_or_else(|| Err("Login items aren't available on this Mac".into()))
}

/// Opens System Settings → General → Login Items.
pub fn open_settings() {
    unsafe {
        let cls = objc_getClass(c"SMAppService".as_ptr());
        if !cls.is_null() {
            let call: Void = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            call(cls, sel(c"openSystemSettingsLoginItems"));
        }
    }
}
