//! The keyboard's volume keys. macOS can't change the volume of a screen
//! without its own volume control, so while such an output is in use the
//! keys are caught here and turned into software-volume steps.
//! Needs the Accessibility permission; without it the keys stay untouched.

use crate::engine::Command;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Estimated: the same steps as macOS (see MECHANICS.md).
pub const STEP: f32 = 1.0 / 16.0;
pub const FINE_STEP: f32 = 1.0 / 64.0;

const NX_SYSDEFINED: u32 = 14;
const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;
const NX_KEYTYPE_SOUND_UP: i64 = 0;
const NX_KEYTYPE_SOUND_DOWN: i64 = 1;
const NX_KEYTYPE_MUTE: i64 = 7;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_OPTION: u64 = 0x0008_0000;

type EventRef = *mut c_void;
type TapCallback = extern "C" fn(*mut c_void, u32, EventRef, *mut c_void) -> EventRef;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, cb: TapCallback, user: *mut c_void) -> *mut c_void;
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    fn CGEventGetFlags(event: EventRef) -> u64;
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: *mut c_void, order: isize) -> *mut c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: CFStringRef);
    fn CFRunLoopRun();
    static kCFRunLoopCommonModes: CFStringRef;
}

/// Set by the engine: catch the keys only while the output uses software volume.
pub static CATCH_KEYS: AtomicBool = AtomicBool::new(false);
pub static TAP_RUNNING: AtomicBool = AtomicBool::new(false);
static PORT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static TX: OnceLock<Sender<Command>> = OnceLock::new();

pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

static SEEN_KEY: AtomicBool = AtomicBool::new(false);
static PROMPTED: AtomicBool = AtomicBool::new(false);

/// Shows macOS's own "allow Accessibility" prompt once (it also adds this app to the list).
pub fn prompt_once() {
    if PROMPTED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
        AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef() as *const c_void);
    }
}

/// Shows macOS's own "allow Accessibility" prompt and opens the settings page.
pub fn ask_permission() {
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
        AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef() as *const c_void);
    }
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

extern "C" fn on_event(proxy: *mut c_void, kind: u32, event: EventRef, user: *mut c_void) -> EventRef {
    // Nothing here may ever bring the app down: on any fault the key passes through untouched.
    std::panic::catch_unwind(|| handle_event(proxy, kind, event, user)).unwrap_or(event)
}

fn handle_event(_proxy: *mut c_void, kind: u32, event: EventRef, _user: *mut c_void) -> EventRef {
    if kind == TAP_DISABLED_BY_TIMEOUT || kind == TAP_DISABLED_BY_USER_INPUT {
        // macOS switches a slow tap off; switch it straight back on.
        let port = PORT.load(Ordering::Relaxed);
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, true) };
        }
        return event;
    }
    if kind != NX_SYSDEFINED || event.is_null() || !CATCH_KEYS.load(Ordering::Relaxed) {
        return event;
    }
    let Some((key, down)) = media_key(event) else { return event };
    if !matches!(key, NX_KEYTYPE_SOUND_UP | NX_KEYTYPE_SOUND_DOWN | NX_KEYTYPE_MUTE) {
        return event;
    }
    if !SEEN_KEY.swap(true, Ordering::Relaxed) {
        crate::diag::log(format!("volume keys: first key caught (key {key}, down {down})"));
    }
    if down {
        let flags = unsafe { CGEventGetFlags(event) };
        let step = if flags & FLAG_SHIFT != 0 && flags & FLAG_OPTION != 0 { FINE_STEP } else { STEP };
        let shift = flags & FLAG_SHIFT != 0 && flags & FLAG_OPTION == 0;
        let cmd = match key {
            NX_KEYTYPE_SOUND_UP => Command::Key(step, shift),
            NX_KEYTYPE_SOUND_DOWN => Command::Key(-step, shift),
            _ => Command::Key(0.0, shift),
        };
        if let Some(tx) = TX.get() {
            let _ = tx.send(cmd);
        }
    }
    std::ptr::null_mut() // swallowed: macOS would only show "no volume control"
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_msgSend();
    fn objc_getClass(name: *const std::ffi::c_char) -> *mut c_void;
    fn sel_registerName(name: *const std::ffi::c_char) -> *mut c_void;
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

