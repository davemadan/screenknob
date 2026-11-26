//! The monitor's own speaker volume over the display cable (DDC/CI, VESA MCCS
//! code 0x62), on Apple silicon. No sound is captured, so macOS shows no
//! recording indicator. Monitors answer slowly and sometimes not at all, so
//! every call retries and a monitor is only used after it has answered.

#![allow(non_upper_case_globals)]

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use std::ffi::{c_char, c_void};
use std::time::Duration;

type IoObject = u32;
type CFTypeRef = *const c_void;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    static kIOMainPortDefault: u32;
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(port: u32, matching: *mut c_void, it: *mut IoObject) -> i32;
    fn IOIteratorNext(it: IoObject) -> IoObject;
    fn IOObjectRelease(o: IoObject) -> i32;
    fn IORegistryEntryGetPath(e: IoObject, plane: *const c_char, path: *mut c_char) -> i32;
    fn IORegistryEntryCreateCFProperty(e: IoObject, key: CFTypeRef, alloc: CFTypeRef, opts: u32) -> CFTypeRef;
    fn IOAVServiceCreateWithService(alloc: CFTypeRef, service: IoObject) -> CFTypeRef;
    fn IOAVServiceReadI2C(s: CFTypeRef, chip: u32, offset: u32, buf: *mut u8, len: u32) -> i32;
    fn IOAVServiceWriteI2C(s: CFTypeRef, chip: u32, offset: u32, buf: *const u8, len: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(o: CFTypeRef);
}

const VCP_VOLUME: u8 = 0x62;
const CHIP: u32 = 0x37;
const OFFSET: u32 = 0x51;

/// One external screen that can be spoken to over its cable.
pub struct Monitor {
    /// The screen's name as macOS shows it (matches its sound output's name).
    pub name: String,
    av: CFTypeRef,
}

// The handle is only used from the DDC worker thread.
unsafe impl Send for Monitor {}

impl Drop for Monitor {
    fn drop(&mut self) {
        if !self.av.is_null() {
            unsafe { CFRelease(self.av) };
        }
    }
}

fn path(e: IoObject) -> String {
    let mut buf = [0 as c_char; 1024];
    unsafe { IORegistryEntryGetPath(e, c"IOService".as_ptr(), buf.as_mut_ptr()) };
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn services(class: &std::ffi::CStr) -> Vec<IoObject> {
    let mut it: IoObject = 0;
    let mut out = Vec::new();
    unsafe {
        if IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching(class.as_ptr()), &mut it) != 0 {
            return out;
        }
        loop {
            let s = IOIteratorNext(it);
            if s == 0 {
                break;
            }
            out.push(s);
        }
        IOObjectRelease(it);
    }
    out
}

/// The display pipe a registry path belongs to: "disp0", "dispext1"…
fn pipe(path: &str) -> Option<String> {
    for part in path.split('/') {
        let p = part.split(['@', ':']).next().unwrap_or("");
        if p.starts_with("dispext") || p == "disp0" {
            return Some(p.to_string());
        }
        // The built-in pipe's proxy sits under "dcp@…" and drives "disp0".
        if p == "dcp" {
            return Some("disp0".into());
        }
        if let Some(n) = p.strip_prefix("dcpext") {
            return Some(format!("dispext{n}"));
        }
    }
    None
}

fn product_name(e: IoObject) -> Option<String> {
    let key = CFString::new("DisplayAttributes");
    let v = unsafe { IORegistryEntryCreateCFProperty(e, key.as_concrete_TypeRef() as CFTypeRef, std::ptr::null(), 0) };
    if v.is_null() {
        return None;
    }
    let attrs = unsafe { CFType::wrap_under_create_rule(v as _) };
    let dict = attrs.downcast::<CFDictionary>()?;
    let dict: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(dict.as_concrete_TypeRef()) };
    let prod = dict.find(CFString::new("ProductAttributes"))?.downcast::<CFDictionary>()?;
    let prod: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(prod.as_concrete_TypeRef()) };
    prod.find(CFString::new("ProductName"))?.downcast::<CFString>().map(|s| s.to_string())
}

