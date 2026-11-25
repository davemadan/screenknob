//! Thin, safe-ish wrappers over the CoreAudio HAL C API.
//! Every getter returns `Option`/`Result` so a device that disappears mid-call
//! simply yields `None` instead of crashing.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::ptr;

pub type AudioObjectID = u32;
pub type OSStatus = i32;

const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

pub const kAudioObjectSystemObject: AudioObjectID = 1;
pub const kAudioObjectUnknown: AudioObjectID = 0;

pub const SCOPE_GLOBAL: u32 = fourcc(b"glob");
pub const SCOPE_OUTPUT: u32 = fourcc(b"outp");
pub const SCOPE_INPUT: u32 = fourcc(b"inpt");
pub const SCOPE_WILDCARD: u32 = fourcc(b"****");
pub const ELEMENT_MAIN: u32 = 0;
pub const ELEMENT_WILDCARD: u32 = 0xFFFF_FFFF;

pub const PROP_DEVICES: u32 = fourcc(b"dev#");
pub const PROP_DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
pub const PROP_NAME: u32 = fourcc(b"lnam");
pub const PROP_UID: u32 = fourcc(b"uid ");
pub const PROP_TRANSPORT: u32 = fourcc(b"tran");
pub const PROP_STREAMS: u32 = fourcc(b"stm#");
pub const PROP_STREAM_CONFIG: u32 = fourcc(b"slay");
pub const PROP_VOLUME_SCALAR: u32 = fourcc(b"volm");
pub const PROP_MUTE: u32 = fourcc(b"mute");
pub const PROP_IS_ALIVE: u32 = fourcc(b"livn");
pub const PROP_NOMINAL_RATE: u32 = fourcc(b"nsrt");
pub const PROP_HIDDEN: u32 = fourcc(b"hidn");
pub const PROP_PID_TO_PROCESS: u32 = fourcc(b"id2p");
pub const PROP_TAP_FORMAT: u32 = fourcc(b"tfmt");
pub const PROP_TAP_UID: u32 = fourcc(b"tuid");

pub const TRANSPORT_BUILTIN: u32 = fourcc(b"bltn");
pub const TRANSPORT_HDMI: u32 = fourcc(b"hdmi");
pub const TRANSPORT_DISPLAYPORT: u32 = fourcc(b"dprt");
pub const TRANSPORT_USB: u32 = fourcc(b"usb ");
pub const TRANSPORT_BLUETOOTH: u32 = fourcc(b"blue");
pub const TRANSPORT_BLUETOOTH_LE: u32 = fourcc(b"blea");
pub const TRANSPORT_AGGREGATE: u32 = fourcc(b"grup");
pub const TRANSPORT_VIRTUAL: u32 = fourcc(b"virt");
pub const TRANSPORT_AIRPLAY: u32 = fourcc(b"airp");
pub const TRANSPORT_THUNDERBOLT: u32 = fourcc(b"thun");

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

impl AudioObjectPropertyAddress {
    pub const fn new(sel: u32, scope: u32, elem: u32) -> Self {
        Self { mSelector: sel, mScope: scope, mElement: elem }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

#[repr(C)]
pub struct AudioBufferList {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; 1],
}

impl AudioBufferList {
    /// Buffers as a slice. Safety: `list` must point at a valid list from CoreAudio.
    pub unsafe fn buffers<'a>(list: *const AudioBufferList) -> &'a [AudioBuffer] {
        if list.is_null() {
            return &[];
        }
        let n = unsafe { (*list).mNumberBuffers } as usize;
        if n == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts((*list).mBuffers.as_ptr(), n) }
    }
}

pub type ListenerProc = extern "C" fn(
    AudioObjectID,
    u32,
    *const AudioObjectPropertyAddress,
    *mut c_void,
) -> OSStatus;

pub type IOProc = extern "C" fn(
    AudioObjectID,
    *const c_void,
    *const AudioBufferList,
    *const c_void,
    *mut AudioBufferList,
    *const c_void,
    *mut c_void,
) -> OSStatus;