/// (key code, is key down) for a media key event, read through NSEvent with
/// plain Objective-C calls of exactly the right shape.
fn media_key(event: EventRef) -> Option<(i64, bool)> {
    type MsgPtr = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
    type MsgI16 = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i16;
    type MsgIsize = unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize;
    unsafe {
        let cls = objc_getClass(c"NSEvent".as_ptr());
        if cls.is_null() {
            return None;
        }
        let pool = objc_autoreleasePoolPush();
        let send_ptr: MsgPtr = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_i16: MsgI16 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_isize: MsgIsize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let ns = send_ptr(cls, sel_registerName(c"eventWithCGEvent:".as_ptr()), event);
        let mut out = None;
        if !ns.is_null() {
            let subtype = send_i16(ns, sel_registerName(c"subtype".as_ptr()));
            if subtype == NX_SUBTYPE_AUX_CONTROL_BUTTONS {
                let data1 = send_isize(ns, sel_registerName(c"data1".as_ptr())) as i64;
                let key = (data1 & 0xFFFF_0000) >> 16;
                let state = (data1 & 0xFF00) >> 8;
                out = Some((key, state == 0xA));
            }
        }
        objc_autoreleasePoolPop(pool);
        out
    }
}

/// Starts the key watcher on its own thread. It waits for the permission and
/// keeps trying, so granting it later works without a restart.
pub fn start(tx: Sender<Command>) {
    let _ = TX.set(tx);
    std::thread::Builder::new()
        .name("volume-keys".into())
        .spawn(|| {
            let mut said_waiting = false;
            loop {
                if !trusted() {
                    if !said_waiting {
                        crate::diag::log("volume keys: waiting for Accessibility permission");
                        said_waiting = true;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
                let port =
                    unsafe { CGEventTapCreate(1, 0, 0, 1u64 << NX_SYSDEFINED, on_event, std::ptr::null_mut()) };
                if port.is_null() {
                    crate::diag::log("volume keys: macOS refused the key listener; retrying");
                    std::thread::sleep(Duration::from_secs(3));
                    continue;
                }
                PORT.store(port, Ordering::Relaxed);
                unsafe {
                    let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
                    if src.is_null() {
                        std::thread::sleep(Duration::from_secs(3));
                        continue;
                    }
                    CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
                    CGEventTapEnable(port, true);
                    TAP_RUNNING.store(true, Ordering::Relaxed);
                    crate::diag::log("volume keys: listening");
                    CFRunLoopRun();
                }
                TAP_RUNNING.store(false, Ordering::Relaxed);
                said_waiting = false;
            }
        })
        .ok();
}

/// Clears this app's Accessibility entry (it can be left over from an earlier
/// copy of the app, which macOS then won't match) and asks again.
pub fn reset_and_ask() {
    let out = std::process::Command::new("/usr/bin/tccutil")
        .args(["reset", "Accessibility", "craft.monitor-volume"])
        .output();
    crate::diag::log(format!(
        "volume keys: reset permission: {}",
        out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string() + &String::from_utf8_lossy(&o.stderr))
            .unwrap_or_else(|e| e.to_string())
    ));
    PROMPTED.store(false, Ordering::Relaxed);
    prompt_once();
    ask_permission();
}

/// Self-test: builds a fake "volume up" key press and reads it back the way a
/// real one is read. Returns what was decoded.
pub fn selftest_key() -> Option<(i64, bool)> {
    #[repr(C)]
    struct Point {
        x: f64,
        y: f64,
    }
    type Make = unsafe extern "C" fn(
        *mut c_void, *mut c_void, usize, Point, usize, f64, isize, *mut c_void, i16, isize, isize,
    ) -> *mut c_void;
    type Get = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let make: Make = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let get: Get = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let cls = objc_getClass(c"NSEvent".as_ptr());
        let sel = sel_registerName(c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:".as_ptr());
        let data1 = (NX_KEYTYPE_SOUND_UP << 16) | (0xA << 8);
        let ev = make(cls, sel, NX_SYSDEFINED as usize, Point { x: 0.0, y: 0.0 }, 0xa00, 0.0, 0, std::ptr::null_mut(), 8, data1 as isize, -1);
        let out = if ev.is_null() {
            None
        } else {
            let cg = get(ev, sel_registerName(c"CGEvent".as_ptr()));
            if cg.is_null() { None } else { Some(handle_event(std::ptr::null_mut(), NX_SYSDEFINED, cg, std::ptr::null_mut()).is_null()).and(media_key(cg)) }
        };
        objc_autoreleasePoolPop(pool);
        out
    }
}
