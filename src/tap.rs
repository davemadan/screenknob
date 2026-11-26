//! Software volume for outputs that have no volume control of their own
//! (HDMI / DisplayPort screens): the system sound is taken with a CoreAudio
//! process tap, scaled, and played to the same output through a private
//! aggregate device. Same approach as the original (see MECHANICS.md).
//!
//! The tap is "muted only while tapped": if this app stops, the sound plays
//! normally again instead of going silent.

use crate::ca::{self, AudioBufferList, AudioObjectID, AudioObjectPropertyAddress as Addr, IOProcID};
use crate::devices::{Device, OWN_UID_PREFIX};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSArray, NSNumber, NSString};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// CATapMuteBehavior.CATapMutedWhenTapped
const MUTED_WHEN_TAPPED: isize = 2;

/// Values shared with the real-time audio callback. Atomics only: the
/// callback never locks, allocates or panics.
pub struct Shared {
    target_gain: AtomicU32,
    applied_gain: AtomicU32,
    tap_channels: AtomicU32,
    pub callbacks: AtomicU64,
    pub level: AtomicU32,
    /// Diagnostics for the self-test: loudest raw sample captured, channel layout seen.
    pub in_peak: AtomicU32,
    pub layout: AtomicU64,
}

impl Shared {
    pub fn set_gain(&self, g: f32) {
        self.target_gain.store(g.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    pub fn target_gain(&self) -> f32 {
        f32::from_bits(self.target_gain.load(Ordering::Relaxed))
    }

    pub fn applied_bits(&self) -> u32 {
        self.applied_gain.load(Ordering::Relaxed)
    }

    pub fn store_applied(&self, g: f32) {
        self.applied_gain.store(g.to_bits(), Ordering::Relaxed);
    }
}

pub struct Pipeline {
    pub device_uid: String,
    pub device_name: String,
    pub sample_rate: f64,
    pub started: Instant,
    pub shared: Arc<Shared>,
    /// The virtual speaker standing in as the Mac's output (for the native popup), if any.
    pub proxy_uid: Option<String>,
    /// Set when the sound comes from the ScreenKnob driver instead of a tap.
    ring: Option<crate::ring::RingPlayer>,
    tap_id: AudioObjectID,
    agg_id: AudioObjectID,
    proc_id: IOProcID,
    running: bool,
}

// The raw ids are only touched by the worker thread that owns the pipeline.
unsafe impl Send for Pipeline {}

fn cf_key(s: &str) -> CFType {
    CFString::new(s).as_CFType()
}

fn own_process_object() -> Option<u32> {
    let pid = std::process::id() as i32;
    ca::get_q::<u32, i32>(
        ca::kAudioObjectSystemObject,
        Addr::new(ca::PROP_PID_TO_PROCESS, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN),
        Some(&pid),
    )
    .filter(|&o| o != 0)
}

/// Creates the CATapDescription and the tap; returns (tap id, tap uid).
fn create_tap(dev: &Device) -> Result<(AudioObjectID, String), String> {
    let cls = AnyClass::get(c"CATapDescription")
        .ok_or("This Mac's system is too old for software volume (needs macOS 14.2 or newer).")?;
    let excluded: Vec<Retained<NSNumber>> = own_process_object().map(NSNumber::new_u32).into_iter().collect();
    let excluded = NSArray::from_retained_slice(&excluded);
    let uid = NSString::from_str(&dev.uid);
    unsafe {
        // A whole-system stereo tap (minus this app), as the original does. The
        // "one device only" tap is created fine but never delivers sound on macOS 27.
        let _ = &uid;
        let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
        let desc: Option<Retained<AnyObject>> =
            msg_send![alloc, initStereoGlobalTapButExcludeProcesses: &*excluded];
        let desc = desc.ok_or("Couldn't describe the sound tap.")?;
        let name = NSString::from_str("ScreenKnob");
        let _: () = msg_send![&*desc, setName: &*name];
        let _: () = msg_send![&*desc, setPrivate: true];
        let _: () = msg_send![&*desc, setMuteBehavior: MUTED_WHEN_TAPPED];
        let uuid: Option<Retained<AnyObject>> = msg_send![&*desc, UUID];
        let uuid = uuid.ok_or("The sound tap has no id.")?;
        let uuid_str: Option<Retained<NSString>> = msg_send![&*uuid, UUIDString];
        let tap_uid = uuid_str.ok_or("The sound tap has no id.")?.to_string();

        let mut tap_id: AudioObjectID = 0;
        let st = ca::AudioHardwareCreateProcessTap(Retained::as_ptr(&desc) as *mut c_void, &mut tap_id);
        if st != 0 || tap_id == 0 {
            return Err(format!("macOS refused the sound tap (error {st}). Allow \"System Audio Recording\" in Privacy & Security."));
        }
        Ok((tap_id, tap_uid))
    }
}

/// Where the sound comes from: the system tap.
enum Source<'a> {
    Tap(&'a str),
}

fn create_aggregate(dev: &Device, source: Source) -> Result<AudioObjectID, String> {
    let main = CFDictionary::from_CFType_pairs(&[(cf_key("uid"), cf_key(&dev.uid))]);
    let mut pairs = vec![
        (cf_key("name"), cf_key("ScreenKnob Output")),
        (cf_key("master"), cf_key(&dev.uid)),
        (cf_key("private"), CFNumber::from(1).as_CFType()),
        (cf_key("stacked"), CFNumber::from(0).as_CFType()),
    ];
    match source {
        Source::Tap(tap_uid) => {
            let tap = CFDictionary::from_CFType_pairs(&[
                (cf_key("uid"), cf_key(tap_uid)),
                (cf_key("drift"), CFNumber::from(1).as_CFType()),
            ]);
            pairs.push((cf_key("uid"), cf_key(&format!("{OWN_UID_PREFIX}{tap_uid}"))));
            pairs.push((cf_key("tapautostart"), CFNumber::from(1).as_CFType()));
            pairs.push((cf_key("subdevices"), CFArray::from_CFTypes(&[main]).as_CFType()));
            pairs.push((cf_key("taps"), CFArray::from_CFTypes(&[tap]).as_CFType()));
        }
    }
    let dict = CFDictionary::from_CFType_pairs(&pairs);
    let mut agg: AudioObjectID = 0;
    let st = unsafe { ca::AudioHardwareCreateAggregateDevice(dict.as_concrete_TypeRef() as *const c_void, &mut agg) };
    if st != 0 || agg == 0 {
        return Err(format!("Couldn't open the output for software volume (error {st})."));
    }
    Ok(agg)
}

impl Pipeline {
    pub fn start(dev: &Device, gain: f32) -> Result<Pipeline, String> {
        let mut p = Pipeline::empty(dev, gain);
        // On any failure `p` is dropped, which undoes whatever was created.
        let (tap_id, tap_uid) = create_tap(dev)?;
        p.tap_id = tap_id;
        let fmt: Option<ca::AudioStreamBasicDescription> =
            ca::get(tap_id, Addr::new(ca::PROP_TAP_FORMAT, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN));
        let tap_ch = fmt.map(|f| f.mChannelsPerFrame).filter(|&c| c > 0).unwrap_or(2);
        p.shared.tap_channels.store(tap_ch, Ordering::Relaxed);

        p.agg_id = create_aggregate(dev, Source::Tap(&tap_uid))?;
        p.run()?;
        Ok(p)
    }

    /// Plays what apps send to the ScreenKnob driver (`proxy`) on `dev`, read
    /// from shared memory: no capture, so no recording indicator.
    pub fn start_ring(dev: &Device, proxy: &Device, gain: f32) -> Result<Pipeline, String> {
        let mut p = Pipeline::empty(dev, gain);
        p.proxy_uid = Some(proxy.uid.clone());
        p.ring = Some(crate::ring::RingPlayer::start(dev, p.shared.clone())?);
        Ok(p)
    }

    pub fn uses_driver(&self) -> bool {
        self.ring.is_some()
    }

    fn empty(dev: &Device, gain: f32) -> Pipeline {
        let shared = Arc::new(Shared {
            target_gain: AtomicU32::new(gain.to_bits()),
            applied_gain: AtomicU32::new(gain.to_bits()),
            tap_channels: AtomicU32::new(2),
            callbacks: AtomicU64::new(0),
            level: AtomicU32::new(0),
            in_peak: AtomicU32::new(0),
            layout: AtomicU64::new(0),
        });
        Pipeline {
            device_uid: dev.uid.clone(),
            device_name: dev.name.clone(),
            sample_rate: dev.sample_rate,
            started: Instant::now(),
            shared,
            proxy_uid: None,
            ring: None,
            tap_id: 0,
            agg_id: 0,
            proc_id: std::ptr::null_mut(),
            running: false,
        }
    }

    fn run(&mut self) -> Result<(), String> {
        let p = self;
        let client = Arc::as_ptr(&p.shared) as *mut c_void;
        let mut proc_id: IOProcID = std::ptr::null_mut();
        let st = unsafe { ca::AudioDeviceCreateIOProcID(p.agg_id, io_proc, client, &mut proc_id) };
        if st != 0 || proc_id.is_null() {
            return Err(format!("Couldn't start the audio stream (error {st})."));
        }
        p.proc_id = proc_id;
        let st = unsafe { ca::AudioDeviceStart(p.agg_id, p.proc_id) };
        if st != 0 {
            return Err(format!("Couldn't start the audio stream (error {st})."));
        }
        p.running = true;
        Ok(())
    }

    fn teardown(&mut self) {
        self.ring = None; // stops its callback first
        unsafe {
            if self.running {
                let _ = ca::AudioDeviceStop(self.agg_id, self.proc_id);
                self.running = false;
            }
            if !self.proc_id.is_null() {
                let _ = ca::AudioDeviceDestroyIOProcID(self.agg_id, self.proc_id);
                self.proc_id = std::ptr::null_mut();
            }
            if self.agg_id != 0 {
                let _ = ca::AudioHardwareDestroyAggregateDevice(self.agg_id);
                self.agg_id = 0;
            }
            if self.tap_id != 0 {
                let _ = ca::AudioHardwareDestroyProcessTap(self.tap_id);
                self.tap_id = 0;
            }
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Finds global channel `ch` among `bufs`: (buffer index, offset in frame, channels per frame).
#[inline]
fn locate(bufs: &[ca::AudioBuffer], mut ch: u32) -> Option<(usize, usize, usize)> {
    for (i, b) in bufs.iter().enumerate() {
        if ch < b.mNumberChannels {
            return Some((i, ch as usize, b.mNumberChannels as usize));
        }
        ch -= b.mNumberChannels;
    }
    None
}

/// Real-time callback: copies the tapped sound to the output, scaled by the gain
/// (ramped across the buffer so changes never click).
extern "C" fn io_proc(
    dev: AudioObjectID,
    now: *const c_void,
    input: *const AudioBufferList,
    in_time: *const c_void,
    output: *mut AudioBufferList,
    out_time: *const c_void,
    client: *mut c_void,
) -> i32 {
    std::panic::catch_unwind(|| render(dev, now, input, in_time, output, out_time, client)).unwrap_or(0)
}

fn render(
    _dev: AudioObjectID,
    _now: *const c_void,
    input: *const AudioBufferList,
    _in_time: *const c_void,
    output: *mut AudioBufferList,
    _out_time: *const c_void,
    client: *mut c_void,
) -> i32 {
    if client.is_null() {
        return 0;
    }
    let shared = unsafe { &*(client as *const Shared) };
    shared.callbacks.fetch_add(1, Ordering::Relaxed);
    let ins = unsafe { AudioBufferList::buffers(input) };
    let outs = unsafe { AudioBufferList::buffers(output) };

    let total_in: u32 = ins.iter().map(|b| b.mNumberChannels).sum();
    let tap_ch = shared.tap_channels.load(Ordering::Relaxed).min(total_in);
    let tap_start = total_in - tap_ch;
    let total_out: u32 = outs.iter().map(|b| b.mNumberChannels).sum();
    shared.layout.store(
        (ins.len() as u64) << 48 | (total_in as u64) << 32 | (outs.len() as u64) << 16 | total_out as u64,
        Ordering::Relaxed,
    );
    let mut raw = 0.0f32;
    for b in ins {
        if !b.mData.is_null() {
            let v = unsafe { std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4) };
            for x in v {
                raw = raw.max(x.abs());
            }
        }
    }
    if raw > f32::from_bits(shared.in_peak.load(Ordering::Relaxed)) {
        shared.in_peak.store(raw.to_bits(), Ordering::Relaxed);
    }

    let start = f32::from_bits(shared.applied_gain.load(Ordering::Relaxed));
    let target = f32::from_bits(shared.target_gain.load(Ordering::Relaxed));
    let mut peak = 0.0f32;

    let mut out_ch_global = 0u32;
    for ob in outs {
        let och = ob.mNumberChannels as usize;
        if ob.mData.is_null() || och == 0 {
            out_ch_global += ob.mNumberChannels;
            continue;
        }
        let out = unsafe { std::slice::from_raw_parts_mut(ob.mData as *mut f32, ob.mDataByteSize as usize / 4) };
        let frames = out.len() / och;
        for c in 0..och {
            let oc = out_ch_global + c as u32;
            // Same channel if the tap has it; mono taps feed every channel; extra channels stay silent.
            let src = if tap_ch == 0 {
                None
            } else if oc < tap_ch {
                Some(oc)
            } else if tap_ch == 1 {
                Some(0)
            } else {
                None
            };
            let src = src.and_then(|s| locate(ins, tap_start + s));
            match src {
                Some((bi, off, stride)) if !ins[bi].mData.is_null() => {
                    let ib = &ins[bi];
                    let inp = unsafe {
                        std::slice::from_raw_parts(ib.mData as *const f32, ib.mDataByteSize as usize / 4)
                    };
                    let in_frames = inp.len() / stride.max(1);
                    let n = frames.min(in_frames);
                    let step = if frames > 0 { (target - start) / frames as f32 } else { 0.0 };
                    for f in 0..frames {
                        let v = if f < n {
                            let g = start + step * f as f32;
                            inp.get(f * stride + off).copied().unwrap_or(0.0) * g
                        } else {
                            0.0
                        };
                        let v = v.clamp(-1.0, 1.0);
                        peak = peak.max(v.abs());
                        if let Some(o) = out.get_mut(f * och + c) {
                            *o = v;
                        }
                    }
                }
                _ => {
                    for f in 0..frames {
                        if let Some(o) = out.get_mut(f * och + c) {
                            *o = 0.0;
                        }
                    }
                }
            }
        }
        out_ch_global += ob.mNumberChannels;
    }
    shared.applied_gain.store(target.to_bits(), Ordering::Relaxed);
    let prev = f32::from_bits(shared.level.load(Ordering::Relaxed));
    shared.level.store(peak.max(prev * 0.9).to_bits(), Ordering::Relaxed);
    0
}