pub type IOProcID = *mut c_void;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectHasProperty(id: AudioObjectID, addr: *const AudioObjectPropertyAddress) -> u8;
    fn AudioObjectIsPropertySettable(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        out: *mut u8,
    ) -> OSStatus;
    fn AudioObjectGetPropertyDataSize(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qsize: u32,
        qdata: *const c_void,
        out_size: *mut u32,
    ) -> OSStatus;
    fn AudioObjectGetPropertyData(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qsize: u32,
        qdata: *const c_void,
        io_size: *mut u32,
        out: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectSetPropertyData(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qsize: u32,
        qdata: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
    fn AudioObjectAddPropertyListener(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        proc_: ListenerProc,
        client: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectRemovePropertyListener(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        proc_: ListenerProc,
        client: *mut c_void,
    ) -> OSStatus;
    pub fn AudioHardwareCreateAggregateDevice(desc: *const c_void, out: *mut AudioObjectID) -> OSStatus;
    pub fn AudioHardwareDestroyAggregateDevice(id: AudioObjectID) -> OSStatus;
    pub fn AudioHardwareCreateProcessTap(desc: *mut c_void, out: *mut AudioObjectID) -> OSStatus;
    pub fn AudioHardwareDestroyProcessTap(id: AudioObjectID) -> OSStatus;
    pub fn AudioDeviceCreateIOProcID(
        dev: AudioObjectID,
        proc_: IOProc,
        client: *mut c_void,
        out: *mut IOProcID,
    ) -> OSStatus;
    pub fn AudioDeviceDestroyIOProcID(dev: AudioObjectID, id: IOProcID) -> OSStatus;
    pub fn AudioDeviceStart(dev: AudioObjectID, id: IOProcID) -> OSStatus;
    pub fn AudioDeviceStop(dev: AudioObjectID, id: IOProcID) -> OSStatus;
}

pub fn has(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> bool {
    unsafe { AudioObjectHasProperty(id, &addr) != 0 }
}

pub fn settable(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> bool {
    if !has(id, addr) {
        return false;
    }
    let mut out: u8 = 0;
    let st = unsafe { AudioObjectIsPropertySettable(id, &addr, &mut out) };
    st == 0 && out != 0
}

pub fn get<T: Copy + Default>(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> Option<T> {
    get_q::<T, u8>(id, addr, None)
}

pub fn get_q<T: Copy + Default, Q>(
    id: AudioObjectID,
    addr: AudioObjectPropertyAddress,
    qualifier: Option<&Q>,
) -> Option<T> {
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let (qs, qp) = match qualifier {
        Some(q) => (std::mem::size_of::<Q>() as u32, q as *const Q as *const c_void),
        None => (0, ptr::null()),
    };
    let st = unsafe {
        AudioObjectGetPropertyData(id, &addr, qs, qp, &mut size, &mut value as *mut T as *mut c_void)
    };
    if st == 0 && size as usize == std::mem::size_of::<T>() { Some(value) } else { None }
}

pub fn set<T: Copy>(id: AudioObjectID, addr: AudioObjectPropertyAddress, value: T) -> OSStatus {
    unsafe {
        AudioObjectSetPropertyData(
            id,
            &addr,
            0,
            ptr::null(),
            std::mem::size_of::<T>() as u32,
            &value as *const T as *const c_void,
        )
    }
}

pub fn get_u32_list(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> Vec<u32> {
    let mut size: u32 = 0;
    if unsafe { AudioObjectGetPropertyDataSize(id, &addr, 0, ptr::null(), &mut size) } != 0 {
        return Vec::new();
    }
    let n = size as usize / 4;
    if n == 0 {
        return Vec::new();
    }
    let mut v = vec![0u32; n];
    let st = unsafe {
        AudioObjectGetPropertyData(id, &addr, 0, ptr::null(), &mut size, v.as_mut_ptr() as *mut c_void)
    };
    if st != 0 {
        return Vec::new();
    }
    v.truncate(size as usize / 4);
    v
}

pub fn get_string(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> Option<String> {
    let mut s: CFStringRef = ptr::null();
    let mut size = std::mem::size_of::<CFStringRef>() as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(id, &addr, 0, ptr::null(), &mut size, &mut s as *mut _ as *mut c_void)
    };
    if st != 0 || s.is_null() {
        return None;
    }
    let cf = unsafe { CFString::wrap_under_create_rule(s) };
    Some(cf.to_string())
}

/// Total channels of the device's output streams (from the stream configuration).
pub fn output_channels(id: AudioObjectID) -> u32 {
    channels(id, SCOPE_OUTPUT)
}

/// Total channels of the device's input streams.
pub fn input_channels(id: AudioObjectID) -> u32 {
    channels(id, SCOPE_INPUT)
}

fn channels(id: AudioObjectID, scope: u32) -> u32 {
    let addr = AudioObjectPropertyAddress::new(PROP_STREAM_CONFIG, scope, ELEMENT_MAIN);
    let mut size: u32 = 0;
    if unsafe { AudioObjectGetPropertyDataSize(id, &addr, 0, ptr::null(), &mut size) } != 0 || size < 8 {
        return 0;
    }
    // u64 backing keeps the buffer aligned for AudioBufferList.
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let st = unsafe {
        AudioObjectGetPropertyData(id, &addr, 0, ptr::null(), &mut size, buf.as_mut_ptr() as *mut c_void)
    };
    if st != 0 {
        return 0;
    }
    let list = buf.as_ptr() as *const AudioBufferList;
    unsafe { AudioBufferList::buffers(list) }.iter().map(|b| b.mNumberChannels).sum()
}

pub fn add_listener(id: AudioObjectID, addr: AudioObjectPropertyAddress, f: ListenerProc) -> bool {
    unsafe { AudioObjectAddPropertyListener(id, &addr, f, ptr::null_mut()) == 0 }
}

pub fn remove_listener(id: AudioObjectID, addr: AudioObjectPropertyAddress, f: ListenerProc) {
    unsafe {
        let _ = AudioObjectRemovePropertyListener(id, &addr, f, ptr::null_mut());
    }
}