/// Every external screen with a DDC channel, named like its sound output.
pub fn monitors() -> Vec<Monitor> {
    let mut names = Vec::new();
    for e in services(c"AppleCLCD2") {
        if let (Some(p), Some(n)) = (pipe(&path(e)), product_name(e)) {
            names.push((p, n));
        }
        unsafe { IOObjectRelease(e) };
    }
    let mut out = Vec::new();
    for e in services(c"DCPAVServiceProxy") {
        let key = CFString::new("Location");
        let loc = unsafe { IORegistryEntryCreateCFProperty(e, key.as_concrete_TypeRef() as CFTypeRef, std::ptr::null(), 0) };
        let external = !loc.is_null() && {
            let v = unsafe { CFType::wrap_under_create_rule(loc as _) };
            v.downcast::<CFString>().is_some_and(|s| s.to_string() == "External")
        };
        let name = pipe(&path(e)).and_then(|p| names.iter().find(|(q, _)| *q == p).map(|(_, n)| n.clone()));
        if let (true, Some(name)) = (external, name) {
            let av = unsafe { IOAVServiceCreateWithService(std::ptr::null(), e) };
            if !av.is_null() {
                out.push(Monitor { name, av });
            }
        }
        unsafe { IOObjectRelease(e) };
    }
    out
}

impl Monitor {
    fn send(&self, data: &[u8]) -> bool {
        // [0x80 | length, …data…, checksum]; the checksum covers the addresses too.
        let mut pkt = Vec::with_capacity(data.len() + 2);
        pkt.push(0x80 | data.len() as u8);
        pkt.extend_from_slice(data);
        let chk = pkt.iter().fold(0x6E ^ 0x51u8, |a, b| a ^ b);
        pkt.push(chk);
        unsafe { IOAVServiceWriteI2C(self.av, CHIP, OFFSET, pkt.as_ptr(), pkt.len() as u32) == 0 }
    }

    /// The speaker volume as (current, maximum), or None if the screen doesn't answer.
    pub fn volume(&self) -> Option<(u16, u16)> {
        for _ in 0..5 {
            if !self.send(&[0x01, VCP_VOLUME]) {
                std::thread::sleep(Duration::from_millis(40));
                continue;
            }
            std::thread::sleep(Duration::from_millis(50));
            let mut r = [0u8; 11];
            let ok = unsafe { IOAVServiceReadI2C(self.av, CHIP, OFFSET, r.as_mut_ptr(), r.len() as u32) == 0 };
            // Reply: …, 0x02, result 0, the code, type, max hi/lo, current hi/lo, checksum.
            if ok && r[2] == 0x02 && r[3] == 0 && r[4] == VCP_VOLUME {
                let max = u16::from_be_bytes([r[6], r[7]]);
                let cur = u16::from_be_bytes([r[8], r[9]]);
                if max > 0 && cur <= max {
                    return Some((cur, max));
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        None
    }

    /// Sets the speaker volume (sent twice: screens drop commands now and then).
    pub fn set_volume(&self, value: u16) -> bool {
        let [hi, lo] = value.to_be_bytes();
        let mut ok = false;
        for _ in 0..2 {
            ok |= self.send(&[0x03, VCP_VOLUME, hi, lo]);
            std::thread::sleep(Duration::from_millis(20));
        }
        ok
    }
}

/// Requests for the DDC worker thread.
pub enum Req {
    /// Look for screens again and read their volume.
    Probe,
    /// Set a screen's volume (0..1), by name.
    Set(String, f32),
}

/// Runs DDC on its own thread (screens answer slowly). Each probe reports every
/// screen's name and volume (None when it didn't answer) through `report`.
pub fn start(report: impl Fn(Vec<(String, Option<f32>)>) + Send + 'static) -> std::sync::mpsc::Sender<Req> {
    let (tx, rx) = std::sync::mpsc::channel::<Req>();
    std::thread::Builder::new()
        .name("ddc".into())
        .spawn(move || {
            let mut screens: Vec<(Monitor, u16)> = Vec::new();
            while let Ok(first) = rx.recv() {
                // Coalesce: one probe, and only the latest level per screen.
                let mut probe = false;
                let mut sets: Vec<(String, f32)> = Vec::new();
                for req in std::iter::once(first).chain(rx.try_iter()) {
                    match req {
                        Req::Probe => probe = true,
                        Req::Set(n, v) => {
                            sets.retain(|(m, _)| *m != n);
                            sets.push((n, v));
                        }
                    }
                }
                if probe {
                    let found = std::panic::catch_unwind(monitors).unwrap_or_default();
                    let mut out = Vec::new();
                    screens.clear();
                    for m in found {
                        let v = m.volume();
                        out.push((m.name.clone(), v.map(|(c, max)| c as f32 / max as f32)));
                        if let Some((_, max)) = v {
                            screens.push((m, max));
                        }
                    }
                    report(out);
                }
                for (name, v) in sets {
                    if let Some((m, max)) = screens.iter().find(|(m, _)| m.name == name) {
                        m.set_volume((v.clamp(0.0, 1.0) * *max as f32).round() as u16);
                    }
                }
            }
        })
        .ok();
    tx
}
